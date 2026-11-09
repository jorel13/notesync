use axum::{
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;

/// A path whose server version differs from the version the client expected.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PathConflict {
    pub path: String,
    pub current_hash: Option<String>,
    pub current_seq: Option<i64>,
}

/// Every API error, rendered as `{"error": {"code", "message"}}`.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication required")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("some paths changed on the server")]
    Conflict(Vec<PathConflict>),
    #[error("path differs only by letter case from existing path {0:?}")]
    CaseConflict(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("content does not match its hash")]
    HashMismatch,
    #[error("file exceeds the size limit")]
    TooLarge,
    #[error("content {0} has not been uploaded")]
    MissingBlob(String),
    #[error("a vault with this name already exists")]
    NameTaken,
    #[error("{0}")]
    RateLimited(String),
    #[error("client version is too old; the minimum is {0}")]
    ClientTooOld(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) | Self::CaseConflict(_) | Self::NameTaken => StatusCode::CONFLICT,
            Self::InvalidPath(_)
            | Self::InvalidRequest(_)
            | Self::HashMismatch
            | Self::MissingBlob(_) => StatusCode::BAD_REQUEST,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::ClientTooOld(_) => StatusCode::UPGRADE_REQUIRED,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::CaseConflict(_) => "case_conflict",
            Self::InvalidPath(_) => "invalid_path",
            Self::InvalidRequest(_) => "invalid_request",
            Self::HashMismatch => "hash_mismatch",
            Self::TooLarge => "too_large",
            Self::MissingBlob(_) => "missing_blob",
            Self::NameTaken => "name_taken",
            Self::RateLimited(_) => "rate_limited",
            Self::ClientTooOld(_) => "client_too_old",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<JsonRejection> for ApiError {
    fn from(err: JsonRejection) -> Self {
        if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self::TooLarge;
        }
        Self::InvalidRequest(err.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(err: PathRejection) -> Self {
        Self::InvalidRequest(err.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(err: QueryRejection) -> Self {
        Self::InvalidRequest(err.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(err) = &self {
            tracing::error!("internal error: {err:#}");
        }
        let mut body = json!({ "code": self.code(), "message": self.to_string() });
        if let Self::Conflict(conflicts) = &self {
            body["conflicts"] = json!(conflicts);
        }
        (self.status(), Json(json!({ "error": body }))).into_response()
    }
}
