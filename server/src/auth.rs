//! Passwords, device tokens, sign-in lockouts, rate limits and the
//! `AuthUser` extractor.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::{config::Rate, db::now, error::ApiError, state::AppState};

// ---- Passwords ----

/// Hashes with Argon2id and a random salt per password. The salt is stored
/// inside the resulting string.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("dummy-password").expect("hashing dummy"))
}

/// Runs password hashing off the async threads, a few at a time, so a burst
/// of sign-ins can't stall syncing or use a lot of memory.
async fn limited<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce() -> T + Send + 'static,
) -> anyhow::Result<T> {
    let _slot = state.hash_slots.acquire().await?;
    Ok(tokio::task::spawn_blocking(work).await?)
}

pub async fn hash_password_limited(state: &AppState, password: String) -> anyhow::Result<String> {
    limited(state, move || hash_password(&password)).await?
}

/// Checks a password. `hash` is `None` for an unknown user; the check then
/// takes just as long, so usernames can't be discovered by timing.
async fn verify_limited(
    state: &AppState,
    password: String,
    hash: Option<String>,
) -> anyhow::Result<bool> {
    limited(state, move || {
        let ok = verify_password(&password, hash.as_deref().unwrap_or(dummy_hash()));
        ok && hash.is_some()
    })
    .await
}

// ---- Sign-in failures and lockouts (stored in the database) ----

/// Failed sign-ins allowed within the window before the account is locked.
pub const MAX_FAILURES: i64 = 10;
pub const FAILURE_WINDOW_SECS: i64 = 15 * 60;
pub const LOCKOUT_SECS: i64 = 15 * 60;

pub fn limit_key(username: &str) -> String {
    username.to_lowercase()
}

/// Seconds left on a lockout, if the username is locked.
pub async fn locked_for(pool: &SqlitePool, key: &str) -> Result<Option<i64>, sqlx::Error> {
    let until: Option<i64> =
        sqlx::query_scalar("SELECT locked_until FROM login_failures WHERE username_key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    let now = now();
    Ok(until.filter(|&u| u > now).map(|u| u - now))
}

/// Counts a failure; returns true if it locked the username.
async fn record_failure(pool: &SqlitePool, key: &str) -> Result<bool, sqlx::Error> {
    let now = now();
    let failures: i64 = sqlx::query_scalar(
        "INSERT INTO login_failures (username_key, failures, window_start) VALUES (?1, 1, ?2)
         ON CONFLICT (username_key) DO UPDATE SET
             failures = CASE WHEN window_start <= ?2 - ?3 THEN 1 ELSE failures + 1 END,
             window_start = CASE WHEN window_start <= ?2 - ?3 THEN ?2 ELSE window_start END
         RETURNING failures",
    )
    .bind(key)
    .bind(now)
    .bind(FAILURE_WINDOW_SECS)
    .fetch_one(pool)
    .await?;
    if failures < MAX_FAILURES {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE login_failures SET locked_until = ?, failures = 0, window_start = ?
         WHERE username_key = ?",
    )
    .bind(now + LOCKOUT_SECS)
    .bind(now)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(true)
}

pub async fn clear_failures(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM login_failures WHERE username_key = ?")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

fn locked_error(secs: i64) -> ApiError {
    let minutes = (secs + 59) / 60;
    ApiError::RateLimited(format!(
        "Too many failed sign-ins for this account. Try again in {minutes} minute{}.",
        if minutes == 1 { "" } else { "s" }
    ))
}

/// Checks a password against a username's lockout and failure count.
/// `hash` is `None` for an unknown user. Logs failures and lockouts.
pub async fn check_password(
    state: &AppState,
    username: &str,
    password: &str,
    hash: Option<String>,
) -> Result<bool, ApiError> {
    let key = limit_key(username);
    if let Some(secs) = locked_for(&state.pool, &key).await? {
        return Err(locked_error(secs));
    }
    if verify_limited(state, password.to_string(), hash).await? {
        clear_failures(&state.pool, &key).await?;
        return Ok(true);
    }
    tracing::warn!(username = %key, "failed sign-in");
    if record_failure(&state.pool, &key).await? {
        tracing::warn!(username = %key, "account locked after repeated failed sign-ins");
        return Err(locked_error(LOCKOUT_SECS));
    }
    Ok(false)
}

// ---- Device tokens ----

/// A new random device token. Only its hash is stored.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("ns_{}", hex::encode(bytes))
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// How long the previous token keeps working after a refresh, in case the
/// device never received the new one.
pub const REFRESH_GRACE_SECS: i64 = 10 * 60;

#[derive(Debug, Serialize)]
pub struct IssuedToken {
    pub token: String,
    /// Unix seconds after which the token stops working.
    pub expires_at: i64,
    /// Unix seconds after which the device should ask for a new token.
    pub refresh_after: i64,
}

fn token_times(state: &AppState) -> (i64, i64, i64) {
    let now = now();
    (
        now,
        now + state.config.token_lifetime_secs(),
        now + state.config.token_refresh_secs(),
    )
}

/// Registers a new device for the user and returns its token.
pub async fn issue_for_new_device(
    state: &AppState,
    user_id: i64,
    device_name: &str,
) -> Result<(i64, IssuedToken), ApiError> {
    let token = new_token();
    let (now, expires_at, refresh_after) = token_times(state);
    let device_id = sqlx::query(
        "INSERT INTO devices (user_id, name, token_hash, created_at, last_seen, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(device_name)
    .bind(hash_token(&token))
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    Ok((
        device_id,
        IssuedToken {
            token,
            expires_at,
            refresh_after,
        },
    ))
}

/// Replaces a device's token. The old one keeps working for a few minutes.
pub async fn refresh(state: &AppState, device_id: i64) -> Result<IssuedToken, ApiError> {
    let token = new_token();
    let (now, expires_at, refresh_after) = token_times(state);
    sqlx::query(
        "UPDATE devices SET prev_token_hash = token_hash, prev_expires_at = ?,
                            token_hash = ?, expires_at = ?
         WHERE id = ?",
    )
    .bind(now + REFRESH_GRACE_SECS)
    .bind(hash_token(&token))
    .bind(expires_at)
    .bind(device_id)
    .execute(&state.pool)
    .await?;
    Ok(IssuedToken {
        token,
        expires_at,
        refresh_after,
    })
}

// ---- Rate limits (in memory) ----

/// Token buckets per key. Kept in memory: they protect the server from load,
/// so starting fresh after a restart is fine.
pub struct RateLimiter {
    rate: Rate,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one from the key's bucket, or returns how long until one is available.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        let per_sec = 1.0 / self.rate.every.as_secs_f64();
        let burst = self.rate.burst as f64;
        let now = Instant::now();
        let refill = |tokens: f64, at: Instant| {
            (tokens + now.duration_since(at).as_secs_f64() * per_sec).min(burst)
        };
        let mut buckets = self.buckets.lock().expect("rate limiter lock");
        if buckets.len() > 10_000 {
            // Full buckets carry no information; drop them.
            buckets.retain(|_, (tokens, at)| refill(*tokens, *at) < burst);
        }
        let bucket = buckets.entry(key.to_string()).or_insert((burst, now));
        bucket.0 = refill(bucket.0, bucket.1);
        bucket.1 = now;
        if bucket.0 >= 1.0 {
            bucket.0 -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.0) / per_sec))
        }
    }
}

