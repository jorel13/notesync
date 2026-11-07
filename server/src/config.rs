use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;

/// Server settings, read from `NOTESYNC_*` environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub max_file_bytes: u64,
    pub history_days: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind_addr = env_or("NOTESYNC_BIND_ADDR", "127.0.0.1:8080")
            .parse()
            .context("NOTESYNC_BIND_ADDR must be an address like 127.0.0.1:8080")?;
        let data_dir = PathBuf::from(env_or("NOTESYNC_DATA_DIR", "./data"));
        let max_file_mb: u64 = env_or("NOTESYNC_MAX_FILE_MB", "100")
            .parse()
            .context("NOTESYNC_MAX_FILE_MB must be a whole number")?;
        let history_days = env_or("NOTESYNC_HISTORY_DAYS", "30")
            .parse()
            .context("NOTESYNC_HISTORY_DAYS must be a whole number")?;
        Ok(Self {
            bind_addr,
            data_dir,
            max_file_bytes: max_file_mb * 1024 * 1024,
            history_days,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("notesync.db")
    }

    pub fn blob_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}
