use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::{
    app::state::{AppState, PlaybackSession, PlaybackState, SessionCapabilities},
    db::row_ext::QueryResultExt,
    entities::{
        access_tokens, access_tokens::Entity as AccessTokens, api_keys,
        api_keys::Entity as ApiKeys, app_settings, app_settings::Entity as AppSettings, users,
        users::Entity as Users,
    },
    jellyfin::routes::internal_error,
    util::{hash_password, now_unix, stable_text_id, unix_to_jellyfin_date, verify_password},
};

const PUBLIC_PATHS: &[&str] = &[
    "/GetUtcTime",
    "/System/Info/Public",
    "/System/Endpoint",
    "/System/Ping",
    "/Users/authenticatebyname",
    "/Users/AuthenticateByName",
    "/users/authenticatebyname",
    "/Users/Public",
    "/users/public",
    "/Users/ForgotPassword",
    "/Users/ForgotPassword/Pin",
    "/QuickConnect/Enabled",
    "/QuickConnect/Authorize",
    "/QuickConnect/Connect",
    "/QuickConnect/Initiate",
    "/Branding/Configuration",
    "/Branding/Css",
    "/Branding/Css.css",
    "/Branding/Splashscreen",
    "/description.xml",
    "/web/ConfigurationPages",
    "/web/ConfigurationPage",
    "/web/strings",
    "/web/stringset",
    "/openapi",
    "/openapi.json",
    "/swagger",
    "/swagger.json",
];

const ADMIN_PREFIXES: &[&str] = &[
    "/Auth/PasswordResetProviders",
    "/Auth/Providers",
    "/Auth/Keys",
    "/Backup",
    "/BackupRestore",
    "/Branding/Splashscreen",
    "/Connect/Pending",
    "/Devices",
    "/Dlna/ProfileInfos",
    "/Dlna/Profiles",
    "/Environment",
    "/Library",
    "/Libraries",
    "/Notification/SMTP",
    "/Notifications/Admin",
    "/Notifications/Services",
    "/Packages/Updates",
    "/Plugins",
    "/Repositories",
    "/Reports",
    "/ScheduledTasks",
    "/System/ActivityLog",
    "/System/Configuration",
    "/System/Info/Storage",
    "/System/Logs",
    "/System/Restart",
    "/System/Shutdown",
    "/Startup",
    "/user_usage_stats/import_backup",
    "/user_usage_stats/load_backup",
    "/user_usage_stats/save_backup",
    "/user_usage_stats/submit_custom_query",
    "/user_usage_stats/user_manage",
    "/Users/New",
    "/Users/Configuration",
    "/Users/Password",
];

const ADMIN_CONTAINS: &[&str] = &[
    "/ContentType",
    "/Lyrics",
    "/MakePrivate",
    "/MakePublic",
    "/MetadataEditor",
    "/MergeVersions",
    "/Password",
    "/Policy",
    "/Connect/Link",
    "/RemoteImages/Download",
    "/RemoteSearch/Apply",
    "/Refresh",
    "/Subtitles",
    "/Tags",
    "/metadata/reset",
];

const QUICK_CONNECT_PREFIX: &str = "quick_connect:";
const QUICK_CONNECT_CODE_PREFIX: &str = "quick_connect_code:";
const QUICK_CONNECT_TTL_SECONDS: i64 = 10 * 60;
const MAX_API_KEY_APP_LEN: usize = 128;
const MAX_API_KEY_TOKEN_LEN: usize = 256;
const MAX_USER_NAME_LEN: usize = 128;
const MAX_USER_PASSWORD_LEN: usize = 1024;
const MAX_USER_SETTING_STRING_LEN: usize = 512;
const MAX_USER_SETTING_ARRAY_ITEMS: usize = 256;
const MAX_USER_SETTING_OBJECT_FIELDS: usize = 64;
const MAX_USER_SETTING_DEPTH: usize = 3;

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw", alias = "Password", default)]
    password: String,
    #[serde(rename = "DeviceId")]
    device_id: Option<String>,
}

#[derive(Deserialize)]
pub struct AuthenticateUserRequest {
    #[serde(rename = "Pw", alias = "Password", default)]
    password: String,
    #[serde(rename = "DeviceId")]
    device_id: Option<String>,
}

#[derive(Deserialize)]
pub struct QuickConnectAuthenticateRequest {
    #[serde(rename = "Secret")]
    secret: String,
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
    #[serde(rename = "app", alias = "App")]
    app: Option<String>,
}

