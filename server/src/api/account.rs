use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::extract::{ApiJson, ApiPath};
use crate::{
    auth::{self, AuthUser},
    db::now,
    error::ApiError,
    state::AppState,
    users,
};

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
    device_name: String,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    id: i64,
    username: String,
    is_admin: bool,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    token: String,
    device_id: i64,
    user: UserInfo,
}

pub async fn login(
    State(app): State<AppState>,
    ApiJson(req): ApiJson<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let device_name = req.device_name.trim();
    if device_name.is_empty() || device_name.chars().count() > 100 {
        return Err(ApiError::InvalidRequest(
            "device_name must be 1 to 100 characters".into(),
        ));
    }
    let limit_key = format!("user:{}", req.username.to_lowercase());
    if app.login_limiter.is_blocked(&limit_key) {
        return Err(ApiError::RateLimited);
    }

    let user: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT id, username, password_hash, is_admin, disabled FROM users WHERE username = ?",
    )
    .bind(&req.username)
    .fetch_optional(&app.pool)
    .await?;
    let Some((user_id, username, password_hash, is_admin, disabled)) = user else {
        auth::verify_dummy(&req.password);
        app.login_limiter.record_failure(&limit_key);
        return Err(ApiError::Unauthorized);
    };
    if !auth::verify_password(&req.password, &password_hash) || disabled != 0 {
        app.login_limiter.record_failure(&limit_key);
        return Err(ApiError::Unauthorized);
    }
    app.login_limiter.clear(&limit_key);

    let token = auth::new_token();
    let created = now();
    let device_id = sqlx::query(
        "INSERT INTO devices (user_id, name, token_hash, created_at, last_seen) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(device_name)
    .bind(auth::hash_token(&token))
    .bind(created)
    .bind(created)
    .execute(&app.pool)
    .await?
    .last_insert_rowid();

    Ok(Json(LoginResponse {
        token,
        device_id,
        user: UserInfo {
            id: user_id,
            username,
            is_admin: is_admin != 0,
        },
    }))
}

pub async fn logout(user: AuthUser, State(app): State<AppState>) -> Result<StatusCode, ApiError> {
    sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ?")
        .bind(user.device_id)
        .execute(&app.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(user: AuthUser) -> Json<UserInfo> {
    Json(UserInfo {
        id: user.user_id,
        username: user.username,
        is_admin: user.is_admin,
    })
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
    #[serde(default)]
    sign_out_others: bool,
}

pub async fn change_password(
    user: AuthUser,
    State(app): State<AppState>,
    ApiJson(req): ApiJson<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    let limit_key = format!("user:{}", user.username.to_lowercase());
    if app.login_limiter.is_blocked(&limit_key) {
        return Err(ApiError::RateLimited);
    }
    let current_hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.user_id)
        .fetch_one(&app.pool)
        .await?;
    if !auth::verify_password(&req.current_password, &current_hash) {
        app.login_limiter.record_failure(&limit_key);
        return Err(ApiError::Unauthorized);
    }
    users::validate_password(&req.new_password).map_err(ApiError::InvalidRequest)?;
    let new_hash = auth::hash_password(&req.new_password)?;

    let mut tx = app.pool.begin().await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(new_hash)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    if req.sign_out_others {
        sqlx::query("UPDATE devices SET revoked = 1 WHERE user_id = ? AND id <> ?")
            .bind(user.user_id)
            .bind(user.device_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    id: i64,
    name: String,
    created_at: i64,
    last_seen: i64,
    current: bool,
}

pub async fn devices(
    user: AuthUser,
    State(app): State<AppState>,
) -> Result<Json<Vec<DeviceInfo>>, ApiError> {
    let rows: Vec<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT id, name, created_at, last_seen FROM devices
         WHERE user_id = ? AND revoked = 0 ORDER BY last_seen DESC",
    )
    .bind(user.user_id)
    .fetch_all(&app.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, created_at, last_seen)| DeviceInfo {
                id,
                name,
                created_at,
                last_seen,
                current: id == user.device_id,
            })
            .collect(),
    ))
}

pub async fn revoke_device(
    user: AuthUser,
    State(app): State<AppState>,
    ApiPath(device_id): ApiPath<i64>,
) -> Result<StatusCode, ApiError> {
    let done =
        sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ? AND user_id = ? AND revoked = 0")
            .bind(device_id)
            .bind(user.user_id)
            .execute(&app.pool)
            .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
