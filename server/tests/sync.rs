mod common;

use axum::http::StatusCode;
use common::*;
use notesync_server::{db::now, gc, storage::sha256_hex};
use serde_json::{json, Value};

async fn setup() -> (TestApp, String, i64) {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let vault = app.create_vault(&token, "Notes").await;
    (app, token, vault)
}

async fn changes(app: &TestApp, token: &str, vault: i64, query: &str) -> Value {
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{vault}/changes?{query}"),
            Some(token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn commit_updates_state_and_changes() {
    let (app, token, vault) = setup().await;

    let seq1 = app.put_file(&token, vault, "Note.md", b"v1", None).await;
    assert_eq!(seq1, 1);
    let seq2 = app
        .put_file(&token, vault, "Note.md", b"v2", Some(seq1))
        .await;
    assert_eq!(seq2, 2);

    let state = app.state_of(&token, vault).await;
    assert_eq!(state["head_seq"], 2);
    assert_eq!(state["files"][0]["path"], "Note.md");
    assert_eq!(state["files"][0]["hash"], sha256_hex(b"v2"));
    assert_eq!(state["files"][0]["size"], 2);
    assert_eq!(state["files"][0]["seq"], 2);

    let feed = changes(&app, &token, vault, "since=0").await;
    let ops: Vec<_> = feed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["seq"].as_i64().unwrap(),
                c["op"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        vec![(1, "upsert".to_string()), (2, "upsert".to_string())]
    );
    assert_eq!(feed["more"], false);
}

#[tokio::test]
async fn stale_writes_are_rejected_with_details() {
    let (app, token, vault) = setup().await;
    let seq1 = app.put_file(&token, vault, "Note.md", b"v1", None).await;
    let seq2 = app
        .put_file(&token, vault, "Note.md", b"v2 from phone", Some(seq1))
        .await;

    // The PC still thinks the note is at seq1.
    let hash = app.upload(&token, vault, b"v2 from pc").await;
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "Note.md", "op": "upsert", "hash": hash, "expect_seq": seq1 }]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "conflict");
    let conflict = &body["error"]["conflicts"][0];
    assert_eq!(conflict["path"], "Note.md");
    assert_eq!(conflict["current_seq"], seq2);
    assert_eq!(conflict["current_hash"], sha256_hex(b"v2 from phone"));

    // Creating a file that already exists is also a conflict.
    let (status, _) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "Note.md", "op": "upsert", "hash": hash }]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn commits_are_all_or_nothing() {
    let (app, token, vault) = setup().await;
    let seq1 = app.put_file(&token, vault, "Old.md", b"v1", None).await;
    app.put_file(&token, vault, "Old.md", b"v2", Some(seq1))
        .await;

    let new_hash = app.upload(&token, vault, b"new file").await;
    let stale_hash = app.upload(&token, vault, b"stale edit").await;
    let (status, _) = app
        .commit(
            &token,
            vault,
            json!([
                { "path": "New.md", "op": "upsert", "hash": new_hash },
                { "path": "Old.md", "op": "upsert", "hash": stale_hash, "expect_seq": seq1 }
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let state = app.state_of(&token, vault).await;
    assert_eq!(state["head_seq"], 2, "nothing was applied");
    assert_eq!(paths_in(&state), vec!["Old.md"]);
}

#[tokio::test]
async fn commit_requires_uploaded_content() {
    let (app, token, vault) = setup().await;
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "Note.md", "op": "upsert", "hash": sha256_hex(b"not uploaded") }]),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "missing_blob");
}

#[tokio::test]
async fn commit_validates_requests() {
    let (app, token, vault) = setup().await;
    let hash = app.upload(&token, vault, b"x").await;

    let cases = [
        (json!([]), "invalid_request"),
        (
            json!([{ "path": "../x.md", "op": "upsert", "hash": hash }]),
            "invalid_path",
        ),
        (
            json!([{ "path": "a.md", "op": "upsert" }]),
            "invalid_request",
        ),
        (
            json!([{ "path": "a.md", "op": "upsert", "hash": "abc" }]),
            "invalid_request",
        ),
        (
            json!([{ "path": "a.md", "op": "delete" }]),
            "invalid_request",
        ),
        (
            json!([
                { "path": "a.md", "op": "upsert", "hash": hash },
                { "path": "a.md", "op": "upsert", "hash": hash }
            ]),
            "invalid_request",
        ),
        (
            json!([{ "path": "a.md", "op": "rename", "hash": hash }]),
            "invalid_request",
        ),
    ];
    for (changes, code) in cases {
        let (status, body) = app.commit(&token, vault, changes.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{changes}: {body}");
        assert_eq!(error_code(&body), code, "{changes}");
    }
}

#[tokio::test]
async fn delete_and_recreate() {
    let (app, token, vault) = setup().await;
    let seq1 = app.put_file(&token, vault, "Note.md", b"v1", None).await;

    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "Note.md", "op": "delete", "expect_seq": seq1 }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let delete_seq = body["applied"][0]["seq"].as_i64().unwrap();
    assert!(paths_in(&app.state_of(&token, vault).await).is_empty());

    let feed = changes(&app, &token, vault, &format!("since={seq1}")).await;
    assert_eq!(feed["changes"][0]["op"], "delete");
    assert_eq!(feed["changes"][0]["hash"], Value::Null);

    // A device that missed the delete gets a conflict that says it was deleted.
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "Note.md", "op": "delete", "expect_seq": seq1 }]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["conflicts"][0]["current_hash"], Value::Null);
    assert_eq!(body["error"]["conflicts"][0]["current_seq"], delete_seq);

    // Recreating the path works like creating a new file.
    app.put_file(&token, vault, "Note.md", b"v2", None).await;
    assert_eq!(
        paths_in(&app.state_of(&token, vault).await),
        vec!["Note.md"]
    );
}