struct UserRow {
    id: String,
    username: String,
    password_hash: Option<String>,
    is_admin: bool,
    is_disabled: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct QuickConnectRecord {
    secret: String,
    code: String,
    device_id: String,
    device_name: String,
    app_name: String,
    app_version: String,
    date_added_unix: i64,
    user_id: Option<String>,
}

pub async fn authenticate_by_name(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Response {
    match authenticate_by_name_inner(&state, &headers, request).await {
        Ok(response) => Json(response).into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

pub async fn list_users(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_users_inner(&state.db).await {
        Ok(users) => Json(filter_users(users, &query)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn public_users(State(state): State<Arc<AppState>>) -> Response {
    match public_users_inner(&state.db).await {
        Ok(users) => Json(users).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn user_by_id(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Path(user_id): Path<String>,
) -> Response {
    match user_by_id_inner(&state.db, &user_id).await {
        Ok(Some(user)) => match user_by_id_inner(&state.db, &request_user_id).await {
            Ok(Some(request_user)) if request_user.is_admin || request_user.id == user.id => {
                Json(user_json_with_config(&state.db, &user).await).into_response()
            }
            Ok(Some(_)) | Ok(None) => (
                StatusCode::FORBIDDEN,
                Json(json!({ "Error": "User access is denied" })),
            )
                .into_response(),
            Err(error) => internal_error(error),
        },
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
            Ok(Some(user)) => Json(user_json_with_config(&state.db, &user).await).into_response(),
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
        Err(error) if user_write_validation_error(&error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_user_password(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Path(user_id): Path<String>,
    Json(request): Json<UpdatePasswordRequest>,
) -> Response {
    update_user_password_response(&state.db, &request_user_id, &user_id, request).await
}

pub async fn update_user_password_legacy(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
    Json(request): Json<UpdatePasswordRequest>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    update_user_password_response(&state.db, &request_user_id, &user_id, request).await
}

async fn update_user_password_response(
    db: &DatabaseConnection,
    request_user_id: &str,
    user_id: &str,
    request: UpdatePasswordRequest,
) -> Response {
    match update_user_password_inner(db, request_user_id, user_id, request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::FORBIDDEN, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) if error.to_string().contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Err(AuthError::Internal(error)) if user_write_validation_error(&error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

pub async fn update_user_configuration(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(configuration): Json<JsonValue>,
) -> Response {
    update_user_configuration_response(&state.db, &user_id, configuration).await
}

pub async fn update_user_configuration_legacy(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
    Json(configuration): Json<JsonValue>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    update_user_configuration_response(&state.db, &user_id, configuration).await
}

async fn update_user_configuration_response(
    db: &DatabaseConnection,
    user_id: &str,
    configuration: JsonValue,
) -> Response {
    if !configuration.is_object() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match update_user_configuration_inner(db, user_id, &configuration).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub(crate) fn query_user_id_or_request(
    query: &HashMap<String, String>,
    request_user_id: &str,
) -> String {
    query_user_target(query)
        .unwrap_or(request_user_id)
        .to_string()
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
    update_user_response(&state.db, &user_id, &request).await
}

pub async fn update_user_legacy(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    Json(request): Json<JsonValue>,
) -> Response {
    let Some(user_id) = query_value(&query, "UserId").filter(|user_id| !user_id.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    update_user_response(&state.db, &user_id, &request).await
}

async fn update_user_response(
    db: &DatabaseConnection,
    user_id: &str,
    request: &JsonValue,
) -> Response {
    let Some(name) = request.get("Name").and_then(JsonValue::as_str) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let name = match validate_user_name(name) {
        Ok(name) => name,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response();
        }
    };

    let result = match Users::find_by_id(user_id).one(db).await {
        Ok(Some(model)) => {
            let mut active: users::ActiveModel = model.into();
            active.username = Set(name.clone());
            active.display_name = Set(name);
            active.updated_at = Set(now_unix());
            active.update(db).await
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "User not found" })),
            )
                .into_response();
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_user_policy(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(policy): Json<JsonValue>,
) -> Response {
    if !policy.is_object() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match update_user_policy_inner(&state.db, &user_id, &policy).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "User not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn forgot_password() -> impl IntoResponse {
    Json(json!({
        "Action": "ContactAdmin",
        "PinFile": "",
        "PinExpirationDate": null
    }))
}

pub async fn auth_providers() -> impl IntoResponse {
    Json(name_id_pairs(&[(
        "Default",
        "Jellyfin.Server.Implementations.Users.DefaultAuthenticationProvider",
    )]))
}

pub async fn password_reset_providers() -> impl IntoResponse {
    Json(name_id_pairs(&[(
        "Default",
        "Jellyfin.Server.Implementations.Users.DefaultPasswordResetProvider",
    )]))
}

pub async fn api_keys(State(state): State<Arc<AppState>>) -> Response {
    match api_keys_inner(&state.db).await {
        Ok(keys) => Json(auth_query_result(keys)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn create_api_key(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CreateApiKeyQuery>,
) -> Response {
    match create_api_key_inner(&state, query).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if api_key_validation_error(&error) => (
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
        Err(error) if api_key_validation_error(&error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn quick_connect_initiate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let capabilities =
        crate::jellyfin::sessions::session_info(&state, &headers, &HashMap::new()).await;
    let now = now_unix();
    let record = QuickConnectRecord {
        secret: Uuid::new_v4().simple().to_string(),
        code: quick_connect_code(),
        device_id: string_or_default(capabilities.device_id, ""),
        device_name: string_or_default(capabilities.device_name, "Unknown Device"),
        app_name: string_or_default(capabilities.client, "jellyfin-rs"),
        app_version: string_or_default(capabilities.application_version, "0.1.0"),
        date_added_unix: now,
        user_id: None,
    };

    match save_quick_connect_record(&state.db, &record).await {
        Ok(()) => Json(quick_connect_result(&record)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn quick_connect_connect(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(secret) = query_value(&query, "secret") else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    match load_quick_connect_record(&state.db, &secret).await {
        Ok(Some(record)) if quick_connect_expired(&record) => {
            if let Err(error) = delete_quick_connect_record(&state.db, &record).await {
                return internal_error(error);
            }
            StatusCode::NOT_FOUND.into_response()
        }
        Ok(Some(record)) => Json(quick_connect_result(&record)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn quick_connect_authorize(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(code) = query_value(&query, "code") else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let requested_user = query_value(&query, "userId").or_else(|| query_value(&query, "UserId"));
    let query_user = HashMap::new();
    let request_user_id = match authenticated_user_id(&state.db, &headers, &query_user).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "Error": "Authentication token is required" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    };
    let target_user_id = requested_user.unwrap_or_else(|| request_user_id.clone());
    if target_user_id != request_user_id {
        match user_by_id_inner(&state.db, &request_user_id).await {
            Ok(Some(user)) if user.is_admin => {}
            Ok(Some(_)) | Ok(None) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "Error": "User access is denied" })),
                )
                    .into_response();
            }
            Err(error) => return internal_error(error),
        }
    }
    match user_by_id_inner(&state.db, &target_user_id).await {
        Ok(Some(user)) if !user.is_disabled => {}
        Ok(Some(_)) => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "Error": "User is disabled" })),
            )
                .into_response();
        }
        Ok(None) => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "Error": "User not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }

    match load_quick_connect_record_by_code(&state.db, &code).await {
        Ok(Some(record)) if quick_connect_expired(&record) => {
            if let Err(error) = delete_quick_connect_record(&state.db, &record).await {
                return internal_error(error);
            }
            Json(false).into_response()
        }
        Ok(Some(mut record)) => {
            record.user_id = Some(target_user_id);
            match save_quick_connect_record(&state.db, &record).await {
                Ok(()) => Json(true).into_response(),
                Err(error) => internal_error(error),
            }
        }
        Ok(None) => Json(false).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn authenticate_with_quick_connect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<QuickConnectAuthenticateRequest>,
) -> Response {
    match authenticate_with_quick_connect_inner(&state, &headers, request).await {
        Ok(response) => Json(response).into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

async fn authenticate_by_name_inner(
    state: &AppState,
    headers: &HeaderMap,
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

    authenticate_user(state, headers, user, &request.password, request.device_id).await
}

async fn authenticate_user(
    state: &AppState,
    headers: &HeaderMap,
    user: UserRow,
    password: &str,
    device_id: Option<String>,
) -> Result<JsonValue, AuthError> {
    let Some(password_hash) = &user.password_hash else {
        return Err(AuthError::Unauthorized(
            "Password is not configured".to_string(),
        ));
    };
    if !verify_password(password, password_hash) {
        return Err(AuthError::Unauthorized(
            "Invalid username or password".to_string(),
        ));
    }

    issue_authentication_result(state, headers, user, device_id).await
}

async fn issue_authentication_result(
    state: &AppState,
    headers: &HeaderMap,
    user: UserRow,
    device_id: Option<String>,
) -> Result<JsonValue, AuthError> {
    if user.is_disabled {
        return Err(AuthError::Unauthorized("User is disabled".to_string()));
    }

    let now = now_unix();
    let token = Uuid::new_v4().simple().to_string();
    let device_id = device_id.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    });
    let db = &state.db;

    let token_active = access_tokens::ActiveModel {
        id: Set(Uuid::new_v4().to_string()),
        user_id: Set(user.id.clone()),
        token_hash: Set(stable_text_id(&token)),
        name: Set(Some("login-token".to_string())),
        device_id: Set(device_id.clone()),
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

    let user_dto = user_json_with_config(db, &user).await;
    let capabilities =
        crate::jellyfin::sessions::session_info(state, headers, &HashMap::new()).await;
    let session_info = authentication_session_info(
        &user,
        &token,
        capabilities.clone(),
        device_id.as_deref(),
        now,
    );
    register_login_session(
        state,
        &user,
        &token,
        capabilities,
        device_id.as_deref(),
        now,
    )
    .await;
    Ok(json!({
        "User": user_dto,
        "SessionInfo": session_info,
        "AccessToken": token,
        "ServerId": "jellyfin-rs"
    }))
}

async fn register_login_session(
    state: &AppState,
    user: &UserRow,
    session_id: &str,
    capabilities: SessionCapabilities,
    request_device_id: Option<&str>,
    now: i64,
) {
    let playable_media_types =
        vec_or_default(capabilities.playable_media_types, &["Audio", "Video"]);
    let supported_commands = vec_or_default(
        capabilities.supported_commands,
        &["Play", "Pause", "Stop", "Seek"],
    );
    let device_id = request_device_id
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| capabilities.device_id);
    let client = string_or_default(capabilities.client, "jellyfin-rs");
    let device_name = string_or_default(capabilities.device_name, "Unknown Device");
    let application_version = string_or_default(capabilities.application_version, "0.1.0");
    let supports_media_control = capabilities.supports_media_control;
    let supports_persistent_identifier = capabilities.supports_persistent_identifier;
    let last_activity_date = unix_to_jellyfin_date(now);
    let session = PlaybackSession {
        id: session_id.to_string(),
        user_id: user.id.clone(),
        play_session_id: session_id.to_string(),
        item_id: String::new(),
        item_name: None,
        now_playing_queue: Vec::new(),
        additional_users: Vec::new(),
        client: client.clone(),
        device_name: device_name.clone(),
        device_id: device_id.clone(),
        application_version: application_version.clone(),
        is_active: true,
        last_activity_date: last_activity_date.clone(),
        last_playback_check_in: last_activity_date,
        last_activity_unix: now,
        play_state: PlaybackState {
            position_ticks: 0,
            is_paused: false,
            can_seek: true,
        },
        playable_media_types: playable_media_types.clone(),
        supports_media_control_commands: supported_commands.clone(),
        supported_commands: supported_commands.clone(),
        supports_media_control,
        supports_remote_control: supports_media_control,
        supports_persistent_identifier,
        capabilities: SessionCapabilities {
            user_id: user.id.clone(),
            client,
            device_name,
            device_id,
            application_version,
            playable_media_types: playable_media_types.clone(),
            supported_commands: supported_commands.clone(),
            supports_media_control,
            supports_persistent_identifier,
        },
    };

    state
        .playback_sessions
        .write()
        .await
        .insert(session_id.to_string(), session);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
}

fn authentication_session_info(
    user: &UserRow,
    session_id: &str,
    capabilities: SessionCapabilities,
    request_device_id: Option<&str>,
    now: i64,
) -> JsonValue {
    let playable_media_types =
        vec_or_default(capabilities.playable_media_types, &["Audio", "Video"]);
    let supported_commands = vec_or_default(
        capabilities.supported_commands,
        &["Play", "Pause", "Stop", "Seek"],
    );
    let device_id = request_device_id
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| capabilities.device_id);
    let last_activity_date = unix_to_jellyfin_date(now);

    json!({
        "Id": session_id,
        "UserId": user.id,
        "UserName": user.username,
        "Client": string_or_default(capabilities.client, "jellyfin-rs"),
        "DeviceName": string_or_default(capabilities.device_name, "Unknown Device"),
        "DeviceId": device_id,
        "ApplicationVersion": string_or_default(capabilities.application_version, "0.1.0"),
        "IsActive": true,
        "LastActivityDate": last_activity_date,
        "LastPlaybackCheckIn": last_activity_date,
        "PlayState": {
            "PositionTicks": 0,
            "IsPaused": false,
            "CanSeek": true
        },
        "PlayableMediaTypes": playable_media_types.clone(),
        "SupportedCommands": supported_commands.clone(),
        "SupportsMediaControl": capabilities.supports_media_control,
        "SupportsRemoteControl": capabilities.supports_media_control,
        "SupportsPersistentIdentifier": capabilities.supports_persistent_identifier,
        "AdditionalUsers": [],
        "NowPlayingQueue": [],
        "Capabilities": {
            "PlayableMediaTypes": playable_media_types,
            "SupportedCommands": supported_commands,
            "SupportsMediaControl": capabilities.supports_media_control,
            "SupportsPersistentIdentifier": capabilities.supports_persistent_identifier
        },
        "ServerId": "jellyfin-rs"
    })
}

fn string_or_default(value: String, default: &str) -> String {
    (!value.trim().is_empty())
        .then_some(value)
        .unwrap_or_else(|| default.to_string())
}

fn vec_or_default(values: Vec<String>, default: &[&str]) -> Vec<String> {
    if values.is_empty() {
        default.iter().map(ToString::to_string).collect()
    } else {
        values
    }
}

async fn authenticate_with_quick_connect_inner(
    state: &AppState,
    headers: &HeaderMap,
    request: QuickConnectAuthenticateRequest,
) -> Result<JsonValue, AuthError> {
    let secret = request.secret.trim();
    if secret.is_empty() {
        return Err(AuthError::Unauthorized(
            "Quick connect secret is required".to_string(),
        ));
    }

    let Some(record) = load_quick_connect_record(&state.db, secret)
        .await
        .map_err(AuthError::Internal)?
    else {
        return Err(AuthError::Unauthorized(
            "Quick connect request not found".to_string(),
        ));
    };
    if quick_connect_expired(&record) {
        delete_quick_connect_record(&state.db, &record)
            .await
            .map_err(AuthError::Internal)?;
        return Err(AuthError::Unauthorized(
            "Quick connect request expired".to_string(),
        ));
    }
    let user_id = record.user_id.clone().ok_or_else(|| {
        AuthError::Unauthorized("Quick connect request is not authorized".to_string())
    })?;
    let user = user_by_id_inner(&state.db, &user_id)
        .await
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Unauthorized("User not found".to_string()))?;
    let result =
        issue_authentication_result(state, headers, user, Some(record.device_id.clone())).await?;
    delete_quick_connect_record(&state.db, &record)
        .await
        .map_err(AuthError::Internal)?;
    Ok(result)
}

async fn save_quick_connect_record(
    db: &DatabaseConnection,
    record: &QuickConnectRecord,
) -> anyhow::Result<()> {
    let value = serde_json::to_string(record)?;
    set_app_setting(db, &quick_connect_secret_key(&record.secret), &value).await?;
    set_app_setting(db, &quick_connect_code_key(&record.code), &record.secret).await
}

async fn load_quick_connect_record(
    db: &DatabaseConnection,
    secret: &str,
) -> anyhow::Result<Option<QuickConnectRecord>> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Ok(None);
    }
    let Some(value) = AppSettings::find_by_id(quick_connect_secret_key(secret))
        .one(db)
        .await?
        .map(|model| model.value)
    else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&value).ok())
}

async fn load_quick_connect_record_by_code(
    db: &DatabaseConnection,
    code: &str,
) -> anyhow::Result<Option<QuickConnectRecord>> {
    let code = code.trim();
    if code.is_empty() {
        return Ok(None);
    }
    let Some(secret) = AppSettings::find_by_id(quick_connect_code_key(code))
        .one(db)
        .await?
        .map(|model| model.value)
    else {
        return Ok(None);
    };
    load_quick_connect_record(db, &secret).await
}

async fn delete_quick_connect_record(
    db: &DatabaseConnection,
    record: &QuickConnectRecord,
) -> anyhow::Result<()> {
    AppSettings::delete_by_id(quick_connect_secret_key(&record.secret))
        .exec(db)
        .await?;
    AppSettings::delete_by_id(quick_connect_code_key(&record.code))
        .exec(db)
        .await?;
    Ok(())
}

fn quick_connect_result(record: &QuickConnectRecord) -> JsonValue {
    json!({
        "Authenticated": record.user_id.is_some(),
        "Secret": record.secret,
        "Code": record.code,
        "DeviceId": record.device_id,
        "DeviceName": record.device_name,
        "AppName": record.app_name,
        "AppVersion": record.app_version,
        "DateAdded": unix_to_jellyfin_date(record.date_added_unix),
    })
}

fn quick_connect_expired(record: &QuickConnectRecord) -> bool {
    record.date_added_unix + QUICK_CONNECT_TTL_SECONDS < now_unix()
}

fn quick_connect_code() -> String {
    format!("{:06}", fastrand::u32(0..1_000_000))
}

fn quick_connect_secret_key(secret: &str) -> String {
    format!("{QUICK_CONNECT_PREFIX}{secret}")
}

fn quick_connect_code_key(code: &str) -> String {
    format!("{QUICK_CONNECT_CODE_PREFIX}{code}")
}

fn auth_query_result(items: Vec<JsonValue>) -> JsonValue {
    json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "StartIndex": 0
    })
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
    let app = validate_api_key_app(query.app.as_deref())?;
    let now = now_unix();
    let token = Uuid::new_v4().simple().to_string();
    let key_id = Uuid::new_v4().to_string();

    let key_active = api_keys::ActiveModel {
        id: Set(key_id),
        access_token: Set(token.clone()),
        name: Set(app.clone()),
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
        name: Set(Some(app)),
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
    let key = validate_api_key_token(key)?;
    let token_hash = stable_text_id(&key);

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

fn api_key_validation_error(error: &anyhow::Error) -> bool {
    matches!(
        error.to_string().as_str(),
        "app is required" | "Invalid app name" | "Invalid api key"
    )
}

fn validate_api_key_app(value: Option<&str>) -> anyhow::Result<String> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty() {
        bail!("app is required");
    }
    if value.chars().count() > MAX_API_KEY_APP_LEN
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        bail!("Invalid app name");
    }
    Ok(value.to_string())
}

fn validate_api_key_token(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_API_KEY_TOKEN_LEN
        || !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
    {
        bail!("Invalid api key");
    }
    Ok(value.to_string())
}

async fn list_users_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let models = Users::find()
        .order_by_asc(users::Column::Username)
        .all(db)
        .await
        .context("failed to list users")?;

    let mut users = Vec::with_capacity(models.len());
    for model in models {
        users.push(
            user_json_with_config(
                db,
                &UserRow {
                    id: model.id,
                    username: model.username,
                    password_hash: model.password_hash,
                    is_admin: model.is_admin != 0,
                    is_disabled: model.is_disabled != 0,
                },
            )
            .await,
        );
    }
    Ok(users)
}

async fn public_users_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let models = Users::find()
        .filter(users::Column::IsDisabled.eq(0))
        .order_by_asc(users::Column::Username)
        .all(db)
        .await
        .context("failed to list public users")?;

    Ok(models
        .iter()
        .map(|m| {
            json!({
                "Name": m.username,
                "Id": m.id,
                "ServerId": "jellyfin-rs",
                "HasPassword": m.password_hash.is_some(),
            })
        })
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
    let username = validate_user_name(&request.name)?;

    let now = now_unix();
    let user_id =
        Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("user:{username}").as_bytes()).to_string();
    let password_hash: Option<String> = match request.password.as_deref() {
        Some(password) if !password.trim().is_empty() => {
            Some(hash_password(validate_user_password(password)?)?)
        }
        _ => None,
    };
    let has_password = password_hash.is_some();

    let active = users::ActiveModel {
        id: Set(user_id.clone()),
        username: Set(username.clone()),
        password_hash: Set(password_hash),
        display_name: Set(username.clone()),
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

    Ok(user_json(&user_id, &username, has_password, false, false))
}

async fn update_user_configuration_inner(
    db: &DatabaseConnection,
    user_id: &str,
    configuration: &JsonValue,
) -> anyhow::Result<()> {
    if Users::find_by_id(user_id).one(db).await?.is_none() {
        anyhow::bail!("user not found");
    }
    set_app_setting(
        db,
        &user_configuration_key(user_id),
        &merge_user_configuration(default_user_configuration(), configuration).to_string(),
    )
    .await
}

async fn update_user_password_inner(
    db: &DatabaseConnection,
    request_user_id: &str,
    user_id: &str,
    request: UpdatePasswordRequest,
) -> Result<(), AuthError> {
    let user = user_by_id_inner(db, user_id)
        .await
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Internal(anyhow::anyhow!("user not found")))?;
    let request_user = user_by_id_inner(db, request_user_id)
        .await
        .map_err(AuthError::Internal)?
        .ok_or_else(|| AuthError::Internal(anyhow::anyhow!("request user not found")))?;
    let admin_changing_other_user = request_user.is_admin && request_user.id != user.id;

    if !password_change_requires_current_password(admin_changing_other_user) {
        // Admin password resets intentionally do not require the target user's current password.
    } else if let Some(current_pw) = request.current_pw.as_deref() {
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
    } else {
        return Err(AuthError::Unauthorized(
            "Current password is required".to_string(),
        ));
    }

    let now = now_unix();
    let password_hash: Option<String> = if password_reset_clears_password(&request) {
        None
    } else {
        let Some(new_pw) = valid_new_password(&request) else {
            return Err(AuthError::Unauthorized(
                "New password is required".to_string(),
            ));
        };
        let new_pw = validate_user_password(new_pw).map_err(AuthError::Internal)?;
        Some(hash_password(new_pw).map_err(AuthError::Internal)?)
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

    revoke_user_tokens(db, user_id, now)
        .await
        .map_err(AuthError::Internal)?;

    Ok(())
}

async fn update_user_policy_inner(
    db: &DatabaseConnection,
    user_id: &str,
    policy: &JsonValue,
) -> anyhow::Result<()> {
    let Some(model) = Users::find_by_id(user_id).one(db).await? else {
        anyhow::bail!("user not found");
    };
    let current_is_admin = model.is_admin != 0;
    let current_is_disabled = model.is_disabled != 0;
    let mut active: users::ActiveModel = model.into();
    let mut disabled = None;
    let mut merged = merge_user_policy(
        user_policy(db, user_id, current_is_admin, current_is_disabled).await,
        policy,
    );
    if let Some(is_admin) = policy_bool(policy, "IsAdministrator") {
        active.is_admin = Set(i64::from(is_admin));
        merged["IsAdministrator"] = json!(is_admin);
    }
    if let Some(is_disabled) = policy_bool(policy, "IsDisabled") {
        active.is_disabled = Set(i64::from(is_disabled));
        merged["IsDisabled"] = json!(is_disabled);
        disabled = Some(is_disabled);
    }
    active.updated_at = Set(now_unix());
    active.update(db).await?;
    set_app_setting(db, &user_policy_key(user_id), &merged.to_string()).await?;

    if disabled == Some(true) {
        revoke_user_tokens(db, user_id, now_unix()).await?;
    }
    Ok(())
}

fn policy_bool(policy: &JsonValue, key: &str) -> Option<bool> {
    policy.get(key).and_then(JsonValue::as_bool)
}

fn user_write_validation_error(error: &anyhow::Error) -> bool {
    matches!(
        error.to_string().as_str(),
        "Name is required" | "Invalid user name" | "Password is too long" | "Invalid password"
    )
}

fn validate_user_name(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("Name is required");
    }
    if value.chars().count() > MAX_USER_NAME_LEN
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        bail!("Invalid user name");
    }
    Ok(value.to_string())
}

fn validate_user_password(value: &str) -> anyhow::Result<&str> {
    let value = value.trim();
    if value.len() > MAX_USER_PASSWORD_LEN {
        bail!("Password is too long");
    }
    if value.contains('\0') {
        bail!("Invalid password");
    }
    Ok(value)
}

async fn revoke_user_tokens(
    db: &DatabaseConnection,
    user_id: &str,
    now: i64,
) -> anyhow::Result<()> {
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE access_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
        vec![now.into(), user_id.into()],
    ))
    .await
    .context("failed to revoke user tokens")?;
    Ok(())
}

fn password_change_requires_current_password(admin_changing_other_user: bool) -> bool {
    !admin_changing_other_user
}

fn password_reset_clears_password(request: &UpdatePasswordRequest) -> bool {
    request.reset_password
}

fn valid_new_password(request: &UpdatePasswordRequest) -> Option<&str> {
    request
        .new_pw
        .as_deref()
        .map(str::trim)
        .filter(|password| !password.is_empty())
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

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = api_path(request.uri().path());
    if is_public_request(request.method(), path) {
        return next.run(request).await;
    }

    let query = query_map(request.uri().query().unwrap_or_default());
    let Some(user_id) = (match authenticated_user_id(&state.db, request.headers(), &query).await {
        Ok(user_id) => user_id,
        Err(error) => return internal_error(error),
    }) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "Error": "Authentication token is required" })),
        )
            .into_response();
    };

    let needs_admin = admin_required(request.method(), path);
    let cross_user_access =
        request_user_target(path, &query).is_some_and(|target| target != user_id);
    let session_control_denied = if let Some(session_id) = session_control_target(path, &query) {
        session_control_allowed(&state, session_id, &user_id)
            .await
            .is_some_and(|allowed| !allowed)
    } else {
        false
    };
    if needs_admin || cross_user_access || session_control_denied {
        match user_by_id_inner(&state.db, &user_id).await {
            Ok(Some(user)) if cross_user_access && !user.is_admin => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "Error": "User access is denied" })),
                )
                    .into_response();
            }
            Ok(Some(user)) if session_control_denied && !user.is_admin => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "Error": "Session access is denied" })),
                )
                    .into_response();
            }
            Ok(Some(user)) if !needs_admin || user.is_admin => {}
            Ok(Some(_)) | Ok(None) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "Error": "Administrator access is required" })),
                )
                    .into_response();
            }
            Err(error) => return internal_error(error),
        }
    }

    request.extensions_mut().insert(user_id);
    next.run(request).await
}

