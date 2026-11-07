mod common;

use axum::http::StatusCode;
use common::*;
use notesync_server::users;
use serde_json::json;

#[tokio::test]
async fn health_needs_no_login() {
    let app = TestApp::new().await;
    let (status, body) = app.call("GET", "/api/v1/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn requests_without_a_valid_token_are_rejected() {
    let app = TestApp::new().await;
    let (status, body) = app.call("GET", "/api/v1/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "unauthorized");

    let (status, _) = app.call("GET", "/api/v1/me", Some("ns_bogus"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_and_logout() {
    let app = TestApp::new().await;
    app.add_user("alice").await;

    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice", "password": "wrong password", "device_name": "pc" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "nobody", "password": PASSWORD, "device_name": "pc" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let token = app.login("alice").await;
    let (status, me) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["username"], "alice");
    assert_eq!(me["is_admin"], false);

    let (status, _) = app
        .call("POST", "/api/v1/auth/logout", Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_rejects_bad_device_names() {
    let app = TestApp::new().await;
    app.add_user("alice").await;
    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice", "password": PASSWORD, "device_name": "  " })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
}

#[tokio::test]
async fn malformed_json_gets_a_json_error() {
    let app = TestApp::new().await;
    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");
}

#[tokio::test]
async fn disabled_users_are_signed_out_and_blocked() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    users::set_disabled(&app.state.pool, "alice", true)
        .await
        .unwrap();

    let (status, _) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice", "password": PASSWORD, "device_name": "pc" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    users::set_disabled(&app.state.pool, "alice", false)
        .await
        .unwrap();
    app.login("alice").await;
}

#[tokio::test]
async fn repeated_failed_logins_are_rate_limited() {
    let app = TestApp::new().await;
    app.add_user("alice").await;
    let wrong = json!({ "username": "alice", "password": "not the password", "device_name": "pc" });
    for _ in 0..10 {
        let (status, _) = app
            .call("POST", "/api/v1/auth/login", None, Some(wrong.clone()))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Even the right password is refused while the limit applies.
    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "ALICE", "password": PASSWORD, "device_name": "pc" })),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "rate_limited");
}

#[tokio::test]
async fn change_password() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let other_device = app.login("alice").await;

    let (status, _) = app
        .call(
            "PUT",
            "/api/v1/me/password",
            Some(&token),
            Some(json!({ "current_password": "wrong password", "new_password": "a new password" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = app
        .call(
            "PUT",
            "/api/v1/me/password",
            Some(&token),
            Some(json!({ "current_password": PASSWORD, "new_password": "short" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_request");

    let (status, _) = app
        .call(
            "PUT",
            "/api/v1/me/password",
            Some(&token),
            Some(json!({
                "current_password": PASSWORD,
                "new_password": "a new password",
                "sign_out_others": true
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "the current device stays signed in");
    let (status, _) = app
        .call("GET", "/api/v1/me", Some(&other_device), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "other devices are signed out"
    );

    let (status, _) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice", "password": "a new password", "device_name": "pc" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn list_and_revoke_devices() {
    let app = TestApp::new().await;
    let token = app.user("alice").await;
    let phone = app.login("alice").await;
    let bob = app.user("bob").await;

    let (status, devices) = app
        .call("GET", "/api/v1/me/devices", Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let devices = devices.as_array().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices.iter().filter(|d| d["current"] == true).count(), 1);
    let phone_id = devices.iter().find(|d| d["current"] == false).unwrap()["id"]
        .as_i64()
        .unwrap();

    // Another user can't revoke Alice's devices.
    let (status, _) = app
        .call(
            "DELETE",
            &format!("/api/v1/me/devices/{phone_id}"),
            Some(&bob),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = app
        .call(
            "DELETE",
            &format!("/api/v1/me/devices/{phone_id}"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.call("GET", "/api/v1/me", Some(&phone), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = app
        .call(
            "DELETE",
            &format!("/api/v1/me/devices/{phone_id}"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn old_clients_are_refused() {
    let app = TestApp::new().await;
    let req = axum::http::Request::builder()
        .uri("/api/v1/health")
        .header("x-notesync-client", "0.0.1")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, bytes) = app.send(req).await;
    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error_code(&body), "client_too_old");

    let req = axum::http::Request::builder()
        .uri("/api/v1/health")
        .header("x-notesync-client", "0.1.0")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = app.send(req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn user_management_rules() {
    let app = TestApp::new().await;
    let pool = &app.state.pool;
    users::create_user(pool, "alice", PASSWORD, false)
        .await
        .unwrap();
    assert!(
        users::create_user(pool, "ALICE", PASSWORD, false)
            .await
            .is_err(),
        "usernames ignore case"
    );
    assert!(users::create_user(pool, "bob", "short", false)
        .await
        .is_err());
    assert!(users::create_user(pool, "bad name", PASSWORD, false)
        .await
        .is_err());
    assert!(users::set_disabled(pool, "nobody", true).await.is_err());

    users::set_admin(pool, "alice", true).await.unwrap();
    let listed = users::list(pool).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].is_admin, 1);

    // An admin password reset signs the user out everywhere.
    let token = app.login("alice").await;
    users::set_password(pool, "alice", "brand new password")
        .await
        .unwrap();
    let (status, _) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