/// Every rate limit the server applies.
pub struct Limits {
    /// Sign-in attempts per username.
    pub login: RateLimiter,
    /// Sign-in attempts across all usernames, which bounds password hashing
    /// work when someone tries many different usernames.
    pub login_total: RateLimiter,
    /// Commits per device.
    pub sync: RateLimiter,
    /// All authenticated requests per device.
    pub requests: RateLimiter,
}

impl Limits {
    pub fn new(config: &crate::Config) -> Self {
        Self {
            login: RateLimiter::new(config.login_rate),
            login_total: RateLimiter::new(Rate {
                burst: 30,
                every: Duration::from_secs(1),
            }),
            sync: RateLimiter::new(config.sync_rate),
            requests: RateLimiter::new(config.request_rate),
        }
    }
}

pub fn too_many(wait: Duration) -> ApiError {
    let secs = wait.as_secs().max(1);
    ApiError::RateLimited(format!(
        "Too many requests. Try again in {secs} second{}.",
        if secs == 1 { "" } else { "s" }
    ))
}

// ---- The signed-in device ----

/// The signed-in user and device, taken from the `Authorization: Bearer` header.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: i64,
    pub device_id: i64,
    pub username: String,
    pub is_admin: bool,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        let now = now();
        let row: Option<(i64, i64, String, i64, i64)> = sqlx::query_as(
            "SELECT d.id, u.id, u.username, u.is_admin, d.last_seen
             FROM devices d JOIN users u ON u.id = d.user_id
             WHERE d.revoked = 0 AND u.disabled = 0
               AND ((d.token_hash = ?1 AND d.expires_at > ?2)
                 OR (d.prev_token_hash = ?1 AND d.prev_expires_at > ?2))",
        )
        .bind(hash_token(token))
        .bind(now)
        .fetch_optional(&state.pool)
        .await?;
        let (device_id, user_id, username, is_admin, last_seen) =
            row.ok_or(ApiError::Unauthorized)?;
        state
            .limits
            .requests
            .check(&format!("device:{device_id}"))
            .map_err(too_many)?;
        // Once a minute is precise enough, and saves a write on every request.
        if now - last_seen >= 60 {
            sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
                .bind(now)
                .bind(device_id)
                .execute(&state.pool)
                .await?;
        }
        Ok(Self {
            user_id,
            device_id,
            username,
            is_admin: is_admin != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let hash = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
        assert!(!verify_password("correct horse", "not a hash"));
    }

    #[test]
    fn same_password_gets_different_salts() {
        assert_ne!(
            hash_password("same").unwrap(),
            hash_password("same").unwrap()
        );
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert!(a.starts_with("ns_"));
        assert_eq!(a.len(), 3 + 64);
        assert_eq!(hash_token(&a), hash_token(&a));
        assert_ne!(hash_token(&a), a);
    }

    #[test]
    fn rate_limiter_allows_a_burst_then_refills() {
        let limiter = RateLimiter::new(Rate {
            burst: 3,
            every: Duration::from_millis(20),
        });
        for _ in 0..3 {
            assert!(limiter.check("a").is_ok());
        }
        let wait = limiter.check("a").unwrap_err();
        assert!(wait <= Duration::from_millis(20));
        assert!(limiter.check("b").is_ok(), "keys are independent");
        std::thread::sleep(Duration::from_millis(30));
        assert!(limiter.check("a").is_ok());
    }
}
