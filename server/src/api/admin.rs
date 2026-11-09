//! The admin web page and its API under `/api/v1/admin`.
//!
//! Only admin accounts can sign in. A sign-in is a short session in an
//! `HttpOnly`, `SameSite=Strict` cookie, separate from device tokens. Every
//! request must also carry the `x-notesync-admin` header, which other sites
//! can't add without the browser asking the server first, so a link or form on
//! another site can't act as a signed-in admin. Granting admin rights and
//! setting someone's password need the admin's own password again.

use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, SET_COOKIE},
        request::Parts,
        HeaderMap, HeaderValue, StatusCode,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::extract::{ApiJson, ApiPath};
use crate::{
    auth,
    db::now,
    error::ApiError,
    gc,
    state::AppState,
    users::{self, MAX_PASSWORD_CHARS},
};

/// A session ends after this long without a request...
pub const IDLE_SECS: i64 = 30 * 60;
/// ...or this long after signing in, whichever comes first.
pub const MAX_SESSION_SECS: i64 = 12 * 60 * 60;

pub const COOKIE_NAME: &str = "notesync_admin";
pub const HEADER: &str = "x-notesync-admin";
const COOKIE_PATH: &str = "/api/v1/admin";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/login",
            post(login).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/logout", post(logout))
        .route("/session", get(session))
        .route("/overview", get(overview))
        .route("/gc", post(run_gc))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{name}", get(user_detail))
        .route("/users/{name}/password", put(set_password))
        .route("/users/{name}/disabled", put(set_disabled))
        .route("/users/{name}/admin", put(set_admin))
        .route("/users/{name}/unlock", post(unlock))
        .route("/users/{name}/devices/{id}", delete(revoke_device))
        .route("/users/{name}/vaults/{id}/restore", post(restore_vault))
        .layer(middleware::map_response(no_store))
}

async fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// ---- The page itself ----

const PAGE_HTML: &str = include_str!("../admin/index.html");
const PAGE_JS: &str = include_str!("../admin/admin.js");
const PAGE_CSS: &str = include_str!("../admin/admin.css");

/// Routes for the page's files, served outside `/api/v1`.
pub fn page_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin",
            get(|| page(PAGE_HTML, "text/html; charset=utf-8")),
        )
        .route(
            "/admin/",
            get(|| page(PAGE_HTML, "text/html; charset=utf-8")),
        )
        .route(
            "/admin/admin.js",
            get(|| page(PAGE_JS, "text/javascript; charset=utf-8")),
        )
        .route(
            "/admin/admin.css",
            get(|| page(PAGE_CSS, "text/css; charset=utf-8")),
        )
}

async fn page(body: &'static str, content_type: &'static str) -> Response {
    let mut res = body.into_response();
    let headers = res.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    // Only this server's own script and styles, no frames, no inline code.
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             img-src 'self' data:; form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

// ---- Sessions ----

/// A signed-in admin, from the session cookie.
#[derive(Debug, Clone)]
pub struct AdminSession {
    pub session_id: i64,
    pub user_id: i64,
    pub username: String,
    pub expires_at: i64,
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

fn require_header(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key(HEADER) {
        Ok(())
    } else {
        Err(ApiError::Forbidden(format!(
            "admin requests must include the {HEADER} header"
        )))
    }
}

impl FromRequestParts<AppState> for AdminSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        require_header(&parts.headers)?;
        let token = cookie_value(&parts.headers, COOKIE_NAME).ok_or(ApiError::Unauthorized)?;
        let now = now();
        let row: Option<(i64, i64, String, i64)> = sqlx::query_as(
            "SELECT s.id, u.id, u.username, s.expires_at
             FROM admin_sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ? AND s.expires_at > ? AND s.last_seen > ?
               AND u.is_admin = 1 AND u.disabled = 0",
        )
        .bind(auth::hash_token(token))
        .bind(now)
        .bind(now - IDLE_SECS)
        .fetch_optional(&state.pool)
        .await?;
        let (session_id, user_id, username, expires_at) = row.ok_or(ApiError::Unauthorized)?;
        state
            .limits
            .requests
            .check(&format!("admin:{session_id}"))
            .map_err(auth::too_many)?;
        sqlx::query("UPDATE admin_sessions SET last_seen = ? WHERE id = ?")
            .bind(now)
            .bind(session_id)
            .execute(&state.pool)
            .await?;
        Ok(Self {
            session_id,
            user_id,
            username,
            expires_at,
        })
    }
}

