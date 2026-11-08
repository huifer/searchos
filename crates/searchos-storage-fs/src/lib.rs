//! Content-addressed filesystem object store (RFC-006 6.5, ADR-010).
//!
//! ```text
//! objects/
//!   └── sha256/
//!        └── ab/
//!             └── abcdef...
//! ```
//!
//! Refs stored in DB columns use the `sha256:<hex>` form (see
//! `searchos-document::hash`).

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Object store trait (implemented by the local filesystem store; S3-style
/// stores can implement the same port later, RFC Part 22 objects: local|s3).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Writes content, returning the `sha256:<hex>` ref.
    async fn put(&self, bytes: &[u8]) -> Result<String, StoreError>;

    /// Reads content by ref.
    async fn get(&self, reference: &str) -> Result<Vec<u8>, StoreError>;

    /// Deletes content by ref.
    async fn delete(&self, reference: &str) -> Result<(), StoreError>;

    /// Whether a ref exists.
    async fn exists(&self, reference: &str) -> Result<bool, StoreError>;
}

/// Storage failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid ref: {0}")]
    InvalidRef(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("object not found: {0}")]
    NotFound(String),
}

/// Local content-addressed store rooted at `objects/`.
pub struct LocalObjectStore {
    root: PathBuf,
}

impl LocalObjectStore {
    /// Store rooted at a directory (created on first write).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Ref → relative path segments (`sha256/ab/abcdef...`).
    fn path_for(&self, hex_hash: &str) -> Result<PathBuf, StoreError> {
        if hex_hash.len() != 64 || !hex_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidRef(hex_hash.to_string()));
        }
        Ok(self.root.join("sha256").join(&hex_hash[..2]).join(hex_hash))
    }

    /// Absolute path from a full ref.
    fn path_for_ref(&self, reference: &str) -> Result<PathBuf, StoreError> {
        let hex_hash = reference
            .strip_prefix("sha256:")
            .ok_or_else(|| StoreError::InvalidRef(reference.to_string()))?;
        self.path_for(hex_hash)
    }
}

#[async_trait]
impl ObjectStore for LocalObjectStore {
    async fn put(&self, bytes: &[u8]) -> Result<String, StoreError> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let hex_hash = hex::encode(hasher.finalize());
        let path = self.path_for(&hex_hash)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::Io(e.to_string()))?;
        }
        // Write atomically: temp file + rename.
        let tmp = path.with_extension("tmp");
        let mut f = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| StoreError::Io(e.to_string()))?;
        f.write_all(bytes)
            .await
            .map_err(|e| StoreError::Io(e.to_string()))?;
        f.sync_all()
            .await
            .map_err(|e| StoreError::Io(e.to_string()))?;
        drop(f);
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(format!("sha256:{hex_hash}"))
    }

    async fn get(&self, reference: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.path_for_ref(reference)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(reference.to_string()))
            }
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }

    async fn delete(&self, reference: &str) -> Result<(), StoreError> {
        let path = self.path_for_ref(reference)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // idempotent
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }

    async fn exists(&self, reference: &str) -> Result<bool, StoreError> {
        let path = self.path_for_ref(reference)?;
        Ok(Path::new(&path).exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> LocalObjectStore {
        let dir = std::env::temp_dir().join(format!(
            "searchos-fs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        LocalObjectStore::new(dir)
    }

    #[tokio::test]
    async fn put_get_roundtrip_content_addressed() {
        let s = store();
        let r1 = s.put(b"hello world").await.unwrap();
        let r2 = s.put(b"hello world").await.unwrap();
        assert_eq!(r1, r2, "same content → same ref");
        assert_eq!(s.get(&r1).await.unwrap(), b"hello world");
        assert!(r1.starts_with("sha256:"));
        assert!(s.exists(&r1).await.unwrap());
    }

    #[tokio::test]
    async fn path_fans_out_two_levels() {
        let s = store();
        let r = s.put(b"fanout").await.unwrap();
        let hex = &r["sha256:".len()..];
        let expected = s.root.join("sha256").join(&hex[..2]).join(hex);
        assert!(expected.exists());
    }

    #[tokio::test]
    async fn delete_is_idempotent_and_missing_is_error() {
        let s = store();
        let r = s.put(b"gone").await.unwrap();
        s.delete(&r).await.unwrap();
        s.delete(&r).await.unwrap(); // idempotent
        assert!(!s.exists(&r).await.unwrap());
        assert!(matches!(s.get(&r).await, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn invalid_ref_rejected() {
        let s = store();
        assert!(matches!(
            s.get("md5:abc").await,
            Err(StoreError::InvalidRef(_))
        ));
        assert!(matches!(
            s.get("sha256:zz").await,
            Err(StoreError::InvalidRef(_))
        ));
    }
}
