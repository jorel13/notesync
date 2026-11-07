//! Vault history: commits, change feeds, current state and per-file history.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::{
    db::now,
    error::{ApiError, PathConflict},
    paths::{self, VaultPath},
    storage::is_valid_hash,
};

pub const MAX_CHANGES_PER_COMMIT: usize = 1000;
pub const MAX_HASHES_PER_REQUEST: usize = 1000;
pub const DEFAULT_PAGE: i64 = 500;
pub const MAX_PAGE: i64 = 1000;
const MAX_HISTORY_ROWS: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Upsert,
    Delete,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Upsert => "upsert",
            Op::Delete => "delete",
        }
    }
}

/// One change in a commit. `expect_seq` is the version of the path the client
/// last saw; `None` means the path should not exist yet.
#[derive(Debug, Deserialize)]
pub struct ChangeRequest {
    pub path: String,
    pub op: Op,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub expect_seq: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Applied {
    pub path: String,
    pub seq: i64,
}

#[derive(Debug, Serialize)]
pub struct CommitResult {
    pub head_seq: i64,
    pub applied: Vec<Applied>,
}

struct Validated {
    path: VaultPath,
    op: Op,
    hash: Option<String>,
    expect_seq: Option<i64>,
}

fn validate(changes: Vec<ChangeRequest>) -> Result<Vec<Validated>, ApiError> {
    if changes.is_empty() {
        return Err(ApiError::InvalidRequest("commit has no changes".into()));
    }
    if changes.len() > MAX_CHANGES_PER_COMMIT {
        return Err(ApiError::InvalidRequest(format!(
            "commit has more than {MAX_CHANGES_PER_COMMIT} changes"
        )));
    }
    let mut seen = HashSet::new();
    let mut upsert_keys: HashMap<String, String> = HashMap::new();
    let mut out = Vec::with_capacity(changes.len());
    for change in changes {
        let path = paths::normalize(&change.path).map_err(ApiError::InvalidPath)?;
        if !seen.insert(path.path.clone()) {
            return Err(ApiError::InvalidRequest(format!(
                "path {:?} appears more than once",
                path.path
            )));
        }
        match change.op {
            Op::Upsert => {
                let hash = change
                    .hash
                    .ok_or_else(|| ApiError::InvalidRequest("upsert needs a hash".into()))?;
                if !is_valid_hash(&hash) {
                    return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
                }
                if let Some(other) = upsert_keys.insert(path.key.clone(), path.path.clone()) {
                    return Err(ApiError::CaseConflict(other));
                }
                out.push(Validated {
                    path,
                    op: Op::Upsert,
                    hash: Some(hash),
                    expect_seq: change.expect_seq,
                });
            }
            Op::Delete => {
                if change.expect_seq.is_none() {
                    return Err(ApiError::InvalidRequest("delete needs expect_seq".into()));
                }
                out.push(Validated {
                    path,
                    op: Op::Delete,
                    hash: None,
                    expect_seq: change.expect_seq,
                });
            }
        }
    }
    // Deletes first, so a rename that only changes letter case frees the old name.
    out.sort_by_key(|c| c.op != Op::Delete);
    Ok(out)
}

/// Applies a batch of changes all at once, or none of them.
///
/// Callers must hold `AppState::write_lock`.
pub async fn commit(
    pool: &SqlitePool,
    vault_id: i64,
    device_id: i64,
    changes: Vec<ChangeRequest>,
) -> Result<CommitResult, ApiError> {
    let changes = validate(changes)?;
    let mut tx = pool.begin().await?;

    let mut conflicts = Vec::new();
    for change in &changes {
        if let Some(hash) = &change.hash {
            let stored: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM blobs WHERE vault_id = ? AND hash = ?")
                    .bind(vault_id)
                    .bind(hash)
                    .fetch_optional(&mut *tx)
                    .await?;
            if stored.is_none() {
                return Err(ApiError::MissingBlob(hash.clone()));
            }
        }

        let current: Option<(i64, String)> =
            sqlx::query_as("SELECT seq, hash FROM current_files WHERE vault_id = ? AND path = ?")
                .bind(vault_id)
                .bind(&change.path.path)
                .fetch_optional(&mut *tx)
                .await?;
        let up_to_date = match (&current, change.expect_seq) {
            (None, None) => true,
            (Some((seq, _)), Some(expected)) => *seq == expected,
            _ => false,
        };
        if !up_to_date {
            let (current_seq, current_hash) = match current {
                Some((seq, hash)) => (Some(seq), Some(hash)),
                None => (last_seq(&mut tx, vault_id, &change.path.path).await?, None),
            };
            conflicts.push(PathConflict {
                path: change.path.path.clone(),
                current_hash,
                current_seq,
            });
            continue;
        }

        if change.op == Op::Upsert && current.is_none() {
            let other: Option<String> = sqlx::query_scalar(
                "SELECT path FROM current_files WHERE vault_id = ? AND path_key = ? AND path <> ?",
            )
            .bind(vault_id)
            .bind(&change.path.key)
            .bind(&change.path.path)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(other) = other {
                let freed = changes
                    .iter()
                    .any(|c| c.op == Op::Delete && c.path.path == other);
                if !freed {
                    return Err(ApiError::CaseConflict(other));
                }
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(ApiError::Conflict(conflicts));
    }

    let mut head: i64 = sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await?;
    let created_at = now();
    let mut applied = Vec::with_capacity(changes.len());
    for change in &changes {
        head += 1;
        match change.op {
            Op::Delete => {
                sqlx::query(
                    "INSERT INTO changes (vault_id, seq, path, op, device_id, created_at)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(vault_id)
                .bind(head)
                .bind(&change.path.path)
                .bind(Op::Delete.as_str())
                .bind(device_id)
                .bind(created_at)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM current_files WHERE vault_id = ? AND path = ?")
                    .bind(vault_id)
                    .bind(&change.path.path)
                    .execute(&mut *tx)
                    .await?;
            }
            Op::Upsert => {
                let hash = change
                    .hash
                    .as_deref()
                    .expect("upserts are validated to have a hash");
                let size: i64 =
                    sqlx::query_scalar("SELECT size FROM blobs WHERE vault_id = ? AND hash = ?")
                        .bind(vault_id)
                        .bind(hash)
                        .fetch_one(&mut *tx)
                        .await?;
                sqlx::query(
                    "INSERT INTO changes (vault_id, seq, path, op, hash, size, device_id, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(vault_id)
                .bind(head)
                .bind(&change.path.path)
                .bind(Op::Upsert.as_str())
                .bind(hash)
                .bind(size)
                .bind(device_id)
                .bind(created_at)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO current_files (vault_id, path, path_key, hash, size, seq)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT (vault_id, path) DO UPDATE SET
                         path_key = excluded.path_key, hash = excluded.hash,
                         size = excluded.size, seq = excluded.seq",
                )
                .bind(vault_id)
                .bind(&change.path.path)
                .bind(&change.path.key)
                .bind(hash)
                .bind(size)
                .bind(head)
                .execute(&mut *tx)
                .await?;
            }
        }
        applied.push(Applied {
            path: change.path.path.clone(),
            seq: head,
        });
    }
    sqlx::query("UPDATE vaults SET head_seq = ? WHERE id = ?")
        .bind(head)
        .bind(vault_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(CommitResult {
        head_seq: head,
        applied,
    })
}

async fn last_seq(
    tx: &mut Transaction<'_, Sqlite>,
    vault_id: i64,
    path: &str,
) -> Result<Option<i64>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT MAX(seq) FROM changes WHERE vault_id = ? AND path = ?")
            .bind(vault_id)
            .bind(path)
            .fetch_one(&mut **tx)
            .await?,
    )
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ChangeRow {
    pub seq: i64,
    pub path: String,
    pub op: String,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ChangesPage {
    pub changes: Vec<ChangeRow>,
    pub head_seq: i64,
    pub more: bool,
}

/// Changes after `since`, oldest first.
pub async fn changes(
    pool: &SqlitePool,
    vault_id: i64,
    since: i64,
    limit: Option<i64>,
) -> Result<ChangesPage, ApiError> {
    if since < 0 {
        return Err(ApiError::InvalidRequest(
            "since must not be negative".into(),
        ));
    }
    let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let mut tx = pool.begin().await?;
    let head_seq: i64 = sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await?;
    let mut rows: Vec<ChangeRow> = sqlx::query_as(
        "SELECT seq, path, op, hash, size, created_at FROM changes
         WHERE vault_id = ? AND seq > ? ORDER BY seq LIMIT ?",
    )
    .bind(vault_id)
    .bind(since)
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    Ok(ChangesPage {
        changes: rows,
        head_seq,
        more,
    })
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FileRow {
    pub path: String,
    pub hash: String,
    pub size: i64,
    pub seq: i64,
}

#[derive(Debug, Serialize)]
pub struct VaultState {
    pub head_seq: i64,
    pub files: Vec<FileRow>,
}

/// Every current file, read in one consistent snapshot.
pub async fn state(pool: &SqlitePool, vault_id: i64) -> Result<VaultState, ApiError> {
    let mut tx = pool.begin().await?;
    let head_seq: i64 = sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await?;
    let files = sqlx::query_as(
        "SELECT path, hash, size, seq FROM current_files WHERE vault_id = ? ORDER BY path",
    )
    .bind(vault_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(VaultState { head_seq, files })
}

/// The hashes from `hashes` that this vault doesn't have stored yet.
pub async fn missing(
    pool: &SqlitePool,
    vault_id: i64,
    hashes: Vec<String>,
) -> Result<Vec<String>, ApiError> {
    if hashes.len() > MAX_HASHES_PER_REQUEST {
        return Err(ApiError::InvalidRequest(format!(
            "at most {MAX_HASHES_PER_REQUEST} hashes per request"
        )));
    }
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    for hash in hashes {
        if !is_valid_hash(&hash) {
            return Err(ApiError::InvalidRequest(format!("invalid hash {hash:?}")));
        }
        if !seen.insert(hash.clone()) {
            continue;
        }
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM blobs WHERE vault_id = ? AND hash = ?")
                .bind(vault_id)
                .bind(&hash)
                .fetch_optional(pool)
                .await?;
        if stored.is_none() {
            missing.push(hash);
        }
    }
    Ok(missing)
}

#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub seq: i64,
    pub op: String,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub created_at: i64,
    /// Whether the contents can still be downloaded.
    pub available: bool,
}

/// `(seq, op, hash, size, created_at, available)`
type HistoryRow = (i64, String, Option<String>, Option<i64>, i64, i64);

/// Past versions of one path, newest first.
pub async fn history(
    pool: &SqlitePool,
    vault_id: i64,
    path: &str,
) -> Result<Vec<HistoryEntry>, ApiError> {
    let path = paths::normalize(path).map_err(ApiError::InvalidPath)?;
    let rows: Vec<HistoryRow> = sqlx::query_as(
        "SELECT c.seq, c.op, c.hash, c.size, c.created_at,
                CASE WHEN b.hash IS NULL THEN 0 ELSE 1 END
         FROM changes c
         LEFT JOIN blobs b ON b.vault_id = c.vault_id AND b.hash = c.hash
         WHERE c.vault_id = ? AND c.path = ?
         ORDER BY c.seq DESC LIMIT ?",
    )
    .bind(vault_id)
    .bind(&path.path)
    .bind(MAX_HISTORY_ROWS)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(seq, op, hash, size, created_at, available)| HistoryEntry {
                seq,
                op,
                hash,
                size,
                created_at,
                available: available != 0,
            },
        )
        .collect())
}
