//! HTTP API under `/api/v1`.

mod account;
mod extract;
mod files;
mod vaults;

use axum::{
    extract::{DefaultBodyLimit, Request},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use tower_http::trace::TraceLayer;

use crate::{error::ApiError, state::AppState};

/// Oldest plugin version this server accepts. Raised only when the protocol
/// changes or an older plugin has a security problem, not on every release.
/// 0.2.0: token refresh, and the plugin checks paths sent by the server.
pub const MIN_CLIENT_VERSION: &str = "0.2.0";
pub const CLIENT_VERSION_HEADER: &str = "x-notesync-client";

/// Request size limits. Only file uploads may be large.
const SIGN_IN_BODY_LIMIT: usize = 16 * 1024;
const DEFAULT_BODY_LIMIT: usize = 16 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    // Leave room above the file size limit so oversized uploads get a clear error.
    let upload_limit = state.config.max_file_bytes as usize + 1024 * 1024;
    let api = Router::new()
        .route("/health", get(health))
        .route(
            "/auth/login",
            post(account::login).layer(DefaultBodyLimit::max(SIGN_IN_BODY_LIMIT)),
        )
        .route("/auth/refresh", post(account::refresh))
        .route("/auth/logout", post(account::logout))
        .route("/me", get(account::me))
        .route("/me/password", put(account::change_password))
        .route("/me/devices", get(account::devices))
        .route("/me/devices/{id}", delete(account::revoke_device))
        .route("/vaults", get(vaults::list).post(vaults::create))
        .route("/vaults/{id}", patch(vaults::rename).delete(vaults::delete))
        .route("/vaults/{id}/state", get(files::state))
        .route("/vaults/{id}/changes", get(files::changes))
        .route("/vaults/{id}/blobs/missing", post(files::missing))
        .route(
            "/vaults/{id}/blobs/{hash}",
            put(files::put_blob)
                .layer(DefaultBodyLimit::max(upload_limit))
                .get(files::get_blob),
        )
        .route("/vaults/{id}/commit", post(files::commit))
        .route("/vaults/{id}/history", get(files::history))
        .layer(middleware::from_fn(check_client_version))
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT))
        .with_state(state);
    Router::new()
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn check_client_version(req: Request, next: Next) -> Result<Response, ApiError> {
    let version = req
        .headers()
        .get(CLIENT_VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_version);
    if let (Some(version), Some(min)) = (version, parse_version(MIN_CLIENT_VERSION)) {
        if version < min {
            return Err(ApiError::ClientTooOld(MIN_CLIENT_VERSION.to_string()));
        }
    }
    Ok(next.run(req).await)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("abc"), None);
        assert!(parse_version("0.9.9") < parse_version("0.10.0"));
    }
}
