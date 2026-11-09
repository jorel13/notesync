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

fn sign_in(username: &str, password: &str) -> serde_json::Value {
    json!({ "username": username, "password": password, "device_name": "pc" })
}

#[tokio::test]
async fn repeated_failed_logins_lock_the_account() {
    // A generous rate limit, so only the lockout is tested here.
    let app = TestApp::with_config(|c| c.login_rate.burst = 100).await;
    app.add_user("alice").await;
    for attempt in 1..=10 {
        let (status, body) = app
            .call(
                "POST",
                "/api/v1/auth/login",
                None,
                Some(sign_in("alice", "not the password")),
            )
            .await;
        let expected = if attempt < 10 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(status, expected, "attempt {attempt}: {body}");
    }
    // Even the right password is refused while locked, in any letter case.
    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("ALICE", PASSWORD)),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "rate_limited");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("15 minutes"));

    // The lockout is in the database, so a restart doesn't clear it, and an
    // admin can see and clear it.
    let listed = users::list(&app.state.pool).await.unwrap();
    assert!(listed[0].locked_until > 0);
    users::unlock(&app.state.pool, "alice").await.unwrap();
    app.login("alice").await;
}

#[tokio::test]
async fn a_correct_password_resets_the_failure_count() {
    let app = TestApp::with_config(|c| c.login_rate.burst = 100).await;
    app.add_user("alice").await;
    for _ in 0..2 {
        for _ in 0..9 {
            app.call(
                "POST",
                "/api/v1/auth/login",
                None,
                Some(sign_in("alice", "wrong password")),
            )
            .await;
        }
        app.login("alice").await;
    }
}

#[tokio::test]
async fn sign_ins_are_rate_limited_per_username() {
    let app = TestApp::new().await;
    app.add_user("alice").await;
    app.add_user("bob").await;
    // The default allows a burst of 10, then one every 5 seconds.
    for _ in 0..10 {
        app.login("alice").await;
    }
    let (status, body) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("alice", PASSWORD)),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("second"));
    app.login("bob").await;
}

#[tokio::test]
async fn sign_in_requests_must_be_small() {
    let app = TestApp::new().await;
    app.add_user("alice").await;
    let huge = "x".repeat(100 * 1024);
    let (status, _) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("alice", &huge)),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn tokens_can_be_refreshed_and_expire() {
    let app = TestApp::new().await;
    app.add_user("alice").await;
    let (status, login) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("alice", PASSWORD)),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let old = login["token"].as_str().unwrap().to_string();
    let expires = login["expires_at"].as_i64().unwrap();
    let refresh_after = login["refresh_after"].as_i64().unwrap();
    let now = notesync_server::db::now();
    assert!(
        (expires - now - 30 * 86_400).abs() < 5,
        "30 days by default"
    );
    assert!(
        (refresh_after - now - 86_400).abs() < 5,
        "refresh after a day"
    );

    let (status, fresh) = app
        .call("POST", "/api/v1/auth/refresh", Some(&old), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let new = fresh["token"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    let (status, _) = app.call("GET", "/api/v1/me", Some(&new), None).await;
    assert_eq!(status, StatusCode::OK);
    // The old token works for a few minutes, in case the reply was lost.
    let (status, _) = app.call("GET", "/api/v1/me", Some(&old), None).await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("UPDATE devices SET prev_expires_at = ?")
        .bind(now - 1)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, _) = app.call("GET", "/api/v1/me", Some(&old), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A token that isn't refreshed in time stops working.
    sqlx::query("UPDATE devices SET expires_at = ?")
        .bind(now - 1)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, _) = app.call("GET", "/api/v1/me", Some(&new), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(users::list(&app.state.pool).await.unwrap()[0].devices, 0);
}

#[tokio::test]
async fn token_lifetime_is_configurable() {
    let app = TestApp::with_config(|c| c.token_days = 2).await;
    app.add_user("alice").await;
    let (_, login) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("alice", PASSWORD)),
        )
        .await;
    let now = notesync_server::db::now();
    assert!((login["expires_at"].as_i64().unwrap() - now - 2 * 86_400).abs() < 5);
    assert!((login["refresh_after"].as_i64().unwrap() - now - 86_400).abs() < 5);

    let app = TestApp::with_config(|c| c.token_days = 1).await;
    app.add_user("alice").await;
    let (_, login) = app
        .call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(sign_in("alice", PASSWORD)),
        )
        .await;
    assert!(
        (login["refresh_after"].as_i64().unwrap() - now - 43_200).abs() < 5,
        "halfway through a short lifetime"
    );
}

#[tokio::test]
async fn requests_per_device_are_rate_limited() {
    let app = TestApp::with_config(|c| {
        c.request_rate = notesync_server::config::Rate {
            burst: 5,
            every: std::time::Duration::from_secs(60),
        }
    })
    .await;
    let token = app.user("alice").await;
    for _ in 0..5 {
        let (status, _) = app.call("GET", "/api/v1/me", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = app.call("GET", "/api/v1/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    // Other devices have their own allowance.
    let other = app.login("alice").await;
    let (status, _) = app.call("GET", "/api/v1/me", Some(&other), None).await;
    assert_eq!(status, StatusCode::OK);
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
            // Other devices are signed out without asking.
            Some(json!({
                "current_password": PASSWORD,
                "new_password": "a new password"
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
        .header("x-notesync-client", "0.1.1")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = app.send(req).await;
    assert_eq!(
        status,
        StatusCode::UPGRADE_REQUIRED,
        "0.1.x doesn't check server paths or refresh tokens"
    );

    let req = axum::http::Request::builder()
        .uri("/api/v1/health")
        .header("x-notesync-client", "0.2.0")
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