fn is_public_request(method: &Method, path: &str) -> bool {
    if method == Method::OPTIONS {
        return true;
    }
    if method == Method::POST
        && path
            .strip_prefix("/Users/")
            .is_some_and(|rest| rest.ends_with("/Authenticate"))
    {
        return true;
    }
    if method == Method::POST
        && path.strip_prefix("/Dlna/").is_some_and(|rest| {
            rest.ends_with("/connectionmanager/control")
                || rest.ends_with("/contentdirectory/control")
        })
    {
        return true;
    }
    if matches!(
        path,
        "/Users/authenticatebyname"
            | "/Users/AuthenticateByName"
            | "/Users/AuthenticateWithQuickConnect"
            | "/users/authenticatebyname"
            | "/Users/ForgotPassword"
            | "/Users/ForgotPassword/Pin"
            | "/QuickConnect/Authorize"
            | "/QuickConnect/Initiate"
    ) {
        return method == Method::POST;
    }
    matches!(method, &Method::GET | &Method::HEAD)
        && (PUBLIC_PATHS.contains(&path) || dlna_discovery_path(path))
}

fn admin_required(method: &Method, path: &str) -> bool {
    if matches!(path, "/Users" | "/Users/Query" | "/Users/Prefixes") {
        return true;
    }
    if method == Method::GET {
        return ADMIN_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
            || usage_stats_admin_read(path)
            || metadata_admin_read(path)
            || item_delete_info_read(path)
            || item_file_info_read(path);
    }
    if user_password_path_target(path).is_some() {
        return false;
    }
    ADMIN_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
        || ADMIN_CONTAINS.iter().any(|part| path.contains(part))
        || item_image_write(path)
        || dlna_profile_admin_write(path)
        || live_tv_admin_write(path)
        || package_admin_write(path)
        || remote_metadata_search_write(path)
        || session_user_management_write(path)
        || top_level_resource_write(path, "/Users")
        || top_level_resource_write(path, "/Items")
}

