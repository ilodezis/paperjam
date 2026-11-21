//! HMAC-SHA256 signatures keyed by `PAPERJAM_SECRET_KEY`, domain-separated by
//! a purpose string so a signature minted for one use is useless for another.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::hashing::to_hex;

type HmacSha256 = Hmac<Sha256>;

fn mac(key: &[u8], purpose: &str, data: &str) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(purpose.as_bytes());
    mac.update(&[0]);
    mac.update(data.as_bytes());
    mac
}

/// Lowercase hex signature of `data` for `purpose`.
pub fn sign(key: &[u8], purpose: &str, data: &str) -> String {
    to_hex(&mac(key, purpose, data).finalize().into_bytes())
}

/// Constant-time check of a hex signature.
pub fn verify(key: &[u8], purpose: &str, data: &str, signature_hex: &str) -> bool {
    let Some(sig) = from_hex(signature_hex) else {
        return false;
    };
    mac(key, purpose, data).verify_slice(&sig).is_ok()
}

/// Constant-time check of the first bytes of a hex signature: `prefix_hex`
/// is the leading part of what [`sign`] gives, at least 16 bytes of it.
pub fn verify_prefix(key: &[u8], purpose: &str, data: &str, prefix_hex: &str) -> bool {
    let Some(prefix) = from_hex(prefix_hex) else {
        return false;
    };
    prefix.len() >= 16
        && mac(key, purpose, data)
            .verify_truncated_left(&prefix)
            .is_ok()
}

/// Constant-time comparison of two byte slices of arbitrary length.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    const KEY: &[u8] = b"paperjam-constant-time-eq";
    let mut ma = HmacSha256::new_from_slice(KEY).expect("HMAC accepts any key length");
    ma.update(a);
    let expected = ma.finalize().into_bytes();
    let mut mb = HmacSha256::new_from_slice(KEY).expect("HMAC accepts any key length");
    mb.update(b);
    mb.verify_slice(&expected).is_ok()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn a_signature_prefix_verifies_and_a_short_or_wrong_one_does_not() {
        let sig = sign(KEY, "share", "abc");
        assert!(verify_prefix(KEY, "share", "abc", &sig[..32]));
        assert!(verify_prefix(KEY, "share", "abc", &sig));
        assert!(!verify_prefix(KEY, "share", "abc", &sig[..30]));
        assert!(!verify_prefix(KEY, "share", "abd", &sig[..32]));
        assert!(!verify_prefix(KEY, "other", "abc", &sig[..32]));
    }

    #[test]
    fn sign_then_verify() {
        let sig = sign(KEY, "session", "abc");
        assert!(verify(KEY, "session", "abc", &sig));
    }

    #[test]
    fn wrong_data_purpose_key_or_garbage_fails() {
        let sig = sign(KEY, "session", "abc");
        assert!(!verify(KEY, "session", "abd", &sig));
        assert!(!verify(KEY, "download", "abc", &sig));
        assert!(!verify(
            b"another key another key another!",
            "session",
            "abc",
            &sig
        ));
        assert!(!verify(KEY, "session", "abc", "zz"));
        assert!(!verify(KEY, "session", "abc", ""));
        assert!(!verify(KEY, "session", "abc", &sig[..sig.len() - 2]));
    }

    #[test]
    fn constant_time_eq_compares_bytes_and_lengths() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(
            b"secret-token-value",
            b"secret-token-value"
        ));
        assert!(!constant_time_eq(
            b"secret-token-value",
            b"secret-token-valuX"
        ));
        assert!(!constant_time_eq(b"secret-token", b"secret-token-value"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
    }
}
