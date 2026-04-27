use std::sync::Arc;

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    jellyfin::routes::internal_error,
    util::{hash_password, now_unix, stable_text_id, verify_password},
};

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw", default)]
    password: String,
    #[serde(rename = "DeviceId")]
    device_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Password")]
    password: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdatePasswordRequest {
    #[serde(rename = "CurrentPw")]
    current_pw: Option<String>,
    #[serde(rename = "NewPw")]
    new_pw: Option<String>,
    #[serde(rename = "ResetPassword", default)]
    reset_password: bool,
}

struct UserRow {
    id: String,
    username: String,
    password_hash: Option<String>,
    is_admin: bool,
    is_disabled: bool,
}

pub async fn authenticate_by_name(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LoginRequest>,
) -> Response {
    match authenticate_by_name_inner(&state, request).await {
        Ok(response) => Json(response).into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

pub async fn list_users(State(state): State<Arc<AppState>>) -> Response {
    match list_users_inner(&state.db).await {
        Ok(users) => Json(users).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn public_users(State(state): State<Arc<AppState>>) -> Response {
    match list_users_inner(&state.db).await {
        Ok(users) => Json(users).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn user_by_id(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match user_by_id_inner(&state.db, &user_id).await {
        Ok(Some(user)) => Json(user_json(
            &user.id,
            &user.username,
            user.is_admin,
            user.is_disabled,
        ))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn current_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match authenticated_user_id(&state.db, &headers, &query).await {
        Ok(Some(user_id)) => match user_by_id_inner(&state.db, &user_id).await {
            Ok(Some(user)) => Json(user_json(
                &user.id,
                &user.username,
                user.is_admin,
                user.is_disabled,
            ))
            .into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "User not found" })),
            )
                .into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "Error": "Authentication token is required" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn create_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateUserRequest>,
) -> Response {
    match create_user_inner(&state.db, request).await {
        Ok(user) => Json(user).into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_user_password(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(request): Json<UpdatePasswordRequest>,
) -> Response {
    match update_user_password_inner(&state.db, &user_id, request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::FORBIDDEN, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) if error.to_string().contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

pub async fn update_user_configuration(
    Path(_user_id): Path<String>,
    Json(_configuration): Json<Value>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await
    {
        Ok(result) if result.rows_affected() == 0 => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(request): Json<Value>,
) -> Response {
    let Some(name) = request
        .get("Name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return StatusCode::NO_CONTENT.into_response();
    };

    match sqlx::query(
        "UPDATE users SET username = ?, display_name = ?, updated_at = ? WHERE id = ?",
    )
    .bind(name)
    .bind(name)
    .bind(now_unix())
    .bind(&user_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_user_policy(
    Path(_user_id): Path<String>,
    Json(_policy): Json<Value>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn forgot_password() -> impl IntoResponse {
    Json(json!({
        "Action": "ContactAdmin",
        "PinFile": "",
        "PinExpirationDate": null
    }))
}

async fn authenticate_by_name_inner(
    state: &AppState,
    request: LoginRequest,
) -> Result<Value, AuthError> {
    let db = &state.db;
    let username = request.username.trim();
    if username.is_empty() {
        return Err(AuthError::Unauthorized("Username is required".to_string()));
    }

    let user = find_user_by_name(db, username)
        .await
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Unauthorized("Invalid username or password".to_string()))?;
    if user.is_disabled {
        return Err(AuthError::Unauthorized("User is disabled".to_string()));
    }

    let Some(password_hash) = &user.password_hash else {
        return Err(AuthError::Unauthorized(
            "Password is not configured".to_string(),
        ));
    };
    if !verify_password(&request.password, password_hash) {
        return Err(AuthError::Unauthorized(
            "Invalid username or password".to_string(),
        ));
    }

    let now = now_unix();
    let token = Uuid::new_v4().simple().to_string();
    sqlx::query(
        r#"INSERT INTO access_tokens (id, user_id, token_hash, name, device_id, created_at, last_used_at) VALUES (?, ?, ?, 'login-token', ?, ?, ?)"#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&user.id)
    .bind(stable_text_id(&token))
    .bind(request.device_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .context("failed to create access token")
    .map_err(AuthError::Internal)?;

    sqlx::query("UPDATE users SET last_login_at = ?, updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(now)
        .bind(&user.id)
        .execute(db)
        .await
        .context("failed to update last login time")
        .map_err(AuthError::Internal)?;

    crate::jellyfin::system::log_activity(
        state,
        &format!("User {} logged in", user.username),
        "Authentication",
        Some(&user.id),
        None,
    )
    .await;

    Ok(json!({
        "User": user_json(&user.id, &user.username, user.is_admin, user.is_disabled),
        "AccessToken": token,
        "ServerId": "jellyfin-rs"
    }))
}

async fn list_users_inner(db: &AnyPool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT id, username, password_hash, is_admin, is_disabled FROM users ORDER BY username ASC",
    )
    .fetch_all(db)
    .await
    .context("failed to list users")?;

    rows.into_iter()
        .map(|row| {
            user_from_row(&row)
                .map(|user| user_json(&user.id, &user.username, user.is_admin, user.is_disabled))
        })
        .collect()
}

async fn user_by_id_inner(db: &AnyPool, user_id: &str) -> anyhow::Result<Option<UserRow>> {
    sqlx::query("SELECT id, username, password_hash, is_admin, is_disabled FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await
        .context("failed to fetch user")?
        .as_ref()
        .map(user_from_row)
        .transpose()
}

async fn create_user_inner(db: &AnyPool, request: CreateUserRequest) -> anyhow::Result<Value> {
    let username = request.name.trim();
    if username.is_empty() {
        bail!("Name is required");
    }

    let now = now_unix();
    let user_id =
        Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("user:{username}").as_bytes()).to_string();
    let password_hash = match request.password.as_deref() {
        Some(password) if !password.is_empty() => Some(hash_password(password)?),
        _ => None,
    };

    sqlx::query(r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 0, ?, ?)"#)
        .bind(&user_id)
        .bind(username)
        .bind(&password_hash)
        .bind(username)
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .context("failed to create user")?;

    Ok(user_json(&user_id, username, false, false))
}

async fn update_user_password_inner(
    db: &AnyPool,
    user_id: &str,
    request: UpdatePasswordRequest,
) -> Result<(), AuthError> {
    let user = user_by_id_inner(db, user_id)
        .await
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Internal(anyhow::anyhow!("user not found")))?;

    if let Some(current_pw) = request.current_pw.as_deref() {
        let Some(password_hash) = &user.password_hash else {
            return Err(AuthError::Unauthorized(
                "Current password is not configured".to_string(),
            ));
        };
        if !verify_password(current_pw, password_hash) {
            return Err(AuthError::Unauthorized(
                "Invalid user or password entered".to_string(),
            ));
        }
    }

    let now = now_unix();
    let password_hash = if request.reset_password {
        None
    } else {
        Some(
            hash_password(request.new_pw.as_deref().unwrap_or_default())
                .map_err(AuthError::Internal)?,
        )
    };

    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(password_hash)
        .bind(now)
        .bind(user_id)
        .execute(db)
        .await
        .context("failed to update password")
        .map_err(AuthError::Internal)?;

    sqlx::query("UPDATE access_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
        .bind(now)
        .bind(user_id)
        .execute(db)
        .await
        .context("failed to revoke user tokens")
        .map_err(AuthError::Internal)?;

    Ok(())
}

async fn find_user_by_name(db: &AnyPool, username: &str) -> anyhow::Result<Option<UserRow>> {
    sqlx::query(
        "SELECT id, username, password_hash, is_admin, is_disabled FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .context("failed to fetch user by username")?
    .as_ref()
    .map(user_from_row)
    .transpose()
}

pub async fn authenticated_user_id(
    db: &AnyPool,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> anyhow::Result<Option<String>> {
    let Some(token) = request_token(headers, query) else {
        return Ok(None);
    };
    let now = now_unix();
    let row = sqlx::query(
        r#"SELECT access_tokens.user_id FROM access_tokens JOIN users ON users.id = access_tokens.user_id WHERE access_tokens.token_hash = ? AND access_tokens.revoked_at IS NULL AND users.is_disabled = 0 AND (access_tokens.expires_at IS NULL OR access_tokens.expires_at > ?)"#,
    )
    .bind(stable_text_id(&token))
    .bind(now)
    .fetch_optional(db)
    .await
    .context("failed to validate access token")?;

    let Some(row) = row else {
        return Ok(None);
    };
    let user_id: String = row.try_get("user_id")?;
    sqlx::query("UPDATE access_tokens SET last_used_at = ? WHERE token_hash = ?")
        .bind(now)
        .bind(stable_text_id(&token))
        .execute(db)
        .await
        .context("failed to update access token usage")?;

    Ok(Some(user_id))
}

pub async fn request_user_id_or_default(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> String {
    match authenticated_user_id(&state.db, headers, query).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => query
            .get("UserId")
            .cloned()
            .unwrap_or_else(|| state.user_id.to_string()),
        Err(error) => {
            tracing::warn!("failed to resolve request user: {error:#}");
            state.user_id.to_string()
        }
    }
}

pub fn request_token(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    query
        .get("api_key")
        .or_else(|| query.get("ApiKey"))
        .filter(|token| !token.trim().is_empty())
        .cloned()
        .or_else(|| header_token(headers, "X-Emby-Token"))
        .or_else(|| header_token(headers, header::AUTHORIZATION.as_str()))
        .or_else(|| header_token(headers, "X-Emby-Authorization"))
}

pub fn header_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains("Token=") {
        return Some(value.to_string());
    }
    value.split(',').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("Token=")
            .map(|token| token.trim_matches('"').to_string())
            .filter(|token| !token.is_empty())
    })
}

fn user_from_row(row: &sqlx::any::AnyRow) -> anyhow::Result<UserRow> {
    Ok(UserRow {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        password_hash: row.try_get("password_hash")?,
        is_admin: row.try_get::<i64, _>("is_admin")? != 0,
        is_disabled: row.try_get::<i64, _>("is_disabled")? != 0,
    })
}

fn user_json(user_id: &str, name: &str, is_admin: bool, is_disabled: bool) -> Value {
    json!({
        "Name": name,
        "Id": user_id,
        "ServerId": "jellyfin-rs",
        "HasPassword": true,
        "Configuration": default_user_configuration(),
        "Policy": {
            "IsAdministrator": is_admin,
            "IsDisabled": is_disabled
        }
    })
}

fn default_user_configuration() -> Value {
    json!({
        "AudioLanguagePreference": "",
        "SubtitleLanguagePreference": "",
        "SubtitleMode": "Default",
        "PlayDefaultAudioTrack": true,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true,
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "OrderedViews": [],
        "HidePlayedInLatest": false,
        "CastReceiverId": ""
    })
}

enum AuthError {
    Unauthorized(String),
    Internal(anyhow::Error),
}