fn session_cookie(token: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={token}; Path={COOKIE_PATH}; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict"
    ))
    .expect("cookie is ASCII")
}

#[derive(Debug, Serialize)]
pub struct SessionInfo {
    username: String,
    expires_at: i64,
    idle_secs: i64,
}

impl From<&AdminSession> for SessionInfo {
    fn from(s: &AdminSession) -> Self {
        Self {
            username: s.username.clone(),
            expires_at: s.expires_at,
            idle_secs: IDLE_SECS,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    State(app): State<AppState>,
    headers: HeaderMap,
    ApiJson(req): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    require_header(&headers)?;
    if req.password.chars().count() > MAX_PASSWORD_CHARS {
        return Err(ApiError::Unauthorized);
    }
    app.limits
        .login_total
        .check("all")
        .map_err(auth::too_many)?;
    app.limits
        .login
        .check(&auth::limit_key(&req.username))
        .map_err(auth::too_many)?;

    let user: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, username, password_hash, is_admin, disabled FROM users WHERE username = ?",
    )
    .bind(&req.username)
    .fetch_optional(&app.pool)
    .await?;
    let hash = user.as_ref().map(|u| u.2.clone());
    if !auth::check_password(&app, &req.username, &req.password, hash).await? {
        return Err(ApiError::Unauthorized);
    }
    let Some((user_id, username, _, is_admin, disabled)) = user else {
        return Err(ApiError::Unauthorized);
    };
    if disabled != 0 {
        return Err(ApiError::Unauthorized);
    }
    if is_admin == 0 {
        tracing::warn!(username = %username, "admin page sign-in by a non-admin");
        return Err(ApiError::Forbidden(
            "This account isn't an admin.".to_string(),
        ));
    }

    let token = auth::new_token();
    let now = now();
    let expires_at = now + MAX_SESSION_SECS;
    let session_id = sqlx::query(
        "INSERT INTO admin_sessions (user_id, token_hash, created_at, last_seen, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(auth::hash_token(&token))
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .execute(&app.pool)
    .await?
    .last_insert_rowid();
    tracing::info!(admin = %username, "signed in to the admin page");

    let session = AdminSession {
        session_id,
        user_id,
        username,
        expires_at,
    };
    let mut res = Json(SessionInfo::from(&session)).into_response();
    res.headers_mut()
        .insert(SET_COOKIE, session_cookie(&token, MAX_SESSION_SECS));
    Ok(res)
}

async fn logout(admin: AdminSession, State(app): State<AppState>) -> Result<Response, ApiError> {
    sqlx::query("DELETE FROM admin_sessions WHERE id = ?")
        .bind(admin.session_id)
        .execute(&app.pool)
        .await?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(SET_COOKIE, session_cookie("", 0));
    Ok(res)
}

async fn session(admin: AdminSession) -> Json<SessionInfo> {
    Json(SessionInfo::from(&admin))
}

/// Checks the signed-in admin's own password before a sensitive action.
async fn confirm(app: &AppState, admin: &AdminSession, password: &str) -> Result<(), ApiError> {
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(admin.user_id)
        .fetch_one(&app.pool)
        .await?;
    if auth::check_password(app, &admin.username, password, Some(hash)).await? {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "Your password is incorrect.".to_string(),
        ))
    }
}

