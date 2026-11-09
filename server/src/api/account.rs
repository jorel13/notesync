use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use super::extract::{ApiJson, ApiPath};
use crate::{
    auth::{self, AuthUser},
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
    #[serde(flatten)]
    token: auth::IssuedToken,
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
    if req.password.chars().count() > users::MAX_PASSWORD_CHARS {
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

    let (device_id, token) = auth::issue_for_new_device(&app, user_id, device_name).await?;
    tracing::info!(username = %username, device_id, "signed in");
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

/// Replaces this device's token with a new one. Devices call this regularly
/// so a copied token stops working soon after.
pub async fn refresh(
    user: AuthUser,
    State(app): State<AppState>,
) -> Result<Json<auth::IssuedToken>, ApiError> {
    Ok(Json(auth::refresh(&app, user.device_id).await?))
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
    /// Signs out every other device too. On by default: a password is usually
    /// changed because it may have leaked.
    #[serde(default = "yes")]
    sign_out_others: bool,
}

fn yes() -> bool {
    true
}

/// Checks the signed-in user's password before a sensitive action.
pub(crate) async fn confirm_password(
    app: &AppState,
    user: &AuthUser,
    password: &str,
) -> Result<(), ApiError> {
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.user_id)
        .fetch_one(&app.pool)
        .await?;
    if auth::check_password(app, &user.username, password, Some(hash)).await? {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

pub async fn change_password(
    user: AuthUser,
    State(app): State<AppState>,
    ApiJson(req): ApiJson<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    confirm_password(&app, &user, &req.current_password).await?;
    users::validate_password(&req.new_password).map_err(ApiError::InvalidRequest)?;
    let new_hash = auth::hash_password_limited(&app, req.new_password).await?;

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
    tracing::info!(username = %user.username, "password changed");
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
