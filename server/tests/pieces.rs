//! Large files sent and fetched a piece at a time.

mod common;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use common::*;
use notesync_server::{gc, storage::sha256_hex};
use serde_json::{json, Value};

async fn setup() -> (TestApp, String, i64) {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let vault = app.create_vault(&token, "Notes").await;
    (app, token, vault)
}

async fn put_part(
    app: &TestApp,
    token: &str,
    vault: i64,
    hash: &str,
    offset: usize,
    total: usize,
    piece: &[u8],
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/v1/vaults/{vault}/blobs/{hash}/parts?offset={offset}&total={total}"
        ))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(piece.to_vec()))
        .unwrap();
    let (status, bytes) = app.send(req).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn progress(app: &TestApp, token: &str, vault: i64, hash: &str) -> (StatusCode, Value) {
    app.call(
        "GET",
        &format!("/api/v1/vaults/{vault}/blobs/{hash}/parts"),
        Some(token),
        None,
    )
    .await
}

async fn get_range(
    app: &TestApp,
    token: &str,
    vault: i64,
    hash: &str,
    range: &str,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let req = Request::builder()
        .uri(format!("/api/v1/vaults/{vault}/blobs/{hash}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::RANGE, range)
        .body(Body::empty())
        .unwrap();
    let res = tower::ServiceExt::oneshot(app.router.clone(), req)
        .await
        .unwrap();
    let status = res.status();
    let content_range = res
        .headers()
        .get(header::CONTENT_RANGE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, content_range, bytes)
}

#[tokio::test]
async fn upload_in_pieces_then_commit() {
    let (app, token, vault) = setup().await;
    let content: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let hash = sha256_hex(&content);

    let (status, body) = progress(&app, &token, vault, &hash).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "received": 0, "stored": false }));

    let (status, body) = put_part(&app, &token, vault, &hash, 0, 10_000, &content[..4000]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "received": 4000, "stored": false }));

    // Resuming: the server says where to continue, and refuses anything else.
    assert_eq!(
        progress(&app, &token, vault, &hash).await.1["received"],
        4000
    );
    let (status, body) = put_part(&app, &token, vault, &hash, 0, 10_000, &content[..4000]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "wrong_offset");
    assert_eq!(body["error"]["received"], 4000);

    // Not stored, so it can't be committed yet.
    assert_eq!(
        app.state_of(&token, vault).await["files"],
        json!([]),
        "nothing committed"
    );
    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "big.bin", "op": "upsert", "hash": hash, "expect_seq": null }]),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = put_part(&app, &token, vault, &hash, 4000, 10_000, &content[4000..]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "received": 10_000, "stored": true }));
    assert_eq!(progress(&app, &token, vault, &hash).await.1["stored"], true);

    let (status, body) = app
        .commit(
            &token,
            vault,
            json!([{ "path": "big.bin", "op": "upsert", "hash": hash, "expect_seq": null }]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(app.download(&token, vault, &hash).await.1, content);

    // Sending a piece of content that's already stored is a no-op.
    let (status, body) = put_part(&app, &token, vault, &hash, 0, 10_000, &content[..10]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stored"], true);
}

#[tokio::test]
async fn a_wrong_upload_is_rejected_and_discarded() {
    let (app, token, vault) = setup().await;
    let hash = sha256_hex(b"what was promised");
    let (status, body) = put_part(&app, &token, vault, &hash, 0, 9, b"something").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "hash_mismatch");
    assert_eq!(progress(&app, &token, vault, &hash).await.1["received"], 0);
}

#[tokio::test]
async fn piece_uploads_are_checked() {
    let (app, token, vault) = setup().await;
    let hash = sha256_hex(b"x");

    let too_big = MAX_FILE_BYTES as usize + 1;
    let (status, body) = put_part(&app, &token, vault, &hash, 0, too_big, b"x").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), "too_large");

    let (status, body) = put_part(&app, &token, vault, &hash, 0, 2, b"xyz").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    let (status, body) = put_part(&app, &token, vault, &hash, 0, 2, b"").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    let (status, body) = put_part(&app, &token, vault, "not-a-hash", 0, 1, b"x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    // Another user's vault is invisible.
    let bob = app.user("bob").await;
    let (status, _) = put_part(&app, &bob, vault, &hash, 0, 1, b"x").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = progress(&app, &bob, vault, &hash).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn download_in_ranges() {
    let (app, token, vault) = setup().await;
    let content = b"0123456789abcdef";
    let hash = app.upload(&token, vault, content).await;

    let (status, range, bytes) = get_range(&app, &token, vault, &hash, "bytes=0-3").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.as_deref(), Some("bytes 0-3/16"));
    assert_eq!(bytes, b"0123");

    let (status, range, bytes) = get_range(&app, &token, vault, &hash, "bytes=10-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.as_deref(), Some("bytes 10-15/16"));
    assert_eq!(bytes, b"abcdef");

    let (status, range, _) = get_range(&app, &token, vault, &hash, "bytes=16-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(range.as_deref(), Some("bytes */16"));

    // Without a range, the whole file.
    assert_eq!(app.download(&token, vault, &hash).await.1, content);

    let bob = app.user("bob").await;
    let (status, _, _) = get_range(&app, &bob, vault, &hash, "bytes=0-3").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cleanup_removes_abandoned_uploads() {
    let (app, token, vault) = setup().await;
    let hash = sha256_hex(b"never finished");
    put_part(&app, &token, vault, &hash, 0, 14, b"never").await;
    // Fresh, so it stays.
    assert_eq!(gc::run(&app.state).await.unwrap().uploads_removed, 0);
    assert_eq!(progress(&app, &token, vault, &hash).await.1["received"], 5);
    assert_eq!(
        app.state
            .blobs
            .remove_stale_parts(std::time::Duration::ZERO)
            .await
            .unwrap(),
        1
    );
    assert_eq!(progress(&app, &token, vault, &hash).await.1["received"], 0);
}
