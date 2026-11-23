//! AES-256-GCM encryption for sensitive runtime settings stored in SQLite.
//!
//! Keys are derived from the server's master secret using HMAC-SHA256 with a
//! fixed domain-separation label, and each secret binds its setting key/purpose
//! as AEAD associated data so ciphertexts cannot be transplanted between keys.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;

use crate::error::{Result, ServiceError};

type HmacSha256 = Hmac<Sha256>;

const KEY_CONTEXT: &[u8] = b"paperjam-settings-enc-v1\0";
const VERSION_PREFIX: &str = "v1:";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

fn derive_key(master_key: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(master_key).expect("HMAC accepts any key length");
    mac.update(KEY_CONTEXT);
    mac.finalize().into_bytes().into()
}

fn decrypt_failed() -> ServiceError {
    ServiceError::internal(
        "secret_decrypt_failed",
        "stored secret could not be decrypted",
    )
}

/// Encrypts `plaintext` with AES-256-GCM using a key derived from `master_key`
/// and `purpose` bound as authenticated associated data.
pub fn encrypt_secret(master_key: &[u8], purpose: &str, plaintext: &str) -> Result<String> {
    let key = derive_key(master_key);
    let cipher = Aes256Gcm::new((&key).into());

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: purpose.as_bytes(),
            },
        )
        .map_err(|_| ServiceError::internal("secret_encrypt_failed", "could not encrypt secret"))?;

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(format!("{VERSION_PREFIX}{}", BASE64.encode(&combined)))
}

/// Decrypts a `"v1:"`-prefixed AES-256-GCM secret and verifies `purpose` AAD.
pub fn decrypt_secret(master_key: &[u8], purpose: &str, stored: &str) -> Result<String> {
    let encoded = stored
        .strip_prefix(VERSION_PREFIX)
        .ok_or_else(decrypt_failed)?;
    let bytes = BASE64.decode(encoded).map_err(|_| decrypt_failed())?;
    if bytes.len() < NONCE_LEN + TAG_LEN {
        return Err(decrypt_failed());
    }

    let (nonce_bytes, ciphertext) = bytes.split_at(NONCE_LEN);
    let nonce_arr: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| decrypt_failed())?;
    let nonce = Nonce::from(nonce_arr);

    let key = derive_key(master_key);
    let cipher = Aes256Gcm::new((&key).into());

    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: purpose.as_bytes(),
            },
        )
        .map_err(|_| decrypt_failed())?;

    String::from_utf8(plaintext).map_err(|_| decrypt_failed())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER_KEY: &[u8] = b"test-master-secret-key-0123456789abcdef";

    #[test]
    fn roundtrip_succeeds() {
        for plaintext in ["", "sk-or-v1-abc123", "unicode-секрет-🔐"] {
            let stored = encrypt_secret(MASTER_KEY, "llm_api_key", plaintext).unwrap();
            assert!(stored.starts_with("v1:"));
            let back = decrypt_secret(MASTER_KEY, "llm_api_key", &stored).unwrap();
            assert_eq!(back, plaintext);
        }
    }

    #[test]
    fn wrong_master_key_fails() {
        let stored = encrypt_secret(MASTER_KEY, "llm_api_key", "secret-value").unwrap();
        let err = decrypt_secret(b"other-master-key-0123456789abcdef", "llm_api_key", &stored)
            .unwrap_err();
        assert_eq!(err.code, "secret_decrypt_failed");
    }

    #[test]
    fn wrong_purpose_aad_fails() {
        let stored = encrypt_secret(MASTER_KEY, "llm_api_key", "secret-value").unwrap();
        let err = decrypt_secret(MASTER_KEY, "other_setting", &stored).unwrap_err();
        assert_eq!(err.code, "secret_decrypt_failed");
    }

    #[test]
    fn tampered_or_malformed_ciphertext_fails() {
        let stored = encrypt_secret(MASTER_KEY, "llm_api_key", "secret-value").unwrap();
        let encoded = stored.strip_prefix("v1:").unwrap();
        let mut bytes = BASE64.decode(encoded).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        let tampered = format!("v1:{}", BASE64.encode(bytes));
        assert_eq!(
            decrypt_secret(MASTER_KEY, "llm_api_key", &tampered)
                .unwrap_err()
                .code,
            "secret_decrypt_failed"
        );
        assert_eq!(
            decrypt_secret(MASTER_KEY, "llm_api_key", "v2:AAAA")
                .unwrap_err()
                .code,
            "secret_decrypt_failed"
        );
        assert_eq!(
            decrypt_secret(MASTER_KEY, "llm_api_key", "v1:short")
                .unwrap_err()
                .code,
            "secret_decrypt_failed"
        );
    }

    #[test]
    fn encrypting_twice_produces_different_ciphertexts() {
        let a = encrypt_secret(MASTER_KEY, "llm_api_key", "same-secret").unwrap();
        let b = encrypt_secret(MASTER_KEY, "llm_api_key", "same-secret").unwrap();
        assert_ne!(a, b);
    }
}
