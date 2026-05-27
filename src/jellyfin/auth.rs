use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        access_tokens, access_tokens::Entity as AccessTokens, api_keys,
        api_keys::Entity as ApiKeys, users, users::Entity as Users,
    },
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
    match Users::delete_by_id(user_id.clone()).exec(&state.db).await {
        Ok(result) if result.rows_affected == 0 => (
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

    let result = match Users::find_by_id(user_id.clone()).one(&state.db).await {
        Ok(Some(model)) => {
            let mut active: users::ActiveModel = model.into();
            active.username = Set(name.to_string());
            active.display_name = Set(name.to_string());
            active.updated_at = Set(now_unix());
            active.update(&state.db).await
        }
        Ok(None) => return StatusCode::NO_CONTENT.into_response(),
        Err(e) => Err(e),
    };

    match result {
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

    let token_active = access_tokens::ActiveModel {
        id: Set(Uuid::new_v4().to_string()),
        user_id: Set(user.id.clone()),
        token_hash: Set(stable_text_id(&token)),
        name: Set(Some("login-token".to_string())),
        device_id: Set(request.device_id),
        created_at: Set(now),
        last_used_at: Set(Some(now)),
        ..Default::default()
    };
    AccessTokens::insert(token_active)
        .exec(db)
        .await
        .context("failed to create access token")
        .map_err(AuthError::Internal)?;

    let user_model = Users::find_by_id(&user.id)
        .one(db)
        .await
        .context("failed to find user")
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Internal(anyhow::anyhow!("user not found")))?;
    let mut user_active: users::ActiveModel = user_model.into();
    user_active.last_login_at = Set(Some(now));
    user_active.updated_at = Set(now);
    user_active
        .update(db)
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
    let models = ApiKeys::find()
        .order_by_asc(api_keys::Column::CreatedAt)
        .all(db)
        .await
        .context("failed to list api keys")?;

    Ok(models
        .into_iter()
        .map(|m| {
            json!({
                "Id": stable_text_id(&m.id),
                "AccessToken": m.access_token,
                "DeviceId": "",
                "AppName": m.name,
                "AppVersion": "",
                "DeviceName": "",
                "UserId": m.user_id,
                "IsActive": true,
                "DateCreated": unix_to_jellyfin_date(m.created_at),
                "DateRevoked": null,
                "DateLastActivity": unix_to_jellyfin_date(m.last_used_at.unwrap_or(m.created_at)),
                "UserName": null
            })
        })
        .collect())
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

    let key_active = api_keys::ActiveModel {
        id: Set(key_id),
        access_token: Set(token.clone()),
        name: Set(app.to_string()),
        user_id: Set(state.user_id.to_string()),
        created_at: Set(now),
        ..Default::default()
    };
    ApiKeys::insert(key_active)
        .exec(&state.db)
        .await
        .context("failed to create api key")?;

    let token_active = access_tokens::ActiveModel {
        id: Set(Uuid::new_v4().to_string()),
        user_id: Set(state.user_id.to_string()),
        token_hash: Set(stable_text_id(&token)),
        name: Set(Some(app.to_string())),
        created_at: Set(now),
        ..Default::default()
    };
    AccessTokens::insert(token_active)
        .exec(&state.db)
        .await
        .context("failed to create api key access token")?;

    Ok(())
}

async fn delete_api_key_inner(db: &DatabaseConnection, key: &str) -> anyhow::Result<()> {
    let token_hash = stable_text_id(key);

    ApiKeys::delete_many()
        .filter(api_keys::Column::AccessToken.eq(key))
        .exec(db)
        .await
        .context("failed to delete api key")?;

    AccessTokens::delete_many()
        .filter(access_tokens::Column::TokenHash.eq(token_hash))
        .exec(db)
        .await
        .context("failed to delete api key access token")?;

    Ok(())
}

async fn list_users_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let models = Users::find()
        .order_by_asc(users::Column::Username)
        .all(db)
        .await
        .context("failed to list users")?;

    Ok(models
        .iter()
        .map(|m| user_json(&m.id, &m.username, m.is_admin != 0, m.is_disabled != 0))
        .collect())
}