/// Turns an error from `users` into an API error: unknown users are 404,
/// rule violations 400, database failures 500.
fn user_error(err: anyhow::Error) -> ApiError {
    if err.downcast_ref::<sqlx::Error>().is_some() {
        return ApiError::Internal(err);
    }
    let message = err.to_string();
    if message.starts_with("no user named") {
        ApiError::NotFound
    } else {
        ApiError::InvalidRequest(message)
    }
}

// ---- Server ----

async fn overview(
    _admin: AdminSession,
    State(app): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let now = now();
    let (users, admins, disabled): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(is_admin), 0), COALESCE(SUM(disabled), 0) FROM users",
    )
    .fetch_one(&app.pool)
    .await?;
    let locked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM login_failures WHERE locked_until > ?")
            .bind(now)
            .fetch_one(&app.pool)
            .await?;
    let devices: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE revoked = 0 AND expires_at > ?")
            .bind(now)
            .fetch_one(&app.pool)
            .await?;
    let (vaults, deleted_vaults): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(deleted_at IS NULL), 0), COALESCE(SUM(deleted_at IS NOT NULL), 0)
         FROM vaults",
    )
    .fetch_one(&app.pool)
    .await?;
    let (files, file_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM current_files")
            .fetch_one(&app.pool)
            .await?;
    let stored_bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM blobs")
        .fetch_one(&app.pool)
        .await?;
    let mut database_bytes = 0;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = app.config.db_path().into_os_string();
        path.push(suffix);
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            database_bytes += meta.len();
        }
    }
    let config = &app.config;
    let rate =
        |r: crate::config::Rate| json!({ "burst": r.burst, "every_secs": r.every.as_secs_f64() });
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "min_client_version": super::MIN_CLIENT_VERSION,
        "started_at": app.started_at,
        "now": now,
        "users": users,
        "admins": admins,
        "disabled_users": disabled,
        "locked_users": locked,
        "devices": devices,
        "vaults": vaults,
        "deleted_vaults": deleted_vaults,
        "files": files,
        "file_bytes": file_bytes,
        "stored_bytes": stored_bytes,
        "database_bytes": database_bytes,
        "data_dir": config.data_dir.display().to_string(),
        "settings": {
            "max_file_bytes": config.max_file_bytes,
            "history_days": config.history_days,
            "token_days": config.token_days,
            "login_rate": rate(config.login_rate),
            "sync_rate": rate(config.sync_rate),
            "request_rate": rate(config.request_rate),
        },
    })))
}

async fn run_gc(
    admin: AdminSession,
    State(app): State<AppState>,
) -> Result<Json<gc::GcReport>, ApiError> {
    let report = gc::run(&app).await?;
    tracing::info!(admin = %admin.username, ?report, "cleanup run from the admin page");
    Ok(Json(report))
}

// ---- Users ----

async fn list_users(
    _admin: AdminSession,
    State(app): State<AppState>,
) -> Result<Json<Vec<users::UserSummary>>, ApiError> {
    Ok(Json(users::list(&app.pool).await?))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
    /// Needed only to create another admin.
    #[serde(default)]
    admin_password: String,
}

async fn create_user(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiJson(req): ApiJson<CreateUserRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    users::validate_username(&req.username).map_err(ApiError::InvalidRequest)?;
    users::validate_password(&req.password).map_err(ApiError::InvalidRequest)?;
    if req.is_admin {
        confirm(&app, &admin, &req.admin_password).await?;
    }
    let hash = auth::hash_password_limited(&app, req.password).await?;
    let id = users::insert_user(&app.pool, &req.username, &hash, req.is_admin)
        .await
        .map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %req.username, is_admin = req.is_admin, "user created");
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "username": req.username })),
    ))
}

