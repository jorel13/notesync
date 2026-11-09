use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::{
    account::confirm_password,
    extract::{ApiJson, ApiPath},
};
use crate::{
    auth::AuthUser,
    db::{is_unique_violation, now},
    error::ApiError,
    state::AppState,
};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct VaultInfo {
    id: i64,
    name: String,
    head_seq: i64,
    created_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct VaultNameRequest {
    name: String,
}

/// Returns `NotFound` unless the vault exists and belongs to the user, so
/// other users' vaults are indistinguishable from missing ones.
pub(crate) async fn owned_vault(
    pool: &SqlitePool,
    user_id: i64,
    vault_id: i64,
) -> Result<(), ApiError> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM vaults WHERE id = ? AND owner_id = ? AND deleted_at IS NULL",
    )
    .bind(vault_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    found.map(|_| ()).ok_or(ApiError::NotFound)
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    let chars = name.chars().count();
    if chars == 0 || chars > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::InvalidRequest(
            "vault name must be 1 to 100 characters with no control characters".into(),
        ));
    }
    Ok(name.to_string())
}

async fn load(pool: &SqlitePool, vault_id: i64) -> Result<VaultInfo, ApiError> {
    Ok(
        sqlx::query_as("SELECT id, name, head_seq, created_at FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_one(pool)
            .await?,
    )
}

pub async fn list(
    user: AuthUser,
    State(app): State<AppState>,
) -> Result<Json<Vec<VaultInfo>>, ApiError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id, name, head_seq, created_at FROM vaults
             WHERE owner_id = ? AND deleted_at IS NULL ORDER BY name",
        )
        .bind(user.user_id)
        .fetch_all(&app.pool)
        .await?,
    ))
}

pub async fn create(
    user: AuthUser,
    State(app): State<AppState>,
    ApiJson(req): ApiJson<VaultNameRequest>,
) -> Result<(StatusCode, Json<VaultInfo>), ApiError> {
    let name = validate_name(&req.name)?;
    let result = sqlx::query("INSERT INTO vaults (owner_id, name, created_at) VALUES (?, ?, ?)")
        .bind(user.user_id)
        .bind(&name)
        .bind(now())
        .execute(&app.pool)
        .await;
    let id = match result {
        Ok(done) => done.last_insert_rowid(),
        Err(e) if is_unique_violation(&e) => return Err(ApiError::NameTaken),
        Err(e) => return Err(e.into()),
    };
    Ok((StatusCode::CREATED, Json(load(&app.pool, id).await?)))
}

pub async fn rename(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<VaultNameRequest>,
) -> Result<Json<VaultInfo>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    let name = validate_name(&req.name)?;
    match sqlx::query("UPDATE vaults SET name = ? WHERE id = ?")
        .bind(&name)
        .bind(id)
        .execute(&app.pool)
        .await
    {
        Ok(_) => Ok(Json(load(&app.pool, id).await?)),
        Err(e) if is_unique_violation(&e) => Err(ApiError::NameTaken),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteVaultRequest {
    password: String,
}

/// Marks the vault deleted. Needs the account password, so a copied device
/// token can't delete a whole vault. An admin can restore it with
/// `notesync-server vault restore` until cleanup removes it after the
/// history period.
pub async fn delete(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<DeleteVaultRequest>,
) -> Result<StatusCode, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    confirm_password(&app, &user, &req.password).await?;
    sqlx::query("UPDATE vaults SET deleted_at = ? WHERE id = ?")
        .bind(now())
        .bind(id)
        .execute(&app.pool)
        .await?;
    tracing::warn!(username = %user.username, vault_id = id, "vault deleted");
    Ok(StatusCode::NO_CONTENT)
}
