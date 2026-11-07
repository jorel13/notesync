use axum::{
    body::Bytes,
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::{
    extract::{ApiJson, ApiPath, ApiQuery},
    vaults::owned_vault,
};
use crate::{
    auth::AuthUser,
    db::now,
    error::ApiError,
    state::AppState,
    storage::is_valid_hash,
    sync::{self, ChangeRequest, ChangesPage, CommitResult, HistoryEntry, VaultState},
};

pub async fn state(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> Result<Json<VaultState>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    Ok(Json(sync::state(&app.pool, id).await?))
}

#[derive(Debug, Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    since: i64,
    limit: Option<i64>,
}

pub async fn changes(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(query): ApiQuery<ChangesQuery>,
) -> Result<Json<ChangesPage>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    Ok(Json(
        sync::changes(&app.pool, id, query.since, query.limit).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct MissingRequest {
    hashes: Vec<String>,
}

pub async fn missing(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<MissingRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    let missing = sync::missing(&app.pool, id, req.hashes).await?;
    Ok(Json(serde_json::json!({ "missing": missing })))
}

pub async fn put_blob(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath((id, hash)): ApiPath<(i64, String)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    if !is_valid_hash(&hash) {
        return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
    }
    if body.len() as u64 > app.config.max_file_bytes {
        return Err(ApiError::TooLarge);
    }
    // Write the file outside the lock, then record it under the lock. Cleanup
    // also holds the lock, so it can't remove the file between these steps.
    app.blobs.put(id, &hash, &body).await?;
    let _guard = app.write_lock.lock().await;
    sqlx::query(
        "INSERT INTO blobs (vault_id, hash, size, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (vault_id, hash) DO UPDATE SET created_at = excluded.created_at",
    )
    .bind(id)
    .bind(&hash)
    .bind(body.len() as i64)
    .bind(now())
    .execute(&app.pool)
    .await?;
    if !app.blobs.exists(id, &hash).await? {
        // Cleanup removed an older copy just before the lock was taken.
        app.blobs.put(id, &hash, &body).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_blob(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath((id, hash)): ApiPath<(i64, String)>,
) -> Result<impl IntoResponse, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    if !is_valid_hash(&hash) {
        return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
    }
    let recorded: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM blobs WHERE vault_id = ? AND hash = ?")
            .bind(id)
            .bind(&hash)
            .fetch_optional(&app.pool)
            .await?;
    if recorded.is_none() {
        return Err(ApiError::NotFound);
    }
    let bytes = match app.blobs.read(id, &hash).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ApiError::NotFound),
        Err(e) => return Err(e.into()),
    };
    Ok(([(CONTENT_TYPE, "application/octet-stream")], bytes))
}

#[derive(Debug, Deserialize)]
pub struct CommitRequest {
    changes: Vec<ChangeRequest>,
}

pub async fn commit(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(req): ApiJson<CommitRequest>,
) -> Result<Json<CommitResult>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    let _guard = app.write_lock.lock().await;
    Ok(Json(
        sync::commit(&app.pool, id, user.device_id, req.changes).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    path: String,
}

pub async fn history(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(query): ApiQuery<HistoryQuery>,
) -> Result<Json<Vec<HistoryEntry>>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    Ok(Json(sync::history(&app.pool, id, &query.path).await?))
}