async fn user_detail(
    _admin: AdminSession,
    State(app): State<AppState>,
    ApiPath(name): ApiPath<String>,
) -> Result<Json<Value>, ApiError> {
    let user = users::list(&app.pool)
        .await?
        .into_iter()
        .find(|u| u.username.eq_ignore_ascii_case(&name))
        .ok_or(ApiError::NotFound)?;
    let devices = users::devices(&app.pool, &user.username)
        .await
        .map_err(user_error)?;
    let vaults = users::vaults(&app.pool, &user.username, false)
        .await
        .map_err(user_error)?;
    let deleted = users::vaults(&app.pool, &user.username, true)
        .await
        .map_err(user_error)?;
    let history_secs = app.config.history_days as i64 * 86_400;
    let deleted: Vec<Value> = deleted
        .into_iter()
        .map(|v| {
            let removed_at = v.deleted_at.unwrap_or(0) + history_secs;
            let mut value = json!(v);
            value["removed_at"] = json!(removed_at);
            value
        })
        .collect();
    Ok(Json(json!({
        "user": user,
        "devices": devices,
        "vaults": vaults,
        "deleted_vaults": deleted,
    })))
}

#[derive(Debug, Deserialize)]
pub struct SetPasswordRequest {
    new_password: String,
    admin_password: String,
}

/// Sets a new password and signs the user out everywhere.
async fn set_password(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath(name): ApiPath<String>,
    ApiJson(req): ApiJson<SetPasswordRequest>,
) -> Result<StatusCode, ApiError> {
    users::user_id(&app.pool, &name).await.map_err(user_error)?;
    users::validate_password(&req.new_password).map_err(ApiError::InvalidRequest)?;
    confirm(&app, &admin, &req.admin_password).await?;
    let hash = auth::hash_password_limited(&app, req.new_password).await?;
    users::set_password_hash(&app.pool, &name, &hash)
        .await
        .map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %name, "password set from the admin page");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct DisabledRequest {
    disabled: bool,
}

async fn set_disabled(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath(name): ApiPath<String>,
    ApiJson(req): ApiJson<DisabledRequest>,
) -> Result<StatusCode, ApiError> {
    let id = users::user_id(&app.pool, &name).await.map_err(user_error)?;
    if req.disabled && id == admin.user_id {
        return Err(ApiError::InvalidRequest(
            "You can't disable your own account.".to_string(),
        ));
    }
    users::set_disabled(&app.pool, &name, req.disabled)
        .await
        .map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %name, disabled = req.disabled, "user disabled or enabled");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct AdminRequest {
    is_admin: bool,
    admin_password: String,
}

async fn set_admin(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath(name): ApiPath<String>,
    ApiJson(req): ApiJson<AdminRequest>,
) -> Result<StatusCode, ApiError> {
    let id = users::user_id(&app.pool, &name).await.map_err(user_error)?;
    if !req.is_admin && id == admin.user_id {
        return Err(ApiError::InvalidRequest(
            "You can't remove your own admin rights.".to_string(),
        ));
    }
    confirm(&app, &admin, &req.admin_password).await?;
    users::set_admin(&app.pool, &name, req.is_admin)
        .await
        .map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %name, is_admin = req.is_admin, "admin rights changed");
    Ok(StatusCode::NO_CONTENT)
}

async fn unlock(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath(name): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    users::unlock(&app.pool, &name).await.map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %name, "sign-in lockout cleared");
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_device(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath((name, device_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    users::user_id(&app.pool, &name).await.map_err(user_error)?;
    users::revoke_device(&app.pool, &name, device_id)
        .await
        .map_err(|_| ApiError::NotFound)?;
    tracing::info!(admin = %admin.username, username = %name, device_id, "device signed out");
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_vault(
    admin: AdminSession,
    State(app): State<AppState>,
    ApiPath((name, vault_id)): ApiPath<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    users::restore_vault(&app.pool, &name, vault_id)
        .await
        .map_err(user_error)?;
    tracing::info!(admin = %admin.username, username = %name, vault_id, "vault restored");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("a=1; notesync_admin=abc; b=2"),
        );
        assert_eq!(cookie_value(&headers, COOKIE_NAME), Some("abc"));
        assert_eq!(cookie_value(&headers, "missing"), None);
        assert_eq!(cookie_value(&headers, "notesync"), None);
    }
}
