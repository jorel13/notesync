use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{bail, Context};

/// A token bucket: up to `burst` at once, then one more every `every`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub burst: u32,
    pub every: Duration,
}

/// Server settings, read from `NOTESYNC_*` environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub max_file_bytes: u64,
    pub history_days: u64,
    /// A device token expires this many days after it was issued or last refreshed.
    pub token_days: u64,
    /// Sign-in attempts per username.
    pub login_rate: Rate,
    /// Sync runs (commits) per device.
    pub sync_rate: Rate,
    /// All other requests per device.
    pub request_rate: Rate,
}

impl Config {
    /// The defaults, with data in `data_dir`.
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().expect("valid default address"),
            data_dir: data_dir.into(),
            max_file_bytes: 100 * 1024 * 1024,
            history_days: 30,
            token_days: 30,
            login_rate: Rate {
                burst: 10,
                every: Duration::from_secs(5),
            },
            sync_rate: Rate {
                burst: 10,
                every: Duration::from_secs(5),
            },
            request_rate: Rate {
                burst: 600,
                every: Duration::from_millis(50),
            },
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::with_data_dir(env_or("NOTESYNC_DATA_DIR", "./data"));
        config.bind_addr = env_or("NOTESYNC_BIND_ADDR", "127.0.0.1:8080")
            .parse()
            .context("NOTESYNC_BIND_ADDR must be an address like 127.0.0.1:8080")?;
        config.max_file_bytes = number("NOTESYNC_MAX_FILE_MB", 100)? * 1024 * 1024;
        config.history_days = number("NOTESYNC_HISTORY_DAYS", 30)?;
        config.token_days = number("NOTESYNC_TOKEN_DAYS", 30)?;
        if config.token_days == 0 {
            bail!("NOTESYNC_TOKEN_DAYS must be at least 1");
        }
        config.login_rate = rate("NOTESYNC_LOGIN", config.login_rate)?;
        config.sync_rate = rate("NOTESYNC_SYNC", config.sync_rate)?;
        let per_second = number("NOTESYNC_REQUESTS_PER_SECOND", 20)?.max(1);
        config.request_rate = Rate {
            burst: number("NOTESYNC_REQUEST_BURST", 600)?.max(1) as u32,
            every: Duration::from_secs_f64(1.0 / per_second as f64),
        };
        Ok(config)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("notesync.db")
    }

    pub fn blob_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    pub fn token_lifetime_secs(&self) -> i64 {
        self.token_days as i64 * 86_400
    }

    /// How soon a device should ask for a new token: after a day, or halfway
    /// through a shorter lifetime.
    pub fn token_refresh_secs(&self) -> i64 {
        (self.token_lifetime_secs() / 2).min(86_400)
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn number(name: &str, default: u64) -> anyhow::Result<u64> {
    env_or(name, &default.to_string())
        .parse()
        .with_context(|| format!("{name} must be a whole number"))
}

/// Reads `<prefix>_BURST` and `<prefix>_EVERY_SECONDS`.
fn rate(prefix: &str, default: Rate) -> anyhow::Result<Rate> {
    let burst = number(&format!("{prefix}_BURST"), default.burst as u64)?;
    let every = number(&format!("{prefix}_EVERY_SECONDS"), default.every.as_secs())?;
    if burst == 0 || every == 0 {
        bail!("{prefix}_BURST and {prefix}_EVERY_SECONDS must be at least 1");
    }
    Ok(Rate {
        burst: burst as u32,
        every: Duration::from_secs(every),
    })
}
