use axum::{
    body::Bytes,
    extract::State,
    http::{
        header::{ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use super::{
    extract::{ApiJson, ApiPath, ApiQuery},
    vaults::owned_vault,
};
use crate::{
    auth::{self, AuthUser},
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
    record_blob(&app, id, &hash, body.len() as u64).await?;
    if !app.blobs.exists(id, &hash).await? {
        // Cleanup removed an older copy just before the lock was taken.
        app.blobs.put(id, &hash, &body).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Records stored content, or marks it as new again so cleanup keeps it.
/// Call with the write lock held.
async fn record_blob(app: &AppState, id: i64, hash: &str, size: u64) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO blobs (vault_id, hash, size, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (vault_id, hash) DO UPDATE SET created_at = excluded.created_at",
    )
    .bind(id)
    .bind(hash)
    .bind(size as i64)
    .bind(now())
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// The recorded size of stored content, if it's stored.
async fn stored_size(app: &AppState, id: i64, hash: &str) -> Result<Option<u64>, ApiError> {
    let size: Option<i64> =
        sqlx::query_scalar("SELECT size FROM blobs WHERE vault_id = ? AND hash = ?")
            .bind(id)
            .bind(hash)
            .fetch_optional(&app.pool)
            .await?;
    match size {
        Some(size) if app.blobs.exists(id, hash).await? => Ok(Some(size as u64)),
        _ => Ok(None),
    }
}

/// Where an upload sent in pieces stands.
#[derive(Debug, serde::Serialize)]
pub struct UploadProgress {
    /// Bytes the server has so far.
    received: u64,
    /// True once the content is stored; no more pieces are needed.
    stored: bool,
}

pub async fn get_parts(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath((id, hash)): ApiPath<(i64, String)>,
) -> Result<Json<UploadProgress>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    if !is_valid_hash(&hash) {
        return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
    }
    if let Some(size) = stored_size(&app, id, &hash).await? {
        return Ok(Json(UploadProgress {
            received: size,
            stored: true,
        }));
    }
    let _uploads = app.upload_lock.lock().await;
    let received = app.blobs.part_len(id, &hash).await?;
    Ok(Json(UploadProgress {
        received,
        stored: false,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PartQuery {
    /// Where this piece starts in the file.
    offset: u64,
    /// The whole file's size.
    total: u64,
}

/// Adds one piece of a large upload. Pieces must arrive in order; when the
/// last one arrives, the server checks the hash and stores the content.
pub async fn put_part(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath((id, hash)): ApiPath<(i64, String)>,
    ApiQuery(query): ApiQuery<PartQuery>,
    body: Bytes,
) -> Result<Json<UploadProgress>, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    if !is_valid_hash(&hash) {
        return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
    }
    if query.total > app.config.max_file_bytes {
        return Err(ApiError::TooLarge);
    }
    let end = query.offset.saturating_add(body.len() as u64);
    if end > query.total {
        return Err(ApiError::InvalidRequest(
            "the piece goes past the end of the file".into(),
        ));
    }
    // An empty piece is only for finishing an upload that has every byte
    // (e.g. the server stopped before storing it).
    if body.is_empty() && query.offset != query.total {
        return Err(ApiError::InvalidRequest("the piece is empty".into()));
    }
    if stored_size(&app, id, &hash).await?.is_some() {
        return Ok(Json(UploadProgress {
            received: query.total,
            stored: true,
        }));
    }

    let _uploads = app.upload_lock.lock().await;
    let received = app
        .blobs
        .append_part(id, &hash, query.offset, &body)
        .await?;
    if received < query.total {
        return Ok(Json(UploadProgress {
            received,
            stored: false,
        }));
    }
    // Checked outside the write lock, since reading a large file takes a moment.
    app.blobs.verify_part(id, &hash).await?;
    let _guard = app.write_lock.lock().await;
    app.blobs.install_part(id, &hash).await?;
    record_blob(&app, id, &hash, received).await?;
    Ok(Json(UploadProgress {
        received,
        stored: true,
    }))
}

/// Largest piece sent for one range request.
pub const MAX_RANGE_BYTES: u64 = 16 * 1024 * 1024;

/// Downloads stored content. With `Range: bytes=START-` or
/// `bytes=START-END`, sends just that piece (at most `MAX_RANGE_BYTES`), so
/// large files can be fetched a piece at a time and resumed.
pub async fn get_blob(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath((id, hash)): ApiPath<(i64, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    owned_vault(&app.pool, user.user_id, id).await?;
    if !is_valid_hash(&hash) {
        return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
    }
    let size: Option<i64> =
        sqlx::query_scalar("SELECT size FROM blobs WHERE vault_id = ? AND hash = ?")
            .bind(id)
            .bind(&hash)
            .fetch_optional(&app.pool)
            .await?;
    let Some(size) = size else {
        return Err(ApiError::NotFound);
    };
    let total = size as u64;
    let not_found = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound
        } else {
            e.into()
        }
    };

    let range = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let Some(range) = range else {
        let bytes = app.blobs.read(id, &hash).await.map_err(not_found)?;
        return Ok((
            [
                (CONTENT_TYPE, "application/octet-stream"),
                (ACCEPT_RANGES, "bytes"),
            ],
            bytes,
        )
            .into_response());
    };
    let Some((start, end)) = parse_range(range, total) else {
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response());
    };
    let end = end.min(start + MAX_RANGE_BYTES - 1);
    let bytes = app
        .blobs
        .read_range(id, &hash, start, end - start + 1)
        .await
        .map_err(not_found)?;
    let end = start + bytes.len() as u64 - 1;
    Ok((
        StatusCode::PARTIAL_CONTENT,
        [
            (CONTENT_TYPE, "application/octet-stream".to_string()),
            (CONTENT_RANGE, format!("bytes {start}-{end}/{total}")),
        ],
        bytes,
    )
        .into_response())
}

/// One `bytes=START-END` or `bytes=START-` range within a file of `total`
/// bytes, as inclusive positions. Other forms aren't supported.
fn parse_range(header: &str, total: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => total.checked_sub(1)?,
        end => end.parse::<u64>().ok()?.min(total.checked_sub(1)?),
    };
    (start <= end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_range("bytes=0-", 10), Some((0, 9)));
        assert_eq!(parse_range("bytes=4-6", 10), Some((4, 6)));
        assert_eq!(parse_range("bytes=4-100", 10), Some((4, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_range("bytes=6-4", 10), None);
        assert_eq!(parse_range("bytes=-5", 10), None);
        assert_eq!(parse_range("bytes=0-1,4-5", 10), None);
        assert_eq!(parse_range("items=0-1", 10), None);
    }
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
    // One commit per sync run, so this limits how often a device syncs.
    app.limits
        .sync
        .check(&format!("device:{}", user.device_id))
        .map_err(auth::too_many)?;
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