fn usage_stats_admin_read(path: &str) -> bool {
    matches!(
        path,
        "/user_usage_stats/DurationHistogramReport"
            | "/user_usage_stats/HourlyReport"
            | "/user_usage_stats/MoviesReport"
            | "/user_usage_stats/PlayActivity"
            | "/user_usage_stats/TvShowsReport"
            | "/user_usage_stats/process_list"
            | "/user_usage_stats/resource_usage"
            | "/user_usage_stats/session_list"
            | "/user_usage_stats/type_filter_list"
            | "/user_usage_stats/user_activity"
            | "/user_usage_stats/user_list"
    ) || path.ends_with("/BreakdownReport")
}

fn item_image_write(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/Items/") else {
        return false;
    };
    rest.split('/').nth(1) == Some("Images")
}

fn metadata_admin_read(path: &str) -> bool {
    path.starts_with("/Items/")
        && (path.ends_with("/MetadataEditor") || path.ends_with("/ExternalIdInfos"))
}

fn item_delete_info_read(path: &str) -> bool {
    path.starts_with("/Items/") && path.ends_with("/DeleteInfo")
}

fn item_file_info_read(path: &str) -> bool {
    path == "/Items/File"
        || (path.starts_with("/Items/")
            && path.ends_with("/File")
            && path.trim_matches('/').split('/').count() == 3)
}

fn dlna_discovery_path(path: &str) -> bool {
    path.starts_with("/Dlna/")
        && (path.ends_with("/description")
            || path.ends_with("/description.xml")
            || path.contains("/icons/")
            || path.ends_with("/connectionmanager/connectionmanager")
            || path.ends_with("/connectionmanager/connectionmanager.xml")
            || path.ends_with("/contentdirectory/contentdirectory")
            || path.ends_with("/contentdirectory/contentdirectory.xml"))
}

fn dlna_profile_admin_write(path: &str) -> bool {
    path == "/Dlna/Profiles" || path.starts_with("/Dlna/Profiles/")
}

fn live_tv_admin_write(path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "/LiveTv/ChannelMappingOptions",
        "/LiveTv/ChannelMappings",
        "/LiveTv/ListingProviders",
        "/LiveTv/Manage",
        "/LiveTv/Recordings/",
        "/LiveTv/SeriesTimers",
        "/LiveTv/Timers",
        "/LiveTv/TunerHosts",
        "/LiveTv/Tuners",
    ];
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

fn package_admin_write(path: &str) -> bool {
    path.starts_with("/Packages/Installed/") || path.starts_with("/Packages/Installing/")
}

fn remote_metadata_search_write(path: &str) -> bool {
    path.eq_ignore_ascii_case("/Items/RemoteSearch/Person")
}

fn session_user_management_write(path: &str) -> bool {
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next()
        ),
        (
            Some("Sessions"),
            Some(_),
            Some("User" | "Users"),
            Some(_),
            None
        )
    )
}

fn session_control_target<'a>(
    path: &'a str,
    query: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let path = api_path(path);
    if path == "/Sessions/Playing/Ping" {
        return query
            .get("playSessionId")
            .or_else(|| query.get("PlaySessionId"))
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty());
    }

    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let (Some("Sessions"), Some(session_id), Some(command_root)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let command = parts.next();
    if parts.next().is_some() {
        return None;
    }
    match (command_root, command) {
        ("Viewing" | "Command" | "Playing" | "Message", None)
        | ("Command" | "Playing" | "System", Some(_)) => Some(session_id),
        _ => None,
    }
}

async fn session_control_allowed(
    state: &AppState,
    session_id: &str,
    user_id: &str,
) -> Option<bool> {
    let sessions = state.playback_sessions.read().await;
    sessions.get(session_id).map(|session| {
        session.supports_remote_control
            && (session.user_id == user_id
                || session
                    .additional_users
                    .iter()
                    .any(|user| user.user_id == user_id))
    })
}

fn api_path(path: &str) -> &str {
    path.strip_prefix("/emby")
        .filter(|p| !p.is_empty())
        .unwrap_or(path)
}

fn user_path_target(path: &str) -> Option<&str> {
    if let Some(target) = path
        .strip_prefix("/UserSettings/")
        .and_then(|rest| rest.split('/').next())
    {
        return (!target.is_empty()).then_some(target);
    }
    if let Some(target) = path
        .strip_prefix("/Notifications/")
        .and_then(|rest| rest.split('/').next())
    {
        if !matches!(target, "Admin" | "Services" | "Types") {
            return (!target.is_empty()).then_some(target);
        }
    }

    let target = path.strip_prefix("/Users/")?.split('/').next()?;
    if matches!(
        target,
        "AuthenticateByName"
            | "AuthenticateWithQuickConnect"
            | "Configuration"
            | "ForgotPassword"
            | "ItemAccess"
            | "Me"
            | "New"
            | "Password"
            | "Prefixes"
            | "Public"
            | "Query"
            | "authenticatebyname"
            | "public"
    ) {
        return None;
    }
    (!target.is_empty()).then_some(target)
}

fn user_password_path_target(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/Users/")?;
    let mut parts = rest.split('/');
    let user_id = parts.next()?;
    let action = parts.next()?;
    matches!(action, "Password" | "EasyPassword").then_some(user_id)
}

fn request_user_target<'a>(path: &'a str, query: &'a HashMap<String, String>) -> Option<&'a str> {
    let path = api_path(path);
    if path.starts_with("/DisplayPreferences/")
        || query_scoped_user_path(path)
        || matches!(
            path,
            "/Users/Configuration"
                | "/Users/ItemAccess"
                | "/Users/Password"
                | "/Items/Access"
                | "/user_usage_stats/UserPlaylist"
        )
    {
        return query_user_target(query);
    }
    if let Some(rest) = path.strip_prefix("/user_usage_stats/") {
        let mut parts = rest.split('/');
        let target = parts.next().unwrap_or_default();
        let _date = parts.next();
        if parts.next() == Some("GetItems") && parts.next().is_none() {
            return (!target.is_empty()).then_some(target);
        }
    }
    user_password_path_target(path).or_else(|| user_path_target(path))
}

fn query_user_target(query: &HashMap<String, String>) -> Option<&str> {
    query
        .get("userId")
        .or_else(|| query.get("UserId"))
        .or_else(|| query.get("user_id"))
        .or_else(|| query.get("UserID"))
        .map(String::as_str)
        .filter(|target| !target.trim().is_empty())
}

fn query_scoped_user_path(path: &str) -> bool {
    matches!(
        path,
        "/Artists/InstantMix"
            | "/AudioBooks/NextUp"
            | "/Items"
            | "/Items/Latest"
            | "/Items/Root"
            | "/Items/Counts"
            | "/Items/Suggestions"
            | "/Movies/Recommendations"
            | "/Persons"
            | "/MusicGenres/InstantMix"
            | "/Search/Hints"
            | "/Shows/NextUp"
            | "/Shows/Upcoming"
            | "/Trailers"
            | "/UserItems/Resume"
            | "/UserViews"
    ) || item_query_scoped_user_path(path)
        || media_query_scoped_user_path(path)
        || playlist_query_scoped_user_path(path)
        || person_query_scoped_user_path(path)
        || show_query_scoped_user_path(path)
        || video_query_scoped_user_path(path)
}

fn item_query_scoped_user_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/Items/") else {
        return false;
    };
    matches!(
        rest.split('/').collect::<Vec<_>>().as_slice(),
        [_, "Ancestors"]
            | [_, "InstantMix"]
            | [_, "LocalTrailers"]
            | [_, "SpecialFeatures"]
            | [_, "ThemeMedia"]
            | [_, "ThemeSongs"]
            | [_, "ThemeVideos"]
    )
}

fn playlist_query_scoped_user_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/Playlists/") else {
        return false;
    };
    matches!(
        rest.split('/').collect::<Vec<_>>().as_slice(),
        [_, "AddToPlaylistInfo"] | [_, "InstantMix"]
    )
}

fn media_query_scoped_user_path(path: &str) -> bool {
    ["/Albums/", "/Songs/"].iter().any(|prefix| {
        path.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest.split('/').collect::<Vec<_>>().as_slice(),
                [_, "InstantMix"]
            )
        })
    })
}

fn person_query_scoped_user_path(path: &str) -> bool {
    if path == "/Artists" || path == "/Artists/AlbumArtists" {
        return true;
    }
    if path.strip_prefix("/Persons/").is_some_and(|rest| {
        matches!(
            rest.split('/').collect::<Vec<_>>().as_slice(),
            [_] | [_, "Items"]
        )
    }) {
        return true;
    }
    let Some(rest) = path
        .strip_prefix("/Artists/")
        .or_else(|| path.strip_prefix("/MusicGenres/"))
    else {
        return false;
    };
    matches!(
        rest.split('/').collect::<Vec<_>>().as_slice(),
        [_] | [_, "InstantMix"]
    )
}

fn video_query_scoped_user_path(path: &str) -> bool {
    path.strip_prefix("/Videos/").is_some_and(|rest| {
        matches!(
            rest.split('/').collect::<Vec<_>>().as_slice(),
            [_, "AdditionalParts"]
        )
    })
}

fn show_query_scoped_user_path(path: &str) -> bool {
    path.strip_prefix("/Shows/").is_some_and(|rest| {
        matches!(
            rest.split('/').collect::<Vec<_>>().as_slice(),
            [_, "Seasons"] | [_, "Episodes"]
        )
    })
}

fn top_level_resource_write(path: &str, prefix: &str) -> bool {
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    rest.starts_with('/') && !rest[1..].contains('/')
}

