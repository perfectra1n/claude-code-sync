//! SHA-256 digests, hex-encoded: one helper for every caller (artifact base
//! records, release self-update verification) so they cannot drift apart.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    // One preallocated string, written into: this runs over every
    // artifact byte on every sync, and a format! per digest byte meant
    // millions of throwaway Strings for a single large file.
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn matches_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