#[tokio::test]
async fn rename_in_one_commit() {
    let (app, token, vault) = setup().await;
    let seq = app.put_file(&token, vault, "Draft.md", b"text", None).await;
    let hash = sha256_hex(b"text");

    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([
                { "path": "Final.md", "op": "upsert", "hash": hash },
                { "path": "Draft.md", "op": "delete", "expect_seq": seq }
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["head_seq"], 3);
    assert_eq!(
        paths_in(&app.state_of(&token, vault).await),
        vec!["Final.md"]
    );
}

#[tokio::test]
async fn letter_case_collisions() {
    let (app, token, vault) = setup().await;
    let seq = app.put_file(&token, vault, "Note.md", b"text", None).await;
    let hash = sha256_hex(b"text");

    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "note.md", "op": "upsert", "hash": hash }]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "case_conflict");

    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([
                { "path": "a.md", "op": "upsert", "hash": hash },
                { "path": "A.md", "op": "upsert", "hash": hash }
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "case_conflict");

    // Renaming to a different letter case works when the old name is deleted in the same commit.
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([
                { "path": "note.md", "op": "upsert", "hash": hash },
                { "path": "Note.md", "op": "delete", "expect_seq": seq }
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        paths_in(&app.state_of(&token, vault).await),
        vec!["note.md"]
    );
}

#[tokio::test]
async fn unicode_paths_are_normalized() {
    let (app, token, vault) = setup().await;
    app.put_file(&token, vault, "Cafe\u{0301}.md", b"text", None)
        .await;
    let state = app.state_of(&token, vault).await;
    assert_eq!(paths_in(&state), vec!["Caf\u{00e9}.md"]);
}

#[tokio::test]
async fn changes_are_paginated() {
    let (app, token, vault) = setup().await;
    for i in 0..5 {
        app.put_file(
            &token,
            vault,
            &format!("Note {i}.md"),
            format!("body {i}").as_bytes(),
            None,
        )
        .await;
    }

    let page = changes(&app, &token, vault, "since=0&limit=2").await;
    assert_eq!(page["changes"].as_array().unwrap().len(), 2);
    assert_eq!(page["more"], true);
    assert_eq!(page["head_seq"], 5);

    let page = changes(&app, &token, vault, "since=4&limit=2").await;
    assert_eq!(page["changes"].as_array().unwrap().len(), 1);
    assert_eq!(page["changes"][0]["seq"], 5);
    assert_eq!(page["more"], false);

    let page = changes(&app, &token, vault, "since=5").await;
    assert!(page["changes"].as_array().unwrap().is_empty());

    let (status, _) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{vault}/changes?since=-1"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn history_lists_versions_newest_first() {
    let (app, token, vault) = setup().await;
    let seq1 = app.put_file(&token, vault, "Note.md", b"v1", None).await;
    let seq2 = app
        .put_file(&token, vault, "Note.md", b"v2", Some(seq1))
        .await;
    app.commit(
        &token,
        vault,
        json!([{ "path": "Note.md", "op": "delete", "expect_seq": seq2 }]),
    )
    .await;

    let (status, history) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{vault}/history?path=Note.md"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = history.as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["op"], "delete");
    assert_eq!(entries[1]["hash"], sha256_hex(b"v2"));
    assert_eq!(entries[1]["available"], true);
    assert_eq!(entries[2]["hash"], sha256_hex(b"v1"));

    // The old version can still be downloaded.
    let (status, bytes) = app.download(&token, vault, &sha256_hex(b"v1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"v1");
}

/// Moves every recorded time for the vault `days` into the past.
async fn age_vault(app: &TestApp, vault: i64, days: i64) {
    let old = now() - days * 86_400;
    for table in ["changes", "blobs"] {
        sqlx::query(&format!(
            "UPDATE {table} SET created_at = ? WHERE vault_id = ?"
        ))
        .bind(old)
        .bind(vault)
        .execute(&app.state.pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn cleanup_removes_only_expired_versions() {
    let (app, token, vault) = setup().await;

    // Replaced 40 days ago: removable.
    let seq = app.put_file(&token, vault, "Old.md", b"old v1", None).await;
    app.put_file(&token, vault, "Old.md", b"old v2", Some(seq))
        .await;
    // Uploaded 40 days ago but never committed: removable.
    app.upload(&token, vault, b"abandoned upload").await;
    // Created long ago, replaced just now: kept for history.
    let recent = app
        .put_file(&token, vault, "Recent.md", b"recent v1", None)
        .await;
    age_vault(&app, vault, 40).await;
    app.put_file(&token, vault, "Recent.md", b"recent v2", Some(recent))
        .await;
    // Uploaded just now but not committed yet: kept.
    app.upload(&token, vault, b"upload in progress").await;

    let report = gc::run(&app.state).await.unwrap();
    assert_eq!(report.blobs_removed, 2);

    for (content, expected) in [
        (&b"old v1"[..], StatusCode::NOT_FOUND),
        (b"abandoned upload", StatusCode::NOT_FOUND),
        (b"old v2", StatusCode::OK),
        (b"recent v1", StatusCode::OK),
        (b"recent v2", StatusCode::OK),
        (b"upload in progress", StatusCode::OK),
    ] {
        let (status, _) = app.download(&token, vault, &sha256_hex(content)).await;
        assert_eq!(status, expected, "{}", String::from_utf8_lossy(content));
    }
    assert!(!app
        .state
        .blobs
        .exists(vault, &sha256_hex(b"old v1"))
        .await
        .unwrap());

    // History marks the removed version as unavailable.
    let (_, history) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{vault}/history?path=Old.md"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(history[1]["available"], false);

    // Running again finds nothing new.
    assert_eq!(gc::run(&app.state).await.unwrap().blobs_removed, 0);
}

#[tokio::test]
async fn cleanup_removes_vaults_deleted_long_ago() {
    let (app, token, vault) = setup().await;
    app.put_file(&token, vault, "Note.md", b"text", None).await;
    let kept = app.create_vault(&token, "Recently deleted").await;

    for id in [vault, kept] {
        let (status, _) = app
            .call(
                "DELETE",
                &format!("/api/v1/vaults/{id}"),
                Some(&token),
                Some(json!({ "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    sqlx::query("UPDATE vaults SET deleted_at = ? WHERE id = ?")
        .bind(now() - 40 * 86_400)
        .bind(vault)
        .execute(&app.state.pool)
        .await
        .unwrap();

    let report = gc::run(&app.state).await.unwrap();
    assert_eq!(report.vaults_removed, 1);
    assert!(!app
        .state
        .blobs
        .exists(vault, &sha256_hex(b"text"))
        .await
        .unwrap());
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vaults")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[tokio::test]
async fn concurrent_commits_never_lose_updates() {
    let (app, token, vault) = setup().await;
    let seq = app
        .put_file(&token, vault, "Shared.md", b"base", None)
        .await;
    let first = app.upload(&token, vault, b"edit A").await;
    let second = app.upload(&token, vault, b"edit B").await;

    let change = |hash: &str| json!([{ "path": "Shared.md", "op": "upsert", "hash": hash, "expect_seq": seq }]);
    let (a, b) = tokio::join!(
        app.commit(&token, vault, change(&first)),
        app.commit(&token, vault, change(&second))
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "exactly one edit wins"
    );
    assert_eq!(app.state_of(&token, vault).await["head_seq"], 2);
}

#[tokio::test]
async fn syncs_per_device_are_rate_limited() {
    let app = TestApp::with_config(|c| c.sync_rate.burst = 10).await;
    let token = app.user("alice").await;
    let vault = app.create_vault(&token, "Notes").await;
    let hash = app.upload(&token, vault, b"text").await;
    // The default allows a burst of 10 sync runs, then one every 5 seconds.
    for i in 0..10 {
        let (status, body) = app
            .commit(
                &token,
                vault,
                json!([{ "path": format!("Note {i}.md"), "op": "upsert", "hash": hash }]),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "One too many.md", "op": "upsert", "hash": hash }]),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
}

#[tokio::test]
async fn settings_trash_and_git_folders_are_refused() {
    let (app, token, vault) = setup().await;
    let hash = app.upload(&token, vault, b"code").await;
    for path in [
        ".obsidian/app.json",
        ".Obsidian/plugins/notesync/main.js",
        ".TRASH/old.md",
        ".git/config",
    ] {
        let (status, body) = app
            .commit(
                &token,
                vault,
                json!([{ "path": path, "op": "upsert", "hash": hash }]),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(error_code(&body), "invalid_path");
    }
}