fn query_map(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((
                percent_decode(key)?,
                percent_decode(value).unwrap_or_default(),
            ))
        })
        .collect()
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b'%' => return None,
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

pub async fn request_user_id_or_default(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> String {
    match authenticated_user_id(&state.db, headers, query).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => state.user_id.to_string(),
        Err(error) => {
            tracing::warn!("failed to resolve request user: {error:#}");
            state.user_id.to_string()
        }
    }
}

pub async fn request_user_id_and_admin_or_default(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> (String, bool) {
    match authenticated_user_id(&state.db, headers, query).await {
        Ok(Some(user_id)) => {
            let is_admin = user_by_id_inner(&state.db, &user_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|user| user.is_admin);
            (user_id, is_admin)
        }
        Ok(None) => (state.user_id.to_string(), false),
        Err(error) => {
            tracing::warn!("failed to resolve request user: {error:#}");
            (state.user_id.to_string(), false)
        }
    }
}

pub fn request_token(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    query
        .get("api_key")
        .or_else(|| query.get("ApiKey"))
        .or_else(|| query.get("apiKey"))
        .filter(|token| !token.trim().is_empty())
        .cloned()
        .or_else(|| header_token(headers, "X-Emby-Token"))
        .or_else(|| header_token(headers, "X-MediaBrowser-Token"))
        .or_else(|| header_token(headers, header::AUTHORIZATION.as_str()))
        .or_else(|| header_token(headers, "X-Emby-Authorization"))
}

pub fn header_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    auth_header_value_token(value)
        .or_else(|| bearer_token(value))
        .or_else(|| (!value.contains('=')).then(|| value.to_string()))
}

fn auth_header_value_token(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let token_part = part.trim().split_whitespace().find(|part| {
            part.strip_prefix("Token=")
                .or_else(|| part.strip_prefix("token="))
                .is_some()
        })?;
        token_part
            .split_once('=')
            .map(|(_, token)| token.trim_matches('"').to_string())
            .filter(|token| !token.is_empty())
    })
}

fn bearer_token(value: &str) -> Option<String> {
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
}

fn user_json(
    user_id: &str,
    name: &str,
    has_password: bool,
    is_admin: bool,
    is_disabled: bool,
) -> JsonValue {
    json!({
        "Name": name,
        "Id": user_id,
        "ServerId": "jellyfin-rs",
        "HasPassword": has_password,
        "HasConfiguredPassword": has_password,
        "HasConfiguredEasyPassword": false,
        "Configuration": default_user_configuration(),
        "Policy": default_user_policy(is_admin, is_disabled)
    })
}

async fn user_json_with_config(db: &DatabaseConnection, user: &UserRow) -> JsonValue {
    let mut value = user_json(
        &user.id,
        &user.username,
        user.password_hash.is_some(),
        user.is_admin,
        user.is_disabled,
    );
    value["Configuration"] = user_configuration(db, &user.id).await;
    value["Policy"] = user_policy(db, &user.id, user.is_admin, user.is_disabled).await;
    value
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

async fn user_configuration(db: &DatabaseConnection, user_id: &str) -> JsonValue {
    let saved = app_setting(db, &user_configuration_key(user_id), "").await;
    let saved = serde_json::from_str(&saved).unwrap_or(JsonValue::Null);
    merge_user_configuration(default_user_configuration(), &saved)
}

fn user_configuration_key(user_id: &str) -> String {
    format!("user_configuration:{user_id}")
}

fn merge_user_configuration(mut base: JsonValue, saved: &JsonValue) -> JsonValue {
    if let (Some(base), Some(saved)) = (base.as_object_mut(), saved.as_object()) {
        for (key, value) in saved {
            if base.contains_key(key) {
                base.insert(key.clone(), normalize_user_setting_value(value, 0));
            }
        }
    }
    base
}

async fn user_policy(
    db: &DatabaseConnection,
    user_id: &str,
    is_admin: bool,
    is_disabled: bool,
) -> JsonValue {
    let saved = app_setting(db, &user_policy_key(user_id), "").await;
    let saved = serde_json::from_str(&saved).unwrap_or(JsonValue::Null);
    let mut policy = merge_user_policy(default_user_policy(is_admin, is_disabled), &saved);
    policy["IsAdministrator"] = json!(is_admin);
    policy["IsDisabled"] = json!(is_disabled);
    policy
}

fn user_policy_key(user_id: &str) -> String {
    format!("user_policy:{user_id}")
}

fn default_user_policy(is_admin: bool, is_disabled: bool) -> JsonValue {
    let mut policy = serde_json::Map::new();
    for key in [
        "IsHidden",
        "EnableCollectionManagement",
        "EnableSubtitleManagement",
        "EnableLyricManagement",
        "EnableRemoteControlOfOtherUsers",
        "EnableSharedDeviceControl",
        "EnableLiveTvManagement",
        "EnableLiveTvAccess",
        "EnableAudioPlaybackTranscoding",
        "EnableVideoPlaybackTranscoding",
        "ForceRemoteSourceTranscoding",
        "EnableContentDeletion",
        "EnableSyncTranscoding",
        "EnableMediaConversion",
        "EnablePublicSharing",
    ] {
        policy.insert(key.to_string(), json!(false));
    }
    for key in [
        "BlockedTags",
        "AllowedTags",
        "AccessSchedules",
        "BlockUnratedItems",
        "EnableContentDeletionFromFolders",
        "EnabledDevices",
        "EnabledChannels",
        "EnabledFolders",
        "BlockedMediaFolders",
        "BlockedChannels",
    ] {
        policy.insert(key.to_string(), json!([]));
    }
    for (key, value) in [
        ("IsAdministrator", json!(is_admin)),
        ("IsDisabled", json!(is_disabled)),
        ("MaxParentalRating", JsonValue::Null),
        ("MaxParentalSubRating", JsonValue::Null),
        ("EnableUserPreferenceAccess", json!(true)),
        ("EnableRemoteAccess", json!(true)),
        ("EnableMediaPlayback", json!(true)),
        ("EnablePlaybackRemuxing", json!(true)),
        ("EnableContentDownloading", json!(true)),
        ("EnableAllDevices", json!(true)),
        ("EnableAllChannels", json!(true)),
        ("EnableAllFolders", json!(true)),
        ("InvalidLoginAttemptCount", json!(0)),
        ("LoginAttemptsBeforeLockout", json!(-1)),
        ("MaxActiveSessions", json!(0)),
        ("RemoteClientBitrateLimit", json!(0)),
        ("AuthenticationProviderId", json!("Default")),
        ("PasswordResetProviderId", json!("Default")),
        ("SyncPlayAccess", json!("None")),
    ] {
        policy.insert(key.to_string(), value);
    }
    JsonValue::Object(policy)
}

fn merge_user_policy(mut base: JsonValue, saved: &JsonValue) -> JsonValue {
    if let (Some(base), Some(saved)) = (base.as_object_mut(), saved.as_object()) {
        for (key, value) in saved {
            if base.contains_key(key) {
                base.insert(key.clone(), normalize_user_setting_value(value, 0));
            }
        }
    }
    base
}

fn normalize_user_setting_value(value: &JsonValue, depth: usize) -> JsonValue {
    match value {
        JsonValue::String(value) => JsonValue::String(normalize_user_setting_string(value)),
        JsonValue::Array(values) => JsonValue::Array(
            values
                .iter()
                .take(MAX_USER_SETTING_ARRAY_ITEMS)
                .map(|value| normalize_user_setting_value(value, depth + 1))
                .collect(),
        ),
        JsonValue::Object(values) => {
            if depth >= MAX_USER_SETTING_DEPTH {
                return JsonValue::Object(serde_json::Map::new());
            }
            let mut normalized = serde_json::Map::new();
            for (key, value) in values.iter().take(MAX_USER_SETTING_OBJECT_FIELDS) {
                let key = normalize_user_setting_string(key);
                if key.is_empty() {
                    continue;
                }
                normalized.insert(key, normalize_user_setting_value(value, depth + 1));
            }
            JsonValue::Object(normalized)
        }
        _ => value.clone(),
    }
}

fn normalize_user_setting_string(value: &str) -> String {
    value
        .chars()
        .filter(|value| !value.is_control())
        .take(MAX_USER_SETTING_STRING_LEN)
        .collect()
}

async fn app_setting(db: &DatabaseConnection, key: &str, default: &str) -> String {
    AppSettings::find_by_id(key)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|model| model.value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

async fn set_app_setting(db: &DatabaseConnection, key: &str, value: &str) -> anyhow::Result<()> {
    let now = now_unix();
    let existing = AppSettings::find_by_id(key).one(db).await?;
    if let Some(model) = existing {
        let mut active: app_settings::ActiveModel = model.into();
        active.value = Set(value.to_string());
        active.updated_at = Set(now);
        active.update(db).await?;
    } else {
        let active = app_settings::ActiveModel {
            key: Set(key.to_string()),
            value: Set(value.to_string()),
            updated_at: Set(now),
        };
        AppSettings::insert(active).exec(db).await?;
    }
    Ok(())
}

fn name_id_pairs(values: &[(&str, &str)]) -> Vec<JsonValue> {
    values
        .iter()
        .map(|(name, id)| json!({ "Name": name, "Id": id }))
        .collect()
}

/// GET /Users/Query — paginated user list with filtering
pub async fn users_query(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let start_index = query_value(&query, "StartIndex")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query_value(&query, "Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100);

    match list_users_inner(&state.db).await {
        Ok(users) => {
            let users = filter_users(users, &query);
            let total = users.len();
            let items: Vec<_> = users.into_iter().skip(start_index).take(limit).collect();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

fn filter_users(mut users: Vec<JsonValue>, query: &HashMap<String, String>) -> Vec<JsonValue> {
    if let Some(term) = query_value(query, "SearchTerm").filter(|term| !term.is_empty()) {
        let term = term.to_lowercase();
        users.retain(|user| {
            user.get("Name")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_lowercase()
                .contains(&term)
        });
    }
    if let Some(disabled) = query_bool(query, "IsDisabled") {
        users.retain(|user| user_policy_bool(user, "IsDisabled") == disabled);
    }
    if let Some(admin) = query_bool(query, "IsAdministrator") {
        users.retain(|user| user_policy_bool(user, "IsAdministrator") == admin);
    }
    users
}

fn user_policy_bool(user: &JsonValue, key: &str) -> bool {
    user.get(key)
        .and_then(JsonValue::as_bool)
        .or_else(|| user.get("Policy")?.get(key)?.as_bool())
        .unwrap_or(false)
}

fn query_bool(query: &HashMap<String, String>, key: &str) -> Option<bool> {
    query_value(query, key).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    })
}

fn query_value(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.trim().to_string())
}

/// GET /Users/{id}/Authenticate — legacy auth endpoint (stub)
pub async fn user_authenticate_legacy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(request): Json<AuthenticateUserRequest>,
) -> Response {
    let user = match user_by_id_inner(&state.db, &user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    if user.is_disabled {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "Error": "User is disabled" })),
        )
            .into_response();
    }

    match authenticate_user(&state, &headers, user, &request.password, request.device_id).await {
        Ok(response) => Json(response).into_response(),
        Err(AuthError::Unauthorized(message)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({ "Error": message }))).into_response()
        }
        Err(AuthError::Internal(error)) => internal_error(error),
    }
}

