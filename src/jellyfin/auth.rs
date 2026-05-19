use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Value};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::routes::internal_error,
    util::{hash_password, now_unix, stable_text_id, unix_to_jellyfin_date, verify_password},
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

#[derive(Deserialize)]
pub struct CreateApiKeyQuery {
    app: Option<String>,
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
    Json(_configuration): Json<JsonValue>,
) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM users WHERE id = ?",
            vec![user_id.clone().into()],
        ))
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
    Json(request): Json<JsonValue>,
) -> Response {
    let Some(name) = request
        .get("Name")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE users SET username = ?, display_name = ?, updated_at = ? WHERE id = ?",
            vec![
                name.into(),
                name.into(),
                now_unix().into(),
                user_id.clone().into(),
            ],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_user_policy(
    Path(_user_id): Path<String>,
    Json(_policy): Json<JsonValue>,
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

pub async fn api_keys(State(state): State<Arc<AppState>>) -> Response {
    match api_keys_inner(&state.db).await {
        Ok(keys) => Json(json!({
            "Items": keys,
            "TotalRecordCount": keys.len()
        }))
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn create_api_key(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CreateApiKeyQuery>,
) -> Response {
    match create_api_key_inner(&state, query).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Response {
    match delete_api_key_inner(&state.db, &key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn authenticate_by_name_inner(
    state: &AppState,
    request: LoginRequest,
) -> Result<JsonValue, AuthError> {
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
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO access_tokens (id, user_id, token_hash, name, device_id, created_at, last_used_at) VALUES (?, ?, ?, 'login-token', ?, ?, ?)"#,
        vec![
            Uuid::new_v4().to_string().into(),
            user.id.clone().into(),
            stable_text_id(&token).into(),
            request.device_id.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to create access token")
    .map_err(AuthError::Internal)?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE users SET last_login_at = ?, updated_at = ? WHERE id = ?",
        vec![now.into(), now.into(), user.id.clone().into()],
    ))
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

async fn api_keys_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT id, access_token, name, user_id, created_at, last_used_at FROM api_keys ORDER BY created_at ASC"#,
            vec![],
        ))
        .await
        .context("failed to list api keys")?;

    rows.into_iter()
        .map(|row| {
            let id: String = row.get_str("id")?;
            let access_token: String = row.get_str("access_token")?;
            let name: String = row.get_str("name")?;
            let user_id: String = row.get_str("user_id")?;
            let created_at: i64 = row.get_i64("created_at")?;
            let last_used_at: Option<i64> = row.get_opt_i64("last_used_at")?;
            Ok(json!({
                "Id": stable_text_id(&id),
                "AccessToken": access_token,
                "DeviceId": "",
                "AppName": name,
                "AppVersion": "",
                "DeviceName": "",
                "UserId": user_id,
                "IsActive": true,
                "DateCreated": unix_to_jellyfin_date(created_at),
                "DateRevoked": null,
                "DateLastActivity": unix_to_jellyfin_date(last_used_at.unwrap_or(created_at)),
                "UserName": null
            }))
        })
        .collect()
}

async fn create_api_key_inner(state: &AppState, query: CreateApiKeyQuery) -> anyhow::Result<()> {
    let app = query
        .app
        .as_deref()
        .map(str::trim)
        .filter(|app| !app.is_empty())
        .ok_or_else(|| anyhow::anyhow!("app is required"))?;
    let now = now_unix();
    let token = Uuid::new_v4().simple().to_string();
    let key_id = Uuid::new_v4().to_string();
    let backend = state.db.get_database_backend();

    state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO api_keys (id, access_token, name, user_id, created_at) VALUES (?, ?, ?, ?, ?)"#,
            vec![
                key_id.into(),
                token.clone().into(),
                app.into(),
                state.user_id.to_string().into(),
                now.into(),
            ],
        ))
        .await
        .context("failed to create api key")?;

    state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO access_tokens (id, user_id, token_hash, name, created_at) VALUES (?, ?, ?, ?, ?)"#,
            vec![
                Uuid::new_v4().to_string().into(),
                state.user_id.to_string().into(),
                stable_text_id(&token).into(),
                app.into(),
                now.into(),
            ],
        ))
        .await
        .context("failed to create api key access token")?;

    Ok(())
}

