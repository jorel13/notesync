//! File contents on disk, stored per vault under their SHA-256 hash.

use std::{
    io::SeekFrom,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

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

    /// Up to `len` bytes from `start`; fewer if the file ends first.
    pub async fn read_range(
        &self,
        vault_id: i64,
        hash: &str,
        start: u64,
        len: u64,
    ) -> std::io::Result<Vec<u8>> {
        let mut file = tokio::fs::File::open(self.path(vault_id, hash)).await?;
        file.seek(SeekFrom::Start(start)).await?;
        let mut bytes = Vec::with_capacity(len as usize);
        file.take(len).read_to_end(&mut bytes).await?;
        Ok(bytes)
    }

    // ---- Uploads sent in pieces ----
    //
    // Pieces are appended to `<root>/<vault_id>/uploads/<hash>.part`. When the
    // last one arrives, the file is checked against its hash and moved into
    // place. Callers serialize access to a part file.

    fn part_path(&self, vault_id: i64, hash: &str) -> PathBuf {
        self.root
            .join(vault_id.to_string())
            .join("uploads")
            .join(format!("{hash}.part"))
    }

    /// Bytes received so far for an unfinished upload (0 if none).
    pub async fn part_len(&self, vault_id: i64, hash: &str) -> std::io::Result<u64> {
        match tokio::fs::metadata(self.part_path(vault_id, hash)).await {
            Ok(meta) => Ok(meta.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Appends a piece that must start where the received bytes end. Returns
    /// the new number of bytes received.
    pub async fn append_part(
        &self,
        vault_id: i64,
        hash: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<u64, ApiError> {
        let received = self.part_len(vault_id, hash).await?;
        if offset != received {
            return Err(ApiError::WrongOffset(received));
        }
        let path = self.part_path(vault_id, hash);
        tokio::fs::create_dir_all(path.parent().expect("part path has a parent")).await?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(bytes).await?;
        file.sync_data().await?;
        Ok(received + bytes.len() as u64)
    }

    /// Checks a finished upload against its hash, reading it a little at a
    /// time. A mismatch discards it, so the client starts again.
    pub async fn verify_part(&self, vault_id: i64, hash: &str) -> Result<(), ApiError> {
        let path = self.part_path(vault_id, hash);
        let mut file = tokio::fs::File::open(&path).await?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        drop(file);
        if hex::encode(hasher.finalize()) != hash {
            ignore_not_found(tokio::fs::remove_file(&path).await)?;
            return Err(ApiError::HashMismatch);
        }
        Ok(())
    }

    /// Moves a verified upload into the store (or drops it if the content is
    /// already stored).
    pub async fn install_part(&self, vault_id: i64, hash: &str) -> std::io::Result<()> {
        let part = self.part_path(vault_id, hash);
        let path = self.path(vault_id, hash);
        if tokio::fs::try_exists(&path).await? {
            return ignore_not_found(tokio::fs::remove_file(&part).await);
        }
        tokio::fs::create_dir_all(path.parent().expect("blob path has a parent")).await?;
        tokio::fs::rename(&part, &path).await
    }

    /// Removes unfinished uploads not added to for `max_age`. Returns how many.
    pub async fn remove_stale_parts(&self, max_age: Duration) -> std::io::Result<u64> {
        let cutoff = SystemTime::now() - max_age;
        let mut removed = 0;
        let mut vaults = match tokio::fs::read_dir(&self.root).await {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        while let Some(vault) = vaults.next_entry().await? {
            let mut parts = match tokio::fs::read_dir(vault.path().join("uploads")).await {
                Ok(dir) => dir,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            while let Some(part) = parts.next_entry().await? {
                let modified = part.metadata().await?.modified()?;
                if modified <= cutoff {
                    ignore_not_found(tokio::fs::remove_file(part.path()).await)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
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
    async fn uploads_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let body = b"first piece, second piece";
        let hash = sha256_hex(body);

        assert_eq!(store.part_len(1, &hash).await.unwrap(), 0);
        assert_eq!(
            store.append_part(1, &hash, 0, &body[..12]).await.unwrap(),
            12
        );
        // A piece that doesn't continue where the last one ended is refused.
        let err = store.append_part(1, &hash, 5, b"x").await.unwrap_err();
        assert!(matches!(err, ApiError::WrongOffset(12)));
        assert_eq!(
            store.append_part(1, &hash, 12, &body[12..]).await.unwrap(),
            25
        );

        store.verify_part(1, &hash).await.unwrap();
        store.install_part(1, &hash).await.unwrap();
        assert_eq!(store.read(1, &hash).await.unwrap(), body);
        assert_eq!(store.part_len(1, &hash).await.unwrap(), 0);
        assert_eq!(store.read_range(1, &hash, 6, 5).await.unwrap(), b"piece");
        assert_eq!(store.read_range(1, &hash, 20, 100).await.unwrap(), b"piece");
    }

    #[tokio::test]
    async fn a_wrong_upload_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let hash = sha256_hex(b"expected");
        store
            .append_part(1, &hash, 0, b"something else")
            .await
            .unwrap();
        let err = store.verify_part(1, &hash).await.unwrap_err();
        assert!(matches!(err, ApiError::HashMismatch));
        assert_eq!(store.part_len(1, &hash).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn stale_parts_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().to_path_buf());
        let hash = sha256_hex(b"x");
        store.append_part(3, &hash, 0, b"half").await.unwrap();
        assert_eq!(
            store
                .remove_stale_parts(Duration::from_secs(3600))
                .await
                .unwrap(),
            0
        );
        assert_eq!(store.remove_stale_parts(Duration::ZERO).await.unwrap(), 1);
        assert_eq!(store.part_len(3, &hash).await.unwrap(), 0);
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