async fn user_by_id_inner(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Option<UserRow>> {
    let Some(model) = Users::find_by_id(user_id)
        .one(db)
        .await
        .context("failed to fetch user")?
    else {
        return Ok(None);
    };
    Ok(Some(UserRow {
        id: model.id,
        username: model.username,
        password_hash: model.password_hash,
        is_admin: model.is_admin != 0,
        is_disabled: model.is_disabled != 0,
    }))
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

    let active = users::ActiveModel {
        id: Set(user_id.clone()),
        username: Set(username.to_string()),
        password_hash: Set(password_hash),
        display_name: Set(username.to_string()),
        is_admin: Set(0),
        is_disabled: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    Users::insert(active)
        .exec(db)
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

    let model = Users::find_by_id(user_id)
        .one(db)
        .await
        .context("failed to find user")
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Internal(anyhow::anyhow!("user not found")))?;
    let mut active: users::ActiveModel = model.into();
    active.password_hash = Set(password_hash);
    active.updated_at = Set(now);
    active
        .update(db)
        .await
        .context("failed to update password")
        .map_err(AuthError::Internal)?;

    let backend = db.get_database_backend();
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
    let Some(model) = Users::find()
        .filter(users::Column::Username.eq(username))
        .one(db)
        .await
        .context("failed to fetch user by username")?
    else {
        return Ok(None);
    };
    Ok(Some(UserRow {
        id: model.id,
        username: model.username,
        password_hash: model.password_hash,
        is_admin: model.is_admin != 0,
        is_disabled: model.is_disabled != 0,
    }))
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
        Ok(None) => request_user_id_from_headers(headers)
            .or_else(|| query.get("UserId").cloned())
            .unwrap_or_else(|| state.user_id.to_string()),
        Err(error) => {
            tracing::warn!("failed to resolve request user: {error:#}");
            request_user_id_from_headers(headers)
                .or_else(|| query.get("UserId").cloned())
                .unwrap_or_else(|| state.user_id.to_string())
        }
    }
}

/// Extract UserId from Emby-style auth headers.
/// Supports: `Emby UserId="xxx", Client="...", Token="..."` and
///           `MediaBrowser UserId="xxx", Client="...", Token="..."`
fn request_user_id_from_headers(headers: &HeaderMap) -> Option<String> {
    for header_name in ["X-Emby-Authorization", header::AUTHORIZATION.as_str()] {
        if let Some(value) = headers.get(header_name).and_then(|v| v.to_str().ok()) {
            for part in value.split(',') {
                let part = part.trim();
                if let Some(id) = part.strip_prefix("UserId=\"").and_then(|s| s.find('"').map(|end| &s[..end])) {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
            }
        }
    }
    None
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

/// GET /Users/Query — paginated user list with filtering
pub async fn users_query(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let start_index = query.get("StartIndex").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let limit = query.get("Limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(100);
    let search_term = query.get("SearchTerm").map(String::as_str);
    let is_disabled = query.get("IsDisabled").and_then(|v| v.parse::<bool>().ok());
    let is_admin = query.get("IsAdministrator").and_then(|v| v.parse::<bool>().ok());

    match list_users_inner(&state.db).await {
        Ok(mut users) => {
            // Apply filters
            if let Some(term) = search_term {
                let term_lower = term.to_lowercase();
                users.retain(|u| {
                    u.get("Name").and_then(JsonValue::as_str).unwrap_or("").to_lowercase().contains(&term_lower)
                });
            }
            if let Some(disabled) = is_disabled {
                users.retain(|u| {
                    u.get("IsDisabled").and_then(JsonValue::as_bool).unwrap_or(false) == disabled
                });
            }
            if let Some(admin) = is_admin {
                users.retain(|u| {
                    u.get("IsAdministrator").and_then(JsonValue::as_bool).unwrap_or(false) == admin
                });
            }

            let total = users.len();
            let items: Vec<_> = users.into_iter().skip(start_index).take(limit).collect();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// GET /Users/{id}/Authenticate — legacy auth endpoint (stub)
pub async fn user_authenticate_legacy() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// POST /Users/{id}/Connect/Link — link user to Emby Connect (stub)
pub async fn user_connect_link() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /Users/{id}/Connect/Link — unlink from Emby Connect (stub)
pub async fn user_connect_link_delete() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

enum AuthError {
    Unauthorized(String),
    Internal(anyhow::Error),
}