async fn delete_api_key_inner(db: &DatabaseConnection, key: &str) -> anyhow::Result<()> {
    let token_hash = stable_text_id(key);
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM api_keys WHERE access_token = ?",
        vec![key.into()],
    ))
    .await
    .context("failed to delete api key")?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM access_tokens WHERE token_hash = ?",
        vec![token_hash.into()],
    ))
    .await
    .context("failed to delete api key access token")?;

    Ok(())
}

async fn list_users_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, username, password_hash, is_admin, is_disabled FROM users ORDER BY username ASC",
            vec![],
        ))
        .await
        .context("failed to list users")?;

    rows.iter()
        .map(|row| {
            user_from_row(row)
                .map(|user| user_json(&user.id, &user.username, user.is_admin, user.is_disabled))
        })
        .collect()
}

async fn user_by_id_inner(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Option<UserRow>> {
    let backend = db.get_database_backend();
    db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT id, username, password_hash, is_admin, is_disabled FROM users WHERE id = ?",
        vec![user_id.into()],
    ))
    .await
    .context("failed to fetch user")?
    .as_ref()
    .map(user_from_row)
    .transpose()
}

async fn create_user_inner(
    db: &DatabaseConnection,
    request: CreateUserRequest,
) -> anyhow::Result<JsonValue> {
    let username = request.name.trim();
    if username.is_empty() {
        bail!("Name is required");
    }

    let now = now_unix();
    let user_id =
        Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("user:{username}").as_bytes()).to_string();
    let password_hash: Option<String> = match request.password.as_deref() {
        Some(password) if !password.is_empty() => Some(hash_password(password)?),
        _ => None,
    };

    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 0, ?, ?)"#,
        vec![
            user_id.clone().into(),
            username.into(),
            Value::from(password_hash),
            username.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to create user")?;

    Ok(user_json(&user_id, username, false, false))
}

async fn update_user_password_inner(
    db: &DatabaseConnection,
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
    let password_hash: Option<String> = if request.reset_password {
        None
    } else {
        Some(
            hash_password(request.new_pw.as_deref().unwrap_or_default())
                .map_err(AuthError::Internal)?,
        )
    };
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?",
        vec![Value::from(password_hash), now.into(), user_id.into()],
    ))
    .await
    .context("failed to update password")
    .map_err(AuthError::Internal)?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE access_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
        vec![now.into(), user_id.into()],
    ))
    .await
    .context("failed to revoke user tokens")
    .map_err(AuthError::Internal)?;

    Ok(())
}

async fn find_user_by_name(
    db: &DatabaseConnection,
    username: &str,
) -> anyhow::Result<Option<UserRow>> {
    let backend = db.get_database_backend();
    db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT id, username, password_hash, is_admin, is_disabled FROM users WHERE username = ?",
        vec![username.into()],
    ))
    .await
    .context("failed to fetch user by username")?
    .as_ref()
    .map(user_from_row)
    .transpose()
}

pub async fn authenticated_user_id(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> anyhow::Result<Option<String>> {
    let Some(token) = request_token(headers, query) else {
        return Ok(None);
    };
    let now = now_unix();
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT access_tokens.user_id FROM access_tokens JOIN users ON users.id = access_tokens.user_id WHERE access_tokens.token_hash = ? AND access_tokens.revoked_at IS NULL AND users.is_disabled = 0 AND (access_tokens.expires_at IS NULL OR access_tokens.expires_at > ?)"#,
            vec![stable_text_id(&token).into(), now.into()],
        ))
        .await
        .context("failed to validate access token")?;

    let Some(row) = row else {
        return Ok(None);
    };
    let user_id: String = row.get_str("user_id")?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE access_tokens SET last_used_at = ? WHERE token_hash = ?",
        vec![now.into(), stable_text_id(&token).into()],
    ))
    .await
    .context("failed to update access token usage")?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE api_keys SET last_used_at = ? WHERE access_token = ?",
        vec![now.into(), token.into()],
    ))
    .await
    .context("failed to update api key usage")?;

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

fn user_from_row(row: &sea_orm::QueryResult) -> anyhow::Result<UserRow> {
    Ok(UserRow {
        id: row.get_str("id")?,
        username: row.get_str("username")?,
        password_hash: row.get_opt_str("password_hash")?,
        is_admin: row.get_bool_from_i64("is_admin")?,
        is_disabled: row.get_bool_from_i64("is_disabled")?,
    })
}

fn user_json(user_id: &str, name: &str, is_admin: bool, is_disabled: bool) -> JsonValue {
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

fn default_user_configuration() -> JsonValue {
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