/// POST /Users/{id}/Connect/Link — link user to Emby Connect (stub)

/// DELETE /Users/{id}/Connect/Link — unlink from Emby Connect (stub)

pub async fn user_connect_link_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Emby Connect linking is not available" })),
    )
        .into_response()
}

#[derive(Debug)]
enum AuthError {
    Unauthorized(String),
    Internal(anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database};
    use tokio::sync::{RwLock, broadcast};

    #[test]
    fn auth_rules_leave_discovery_public() {
        assert!(is_public_request(&Method::GET, "/System/Info/Public"));
        assert!(is_public_request(&Method::GET, "/Branding/Css"));
        assert!(is_public_request(&Method::GET, "/Branding/Splashscreen"));
        assert!(is_public_request(&Method::GET, "/web/ConfigurationPages"));
        assert!(!is_public_request(&Method::GET, "/web/private"));
        assert!(!is_public_request(&Method::POST, "/Branding/Splashscreen"));
        assert!(is_public_request(
            &Method::POST,
            "/Users/AuthenticateByName"
        ));
        assert!(is_public_request(&Method::POST, "/Users/u1/Authenticate"));
        assert!(!is_public_request(&Method::GET, "/Users/u1/Authenticate"));
        assert!(is_public_request(
            &Method::POST,
            "/Users/AuthenticateWithQuickConnect"
        ));
        assert!(is_public_request(
            &Method::POST,
            "/Dlna/jellyfin-rs/connectionmanager/control"
        ));
        assert!(is_public_request(
            &Method::POST,
            "/Dlna/jellyfin-rs/contentdirectory/control"
        ));
        assert!(is_public_request(
            &Method::GET,
            "/Dlna/jellyfin-rs/description.xml"
        ));
        assert!(is_public_request(
            &Method::GET,
            "/Dlna/jellyfin-rs/icons/favicon.ico"
        ));
        assert!(!is_public_request(&Method::GET, "/Dlna/ProfileInfos"));
        assert!(!is_public_request(&Method::GET, "/Dlna/Profiles/Default"));
        assert_eq!(api_path("/emby/System/Info/Public"), "/System/Info/Public");
    }

    #[test]
    fn auth_rules_protect_user_and_admin_writes() {
        assert!(!admin_required(&Method::GET, "/Users/abc"));
        assert!(admin_required(&Method::POST, "/Users/abc"));
        assert!(admin_required(&Method::DELETE, "/Items/abc"));
        assert!(admin_required(&Method::POST, "/System/Configuration"));
        assert!(admin_required(&Method::GET, "/Users"));
        assert!(admin_required(&Method::GET, "/Backup"));
        assert!(admin_required(&Method::GET, "/Auth/Providers"));
        assert!(admin_required(&Method::GET, "/Auth/PasswordResetProviders"));
        assert!(admin_required(&Method::POST, "/Backup/Create"));
        assert!(admin_required(&Method::POST, "/Branding/Splashscreen"));
        assert!(admin_required(&Method::DELETE, "/Branding/Splashscreen"));
        assert!(admin_required(&Method::POST, "/Items/Delete"));
        assert!(admin_required(&Method::POST, "/Items/i1/ContentType"));
        assert!(admin_required(&Method::POST, "/Items/i1/Images/Primary"));
        assert!(admin_required(&Method::DELETE, "/Items/i1/Images/Primary"));
        assert!(admin_required(&Method::POST, "/Items/i1/Tags/Delete"));
        assert!(admin_required(&Method::POST, "/Items/i1/MakePrivate"));
        assert!(admin_required(&Method::POST, "/Items/i1/MakePublic"));
        assert!(admin_required(&Method::POST, "/Videos/MergeVersions"));
        assert!(admin_required(&Method::POST, "/items/metadata/reset"));
        assert!(admin_required(&Method::POST, "/Audio/i1/Lyrics"));
        assert!(admin_required(&Method::DELETE, "/Audio/i1/Lyrics"));
        assert!(admin_required(&Method::POST, "/Videos/i1/Subtitles"));
        assert!(admin_required(&Method::DELETE, "/Videos/i1/Subtitles/2"));
        assert!(admin_required(&Method::GET, "/Items/i1/MetadataEditor"));
        assert!(admin_required(&Method::GET, "/Items/i1/ExternalIdInfos"));
        assert!(admin_required(&Method::GET, "/Items/i1/DeleteInfo"));
        assert!(admin_required(&Method::GET, "/Items/i1/File"));
        assert!(admin_required(&Method::GET, "/Items/File"));
        assert!(admin_required(&Method::POST, "/Items/RemoteSearch/Person"));
        assert!(!admin_required(&Method::POST, "/Items/RemoteSearch/Movie"));
        assert!(admin_required(&Method::GET, "/Dlna/ProfileInfos"));
        assert!(admin_required(&Method::GET, "/Dlna/Profiles/Default"));
        assert!(admin_required(&Method::POST, "/Dlna/Profiles"));
        assert!(admin_required(
            &Method::DELETE,
            "/Dlna/Profiles/living-room"
        ));
        assert!(admin_required(&Method::GET, "/Packages/Updates"));
        assert!(admin_required(&Method::POST, "/Sessions/s1/Users/u1"));
        assert!(!admin_required(&Method::POST, "/Sessions/s1/Command"));
        assert!(!admin_required(&Method::POST, "/Users/u1/Password"));
        assert!(!admin_required(&Method::POST, "/Users/u1/Configuration"));
        assert_eq!(
            request_user_target("/Users/u1/Password", &HashMap::new()),
            Some("u1")
        );
        assert!(admin_required(&Method::GET, "/Reports/Items"));
        assert!(admin_required(&Method::GET, "/Reports/Items/Download"));
        assert!(admin_required(
            &Method::GET,
            "/user_usage_stats/MoviesReport"
        ));
        assert!(admin_required(
            &Method::GET,
            "/user_usage_stats/session_list"
        ));
        assert!(admin_required(
            &Method::GET,
            "/user_usage_stats/User/BreakdownReport"
        ));
        assert!(!admin_required(
            &Method::GET,
            "/user_usage_stats/UserPlaylist"
        ));
        assert!(!admin_required(
            &Method::GET,
            "/user_usage_stats/u1/2026-07-03/GetItems"
        ));
        assert!(admin_required(
            &Method::POST,
            "/user_usage_stats/submit_custom_query"
        ));
        assert!(admin_required(
            &Method::POST,
            "/user_usage_stats/import_backup"
        ));
        assert!(admin_required(
            &Method::GET,
            "/user_usage_stats/load_backup"
        ));
        assert!(admin_required(
            &Method::POST,
            "/user_usage_stats/save_backup"
        ));
        assert!(admin_required(
            &Method::GET,
            "/user_usage_stats/user_manage/list/0"
        ));
        assert!(admin_required(&Method::POST, "/Packages/Installed/plugin"));
        assert!(admin_required(
            &Method::DELETE,
            "/Packages/Installing/plugin"
        ));
        assert!(admin_required(&Method::POST, "/LiveTv/Timers"));
        assert!(admin_required(
            &Method::POST,
            "/LiveTv/Manage/Channels/1/Disabled"
        ));
        assert!(!admin_required(&Method::GET, "/LiveTv/Timers"));
        assert!(!admin_required(&Method::GET, "/Packages"));
        assert!(!admin_required(&Method::GET, "/UserImage"));
        assert!(!admin_required(&Method::POST, "/UserImage"));
        assert!(!admin_required(&Method::POST, "/Users/u1/Images/Primary"));
        assert!(!admin_required(&Method::POST, "/UserFavoriteItems/item"));
        assert!(!admin_required(
            &Method::POST,
            "/Users/u1/FavoriteItems/i1/Delete"
        ));
        assert!(!admin_required(
            &Method::POST,
            "/Users/u1/PlayedItems/i1/Delete"
        ));
    }

    #[tokio::test]
    async fn connect_link_reports_unavailable() {
        let response = user_connect_link_unavailable().await.into_response();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[test]
    fn user_path_target_detects_only_user_scoped_paths() {
        assert_eq!(user_path_target("/Users/u1"), Some("u1"));
        assert_eq!(user_path_target("/Users/u1/Items/i1"), Some("u1"));
        assert_eq!(user_path_target("/UserSettings/u1"), Some("u1"));
        assert_eq!(user_path_target("/Notifications/u1"), Some("u1"));
        assert_eq!(user_path_target("/Notifications/u1/Summary"), Some("u1"));
        assert_eq!(user_path_target("/Users/Public"), None);
        assert_eq!(user_path_target("/Users/Query"), None);
        assert_eq!(user_path_target("/UserFavoriteItems/i1"), None);
        assert_eq!(
            user_path_target(api_path("/emby/Users/u1/Items/i1")),
            Some("u1")
        );
        assert_eq!(
            request_user_target("/emby/Users/u2/Items/i1", &HashMap::new()),
            Some("u2")
        );
    }

    #[test]
    fn request_user_target_reads_display_preferences_query() {
        let query = query_map("userId=u1&client=Web");
        assert_eq!(
            request_user_target("/DisplayPreferences/home", &query),
            Some("u1")
        );
        assert_eq!(
            request_user_target("/Users/Configuration", &query),
            Some("u1")
        );
        assert_eq!(request_user_target("/Users/Password", &query), Some("u1"));
        assert_eq!(
            request_user_target("/emby/DisplayPreferences/home", &query),
            Some("u1")
        );
        assert_eq!(request_user_target("/Items/i1", &query), None);
    }

    #[test]
    fn session_control_target_reads_remote_control_paths() {
        let query = query_map("PlaySessionId=s2");
        assert_eq!(
            session_control_target("/Sessions/Playing/Ping", &query),
            Some("s2")
        );
        assert_eq!(
            session_control_target("/emby/Sessions/s1/Command", &HashMap::new()),
            Some("s1")
        );
        assert_eq!(
            session_control_target("/Sessions/s1/Playing/Pause", &HashMap::new()),
            Some("s1")
        );
        assert_eq!(
            session_control_target("/Sessions/s1/Users/u2", &HashMap::new()),
            None
        );
        assert_eq!(
            session_control_target("/Sessions/Capabilities", &HashMap::new()),
            None
        );
    }

    #[tokio::test]
    async fn session_control_allowed_requires_control_user() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let state = test_state(db);
        state
            .playback_sessions
            .write()
            .await
            .insert("s1".to_string(), test_playback_session());

        assert_eq!(
            session_control_allowed(&state, "s1", "u1").await,
            Some(true)
        );
        assert_eq!(
            session_control_allowed(&state, "s1", "u2").await,
            Some(true)
        );
        assert_eq!(
            session_control_allowed(&state, "s1", "u3").await,
            Some(false)
        );
        assert_eq!(session_control_allowed(&state, "missing", "u1").await, None);
    }

