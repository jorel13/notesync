//! Account management, used by the command line and the API.

use anyhow::{bail, Context};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{auth, db::now, state::AppState};

pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_CHARS: usize = 1024;

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
    insert_user(pool, username, &hash, is_admin).await
}

/// Adds a user whose password is already hashed and validated.
pub async fn insert_user(
    pool: &SqlitePool,
    username: &str,
    hash: &str,
    is_admin: bool,
) -> anyhow::Result<i64> {
    validate_username(username).map_err(anyhow::Error::msg)?;
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

pub async fn user_id(pool: &SqlitePool, username: &str) -> anyhow::Result<i64> {
    sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await?
        .with_context(|| format!("no user named {username:?}"))
}

/// Sets a new password and signs the user out everywhere.
pub async fn set_password(pool: &SqlitePool, username: &str, password: &str) -> anyhow::Result<()> {
    validate_password(password).map_err(anyhow::Error::msg)?;
    let hash = auth::hash_password(password)?;
    set_password_hash(pool, username, &hash).await
}

/// Like `set_password`, with a password that's already validated and hashed.
pub async fn set_password_hash(
    pool: &SqlitePool,
    username: &str,
    hash: &str,
) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
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
    end_admin_sessions(&mut tx, id).await?;
    tx.commit().await?;
    Ok(())
}

/// Signs a user out of the admin page.
pub async fn end_admin_sessions(
    conn: &mut sqlx::SqliteConnection,
    user_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM admin_sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(conn)
        .await?;
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
        end_admin_sessions(&mut tx, id).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removing admin rights also signs the user out of the admin page.
pub async fn set_admin(pool: &SqlitePool, username: &str, is_admin: bool) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET is_admin = ? WHERE id = ?")
        .bind(is_admin as i64)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if !is_admin {
        end_admin_sessions(&mut tx, id).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserSummary {
    pub id: i64,
    pub username: String,
    pub is_admin: i64,
    pub disabled: i64,
    pub created_at: i64,
    /// Devices with a token that still works.
    pub devices: i64,
    /// Unix seconds until which sign-in is locked, or 0.
    pub locked_until: i64,
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<UserSummary>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username, u.is_admin, u.disabled, u.created_at,
                (SELECT COUNT(*) FROM devices d
                 WHERE d.user_id = u.id AND d.revoked = 0 AND d.expires_at > ?1) AS devices,
                COALESCE((SELECT f.locked_until FROM login_failures f
                          WHERE f.username_key = lower(u.username) AND f.locked_until > ?1), 0)
                    AS locked_until
         FROM users u ORDER BY u.username",
    )
    .bind(now())
    .fetch_all(pool)
    .await?)
}

/// Clears a sign-in lockout and the failure count.
pub async fn unlock(pool: &SqlitePool, username: &str) -> anyhow::Result<()> {
    user_id(pool, username).await?;
    auth::clear_failures(pool, &auth::limit_key(username)).await?;
    Ok(())
}

/// Deletes a user with all their vaults, stored files and devices. This
/// can't be undone. Returns how many vaults were removed. The only admin
/// can't be deleted, so the server always keeps one.
pub async fn delete_user(state: &AppState, username: &str) -> anyhow::Result<usize> {
    let id = user_id(&state.pool, username).await?;
    let is_admin: i64 = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if is_admin != 0 {
        let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1")
            .fetch_one(&state.pool)
            .await?;
        if admins <= 1 {
            bail!("{username} is the only admin; make someone else an admin first");
        }
    }
    // Same lock as commits and cleanup, so no upload lands in a vault
    // while its folder is being removed.
    let _guard = state.write_lock.lock().await;
    let vaults: Vec<i64> = sqlx::query_scalar("SELECT id FROM vaults WHERE owner_id = ?")
        .bind(id)
        .fetch_all(&state.pool)
        .await?;
    let mut tx = state.pool.begin().await?;
    // Devices, vaults, their history and admin sessions go with the user.
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM login_failures WHERE username_key = ?")
        .bind(auth::limit_key(username))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    for vault in &vaults {
        state.blobs.remove_vault(*vault).await?;
    }
    Ok(vaults.len())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DeviceSummary {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub expires_at: i64,
}

/// A user's signed-in devices whose token still works.
pub async fn devices(pool: &SqlitePool, username: &str) -> anyhow::Result<Vec<DeviceSummary>> {
    let id = user_id(pool, username).await?;
    Ok(sqlx::query_as(
        "SELECT id, name, created_at, last_seen, expires_at FROM devices
         WHERE user_id = ? AND revoked = 0 AND expires_at > ? ORDER BY last_seen DESC",
    )
    .bind(id)
    .bind(now())
    .fetch_all(pool)
    .await?)
}

/// Signs one of a user's devices out.
pub async fn revoke_device(
    pool: &SqlitePool,
    username: &str,
    device_id: i64,
) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
    let done =
        sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ? AND user_id = ? AND revoked = 0")
            .bind(device_id)
            .bind(id)
            .execute(pool)
            .await?;
    if done.rows_affected() == 0 {
        bail!("{username} has no signed-in device with ID {device_id}");
    }
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct VaultSummary {
    pub id: i64,
    pub name: String,
    pub head_seq: i64,
    pub deleted_at: Option<i64>,
    /// Current files and their total size.
    pub files: i64,
    pub bytes: i64,
}

/// A user's vaults; with `deleted`, the deleted ones that can still be restored.
pub async fn vaults(
    pool: &SqlitePool,
    username: &str,
    deleted: bool,
) -> anyhow::Result<Vec<VaultSummary>> {
    let id = user_id(pool, username).await?;
    Ok(sqlx::query_as(
        "SELECT v.id, v.name, v.head_seq, v.deleted_at,
                (SELECT COUNT(*) FROM current_files f WHERE f.vault_id = v.id) AS files,
                (SELECT COALESCE(SUM(f.size), 0) FROM current_files f WHERE f.vault_id = v.id)
                    AS bytes
         FROM vaults v
         WHERE v.owner_id = ? AND (v.deleted_at IS NOT NULL) = ? ORDER BY v.name",
    )
    .bind(id)
    .bind(deleted)
    .fetch_all(pool)
    .await?)
}

/// Undoes a vault delete, if cleanup hasn't removed it yet.
pub async fn restore_vault(pool: &SqlitePool, username: &str, vault_id: i64) -> anyhow::Result<()> {
    let id = user_id(pool, username).await?;
    let result = sqlx::query(
        "UPDATE vaults SET deleted_at = NULL
         WHERE id = ? AND owner_id = ? AND deleted_at IS NOT NULL",
    )
    .bind(vault_id)
    .bind(id)
    .execute(pool)
    .await;
    match result {
        Ok(done) if done.rows_affected() == 1 => Ok(()),
        Ok(_) => bail!("{username} has no deleted vault with ID {vault_id}"),
        Err(e) if crate::db::is_unique_violation(&e) => {
            bail!("{username} already has a vault with that name; rename it first")
        }
        Err(e) => Err(e.into()),
    }
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
