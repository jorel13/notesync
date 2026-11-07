//! Passwords, device tokens, login rate limiting and the `AuthUser` extractor.

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
use sha2::{Digest, Sha256};

use crate::{db::now, error::ApiError, state::AppState};

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

/// Takes as long as a real password check, so unknown usernames can't be
/// detected by timing.
pub fn verify_dummy(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let hash = DUMMY.get_or_init(|| hash_password("dummy-password").expect("hashing dummy"));
    let _ = verify_password(password, hash);
}

/// A new random device token. Only its hash is stored.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("ns_{}", hex::encode(bytes))
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Counts failed attempts per key within a time window.
pub struct RateLimiter {
    max_failures: u32,
    window: Duration,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            max_failures,
            window,
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_blocked(&self, key: &str) -> bool {
        let mut failures = self.failures.lock().expect("rate limiter lock");
        match failures.get(key) {
            Some(&(count, start)) if start.elapsed() < self.window => count >= self.max_failures,
            Some(_) => {
                failures.remove(key);
                false
            }
            None => false,
        }
    }

    pub fn record_failure(&self, key: &str) {
        let mut failures = self.failures.lock().expect("rate limiter lock");
        if failures.len() > 10_000 {
            let window = self.window;
            failures.retain(|_, (_, start)| start.elapsed() < window);
        }
        let entry = failures
            .entry(key.to_string())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub fn clear(&self, key: &str) {
        self.failures.lock().expect("rate limiter lock").remove(key);
    }
}

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
        let row: Option<(i64, i64, String, i64)> = sqlx::query_as(
            "SELECT d.id, u.id, u.username, u.is_admin
             FROM devices d JOIN users u ON u.id = d.user_id
             WHERE d.token_hash = ? AND d.revoked = 0 AND u.disabled = 0",
        )
        .bind(hash_token(token))
        .fetch_optional(&state.pool)
        .await?;
        let (device_id, user_id, username, is_admin) = row.ok_or(ApiError::Unauthorized)?;
        sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ?")
            .bind(now())
            .bind(device_id)
            .execute(&state.pool)
            .await?;
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
    fn rate_limiter_blocks_after_max_failures() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        for _ in 0..3 {
            assert!(!limiter.is_blocked("user:alice"));
            limiter.record_failure("user:alice");
        }
        assert!(limiter.is_blocked("user:alice"));
        assert!(!limiter.is_blocked("user:bob"));
        limiter.clear("user:alice");
        assert!(!limiter.is_blocked("user:alice"));
    }

    #[test]
    fn rate_limiter_window_expires() {
        let limiter = RateLimiter::new(1, Duration::from_millis(10));
        limiter.record_failure("k");
        assert!(limiter.is_blocked("k"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!limiter.is_blocked("k"));
    }
}
