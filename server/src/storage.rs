//! File contents on disk, stored per vault under their SHA-256 hash.

use std::path::PathBuf;

use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// True for a lower-case, 64-character hex SHA-256 digest.
pub fn is_valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `<root>/<vault_id>/<first two hex chars>/<hash>`
    pub fn path(&self, vault_id: i64, hash: &str) -> PathBuf {
        self.root
            .join(vault_id.to_string())
            .join(&hash[..2])
            .join(hash)
    }

    pub async fn exists(&self, vault_id: i64, hash: &str) -> std::io::Result<bool> {
        tokio::fs::try_exists(self.path(vault_id, hash)).await
    }

    /// Checks the hash, then writes the file if it isn't already stored.
    /// Writes go to a temporary file first, so a crash never leaves a partial file.
    pub async fn put(&self, vault_id: i64, hash: &str, bytes: &[u8]) -> Result<(), ApiError> {
        if !is_valid_hash(hash) || sha256_hex(bytes) != hash {
            return Err(ApiError::HashMismatch);
        }
        let path = self.path(vault_id, hash);
        if tokio::fs::try_exists(&path).await? {
            return Ok(());
        }
        let dir = path.parent().expect("blob path has a parent");
        tokio::fs::create_dir_all(dir).await?;
        let mut suffix = [0u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut suffix);
        let tmp = dir.join(format!("{hash}.tmp-{}", hex::encode(suffix)));
        let mut file = tokio::fs::File::create(&tmp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    pub async fn read(&self, vault_id: i64, hash: &str) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.path(vault_id, hash)).await
    }

    pub async fn remove(&self, vault_id: i64, hash: &str) -> std::io::Result<()> {
        ignore_not_found(tokio::fs::remove_file(self.path(vault_id, hash)).await)
    }

    pub async fn remove_vault(&self, vault_id: i64) -> std::io::Result<()> {
        ignore_not_found(tokio::fs::remove_dir_all(self.root.join(vault_id.to_string())).await)
    }
}

fn ignore_not_found(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_hashes() {
        assert!(is_valid_hash(&sha256_hex(b"hello")));
        assert!(!is_valid_hash("abc"));
        assert!(!is_valid_hash(&sha256_hex(b"hello").to_uppercase()));
        assert!(!is_valid_hash(&"g".repeat(64)));
    }

    #[tokio::test]
    async fn put_read_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let hash = sha256_hex(b"note body");

        store.put(1, &hash, b"note body").await.unwrap();
        assert!(store.exists(1, &hash).await.unwrap());
        assert!(
            !store.exists(2, &hash).await.unwrap(),
            "contents are per vault"
        );
        assert_eq!(store.read(1, &hash).await.unwrap(), b"note body");

        // Storing the same content again is a no-op.
        store.put(1, &hash, b"note body").await.unwrap();

        store.remove(1, &hash).await.unwrap();
        assert!(!store.exists(1, &hash).await.unwrap());
        store.remove(1, &hash).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_wrong_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let wrong = sha256_hex(b"something else");
        let err = store.put(1, &wrong, b"note body").await.unwrap_err();
        assert!(matches!(err, ApiError::HashMismatch));
        assert!(!store.exists(1, &wrong).await.unwrap());
    }

    #[tokio::test]
    async fn remove_vault_deletes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let hash = sha256_hex(b"x");
        store.put(5, &hash, b"x").await.unwrap();
        store.remove_vault(5).await.unwrap();
        assert!(!store.exists(5, &hash).await.unwrap());
        store.remove_vault(5).await.unwrap();
    }
}
