mod common;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use common::*;
use http_body_util::BodyExt;
use notesync_server::{auth, db, users};
use serde_json::{json, Value};
use tower::ServiceExt;

/// A browser talking to the admin API: keeps the session cookie, and sends
/// the admin header unless told not to.
struct Browser<'a> {
    app: &'a TestApp,
    cookie: Option<String>,
}

struct Reply {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

impl<'a> Browser<'a> {
    fn new(app: &'a TestApp) -> Self {
        Self { app, cookie: None }
    }

    async fn raw(&mut self, method: &str, path: &str, body: Option<Value>, header: bool) -> Reply {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/v1/admin{path}"));
        if header {
            builder = builder.header("x-notesync-admin", "1");
        }
        if let Some(cookie) = &self.cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let req = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let res = self.app.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let set_cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().to_string());
        if status != StatusCode::NOT_FOUND {
            assert_eq!(
                res.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store",
                "admin responses must not be cached"
            );
        }
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if let Some(c) = &set_cookie {
            let pair = c.split(';').next().unwrap().to_string();
            self.cookie = if pair.ends_with('=') {
                None
            } else {
                Some(pair)
            };
        }
        Reply {
            status,
            body,
            set_cookie,
        }
    }

    async fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let r = self.raw(method, path, body, true).await;
        (r.status, r.body)
    }

    async fn sign_in(&mut self, username: &str) {
        let (status, body) = self
            .call(
                "POST",
                "/login",
                Some(json!({ "username": username, "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

async fn app_with_admin() -> TestApp {
    let app = TestApp::new().await;
    users::create_user(&app.state.pool, "root", PASSWORD, true)
        .await
        .unwrap();
    app
}

#[tokio::test]
async fn admins_sign_in_with_a_secure_cookie() {
    let app = app_with_admin().await;
    let mut browser = Browser::new(&app);

    let r = browser
        .raw(
            "POST",
            "/login",
            Some(json!({ "username": "root", "password": PASSWORD })),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["username"], "root");
    let cookie = r.set_cookie.unwrap();
    for part in [
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/api/v1/admin",
    ] {
        assert!(cookie.contains(part), "cookie {cookie:?} lacks {part}");
    }

    let (status, body) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "root");

    // Sign out clears the cookie and ends the session on the server.
    let old_cookie = browser.cookie.clone();
    let r = browser.raw("POST", "/logout", None, true).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(r.set_cookie.unwrap().contains("Max-Age=0"));
    browser.cookie = old_cookie;
    let (status, _) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_admins_can_sign_in() {
    let app = app_with_admin().await;
    app.add_user("alice").await;
    let mut browser = Browser::new(&app);

    let (status, body) = browser
        .call(
            "POST",
            "/login",
            Some(json!({ "username": "alice", "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(browser.cookie.is_none());

    let (status, _) = browser
        .call(
            "POST",
            "/login",
            Some(json!({ "username": "root", "password": "wrong password" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = browser
        .call(
            "POST",
            "/login",
            Some(json!({ "username": "nobody", "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A device token is not an admin session, even for an admin.
    let token = app.login("root").await;
    let (status, _) = app
        .call("GET", "/api/v1/admin/users", Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no admin header");
    let req = Request::builder()
        .uri("/api/v1/admin/users")
        .header("x-notesync-admin", "1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = app.send(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_sign_in_failures_count_towards_the_lockout() {
    let app = app_with_admin().await;
    let mut browser = Browser::new(&app);
    for _ in 0..9 {
        let (status, _) = browser
            .call(
                "POST",
                "/login",
                Some(json!({ "username": "root", "password": "wrong password" })),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body) = browser
        .call(
            "POST",
            "/login",
            Some(json!({ "username": "root", "password": "wrong password" })),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(
        auth::locked_for(&app.state.pool, "root")
            .await
            .unwrap()
            .is_some(),
        "the account is locked, for device sign-ins too"
    );
}

#[tokio::test]
async fn requests_without_the_admin_header_are_refused() {
    let app = app_with_admin().await;
    let mut browser = Browser::new(&app);
    browser.sign_in("root").await;

    // What a form on another site could send: the cookie but no custom header.
    let r = browser
        .raw(
            "POST",
            "/users",
            Some(json!({ "username": "evil", "password": "12345678" })),
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(users::user_id(&app.state.pool, "evil").await.is_err());

    let r = browser
        .raw(
            "POST",
            "/login",
            Some(json!({ "username": "root", "password": PASSWORD })),
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn sessions_end_when_idle_or_when_the_admin_changes() {
    let app = app_with_admin().await;
    let mut browser = Browser::new(&app);
    browser.sign_in("root").await;

    sqlx::query("UPDATE admin_sessions SET last_seen = ?")
        .bind(db::now() - 31 * 60)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, _) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "idle for 31 minutes");

    browser.sign_in("root").await;
    sqlx::query("UPDATE admin_sessions SET expires_at = ?")
        .bind(db::now() - 1)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, _) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "past the 12 hour limit");

    browser.sign_in("root").await;
    users::set_password(&app.state.pool, "root", "a new password")
        .await
        .unwrap();
    let (status, _) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "password changed");

    users::set_password(&app.state.pool, "root", PASSWORD)
        .await
        .unwrap();
    browser.sign_in("root").await;
    users::create_user(&app.state.pool, "other", PASSWORD, true)
        .await
        .unwrap();
    users::set_admin(&app.state.pool, "root", false)
        .await
        .unwrap();
    let (status, _) = browser.call("GET", "/session", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no longer an admin");
}

#[tokio::test]
async fn manage_users() {
    let app = app_with_admin().await;
    let alice = app.user("alice").await;
    let mut browser = Browser::new(&app);
    browser.sign_in("root").await;

    // Add a user; a normal user needs no extra password.
    let (status, body) = browser
        .call(
            "POST",
            "/users",
            Some(json!({ "username": "bob", "password": "bob password" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = browser
        .call(
            "POST",
            "/users",
            Some(json!({ "username": "BOB", "password": "bob password" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = browser
        .call(
            "POST",
            "/users",
            Some(json!({ "username": "carol", "password": "short" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Adding an admin needs the admin's own password.
    let (status, _) = browser
        .call(
            "POST",
            "/users",
            Some(json!({ "username": "dave", "password": "dave password", "is_admin": true })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = browser
        .call(
            "POST",
            "/users",
            Some(json!({ "username": "dave", "password": "dave password", "is_admin": true, "admin_password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, list) = browser.call("GET", "/users", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alice", "bob", "dave", "root"]);

    // Detail shows devices and vaults.
    app.create_vault(&alice, "Notes").await;
    let (status, detail) = browser.call("GET", "/users/alice", None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["devices"].as_array().unwrap().len(), 1);
    assert_eq!(detail["vaults"][0]["name"], "Notes");
    let (status, _) = browser.call("GET", "/users/nobody", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Sign a device out.
    let device = detail["devices"][0]["id"].as_i64().unwrap();
    let (status, _) = browser
        .call("DELETE", &format!("/users/alice/devices/{device}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.call("GET", "/api/v1/me", Some(&alice), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = browser
        .call("DELETE", &format!("/users/bob/devices/{device}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not bob's device");

    // Setting a password needs the admin's password and signs the user out.
    let alice = app.login("alice").await;
    let (status, _) = browser
        .call(
            "PUT",
            "/users/alice/password",
            Some(
                json!({ "new_password": "alice new password", "admin_password": "wrong password" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = browser
        .call(
            "PUT",
            "/users/alice/password",
            Some(json!({ "new_password": "alice new password", "admin_password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app.call("GET", "/api/v1/me", Some(&alice), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Disable and enable.
    let (status, _) = browser
        .call(
            "PUT",
            "/users/bob/disabled",
            Some(json!({ "disabled": true })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, detail) = browser.call("GET", "/users/bob", None).await;
    assert_eq!(detail["user"]["disabled"], 1);
    let (status, _) = browser
        .call(
            "PUT",
            "/users/bob/disabled",
            Some(json!({ "disabled": false })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Admin rights need the password; admins can't lock themselves out.
    let (status, _) = browser
        .call(
            "PUT",
            "/users/bob/admin",
            Some(json!({ "is_admin": true, "admin_password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = browser
        .call(
            "PUT",
            "/users/root/admin",
            Some(json!({ "is_admin": false, "admin_password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = browser
        .call(
            "PUT",
            "/users/root/disabled",
            Some(json!({ "disabled": true })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unlock_restore_and_cleanup() {
    // Room for alice's sign-in plus ten wrong passwords.
    let app = TestApp::with_config(|c| c.login_rate.burst = 100).await;
    users::create_user(&app.state.pool, "root", PASSWORD, true)
        .await
        .unwrap();
    let alice = app.user("alice").await;
    let vault = app.create_vault(&alice, "Old").await;
    let (status, _) = app
        .call(
            "DELETE",
            &format!("/api/v1/vaults/{vault}"),
            Some(&alice),
            Some(json!({ "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for _ in 0..10 {
        app.call(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({ "username": "alice", "password": "wrong password", "device_name": "x" })),
        )
        .await;
    }

    let mut browser = Browser::new(&app);
    browser.sign_in("root").await;

    let (_, detail) = browser.call("GET", "/users/alice", None).await;
    assert!(detail["user"]["locked_until"].as_i64().unwrap() > db::now());
    assert_eq!(detail["deleted_vaults"][0]["name"], "Old");
    assert!(detail["deleted_vaults"][0]["removed_at"].as_i64().unwrap() > db::now());

    let (status, _) = browser.call("POST", "/users/alice/unlock", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    app.login("alice").await;

    let (status, _) = browser
        .call(
            "POST",
            &format!("/users/alice/vaults/{vault}/restore"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, detail) = browser.call("GET", "/users/alice", None).await;
    assert_eq!(detail["vaults"][0]["name"], "Old");

    let (status, report) = browser.call("POST", "/gc", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["vaults_removed"], 0);

    let (status, overview) = browser.call("GET", "/overview", None).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["users"], 2);
    assert_eq!(overview["admins"], 1);
    assert_eq!(overview["vaults"], 1);
    assert_eq!(overview["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn the_page_is_served_with_strict_headers() {
    let app = TestApp::new().await;
    for (path, kind) in [
        ("/admin", "text/html"),
        ("/admin/admin.js", "text/javascript"),
        ("/admin/admin.css", "text/css"),
    ] {
        let res = app
            .router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let headers = res.headers();
        assert!(headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with(kind));
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"));
        assert_eq!(headers["x-frame-options"], "DENY");
    }
}

#[tokio::test]
async fn the_page_can_be_turned_off() {
    let app = TestApp::with_config(|c| c.admin_page = false).await;
    users::create_user(&app.state.pool, "root", PASSWORD, true)
        .await
        .unwrap();
    let (status, _) = app.call("GET", "/admin", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut browser = Browser::new(&app);
    let r = browser
        .raw(
            "POST",
            "/login",
            Some(json!({ "username": "root", "password": PASSWORD })),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
