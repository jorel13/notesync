mod common;

use axum::http::StatusCode;
use common::*;
use notesync_server::{storage::sha256_hex, users};
use serde_json::json;

#[tokio::test]
async fn create_list_rename_delete() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;

    let personal = app.create_vault(&token, "Personal").await;
    app.create_vault(&token, "Work").await;

    let (status, body) = app
        .call(
            "POST",
            "/api/v1/vaults",
            Some(&token),
            Some(json!({ "name": "personal" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "names are unique per user, ignoring case"
    );
    assert_eq!(error_code(&body), "name_taken");

    let (status, body) = app
        .call(
            "POST",
            "/api/v1/vaults",
            Some(&token),
            Some(json!({ "name": "   " })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    let (status, vaults) = app.call("GET", "/api/v1/vaults", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = vaults
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("Personal"), json!("Work")]);

    let (status, body) = app
        .call(
            "PATCH",
            &format!("/api/v1/vaults/{personal}"),
            Some(&token),
            Some(json!({ "name": "Work" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "name_taken");

    let (status, body) = app
        .call(
            "PATCH",
            &format!("/api/v1/vaults/{personal}"),
            Some(&token),
            Some(json!({ "name": "Home" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Home");

    // Deleting needs the account password; the device token isn't enough.
    let delete = format!("/api/v1/vaults/{personal}");
    let (status, body) = app.call("DELETE", &delete, Some(&token), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = app
        .call(
            "DELETE",
            &delete,
            Some(&token),
            Some(json!({ "password": "wrong password" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, vaults) = app.call("GET", "/api/v1/vaults", Some(&token), None).await;
    assert_eq!(vaults.as_array().unwrap().len(), 2, "still there");

    let (status, _) = app
        .call(
            "DELETE",
            &delete,
            Some(&token),
            Some(json!({ "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, vaults) = app.call("GET", "/api/v1/vaults", Some(&token), None).await;
    assert_eq!(vaults.as_array().unwrap().len(), 1);
    let (status, _) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{personal}/state"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "deleted vaults are gone from the API"
    );

    // The name can be reused once the old vault is deleted.
    let reused = app.create_vault(&token, "Home").await;

    // An admin can restore it, once the name is free again.
    let pool = &app.state.pool;
    let deleted = users::vaults(pool, "alice", true).await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert!(users::restore_vault(pool, "alice", personal).await.is_err());
    sqlx::query("UPDATE vaults SET name = 'Home 2' WHERE id = ?")
        .bind(reused)
        .execute(pool)
        .await
        .unwrap();
    users::restore_vault(pool, "alice", personal).await.unwrap();
    let (status, _) = app
        .call(
            "GET",
            &format!("/api/v1/vaults/{personal}/state"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn users_cannot_see_each_others_vaults() {
    let app = TestApp::new().await;
    let alice = app.user("alice").await;
    let bob = app.user("bob").await;
    let vault = app.create_vault(&alice, "Private").await;
    let hash = app.upload(&alice, vault, b"secret").await;
    app.put_file(&alice, vault, "Secret.md", b"secret", None)
        .await;

    let (_, bob_vaults) = app.call("GET", "/api/v1/vaults", Some(&bob), None).await;
    assert!(bob_vaults.as_array().unwrap().is_empty());

    let base = format!("/api/v1/vaults/{vault}");
    let attempts = [
        ("GET", format!("{base}/state"), None),
        ("GET", format!("{base}/changes"), None),
        ("GET", format!("{base}/history?path=Secret.md"), None),
        (
            "POST",
            format!("{base}/blobs/missing"),
            Some(json!({ "hashes": [hash] })),
        ),
        (
            "POST",
            format!("{base}/commit"),
            Some(json!({ "changes": [{ "path": "x.md", "op": "upsert", "hash": hash }] })),
        ),
        ("PATCH", base.clone(), Some(json!({ "name": "Mine now" }))),
        (
            "DELETE",
            base.clone(),
            Some(json!({ "password": PASSWORD })),
        ),
    ];
    for (method, uri, body) in attempts {
        let (status, response) = app.call(method, &uri, Some(&bob), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}: {response}");
    }

    let (status, _) = app.download(&bob, vault, &hash).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.upload_raw(&bob, vault, &sha256_hex(b"x"), b"x").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Alice's vault is untouched.
    let state = app.state_of(&alice, vault).await;
    assert_eq!(paths_in(&state), vec!["Secret.md"]);
}

#[tokio::test]
async fn upload_and_download() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let vault = app.create_vault(&token, "Notes").await;

    let content = b"# Hello\n\nFirst note.";
    let hash = app.upload(&token, vault, content).await;
    let (status, bytes) = app.download(&token, vault, &hash).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, content);

    // Uploading the same content twice is fine.
    app.upload(&token, vault, content).await;

    let (status, body) = app
        .upload_raw(&token, vault, &hash, b"different content")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "hash_mismatch");

    let (status, body) = app.upload_raw(&token, vault, "not-a-hash", b"x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    let too_big = vec![b'a'; MAX_FILE_BYTES as usize + 1];
    let (status, body) = app
        .upload_raw(&token, vault, &sha256_hex(&too_big), &too_big)
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(&body), "too_large");

    let (status, _) = app
        .download(&token, vault, &sha256_hex(b"never uploaded"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_reports_only_unknown_hashes() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let vault = app.create_vault(&token, "Notes").await;
    let known = app.upload(&token, vault, b"known").await;
    let unknown = sha256_hex(b"unknown");

    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v1/vaults/{vault}/blobs/missing"),
            Some(&token),
            Some(json!({ "hashes": [known, unknown, unknown] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["missing"], json!([unknown]));

    let (status, _) = app
        .call(
            "POST",
            &format!("/api/v1/vaults/{vault}/blobs/missing"),
            Some(&token),
            Some(json!({ "hashes": ["bad"] })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn contents_are_not_shared_between_vaults() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let first = app.create_vault(&token, "First").await;
    let second = app.create_vault(&token, "Second").await;
    let hash = app.upload(&token, first, b"shared text").await;

    let (_, body) = app
        .call(
            "POST",
            &format!("/api/v1/vaults/{second}/blobs/missing"),
            Some(&token),
            Some(json!({ "hashes": [hash] })),
        )
        .await;
    assert_eq!(body["missing"], json!([hash]));
}
