//! Cryptographically secure random strings for tokens and session ids.

use crate::hashing::to_hex;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `len` random characters from `[0-9A-Za-z]`.
pub fn base62(len: usize) -> String {
    (0..len)
        .map(|_| BASE62[rand::random_range(0..BASE62.len())] as char)
        .collect()
}

/// `bytes` random bytes as lowercase hex (`2 * bytes` characters).
pub fn hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    to_hex(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base62_has_the_requested_length_and_charset() {
        let s = base62(64);
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(base62(32), base62(32));
    }

    #[test]
    fn hex_has_two_chars_per_byte() {
        let s = hex(32);
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hex(16), hex(16));
    }
}
