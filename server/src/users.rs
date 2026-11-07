//! Account management, used by the command line and the API.

use anyhow::{bail, Context};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{auth, db::now};

pub const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_CHARS: usize = 1024;

pub fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() || username.len() > 64 {
        return Err("username must be 1 to 64 characters".into());
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("username may only contain letters, digits, '.', '_' and '-'".into());
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), String> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD_CHARS {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    if chars > MAX_PASSWORD_CHARS {
        return Err("password is too long".into());
    }
    Ok(())
}

pub async fn create_user(
    pool: &SqlitePool,
    username: &str,
    password: &str,
    is_admin: bool,
) -> anyhow::Result<i64> {
    validate_username(username).map_err(anyhow::Error::msg)?;
    validate_password(password).map_err(anyhow::Error::msg)?;
    let hash = auth::hash_password(password)?;
    let result = sqlx::query(
        "INSERT INTO users (username, password_hash, is_admin, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(username)
    .bind(hash)
    .bind(is_admin as i64)
    .bind(now())
    .execute(pool)
    .await;
    match result {
        Ok(done) => Ok(done.last_insert_rowid()),
        Err(e) if crate::db::is_unique_violation(&e) => bail!("user {username:?} already exists"),
        Err(e) => Err(e.into()),
    }
}

async fn user_id(pool: &SqlitePool, username: &str) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await?
        .with_context(|| format!("no user named {username:?}"))
}

/// Sets a new password and signs the user out everywhere.
pub async fn set_password(pool: &SqlitePool, username: &str, password: &str) -> anyhow::Result<()> {
    validate_password(password).map_err(anyhow::Error::msg)?;
    let id = user_id(pool, username).await?;
    let hash = auth::hash_password(password)?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE devices SET revoked = 1 WHERE user_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Disabling a user also signs them out on every device.
pub async fn set_disabled(pool: &SqlitePool, username: &str, disabled: bool) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
        .bind(disabled as i64)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if disabled {
        sqlx::query("UPDATE devices SET revoked = 1 WHERE user_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_admin(pool: &SqlitePool, username: &str, is_admin: bool) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
    sqlx::query("UPDATE users SET is_admin = ? WHERE id = ?")
        .bind(is_admin as i64)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserSummary {
    pub id: i64,
    pub username: String,
    pub is_admin: i64,
    pub disabled: i64,
    pub created_at: i64,
    pub devices: i64,
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<UserSummary>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username, u.is_admin, u.disabled, u.created_at,
                (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.revoked = 0) AS devices
         FROM users u ORDER BY u.username",
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rules() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("a.b_c-1").is_ok());
        assert!(validate_username("").is_err());
        assert!(validate_username("has space").is_err());
        assert!(validate_username(&"x".repeat(65)).is_err());
    }

    #[test]
    fn password_rules() {
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("1234567").is_err());
        assert!(validate_password(&"x".repeat(1025)).is_err());
    }
}
