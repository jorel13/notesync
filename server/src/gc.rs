//! Cleanup of old file contents and deleted vaults.

use serde::Serialize;

use crate::{db::now, state::AppState};

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct GcReport {
    pub blobs_removed: u64,
    pub vaults_removed: u64,
}

/// Removes contents that no current file uses and that were replaced (or
/// uploaded but never committed) more than `history_days` ago, and removes
/// vaults deleted more than `history_days` ago.
pub async fn run(state: &AppState) -> anyhow::Result<GcReport> {
    let _guard = state.write_lock.lock().await;
    let now = now();
    let cutoff = now - state.config.history_days as i64 * 86_400;
    let mut report = GcReport::default();

    // A version stays needed until `history_days` after the change that replaced it.
    let doomed: Vec<(i64, String)> = sqlx::query_as(
        "SELECT b.vault_id, b.hash FROM blobs b
         WHERE b.created_at < ?1
           AND NOT EXISTS (
               SELECT 1 FROM current_files f WHERE f.vault_id = b.vault_id AND f.hash = b.hash)
           AND NOT EXISTS (
               SELECT 1 FROM changes c
               WHERE c.vault_id = b.vault_id AND c.hash = b.hash
                 AND COALESCE(
                     (SELECT MIN(n.created_at) FROM changes n
                      WHERE n.vault_id = c.vault_id AND n.path = c.path AND n.seq > c.seq),
                     ?2) >= ?1)",
    )
    .bind(cutoff)
    .bind(now)
    .fetch_all(&state.pool)
    .await?;

    for (vault_id, hash) in doomed {
        let deleted =
            sqlx::query("DELETE FROM blobs WHERE vault_id = ? AND hash = ? AND created_at < ?")
                .bind(vault_id)
                .bind(&hash)
                .bind(cutoff)
                .execute(&state.pool)
                .await?;
        if deleted.rows_affected() == 1 {
            state.blobs.remove(vault_id, &hash).await?;
            report.blobs_removed += 1;
        }
    }

    let vaults: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM vaults WHERE deleted_at IS NOT NULL AND deleted_at < ?")
            .bind(cutoff)
            .fetch_all(&state.pool)
            .await?;
    for vault_id in vaults {
        sqlx::query("DELETE FROM vaults WHERE id = ?")
            .bind(vault_id)
            .execute(&state.pool)
            .await?;
        state.blobs.remove_vault(vault_id).await?;
        report.vaults_removed += 1;
    }

    // Old sign-in failures and finished lockouts.
    sqlx::query("DELETE FROM login_failures WHERE window_start < ? AND locked_until < ?")
        .bind(now - crate::auth::FAILURE_WINDOW_SECS)
        .bind(now)
        .execute(&state.pool)
        .await?;

    Ok(report)
}
