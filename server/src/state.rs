use std::{sync::Arc, time::Duration};

use anyhow::Context;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::{auth::RateLimiter, config::Config, db, storage::BlobStore};

/// Shared state for request handlers and maintenance tasks.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub blobs: BlobStore,
    /// Serializes commits, upload bookkeeping and cleanup, so cleanup never
    /// removes content that a commit is about to reference.
    pub write_lock: Arc<Mutex<()>>,
    pub login_limiter: Arc<RateLimiter>,
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
            config: Arc::new(config),
            write_lock: Arc::new(Mutex::new(())),
            login_limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(15 * 60))),
        })
    }
}
