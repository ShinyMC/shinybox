//! Random identifiers and secrets.
//!
//! Everything here is drawn from the thread-local CSPRNG, so share codes and
//! tokens cannot be guessed or enumerated.

use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};

/// Length of a public share code: 62^10 ≈ 8 × 10^17 possibilities.
pub const SHARE_CODE_LEN: usize = 10;
const FILE_ID_LEN: usize = 16;
const SECRET_LEN: usize = 32;

pub fn share_code() -> String {
    random_string(SHARE_CODE_LEN)
}

pub fn file_id() -> String {
    random_string(FILE_ID_LEN)
}

pub fn secret() -> String {
    random_string(SECRET_LEN)
}

/// Whether `value` could be a share code. Used to reject junk before it reaches the database.
pub fn is_share_code(value: &str) -> bool {
    value.len() == SHARE_CODE_LEN && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Hex-encoded SHA-256, used to store secrets without keeping them in plain text.
pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Compares two secrets without leaking their length or common prefix through timing.
pub fn secrets_match(given: &str, expected: &str) -> bool {
    let (given, expected) = (Sha256::digest(given), Sha256::digest(expected));
    given
        .iter()
        .zip(expected.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn random_string(len: usize) -> String {
    Alphanumeric.sample_string(&mut rand::rng(), len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_codes_are_well_formed_and_distinct() {
        let (a, b) = (share_code(), share_code());
        assert!(is_share_code(&a));
        assert_ne!(a, b);
    }

    #[test]
    fn rejects_malformed_share_codes() {
        assert!(!is_share_code("short"));
        assert!(!is_share_code("../../etc/"));
        assert!(!is_share_code("abcdefghijk"));
    }

    #[test]
    fn hash_is_stable_hex() {
        assert_eq!(
            hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn compares_secrets() {
        assert!(secrets_match("token", "token"));
        assert!(!secrets_match("token", "tokem"));
        assert!(!secrets_match("token", "token-longer"));
    }
}
