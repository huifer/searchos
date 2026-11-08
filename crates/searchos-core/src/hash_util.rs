//! Hash utilities shared with tests and binaries.

use sha2::{Digest, Sha256};

/// SHA-256 hex of bytes (content hash, RFC-006 6.4).
pub fn content_hash_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_is_stable() {
        assert_eq!(
            super::content_hash_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
