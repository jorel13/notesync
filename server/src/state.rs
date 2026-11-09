use std::sync::Arc;

use anyhow::Context;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Semaphore};

use crate::{auth::Limits, config::Config, db, storage::BlobStore};

/// Password checks allowed at the same time. Each uses about 19 MB.
const HASH_SLOTS: usize = 2;

/// Shared state for request handlers and maintenance tasks.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub blobs: BlobStore,
    /// Serializes commits, upload bookkeeping and cleanup, so cleanup never
    /// removes content that a commit is about to reference.
    pub write_lock: Arc<Mutex<()>>,
    pub limits: Arc<Limits>,
    pub hash_slots: Arc<Semaphore>,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(config.blob_dir())
            .await
            .with_context(|| format!("creating {}", config.blob_dir().display()))?;
        let pool = db::connect(&config.db_path()).await?;
        Ok(Self {
            pool,
            blobs: BlobStore::new(config.blob_dir()),
            limits: Arc::new(Limits::new(&config)),
            config: Arc::new(config),
            write_lock: Arc::new(Mutex::new(())),
            hash_slots: Arc::new(Semaphore::new(HASH_SLOTS)),
        })
    }
}