    #[tokio::test]
    async fn session_control_allowed_requires_remote_control_support() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let state = test_state(db);
        let mut session = test_playback_session();
        session.supports_remote_control = false;
        state
            .playback_sessions
            .write()
            .await
            .insert("s1".to_string(), session);

        assert_eq!(
            session_control_allowed(&state, "s1", "u1").await,
            Some(false)
        );
    }

    #[test]
    fn request_user_target_reads_query_scoped_plugin_paths() {
        let query = query_map("user_id=u2&filter=Movie");
        assert_eq!(
            request_user_target("/user_usage_stats/UserPlaylist", &query),
            Some("u2")
        );

        let query = query_map("UserId=u3");
        assert_eq!(request_user_target("/Users/ItemAccess", &query), Some("u3"));
        assert_eq!(request_user_target("/Items/Access", &query), Some("u3"));
    }

    #[test]
    fn request_user_target_reads_query_scoped_media_paths() {
        let query = query_map("UserId=u7");
        for path in [
            "/Items",
            "/Items/Latest",
            "/Items/Root",
            "/Items/Suggestions",
            "/Search/Hints",
            "/Shows/s1/Episodes",
            "/Shows/NextUp",
            "/Shows/s1/Seasons",
            "/Shows/Upcoming",
            "/AudioBooks/NextUp",
            "/Movies/Recommendations",
            "/Persons",
            "/Trailers",
            "/UserItems/Resume",
            "/UserViews",
            "/Items/i1/Ancestors",
            "/Items/i1/InstantMix",
            "/Items/i1/LocalTrailers",
            "/Items/i1/SpecialFeatures",
            "/Items/i1/ThemeMedia",
            "/Items/i1/ThemeSongs",
            "/Items/i1/ThemeVideos",
            "/Artists",
            "/Artists/AlbumArtists",
            "/Artists/queen",
            "/Artists/InstantMix",
            "/Artists/a1/InstantMix",
            "/MusicGenres/InstantMix",
            "/MusicGenres/rock/InstantMix",
            "/Albums/a1/InstantMix",
            "/Persons/actor",
            "/Persons/actor/Items",
            "/Playlists/p1/AddToPlaylistInfo",
            "/Playlists/p1/InstantMix",
            "/Songs/s1/InstantMix",
            "/Videos/v1/AdditionalParts",
        ] {
            assert_eq!(request_user_target(path, &query), Some("u7"), "{path}");
        }
    }

    #[test]
    fn user_usage_stats_path_target_is_cross_user_checked() {
        assert_eq!(
            request_user_target("/user_usage_stats/u4/2026-07-03/GetItems", &HashMap::new()),
            Some("u4")
        );
        assert_eq!(
            request_user_target("/user_usage_stats/session_list", &HashMap::new()),
            None
        );
    }

    #[test]
    fn query_parser_decodes_tokens() {
        let query = query_map("api_key=a%20b&UserId=u+1");
        assert_eq!(query.get("api_key").map(String::as_str), Some("a b"));
        assert_eq!(query.get("UserId").map(String::as_str), Some("u 1"));

        let query = query_map("apiKey=camel");
        assert_eq!(
            request_token(&HeaderMap::new(), &query).as_deref(),
            Some("camel")
        );
    }

    #[test]
    fn token_parser_accepts_emby_auth_header() {
        assert_eq!(
            auth_header_value_token(
                r#"MediaBrowser Client="x", Device="y", DeviceId="z", Token="abc""#
            )
            .as_deref(),
            Some("abc")
        );
        assert_eq!(
            auth_header_value_token(r#"Emby UserId="u", token="lower""#).as_deref(),
            Some("lower")
        );
    }

    #[test]
    fn request_token_accepts_bearer_and_mediabrowser_token_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer abc".parse().unwrap());
        assert_eq!(
            request_token(&headers, &HashMap::new()).as_deref(),
            Some("abc")
        );

        let mut headers = HeaderMap::new();
        headers.insert("X-MediaBrowser-Token", "xyz".parse().unwrap());
        assert_eq!(
            request_token(&headers, &HashMap::new()).as_deref(),
            Some("xyz")
        );
    }

    #[test]
    fn auth_provider_lists_have_name_id_shape() {
        let providers = name_id_pairs(&[("Default", "Provider")]);
        assert_eq!(providers[0]["Name"], "Default");
        assert_eq!(providers[0]["Id"], "Provider");
    }

    #[test]
    fn auth_query_results_include_start_index() {
        let result = auth_query_result(vec![json!({ "AppName": "Web" })]);
        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["StartIndex"], 0);
        assert_eq!(result["Items"][0]["AppName"], "Web");
    }

    #[test]
    fn api_key_inputs_are_normalized_and_limited() {
        assert_eq!(
            validate_api_key_app(Some("  Dashboard  ")).unwrap(),
            "Dashboard"
        );
        assert!(validate_api_key_app(None).is_err());
        assert!(validate_api_key_app(Some("bad\napp")).is_err());
        assert!(validate_api_key_app(Some(&"x".repeat(MAX_API_KEY_APP_LEN + 1))).is_err());

        assert_eq!(
            validate_api_key_token(" abc-DEF_123.456 ").unwrap(),
            "abc-DEF_123.456"
        );
        assert!(validate_api_key_token("").is_err());
        assert!(validate_api_key_token("bad/key").is_err());
        assert!(validate_api_key_token(&"x".repeat(MAX_API_KEY_TOKEN_LEN + 1)).is_err());

        let query: CreateApiKeyQuery = serde_json::from_value(json!({ "App": "Emby" })).unwrap();
        assert_eq!(query.app.as_deref(), Some("Emby"));
    }

    #[tokio::test]
    async fn api_key_create_and_delete_manage_tokens() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let state = test_state(db);
        let statement = crate::db::helpers::portable_statement(
            state.db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, 'admin', 'admin', 1, 0, 1, 1)",
            vec![state.user_id.to_string().into()],
        );
        state.db.execute(statement).await.unwrap();

        create_api_key_inner(
            &state,
            CreateApiKeyQuery {
                app: Some("  Dashboard  ".to_string()),
            },
        )
        .await
        .unwrap();

        let keys = api_keys_inner(&state.db).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["AppName"], "Dashboard");
        let token = keys[0]["AccessToken"].as_str().unwrap().to_string();
        assert_eq!(AccessTokens::find().all(&state.db).await.unwrap().len(), 1);

        delete_api_key_inner(&state.db, &token).await.unwrap();
        assert!(api_keys_inner(&state.db).await.unwrap().is_empty());
        assert!(
            AccessTokens::find()
                .all(&state.db)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(delete_api_key_inner(&state.db, "../bad").await.is_err());
    }

    #[test]
    fn password_policy_requires_current_password_for_self_change() {
        assert!(password_change_requires_current_password(false));
        assert!(!password_change_requires_current_password(true));
    }

    #[test]
    fn password_update_requires_non_empty_new_password_unless_resetting() {
        let missing = UpdatePasswordRequest {
            current_pw: Some("old".to_string()),
            new_pw: None,
            reset_password: false,
        };
        assert!(valid_new_password(&missing).is_none());
        assert!(!password_reset_clears_password(&missing));

        let blank = UpdatePasswordRequest {
            current_pw: Some("old".to_string()),
            new_pw: Some("  ".to_string()),
            reset_password: false,
        };
        assert!(valid_new_password(&blank).is_none());

        let reset = UpdatePasswordRequest {
            current_pw: None,
            new_pw: None,
            reset_password: true,
        };
        assert!(password_reset_clears_password(&reset));
    }

    #[test]
    fn user_write_inputs_are_validated() {
        assert_eq!(validate_user_name("  Alice  ").unwrap(), "Alice");
        assert!(validate_user_name("").is_err());
        assert!(validate_user_name("bad\nname").is_err());
        assert!(validate_user_name(&"x".repeat(MAX_USER_NAME_LEN + 1)).is_err());

        assert_eq!(validate_user_password("  secret  ").unwrap(), "secret");
        assert!(validate_user_password(&"x".repeat(MAX_USER_PASSWORD_LEN + 1)).is_err());
        assert!(validate_user_password("bad\0password").is_err());
    }

    #[test]
    fn policy_bool_reads_supported_policy_flags() {
        let policy = json!({ "IsAdministrator": true, "IsDisabled": false });
        assert_eq!(policy_bool(&policy, "IsAdministrator"), Some(true));
        assert_eq!(policy_bool(&policy, "IsDisabled"), Some(false));
        assert_eq!(policy_bool(&policy, "EnableAllFolders"), None);
    }

    #[test]
    fn user_filters_read_policy_flags() {
        let users = vec![
            user_json("u1", "alice", true, true, false),
            user_json("u2", "bob", true, false, true),
        ];
        let mut query = HashMap::new();
        query.insert("isDisabled".to_string(), "true".to_string());

        let disabled = filter_users(users.clone(), &query);
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0]["Id"], "u2");

        query.clear();
        query.insert("IsAdministrator".to_string(), "true".to_string());
        let admins = filter_users(users, &query);
        assert_eq!(admins.len(), 1);
        assert_eq!(admins[0]["Id"], "u1");
    }

    #[test]
    fn user_json_reports_real_password_state() {
        let user = user_json("u1", "alice", false, false, false);
        assert_eq!(user["HasPassword"], false);
        assert_eq!(user["HasConfiguredPassword"], false);
        assert_eq!(user["HasConfiguredEasyPassword"], false);
    }

    #[test]
    fn authentication_session_info_matches_client_headers() {
        let user = UserRow {
            id: "u1".to_string(),
            username: "alice".to_string(),
            password_hash: Some("hash".to_string()),
            is_admin: false,
            is_disabled: false,
        };
        let value = authentication_session_info(
            &user,
            "token-1",
            SessionCapabilities {
                user_id: "u1".to_string(),
                client: "Jellyfin Web".to_string(),
                device_name: "Firefox".to_string(),
                device_id: "header-device".to_string(),
                application_version: "10.10.0".to_string(),
                playable_media_types: vec!["Video".to_string()],
                supported_commands: vec!["Play".to_string(), "Stop".to_string()],
                supports_media_control: true,
                supports_persistent_identifier: true,
            },
            Some("body-device"),
            1,
        );

        assert_eq!(value["Id"], "token-1");
        assert_eq!(value["UserId"], "u1");
        assert_eq!(value["UserName"], "alice");
        assert_eq!(value["Client"], "Jellyfin Web");
        assert_eq!(value["DeviceName"], "Firefox");
        assert_eq!(value["DeviceId"], "body-device");
        assert_eq!(value["ApplicationVersion"], "10.10.0");
        assert_eq!(
            value["Capabilities"]["PlayableMediaTypes"],
            json!(["Video"])
        );
        assert_eq!(value["SupportedCommands"], json!(["Play", "Stop"]));
        assert_eq!(value["AdditionalUsers"], json!([]));
        assert_eq!(value["NowPlayingQueue"], json!([]));
    }

    #[tokio::test]
    async fn authenticate_result_includes_session_info() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, password_hash, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'alice', ?, 0, 0, 1, 1)",
            vec![hash_password("secret").unwrap().into()],
        ))
        .await
        .unwrap();

        let state = test_state(db);
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Emby-Authorization",
            r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="header-device", Version="10.10.0""#
                .parse()
                .unwrap(),
        );
        let response = authenticate_by_name_inner(
            &state,
            &headers,
            LoginRequest {
                username: "alice".to_string(),
                password: "secret".to_string(),
                device_id: Some("body-device".to_string()),
            },
        )
        .await
        .unwrap();

        assert!(
            response["AccessToken"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
        );
        assert_eq!(response["SessionInfo"]["UserId"], "u1");
        assert_eq!(response["SessionInfo"]["Client"], "Jellyfin Web");
        assert_eq!(response["SessionInfo"]["DeviceName"], "Firefox");
        assert_eq!(response["SessionInfo"]["DeviceId"], "body-device");
        assert_eq!(response["SessionInfo"]["ApplicationVersion"], "10.10.0");
        assert_eq!(
            response["SessionInfo"]["Capabilities"]["PlayableMediaTypes"],
            json!(["Audio", "Video"])
        );

        let sessions = state.playback_sessions.read().await;
        let session = sessions
            .get(response["AccessToken"].as_str().unwrap())
            .expect("login session should be tracked");
        assert_eq!(session.user_id, "u1");
        assert_eq!(session.client, "Jellyfin Web");
        assert_eq!(session.device_name, "Firefox");
        assert_eq!(session.device_id, "body-device");
        assert_eq!(session.item_id, "");
    }

    #[tokio::test]
    async fn quick_connect_authorizes_and_logs_in_once() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, password_hash, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'alice', ?, 0, 0, 1, 1)",
            vec![hash_password("secret").unwrap().into()],
        ))
        .await
        .unwrap();

        let state = Arc::new(test_state(db));
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Emby-Authorization",
            r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="quick-device", Version="10.10.0", Token="existing-token""#
                .parse()
                .unwrap(),
        );
        let login = authenticate_by_name_inner(
            &state,
            &headers,
            LoginRequest {
                username: "alice".to_string(),
                password: "secret".to_string(),
                device_id: None,
            },
        )
        .await
        .unwrap();
        let token = login["AccessToken"].as_str().unwrap().to_string();
        headers.insert(
            "X-Emby-Authorization",
            format!(
                r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="quick-device", Version="10.10.0", Token="{token}""#
            )
            .parse()
            .unwrap(),
        );

        let record = QuickConnectRecord {
            secret: "secret-1".to_string(),
            code: "123456".to_string(),
            device_id: "quick-device".to_string(),
            device_name: "Firefox".to_string(),
            app_name: "Jellyfin Web".to_string(),
            app_version: "10.10.0".to_string(),
            date_added_unix: now_unix(),
            user_id: None,
        };
        save_quick_connect_record(&state.db, &record).await.unwrap();
        let unauthorized = authenticate_with_quick_connect_inner(
            &state,
            &headers,
            QuickConnectAuthenticateRequest {
                secret: record.secret.clone(),
            },
        )
        .await;
        assert!(matches!(unauthorized, Err(AuthError::Unauthorized(_))));

        let mut query = HashMap::new();
        query.insert("code".to_string(), record.code.clone());
        let response =
            quick_connect_authorize(State(state.clone()), headers.clone(), Query(query)).await;
        assert_eq!(response.status(), StatusCode::OK);

        let authorized = load_quick_connect_record(&state.db, &record.secret)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(authorized.user_id.as_deref(), Some("u1"));
        assert_eq!(quick_connect_result(&authorized)["Authenticated"], true);

        let result = authenticate_with_quick_connect_inner(
            &state,
            &headers,
            QuickConnectAuthenticateRequest {
                secret: record.secret.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result["User"]["Id"], "u1");
        assert!(
            result["AccessToken"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(result["SessionInfo"]["DeviceId"], "quick-device");
        assert!(
            load_quick_connect_record(&state.db, &record.secret)
                .await
                .unwrap()
                .is_none()
        );

        let replay = authenticate_with_quick_connect_inner(
            &state,
            &headers,
            QuickConnectAuthenticateRequest {
                secret: record.secret,
            },
        )
        .await;
        assert!(matches!(replay, Err(AuthError::Unauthorized(_))));
    }

    #[test]
    fn users_update_requires_query_user_id() {
        assert!(query_value(&HashMap::new(), "UserId").is_none());

        let mut query = HashMap::new();
        query.insert("userId".to_string(), " u1 ".to_string());
        assert_eq!(query_value(&query, "UserId").as_deref(), Some("u1"));
    }

    #[tokio::test]
    async fn update_user_response_changes_name() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'old', 'old', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();

        let response = update_user_response(&db, "u1", &json!({ "Name": "new" })).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let user = user_by_id_inner(&db, "u1").await.unwrap().unwrap();
        assert_eq!(user.username, "new");
    }

    #[tokio::test]
    async fn update_user_response_rejects_invalid_name() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'old', 'old', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();

        let response = update_user_response(&db, "u1", &json!({ "Name": "bad\nname" })).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let user = user_by_id_inner(&db, "u1").await.unwrap().unwrap();
        assert_eq!(user.username, "old");
    }

    #[tokio::test]
    async fn user_policy_round_trips_saved_fields() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'alice', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();

        update_user_policy_inner(
            &db,
            "u1",
            &json!({
                "IsAdministrator": true,
                "EnableAllFolders": false,
                "EnabledFolders": ["library-1"],
                "Unexpected": true
            }),
        )
        .await
        .unwrap();

        let user = user_by_id_inner(&db, "u1").await.unwrap().unwrap();
        let dto = user_json_with_config(&db, &user).await;
        assert_eq!(dto["Policy"]["IsAdministrator"], true);
        assert_eq!(dto["Policy"]["EnableAllFolders"], false);
        assert_eq!(dto["Policy"]["EnabledFolders"][0], "library-1");
        assert!(dto["Policy"].get("Unexpected").is_none());
    }

    #[tokio::test]
    async fn user_list_includes_saved_policy() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'alice', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();

        update_user_policy_inner(&db, "u1", &json!({ "EnableAllFolders": false }))
            .await
            .unwrap();

        let users = list_users_inner(&db).await.unwrap();
        assert_eq!(users[0]["Policy"]["EnableAllFolders"], false);
    }

    #[test]
    fn user_configuration_merge_keeps_known_fields_only() {
        let merged = merge_user_configuration(
            default_user_configuration(),
            &json!({
                "SubtitleMode": "Always",
                "EnableNextEpisodeAutoPlay": false,
                "Unexpected": true
            }),
        );
        assert_eq!(merged["SubtitleMode"], "Always");
        assert_eq!(merged["EnableNextEpisodeAutoPlay"], false);
        assert!(merged.get("Unexpected").is_none());
    }

    #[test]
    fn user_configuration_and_policy_values_are_limited() {
        let merged = merge_user_configuration(
            default_user_configuration(),
            &json!({
                "SubtitleMode": format!("{}{}", "x".repeat(MAX_USER_SETTING_STRING_LEN + 20), "\n"),
                "GroupedFolders": (0..MAX_USER_SETTING_ARRAY_ITEMS + 20)
                    .map(|index| json!(format!("folder-{index}")))
                    .collect::<Vec<_>>(),
                "Unexpected": "ignored"
            }),
        );
        assert_eq!(
            merged["SubtitleMode"].as_str().unwrap().len(),
            MAX_USER_SETTING_STRING_LEN
        );
        assert!(
            !merged["SubtitleMode"]
                .as_str()
                .unwrap()
                .chars()
                .any(char::is_control)
        );
        assert_eq!(
            merged["GroupedFolders"].as_array().unwrap().len(),
            MAX_USER_SETTING_ARRAY_ITEMS
        );
        assert!(merged.get("Unexpected").is_none());

        let nested = json!({
            "AccessSchedules": [{
                "Name": "Morning\nShift",
                "Level1": { "Level2": { "Level3": { "TooDeep": true } } }
            }]
        });
        let policy = merge_user_policy(default_user_policy(false, false), &nested);
        let schedule = &policy["AccessSchedules"][0];
        assert_eq!(schedule["Name"], "MorningShift");
        assert_eq!(schedule["Level1"]["Level2"], json!({}));
    }

    fn test_state(db: DatabaseConnection) -> AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"test"),
            access_token: "test-token".to_string(),
            db,
            media_dirs: Vec::new(),
            http_client: reqwest::Client::new(),
            tmdb_api_key: RwLock::new(None),
            playback_sessions: RwLock::new(HashMap::<String, PlaybackSession>::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }

    fn test_playback_session() -> PlaybackSession {
        PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
            play_session_id: "s1".to_string(),
            item_id: "i1".to_string(),
            item_name: None,
            now_playing_queue: Vec::new(),
            additional_users: vec![crate::app::state::SessionUserInfo {
                user_id: "u2".to_string(),
                user_name: "guest".to_string(),
            }],
            client: "Web".to_string(),
            device_name: "Browser".to_string(),
            device_id: "device-1".to_string(),
            application_version: "1.0".to_string(),
            is_active: true,
            last_activity_date: "1970-01-01T00:00:00Z".to_string(),
            last_playback_check_in: "1970-01-01T00:00:00Z".to_string(),
            last_activity_unix: 0,
            play_state: PlaybackState {
                position_ticks: 0,
                is_paused: false,
                can_seek: true,
            },
            playable_media_types: vec!["Video".to_string()],
            supports_media_control_commands: vec!["Play".to_string()],
            supported_commands: vec!["Play".to_string()],
            supports_media_control: true,
            supports_remote_control: true,
            supports_persistent_identifier: true,
            capabilities: SessionCapabilities {
                user_id: "u1".to_string(),
                client: "Web".to_string(),
                device_name: "Browser".to_string(),
                device_id: "device-1".to_string(),
                application_version: "1.0".to_string(),
                playable_media_types: vec!["Video".to_string()],
                supported_commands: vec!["Play".to_string()],
                supports_media_control: true,
                supports_persistent_identifier: true,
            },
        }
    }
}
