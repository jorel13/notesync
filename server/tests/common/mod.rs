//! Test harness: runs the API in-process against a temporary database and folder.

#![allow(dead_code)]

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use notesync_server::{api, storage::sha256_hex, users, AppState, Config};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

pub const PASSWORD: &str = "correct horse battery";
/// Small limit so size checks are quick to test.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    _dir: TempDir,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::with_config(|_| {}).await
    }

    /// Like `new`, with some settings changed.
    pub async fn with_config(change: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            max_file_bytes: MAX_FILE_BYTES,
            ..Config::with_data_dir(dir.path())
        };
        // Tests commit far faster than a real device syncs.
        config.sync_rate.burst = 10_000;
        change(&mut config);
        let state = AppState::new(config).await.unwrap();
        Self {
            router: api::router(state.clone()),
            state,
            _dir: dir,
        }
    }

    pub async fn send(&self, req: Request<Body>) -> (StatusCode, Vec<u8>) {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, bytes)
    }

    /// Sends a JSON request and parses the JSON response (`Null` for an empty body).
    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let req = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let (status, bytes) = self.send(req).await;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, value)
    }

    pub async fn add_user(&self, username: &str) {
        users::create_user(&self.state.pool, username, PASSWORD, false)
            .await
            .unwrap();
    }

    pub async fn login(&self, username: &str) -> String {
        let (status, body) = self
            .call(
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({ "username": username, "password": PASSWORD, "device_name": "test device" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    /// Creates a user and returns a signed-in token.
    pub async fn user(&self, username: &str) -> String {
        self.add_user(username).await;
        self.login(username).await
    }

    pub async fn create_vault(&self, token: &str, name: &str) -> i64 {
        let (status, body) = self
            .call(
                "POST",
                "/api/v1/vaults",
                Some(token),
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_i64().unwrap()
    }

    pub async fn upload_raw(
        &self,
        token: &str,
        vault: i64,
        hash: &str,
        content: &[u8],
    ) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/vaults/{vault}/blobs/{hash}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(content.to_vec()))
            .unwrap();
        let (status, bytes) = self.send(req).await;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    /// Uploads content and returns its hash.
    pub async fn upload(&self, token: &str, vault: i64, content: &[u8]) -> String {
        let hash = sha256_hex(content);
        let (status, body) = self.upload_raw(token, vault, &hash, content).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        hash
    }

    pub async fn download(&self, token: &str, vault: i64, hash: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .uri(format!("/api/v1/vaults/{vault}/blobs/{hash}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        self.send(req).await
    }

    pub async fn commit(&self, token: &str, vault: i64, changes: Value) -> (StatusCode, Value) {
        self.call(
            "POST",
            &format!("/api/v1/vaults/{vault}/commit"),
            Some(token),
            Some(json!({ "changes": changes })),
        )
        .await
    }

    /// Uploads and commits one file; returns its new seq.
    pub async fn put_file(
        &self,
        token: &str,
        vault: i64,
        path: &str,
        content: &[u8],
        expect_seq: Option<i64>,
    ) -> i64 {
        let hash = self.upload(token, vault, content).await;
        let (status, body) = self
            .commit(
                token,
                vault,
                json!([{ "path": path, "op": "upsert", "hash": hash, "expect_seq": expect_seq }]),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["applied"][0]["seq"].as_i64().unwrap()
    }

    pub async fn state_of(&self, token: &str, vault: i64) -> Value {
        let (status, body) = self
            .call(
                "GET",
                &format!("/api/v1/vaults/{vault}/state"),
                Some(token),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

pub fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

pub fn paths_in(state: &Value) -> Vec<String> {
    state["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}
