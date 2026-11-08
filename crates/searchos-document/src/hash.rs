//! Content hashing and content-addressed references (RFC-006 6.5).

use sha2::{Digest, Sha256};

/// Prefix for object-store refs.
pub const CONTENT_REF_PREFIX: &str = "sha256:";

/// SHA-256 hex digest of content bytes.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Content-addressed storage path segments per RFC-006 6.5:
/// `objects/sha256/<ab>/<abcdef...>` (first two hex chars as the fan-out dir).
pub fn object_path(hex_hash: &str) -> String {
    format!("sha256/{}/{}", &hex_hash[..2], hex_hash)
}

/// Full ref stored in DB columns: `sha256:<hex>`.
pub fn content_ref(bytes: &[u8]) -> String {
    format!("{}{}", CONTENT_REF_PREFIX, content_hash(bytes))
}

/// Parses a `sha256:<hex>` ref into (hex, object path).
pub fn parse_ref(r: &str) -> Option<(String, String)> {
    let hex_hash = r.strip_prefix(CONTENT_REF_PREFIX)?;
    if hex_hash.len() != 64 || !hex_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((hex_hash.to_string(), object_path(hex_hash)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_sha256() {
        assert_eq!(
            content_hash(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn object_path_fans_out() {
        let h = content_hash(b"hello");
        assert_eq!(object_path(&h), format!("sha256/2c/{}", h));
    }

    #[test]
    fn ref_roundtrip() {
        let r = content_ref(b"hello");
        let (hex, path) = parse_ref(&r).unwrap();
        assert_eq!(hex, content_hash(b"hello"));
        assert_eq!(path, format!("sha256/2c/{}", hex));
        assert!(parse_ref("sha256:zz").is_none());
        assert!(parse_ref("md5:abc").is_none());
    }
}
