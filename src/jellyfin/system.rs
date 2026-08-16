use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Extension, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbBackend,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select, Set, Statement,
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{
        AppState, PlaybackSession, SERVER_NAME, SessionCapabilities, normalize_tmdb_proxy_url,
    },
    db::row_ext::QueryResultExt,
    entities::{
        access_tokens, access_tokens::Entity as AccessTokens, activity_log,
        activity_log::Entity as ActivityLog, task_results, task_results::Entity as TaskResults,
        users, users::Entity as Users,
    },
    jellyfin::item_queries::library_views,
    jellyfin::{
        auth::query_user_id_or_request,
        common::{image as placeholder_image, internal_error},
    },
    library::path_utils,
    util::{now_unix, stable_text_id, system_time_to_unix, unix_to_jellyfin_date},
};

const BRANDING_SPLASHSCREEN_PATH: &str = "data/images/branding-splashscreen";
const BRANDING_SPLASHSCREEN_CONTENT_TYPE_KEY: &str = "BrandingSplashscreenContentType";
const MAX_BRANDING_SPLASHSCREEN_BYTES: usize = 10 * 1024 * 1024;
const MAX_BRANDING_LOGIN_DISCLAIMER_BYTES: usize = 16 * 1024;
const MAX_BRANDING_CUSTOM_CSS_BYTES: usize = 256 * 1024;
const MAX_CLIENT_LOG_BYTES: usize = 1024 * 1024;
const MAX_CAMERA_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
const MAX_USER_USAGE_BACKUP_BYTES: usize = 20 * 1024 * 1024;
const MAX_LOG_LINE_LIMIT: usize = 10_000;
const MAX_DEVICE_ID_LEN: usize = 256;
const MAX_DEVICE_IDS_PER_REQUEST: usize = 128;
const MAX_DEVICE_CUSTOM_NAME_LEN: usize = 128;
const MAX_CAMERA_UPLOAD_FIELD_LEN: usize = 256;
const MAX_PLUGIN_REPOSITORIES: usize = 32;
const MAX_PLUGIN_REPOSITORY_NAME_LEN: usize = 128;
const MAX_PLUGIN_REPOSITORY_URL_LEN: usize = 2048;
const MAX_DOUBAN_COOKIE_LEN: usize = 16 * 1024;
const MAX_TMDB_LLM_API_KEY_LEN: usize = 16 * 1024;
const MAX_TMDB_LLM_BASE_URL_LEN: usize = 2048;
const MAX_TMDB_LLM_MODEL_LEN: usize = 256;
const MAX_SCHEDULED_TASK_TRIGGERS: usize = 32;
const MAX_SCHEDULED_TASK_TRIGGERS_JSON_BYTES: usize = 32 * 1024;
const MAX_NOTIFICATION_NAME_LEN: usize = 256;
const MAX_NOTIFICATION_DESCRIPTION_LEN: usize = 4096;
const MAX_NOTIFICATION_IDS: usize = 512;
const MAX_NOTIFICATION_ID_LEN: usize = 128;
const MAX_ADMIN_SERVER_NAME_LEN: usize = 128;
const EMBY_COMPAT_VERSION: &str = "4.8.0.0";
const CAMERA_UPLOADS_PATH: &str = "data/camera_uploads";
const USER_USAGE_BACKUP_PATH: &str = "data/user_usage_stats";
const FALLBACK_FONTS_PATH: &str = "data/fonts";
const TICKS_PER_SECOND: i64 = 10_000_000;

mod configuration;
mod localization;

pub use configuration::{
    complete_startup, configuration_pages, dashboard_configuration_page, default_metadata_options,
    named_configuration, server_configuration, startup_configuration, startup_user,
    update_named_configuration, update_remote_access, update_server_configuration,
    update_startup_configuration, update_startup_user,
};
pub(crate) use localization::normalize_language_codes;
pub use localization::{
    localization_countries, localization_cultures, localization_options, parental_ratings,
};

pub async fn system_info(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let server_name = app_setting(&state.db, "ServerName", SERVER_NAME).await;
    let startup_completed = app_setting_bool(&state.db, "StartupWizardCompleted", true).await;
    Json(system_info_value(
        server_name,
        startup_completed,
        true,
        Some(request_base_url(&headers)),
    ))
    .into_response()
}

pub async fn public_system_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let server_name = app_setting(&state.db, "ServerName", SERVER_NAME).await;
    let startup_completed = app_setting_bool(&state.db, "StartupWizardCompleted", true).await;
    Json(system_info_value(
        server_name,
        startup_completed,
        false,
        Some(request_base_url(&headers)),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct AdminServerNameRequest {
    #[serde(rename = "ServerName", alias = "serverName", alias = "Name")]
    server_name: String,
}

pub async fn admin_server_name(State(state): State<Arc<AppState>>) -> Response {
    let server_name = app_setting(&state.db, "ServerName", SERVER_NAME).await;
    Json(json!({ "ServerName": server_name })).into_response()
}

pub async fn update_admin_server_name(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AdminServerNameRequest>,
) -> Response {
    let server_name = match normalize_admin_server_name(&request.server_name) {
        Some(value) => value,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": "ServerName must be 1-128 characters" })),
            )
                .into_response();
        }
    };
    match set_app_setting(&state.db, "ServerName", &server_name).await {
        Ok(()) => Json(json!({ "ServerName": server_name })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn admin_http_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let after_id = query
        .get("AfterId")
        .or_else(|| query.get("afterId"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1_000);
    let items = state.admin_http_logs(after_id, limit).await;
    let last_id = items.last().map(|entry| entry.id).unwrap_or(after_id);
    Json(json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "StartIndex": 0,
        "LastId": last_id,
    }))
    .into_response()
}

pub async fn admin_playback_map(State(state): State<Arc<AppState>>) -> Response {
    Json(state.playback_distribution_json().await).into_response()
}

pub async fn admin_playback_stats(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let days = query_value(&query, "Days")
        .or_else(|| query_value(&query, "days"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 366);
    match playback_stats_overview(&state.db, days).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

fn normalize_admin_server_name(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= MAX_ADMIN_SERVER_NAME_LEN)
        .then(|| value.to_string())
}

fn system_info_value(
    server_name: String,
    startup_completed: bool,
    include_private: bool,
    local_address: Option<String>,
) -> JsonValue {
    let local_address = local_address.filter(|value| !value.trim().is_empty());
    let mut value = json!({
        "ServerName": server_name,
        "Version": EMBY_COMPAT_VERSION,
        "ProductName": "Emby Server",
        "PackageName": "jellyfin-rs",
        "SystemUpdateLevel": "Release",
        "OperatingSystem": std::env::consts::OS,
        "Id": "jellyfin-rs",
        "ServerId": "jellyfin-rs",
        "StartupWizardCompleted": startup_completed,
    });
    if let Some(local_address) = local_address.as_ref() {
        value["LocalAddress"] = json!(local_address);
        value["WanAddress"] = json!(local_address);
    }
    if include_private {
        let local_address = local_address.unwrap_or_else(|| "http://127.0.0.1:8096".to_string());
        let http_port = base_url_port(&local_address).unwrap_or(8096);
        let supports_https = local_address.starts_with("https://");
        value["LocalAddress"] = json!(local_address);
        value["WanAddress"] = json!(local_address);
        value["OperatingSystemDisplayName"] = json!(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        value["HasPendingRestart"] = json!(false);
        value["IsShuttingDown"] = json!(false);
        value["HasUpdateAvailable"] = json!(false);
        value["WebSocketPortNumber"] = json!(http_port);
        value["HttpServerPortNumber"] = json!(http_port);
        value["SupportsHttps"] = json!(supports_https);
        value["HttpsPortNumber"] = json!(if supports_https { http_port } else { 0 });
        value["CanSelfRestart"] = json!(false);
        value["CanSelfUpdate"] = json!(false);
        value["CanLaunchWebBrowser"] = json!(false);
        value["SupportsAutoRunAtStartup"] = json!(false);
        value["HardwareAccelerationRequiresPremiere"] = json!(false);
        value["SupportsLibraryMonitor"] = json!(true);
        value["ProgramDataPath"] = json!("");
        value["ItemsByNamePath"] = json!("");
        value["CachePath"] = json!("");
        value["LogPath"] = json!("");
        value["InternalMetadataPath"] = json!("");
        value["TranscodingTempPath"] = json!("");
        value["EncoderLocation"] = json!("System");
        value["SystemArchitecture"] = json!(std::env::consts::ARCH);
        value["CompletedInstallations"] = json!([]);
    }
    value
}

fn base_url_port(value: &str) -> Option<u16> {
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value)
        .split('/')
        .next()
        .unwrap_or_default();
    if authority.starts_with('[') {
        return authority
            .rsplit_once("]:")
            .and_then(|(_, port)| port.parse::<u16>().ok());
    }
    authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
}

fn request_base_url(headers: &HeaderMap) -> String {
    if let Some(public_url) = configured_public_url() {
        return public_url;
    }
    let scheme = header_text(headers, "x-forwarded-proto")
        .and_then(|value| value.split(',').next().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http".to_string());
    let host = header_text(headers, "x-forwarded-host")
        .or_else(|| header_text(headers, "host"))
        .unwrap_or_else(|| "127.0.0.1:8096".to_string());
    format!("{scheme}://{host}")
}

fn configured_public_url() -> Option<String> {
    std::env::var("JELLYFIN_RS_PUBLIC_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub async fn web_strings() -> Response {
    Json(web_strings_value()).into_response()
}

pub async fn web_string_set() -> Response {
    Json(json!({
        "Culture": "en-US",
        "Strings": web_strings_value(),
    }))
    .into_response()
}

pub async fn quick_connect_enabled(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(app_setting_bool(&state.db, "QuickConnectEnabled", false).await)
}

pub async fn branding_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(branding_options(&state.db).await).into_response()
}

pub async fn update_branding_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<JsonValue>,
) -> Response {
    let options = match normalize_branding_options(request) {
        Ok(options) => options,
        Err(error) => return validation_error_response(error),
    };

    match set_app_setting(&state.db, "BrandingOptions", &options.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn branding_css(State(state): State<Arc<AppState>>) -> Response {
    let css = branding_options(&state.db)
        .await
        .get("CustomCss")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        css,
    )
        .into_response()
}

pub async fn branding_splashscreen(State(state): State<Arc<AppState>>) -> Response {
    let path = branding_splashscreen_path();
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return placeholder_image().await;
        }
        Err(error) => return internal_error(error.into()),
    };

    let content_type = app_setting(
        &state.db,
        BRANDING_SPLASHSCREEN_CONTENT_TYPE_KEY,
        "image/png",
    )
    .await;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("image/png")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    (headers, Body::from(bytes)).into_response()
}

pub async fn upload_branding_splashscreen(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if !allowed_branding_image_content_type(content_type) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Splashscreen upload must be an image" })),
        )
            .into_response();
    }
    if body.len() > MAX_BRANDING_SPLASHSCREEN_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Splashscreen image is too large" })),
        )
            .into_response();
    }

    let path = branding_splashscreen_path();
    if let Some(directory) = path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(directory).await {
            return internal_error(error.into());
        }
    }
    if let Err(error) = tokio::fs::write(&path, &body).await {
        return internal_error(error.into());
    }
    match set_app_setting(
        &state.db,
        BRANDING_SPLASHSCREEN_CONTENT_TYPE_KEY,
        content_type,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_branding_splashscreen(State(state): State<Arc<AppState>>) -> Response {
    match tokio::fs::remove_file(branding_splashscreen_path()).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return internal_error(error.into()),
    }

    match set_app_setting(&state.db, BRANDING_SPLASHSCREEN_CONTENT_TYPE_KEY, "").await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn client_log_document(headers: HeaderMap, body: Bytes) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.is_empty() && !content_type.starts_with("text/plain") {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    if body.len() > MAX_CLIENT_LOG_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if std::str::from_utf8(&body).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let file_name = client_log_file_name();
    let path = std::path::PathBuf::from("logs").join(&file_name);
    if let Err(error) = tokio::fs::create_dir_all("logs").await {
        return internal_error(error.into());
    }
    if let Err(error) = tokio::fs::write(&path, &body).await {
        return internal_error(error.into());
    }
    Json(json!({ "FileName": file_name })).into_response()
}

fn client_log_file_name() -> String {
    format!(
        "client-{}-{}.log",
        now_unix(),
        uuid::Uuid::new_v4().simple()
    )
}

#[derive(Deserialize, Default)]
pub struct DevicesQuery {
    #[serde(rename = "userId", alias = "UserId")]
    user_id: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    start_index: usize,
    #[serde(rename = "Limit", alias = "limit")]
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
pub struct DeviceIdQuery {
    #[serde(rename = "id", alias = "Id")]
    id: Option<String>,
}

#[derive(Deserialize)]
pub struct DeviceOptionsRequest {
    #[serde(rename = "CustomName")]
    custom_name: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct CameraUploadQuery {
    #[serde(rename = "DeviceId", alias = "deviceId")]
    device_id: Option<String>,
    #[serde(rename = "Album", alias = "album")]
    album: Option<String>,
    #[serde(rename = "Name", alias = "name")]
    name: Option<String>,
    #[serde(rename = "Id", alias = "id")]
    id: Option<String>,
}

pub async fn devices(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DevicesQuery>,
) -> Response {
    let items = device_records(&state, query.user_id.as_deref())
        .await
        .into_iter()
        .collect::<Vec<_>>();
    let mut custom_names = HashMap::new();
    for record in &items {
        custom_names.insert(
            record.id.clone(),
            device_custom_name(&state.db, &record.id).await,
        );
    }
    let mut items = items
        .into_iter()
        .map(|record| {
            let custom_name = custom_names.get(&record.id).cloned().flatten();
            record.to_json(custom_name)
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a.get("Name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("Name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default(),
            )
    });
    let total = items.len();
    let start = query.start_index.min(total);
    let limit = query.limit.unwrap_or(total);
    let page = items
        .into_iter()
        .skip(start)
        .take(limit)
        .collect::<Vec<_>>();
    Json(json!({
        "Items": page,
        "TotalRecordCount": total,
        "StartIndex": start
    }))
    .into_response()
}

pub async fn delete_devices(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeviceIdQuery>,
) -> Response {
    let ids = match normalize_device_ids(query.id.as_deref()) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

    let now = now_unix();
    for id in ids {
        if let Err(error) = revoke_device(&state, &id, now).await {
            return internal_error(error);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn camera_uploads(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let device_id = match normalize_device_id(
        query_value(&query, "DeviceId")
            .or_else(|| query_value(&query, "deviceId"))
            .as_deref(),
    ) {
        Ok(device_id) => device_id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response();
        }
    };
    match camera_upload_history_value(&state.db, &device_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn upload_camera(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<CameraUploadQuery>,
    body: Bytes,
) -> Response {
    match save_camera_upload(&state.db, headers, query, body).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) if error.to_string().contains("required") => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error)
            if error.to_string().contains("unsupported")
                || error.to_string().contains("too long") =>
        {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error) if error.to_string().contains("too large") => {
            StatusCode::PAYLOAD_TOO_LARGE.into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn device_info(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeviceIdQuery>,
) -> Response {
    let id = match normalize_device_id(query.id.as_deref()) {
        Ok(id) => id,
        Err(_) => return device_not_found(),
    };
    let Some(record) = device_records(&state, None)
        .await
        .into_iter()
        .find(|record| record.id == id)
    else {
        return device_not_found();
    };
    let custom_name = device_custom_name(&state.db, &record.id).await;
    Json(record.to_json(custom_name)).into_response()
}

pub async fn device_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeviceIdQuery>,
) -> Response {
    let id = match normalize_device_id(query.id.as_deref()) {
        Ok(id) => id,
        Err(_) => return device_not_found(),
    };
    let exists = device_records(&state, None)
        .await
        .into_iter()
        .any(|record| record.id == id);
    if exists {
        Json(device_options_result(
            &id,
            device_custom_name(&state.db, &id).await,
        ))
        .into_response()
    } else {
        device_not_found()
    }
}

pub async fn update_device_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeviceIdQuery>,
    Json(request): Json<DeviceOptionsRequest>,
) -> Response {
    let id = match normalize_device_id(query.id.as_deref()) {
        Ok(id) => id,
        Err(_) => return device_not_found(),
    };
    let exists = device_records(&state, None)
        .await
        .into_iter()
        .any(|record| record.id == id);
    if !exists {
        return device_not_found();
    }
    let custom_name = match normalize_device_custom_name(request.custom_name.as_deref()) {
        Ok(custom_name) => custom_name,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response();
        }
    };
    match set_app_setting(
        &state.db,
        &device_options_key(&id),
        custom_name.as_deref().unwrap_or_default(),
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn sync_targets() -> impl IntoResponse {
    Json(sync_targets_result())
}

pub async fn sync_options() -> impl IntoResponse {
    Json(sync_options_result())
}

pub async fn sync_empty_query_result() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 })).into_response()
}

pub async fn sync_data() -> Response {
    Json(sync_data_result()).into_response()
}

pub async fn sync_empty_response() -> Response {
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
pub struct DirectoryContentsQuery {
    #[serde(rename = "path", alias = "Path")]
    path: String,
    #[serde(rename = "includeFiles", alias = "IncludeFiles", default)]
    include_files: bool,
    #[serde(rename = "includeDirectories", alias = "IncludeDirectories", default)]
    include_directories: bool,
}

#[derive(Deserialize)]
pub struct ParentPathQuery {
    #[serde(rename = "path", alias = "Path")]
    path: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct ValidatePathRequest {
    #[serde(rename = "Path", alias = "path")]
    path: Option<String>,
    #[serde(rename = "IsFile", alias = "isFile", alias = "is_file")]
    is_file: Option<bool>,
    #[serde(
        rename = "ValidateWritable",
        alias = "ValidateWriteable",
        alias = "validateWritable",
        alias = "validateWriteable",
        alias = "validate_writable",
        default
    )]
    validate_writable: bool,
}

pub async fn default_directory_browser() -> impl IntoResponse {
    Json(json!({
        "Path": std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string())
    }))
}

pub async fn directory_contents(Query(query): Query<DirectoryContentsQuery>) -> Response {
    match path_utils::directory_entries(&query.path, query.include_files, query.include_directories)
    {
        Ok(entries) => Json(entries).into_response(),
        Err(error)
            if error.to_string().contains("not exist")
                || error.to_string().contains("failed to read") =>
        {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn drives() -> impl IntoResponse {
    Json(path_utils::drive_entries())
}

pub async fn parent_path(Query(query): Query<ParentPathQuery>) -> impl IntoResponse {
    Json(path_utils::parent_path(&query.path))
}

pub async fn validate_path(
    Query(query): Query<HashMap<String, String>>,
    body: Result<Json<ValidatePathRequest>, JsonRejection>,
) -> Response {
    let request = match body {
        Ok(Json(request)) => request,
        Err(JsonRejection::MissingJsonContentType(_)) => ValidatePathRequest::default(),
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.body_text() })),
            )
                .into_response();
        }
    };
    let request = validate_path_request_from_inputs(&query, request);
    let path = request.path.unwrap_or_default();
    match path_utils::validate_path(&path, request.is_file, request.validate_writable) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) if error.to_string().contains("not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

fn validate_path_request_from_inputs(
    query: &HashMap<String, String>,
    mut request: ValidatePathRequest,
) -> ValidatePathRequest {
    if request
        .path
        .as_deref()
        .is_none_or(|path| path.trim().is_empty())
    {
        request.path = query_value(query, "Path");
    }
    if request.is_file.is_none() {
        request.is_file = query_bool_any(query, &["IsFile", "isFile", "is_file"]);
    }
    if !request.validate_writable {
        request.validate_writable = query_bool_any(
            query,
            &[
                "ValidateWritable",
                "ValidateWriteable",
                "validateWritable",
                "validateWriteable",
                "validate_writable",
            ],
        )
        .unwrap_or(false);
    }
    request
}

pub async fn system_endpoint() -> impl IntoResponse {
    Json(json!({
        "IsLocal": true,
        "IsInNetwork": true
    }))
}

pub async fn system_ping() -> impl IntoResponse {
    "Emby Server"
}

pub async fn utc_time() -> impl IntoResponse {
    Json(json!({
        "RequestReceptionTime": crate::util::unix_to_jellyfin_date(now_unix()),
        "ResponseTransmissionTime": crate::util::unix_to_jellyfin_date(now_unix())
    }))
}

pub async fn tmdb_client_configuration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let has_api_key = state
        .tmdb_api_key
        .read()
        .await
        .as_deref()
        .is_some_and(|key| !key.is_empty());
    let provider_enabled = crate::library::tmdb_metadata::tmdb_provider_enabled(&state.db).await;
    let proxy_url = state.tmdb_proxy_url.read().await.clone();
    Json(tmdb_client_configuration_value(
        provider_enabled,
        has_api_key,
        proxy_url.as_deref(),
    ))
}

pub async fn tmdb_llm_configuration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(crate::library::tmdb_metadata::tmdb_llm_configuration_value(&state.db).await)
}

#[derive(Deserialize)]
pub struct TmdbLlmConfigurationRequest {
    #[serde(rename = "Enabled", alias = "enabled")]
    enabled: bool,
    #[serde(rename = "ApiKey", alias = "apiKey", alias = "api_key")]
    api_key: Option<String>,
    #[serde(rename = "BaseUrl", alias = "baseUrl", alias = "base_url")]
    base_url: String,
    #[serde(rename = "Model", alias = "model")]
    model: String,
}

fn invalid_tmdb_llm_text(value: &str, max_len: usize) -> bool {
    value.len() > max_len || value.contains('\0') || value.chars().any(char::is_control)
}

pub async fn update_tmdb_llm_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TmdbLlmConfigurationRequest>,
) -> Response {
    let base_url = request.base_url.trim();
    let model = request.model.trim();
    if invalid_tmdb_llm_text(base_url, MAX_TMDB_LLM_BASE_URL_LEN)
        || invalid_tmdb_llm_text(model, MAX_TMDB_LLM_MODEL_LEN)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Invalid LLM base URL or model" })),
        )
            .into_response();
    }
    let api_key = request.api_key.as_deref().map(str::trim);
    if api_key.is_some_and(|value| invalid_tmdb_llm_text(value, MAX_TMDB_LLM_API_KEY_LEN)) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Invalid LLM API key" })),
        )
            .into_response();
    }

    let settings = &state.db;
    let result = async {
        crate::db::settings::set(
            settings,
            crate::library::tmdb_metadata::TMDB_LLM_ENABLED_KEY,
            if request.enabled { "true" } else { "false" },
        )
        .await?;
        if base_url.is_empty() {
            crate::db::settings::delete(
                settings,
                crate::library::tmdb_metadata::TMDB_LLM_BASE_URL_KEY,
            )
            .await?;
        } else {
            crate::db::settings::set(
                settings,
                crate::library::tmdb_metadata::TMDB_LLM_BASE_URL_KEY,
                base_url,
            )
            .await?;
        }
        if model.is_empty() {
            crate::db::settings::delete(
                settings,
                crate::library::tmdb_metadata::TMDB_LLM_MODEL_KEY,
            )
            .await?;
        } else {
            crate::db::settings::set(
                settings,
                crate::library::tmdb_metadata::TMDB_LLM_MODEL_KEY,
                model,
            )
            .await?;
        }
        if let Some(api_key) = api_key {
            if api_key.is_empty() {
                crate::db::settings::delete(
                    settings,
                    crate::library::tmdb_metadata::TMDB_LLM_API_KEY_KEY,
                )
                .await?;
            } else {
                crate::db::settings::set(
                    settings,
                    crate::library::tmdb_metadata::TMDB_LLM_API_KEY_KEY,
                    api_key,
                )
                .await?;
            }
        }
        crate::library::tmdb_metadata::mark_tmdb_llm_audit_pending(settings).await
    }
    .await;

    match result {
        Ok(()) => {
            Json(crate::library::tmdb_metadata::tmdb_llm_configuration_value(&state.db).await)
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn start_tmdb_llm_audit(State(state): State<Arc<AppState>>) -> Response {
    let config = crate::library::tmdb_metadata::load_tmdb_llm_config(&state.db).await;
    if !config.configured() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "LLM 刮削尚未配置完成" })),
        )
            .into_response();
    }
    let Some(tmdb_api_key) = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|key| !key.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "TMDb API key is not configured" })),
        )
            .into_response();
    };
    if let Err(error) = crate::library::tmdb_metadata::mark_tmdb_llm_audit_pending(&state.db).await
    {
        return internal_error(error);
    }

    let database_url = crate::db::database_url_from_env();
    let http_client = state.http_client.clone();
    let tmdb_proxy_url = state.tmdb_proxy_url.read().await.clone();
    tokio::spawn(async move {
        let db = match crate::db::background_connection(&database_url).await {
            Ok(db) => db,
            Err(error) => {
                tracing::error!("manual TMDb LLM audit could not connect to database: {error:#}");
                return;
            }
        };
        match crate::library::tmdb_metadata::audit_existing_tmdb(
            &db,
            &tmdb_api_key,
            &http_client,
            tmdb_proxy_url.as_deref(),
        )
        .await
        {
            Ok(repaired) => tracing::info!(
                "manual TMDb LLM audit completed: repaired/refreshed {repaired} item(s)"
            ),
            Err(error) => tracing::error!("manual TMDb LLM audit failed: {error:#}"),
        }
    });

    (StatusCode::ACCEPTED, Json(json!({ "Status": "Started" }))).into_response()
}

fn tmdb_client_configuration_value(
    provider_enabled: bool,
    has_api_key: bool,
    proxy_url: Option<&str>,
) -> JsonValue {
    let proxy_url = proxy_url.unwrap_or_default().trim();
    let has_proxy = !proxy_url.is_empty();
    let configured = provider_enabled && has_api_key;
    json!({
        "ProviderEnabled": provider_enabled,
        "Configured": configured,
        "IsTmdbEnabled": configured,
        "IsEnabled": configured,
        "Enabled": configured,
        "HasApiKey": has_api_key,
        "HasProxy": has_proxy,
        "ProxyUrl": proxy_url,
        "TmdbProxyUrl": proxy_url
    })
}

#[derive(Deserialize)]
pub struct TmdbApiKeyRequest {
    #[serde(
        rename = "TmdbApiKey",
        alias = "ApiKey",
        alias = "apiKey",
        alias = "tmdbApiKey"
    )]
    tmdb_api_key: String,
}

pub async fn update_tmdb_api_key(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TmdbApiKeyRequest>,
) -> Response {
    match state.set_tmdb_api_key(request.tmdb_api_key.trim()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(Deserialize)]
pub struct MetadataProviderEnabledRequest {
    #[serde(rename = "Enabled", alias = "enabled")]
    enabled: bool,
}

pub async fn update_tmdb_provider_enabled(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MetadataProviderEnabledRequest>,
) -> Response {
    match crate::db::settings::set(
        &state.db,
        crate::library::tmdb_metadata::TMDB_ENABLED_KEY,
        if request.enabled { "true" } else { "false" },
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(Deserialize)]
pub struct TmdbProxyUrlRequest {
    #[serde(
        rename = "TmdbProxyUrl",
        alias = "ProxyUrl",
        alias = "proxyUrl",
        alias = "tmdbProxyUrl"
    )]
    tmdb_proxy_url: String,
}

pub async fn update_tmdb_proxy_url(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TmdbProxyUrlRequest>,
) -> Response {
    if normalize_tmdb_proxy_url(&request.tmdb_proxy_url).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Invalid TMDb proxy URL" })),
        )
            .into_response();
    }
    match state.set_tmdb_proxy_url(&request.tmdb_proxy_url).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn douban_client_configuration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let has_cookie = state
        .douban_cookie
        .read()
        .await
        .as_deref()
        .is_some_and(|cookie| !cookie.is_empty());
    let provider_enabled =
        crate::library::douban_metadata::douban_provider_enabled(&state.db).await;
    Json(douban_client_configuration_value(
        provider_enabled,
        has_cookie,
    ))
}

fn douban_client_configuration_value(provider_enabled: bool, has_cookie: bool) -> JsonValue {
    let configured = provider_enabled && has_cookie;
    json!({
        "ProviderEnabled": provider_enabled,
        "Configured": configured,
        "IsDoubanEnabled": configured,
        "IsEnabled": configured,
        "Enabled": configured,
        "HasCookie": has_cookie
    })
}

pub async fn update_douban_provider_enabled(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MetadataProviderEnabledRequest>,
) -> Response {
    match crate::db::settings::set(
        &state.db,
        crate::library::douban_metadata::DOUBAN_ENABLED_KEY,
        if request.enabled { "true" } else { "false" },
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

#[derive(Deserialize)]
pub struct DoubanCookieRequest {
    #[serde(
        rename = "DoubanCookie",
        alias = "Cookie",
        alias = "cookie",
        alias = "doubanCookie"
    )]
    douban_cookie: String,
}

pub async fn update_douban_cookie(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DoubanCookieRequest>,
) -> Response {
    let cookie = crate::app::state::normalize_douban_cookie_value(&request.douban_cookie);
    if cookie.len() > MAX_DOUBAN_COOKIE_LEN
        || cookie.contains('\0')
        || cookie.chars().any(|ch| ch.is_control() && ch != '\t')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Invalid Douban cookie" })),
        )
            .into_response();
    }

    match state.set_douban_cookie(&cookie).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn system_logs() -> impl IntoResponse {
    Json(log_files())
}

pub async fn system_log_file(Query(query): Query<HashMap<String, String>>) -> Response {
    let Some(name) = query_value(&query, "Name")
        .or_else(|| query_value(&query, "name"))
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    log_file_response(&name)
}

pub async fn system_storage() -> impl IntoResponse {
    Json(json!({
        "ProgramDataPath": std::env::current_dir().ok().and_then(|path| path.to_str().map(str::to_string)).unwrap_or_default(),
        "WebPath": "",
        "ItemsByNamePath": "",
        "CachePath": "",
        "LogPath": "",
        "InternalMetadataPath": "",
        "TranscodingTempPath": ""
    }))
}

pub async fn bitrate_test(Query(query): Query<HashMap<String, String>>) -> impl IntoResponse {
    let size = query
        .get("Size")
        .or_else(|| query.get("size"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(500_000)
        .min(10_000_000);
    let bytes = vec![0; size];
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    (headers, Body::from(bytes))
}

pub async fn web_manifest() -> impl IntoResponse {
    Json(json!({
        "name": "Emby Server",
        "short_name": "Emby",
        "start_url": "/web/index.html",
        "display": "standalone",
        "background_color": "#101010",
        "theme_color": "#52b54b",
        "icons": []
    }))
}

pub async fn activity_log(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(15)
        .clamp(1, 100);
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let has_user_id = query
        .get("hasUserId")
        .or_else(|| query.get("HasUserId"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));

    let select = activity_log_query(has_user_id);
    let total = match select.clone().count(&state.db).await {
        Ok(total) => total,
        Err(error) => return internal_error(error.into()),
    };

    match select
        .order_by_desc(activity_log::Column::CreatedAt)
        .limit(limit)
        .offset(start_index)
        .all(&state.db)
        .await
    {
        Ok(models) => {
            let items: Vec<JsonValue> = models.iter().map(activity_log_entry_json).collect();
            Json(json!({
                "Items": items,
                "TotalRecordCount": total,
                "StartIndex": start_index
            }))
            .into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

fn activity_log_query(has_user_id: bool) -> Select<activity_log::Entity> {
    let select = ActivityLog::find();
    if has_user_id {
        select.filter(activity_log::Column::UserId.is_not_null())
    } else {
        select
    }
}

fn activity_log_entry_json(model: &activity_log::Model) -> JsonValue {
    json!({
        "Name": model.name,
        "Type": model.log_type,
        "Date": crate::util::unix_to_jellyfin_date(model.created_at),
        "UserId": model.user_id.as_deref().unwrap_or_default(),
        "Severity": model.severity,
    })
}

pub async fn users_item_access(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    match library_views(&state.db).await {
        Ok(libraries) => Json(items_access_value(&user_id, &libraries)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn items_access(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let user_id = query_value(&query, "UserId")
        .or_else(|| {
            body.as_ref()
                .and_then(|Json(body)| json_string(body, "UserId"))
        })
        .unwrap_or_else(|| request_user_id.clone());
    if user_id != request_user_id {
        match Users::find_by_id(&request_user_id).one(&state.db).await {
            Ok(Some(user)) if user.is_admin != 0 => {}
            Ok(Some(_)) | Ok(None) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "Error": "User access is denied" })),
                )
                    .into_response();
            }
            Err(error) => return internal_error(error.into()),
        }
    }
    match library_views(&state.db).await {
        Ok(libraries) => Json(items_access_value(&user_id, &libraries)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn items_shared_leave() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn user_view_grouping_options() -> Response {
    Json(user_view_grouping_options_value()).into_response()
}

pub async fn ui_view(Query(query): Query<HashMap<String, String>>) -> Response {
    let view_id = query_value(&query, "Name")
        .or_else(|| query_value(&query, "ViewName"))
        .or_else(|| query_value(&query, "Id"))
        .unwrap_or_else(|| "home".to_string());
    Json(json!({
        "Id": view_id,
        "Name": view_id,
        "Type": "View",
        "Status": "Available",
        "Items": [],
        "Commands": []
    }))
    .into_response()
}

pub async fn ui_command() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub async fn image_by_name_general() -> Response {
    Json(vec![image_by_name_info("logo", "", "General")]).into_response()
}

pub async fn image_by_name_media_info() -> Response {
    Json(vec![image_by_name_info("video", "default", "MediaInfo")]).into_response()
}

pub async fn image_by_name_ratings() -> Response {
    Json(vec![image_by_name_info("unrated", "default", "Ratings")]).into_response()
}

pub async fn game_system_summaries(State(state): State<Arc<AppState>>) -> Response {
    match game_system_summary_rows(&state.db).await {
        Ok(items) => Json(items).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_user_list(State(state): State<Arc<AppState>>) -> Response {
    match Users::find()
        .order_by_asc(users::Column::Username)
        .all(&state.db)
        .await
    {
        Ok(users) => Json(
            users
                .iter()
                .map(usage_user_entry)
                .collect::<Vec<JsonValue>>(),
        )
        .into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn user_usage_stats_type_filter_list(State(state): State<Arc<AppState>>) -> Response {
    match state
        .db
        .query_all_raw(crate::db::helpers::pg_statement(
            "SELECT item_type, COUNT(*) AS cnt FROM media_items GROUP BY item_type ORDER BY item_type",
            vec![],
        ))
        .await
    {
        Ok(rows) => {
            let items = rows
                .iter()
                .filter_map(|row| {
                    let item_type = row.get_str("item_type").ok()?;
                    let count = row.get_i64("cnt").unwrap_or_default();
                    Some(json!({
                        "Name": item_type,
                        "Id": item_type,
                        "Type": item_type,
                        "Count": count
                    }))
                })
                .collect::<Vec<_>>();
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

pub async fn user_usage_stats_session_list(State(state): State<Arc<AppState>>) -> Response {
    let now = now_unix();
    let timeout = crate::app::state::session_timeout_seconds();
    let mut sessions = state.playback_sessions.write().await;
    sessions.retain(|_, session| now - session.last_activity_unix <= timeout);
    let items = sessions
        .values()
        .map(usage_stats_session_entry)
        .collect::<Vec<_>>();
    Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
}

pub async fn user_usage_stats_process_list() -> Response {
    let process = current_process_usage();
    Json(json!({ "Items": [process], "TotalRecordCount": 1 })).into_response()
}

pub async fn user_usage_stats_resource_usage(
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let hours = query_value(&query, "hours")
        .or_else(|| query_value(&query, "Hours"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1)
        .clamp(1, 24 * 365);
    Json(json!({
        "Items": [current_process_usage()],
        "TotalRecordCount": 1,
        "Hours": hours
    }))
    .into_response()
}

pub async fn user_usage_stats_play_activity(State(state): State<Arc<AppState>>) -> Response {
    match play_activity_rows(&state.db, &[], None, None).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_movies_report(State(state): State<Arc<AppState>>) -> Response {
    user_usage_stats_report(state, &["Movie"]).await
}

pub async fn user_usage_stats_tvshows_report(State(state): State<Arc<AppState>>) -> Response {
    user_usage_stats_report(state, &["Series", "Season", "Episode"]).await
}

async fn user_usage_stats_report(state: Arc<AppState>, item_types: &[&str]) -> Response {
    match play_activity_rows(&state.db, item_types, None, None).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_user_activity(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    user_usage_stats_hourly(state, query).await
}

pub async fn user_usage_stats_hourly_report(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    user_usage_stats_hourly(state, query).await
}

async fn user_usage_stats_hourly(state: Arc<AppState>, query: HashMap<String, String>) -> Response {
    let Some(range) = usage_stats_query_range(&query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_types = usage_stats_filter_types(query_value(&query, "Filter"));
    match play_activity_rows(&state.db, &item_types, None, range).await {
        Ok(rows) => {
            let items = usage_stats_hourly_items(&rows);
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_duration_histogram_report(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(range) = usage_stats_query_range(&query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_types = usage_stats_filter_types(query_value(&query, "Filter"));
    match play_activity_rows(&state.db, &item_types, None, range).await {
        Ok(rows) => {
            let items = usage_stats_duration_histogram_items(&rows);
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_breakdown_report(
    State(state): State<Arc<AppState>>,
    Path(breakdown_type): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(range) = usage_stats_query_range(&query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match play_activity_rows(&state.db, &[], None, range).await {
        Ok(rows) => match usage_stats_breakdown_items(&rows, &breakdown_type) {
            Some(items) => {
                Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
            }
            None => StatusCode::BAD_REQUEST.into_response(),
        },
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_user_playlist(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(user_id) = query_value(&query, "user_id")
        .or_else(|| query_value(&query, "UserId"))
        .or_else(|| query_value(&query, "UserID"))
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_types = usage_stats_filter_types(query_value(&query, "Filter"));
    let Some(range) = usage_stats_query_range(&query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match play_activity_rows(&state.db, &item_types, Some(&user_id), range).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_user_date_items(
    State(state): State<Arc<AppState>>,
    Path((user_id, date)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if user_id.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(day) = usage_stats_day_range(&date) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_types = usage_stats_filter_types(query_value(&query, "Filter"));
    match play_activity_rows(&state.db, &item_types, Some(user_id.trim()), Some(day)).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_save_backup(State(state): State<Arc<AppState>>) -> Response {
    match user_usage_stats_save_backup_inner(&state.db).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_load_backup(
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(file_name) = query_value(&query, "backupfile")
        .or_else(|| query_value(&query, "BackupFile"))
        .filter(|value| !value.trim().is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match user_usage_stats_load_backup_inner(&file_name).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) if error.to_string().contains("invalid") => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_import_backup(
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let file_name = query_value(&query, "backupfile")
        .or_else(|| query_value(&query, "BackupFile"))
        .or_else(|| query_value(&query, "FileName"))
        .or_else(|| query_value(&query, "Name"))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("imported-user-usage-{}.json", now_unix()));
    match user_usage_stats_import_backup_inner(&file_name, body).await {
        Ok(value) => Json(value).into_response(),
        Err(error) if error.to_string().contains("invalid") => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error) => internal_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CustomQueryRequest {
    #[serde(default)]
    custom_query_string: String,
    #[serde(default)]
    replace_user_id: bool,
}

pub async fn user_usage_stats_submit_custom_query(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CustomQueryRequest>,
) -> Response {
    match run_user_usage_custom_query(&state.db, &request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) if error.to_string().contains("invalid custom query") => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_usage_stats_user_manage(
    State(state): State<Arc<AppState>>,
    Path((action, id)): Path<(String, String)>,
) -> Response {
    if !matches!(
        action.to_ascii_lowercase().as_str(),
        "get" | "list" | "users"
    ) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if id.trim().is_empty() || id == "0" || id.eq_ignore_ascii_case("all") {
        return user_usage_stats_user_list(State(state)).await;
    }
    match Users::find_by_id(id).one(&state.db).await {
        Ok(Some(user)) => Json(usage_user_entry(&user)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

type Response = axum::response::Response;

pub(crate) async fn app_setting(db: &DatabaseConnection, key: &str, default: &str) -> String {
    crate::db::settings::get_non_empty_or_default(db, key, default).await
}

pub(crate) async fn server_config_json(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "server_config", "{}").await)
        .unwrap_or_else(|_| json!({}))
}

pub(crate) async fn app_setting_bool(db: &DatabaseConnection, key: &str, default: bool) -> bool {
    crate::db::settings::get_bool(db, key, default).await
}

pub(super) async fn set_app_setting(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    crate::db::settings::set(db, key, value).await
}

pub(super) async fn first_admin_user(
    db: &DatabaseConnection,
) -> anyhow::Result<Option<(String, String)>> {
    let Some(model) = Users::find()
        .filter(users::Column::IsAdmin.eq(1_i64))
        .order_by_asc(users::Column::CreatedAt)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((model.id, model.username)))
}

pub async fn scheduled_tasks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(vec![
        scan_library_task_for_state(&state).await,
        chapter_images_task_for_state(&state).await,
    ])
}

pub async fn scheduled_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    Json(scheduled_task_for_state(&state, &task_id).await).into_response()
}

pub async fn scheduled_task_triggers(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    Json(scheduled_task_triggers_value(&state.db, &task_id).await).into_response()
}

pub async fn update_scheduled_task_triggers(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    Json(triggers): Json<JsonValue>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let triggers = match normalize_scheduled_task_triggers(triggers) {
        Ok(triggers) => triggers,
        Err(error) => return validation_error_response(error),
    };

    let key = format!("ScheduledTask.{task_id}.Triggers");
    match set_app_setting(&state.db, &key, &triggers.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn start_scheduled_task(
    state: State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match task_id.as_str() {
        "scan-library" => crate::jellyfin::items::scan_handler(state).await,
        "RefreshChapterImages" => start_chapter_images_task(state).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn stop_scheduled_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    if task_id == "RefreshChapterImages" {
        if let Some(cancel) = state.chapter_image_task_cancel.lock().await.as_ref() {
            let _ = cancel.send(true);
        }
        return StatusCode::NO_CONTENT.into_response();
    }

    let now = now_unix();
    upsert_task_result(&state, &task_id, "Cancelled", now, now, None).await;
    StatusCode::NO_CONTENT.into_response()
}

fn is_known_scheduled_task(task_id: &str) -> bool {
    matches!(task_id, "scan-library" | "RefreshChapterImages")
}

async fn scheduled_task_for_state(state: &AppState, task_id: &str) -> JsonValue {
    match task_id {
        "RefreshChapterImages" => chapter_images_task_for_state(state).await,
        _ => scan_library_task_for_state(state).await,
    }
}

async fn start_chapter_images_task(State(state): State<Arc<AppState>>) -> Response {
    if !queue_chapter_images_task(state, None).await {
        return Json(json!({ "Running": true, "AlreadyRunning": true })).into_response();
    }
    Json(json!({ "Running": true })).into_response()
}

async fn queue_chapter_images_task(state: Arc<AppState>, max_runtime: Option<Duration>) -> bool {
    let (cancel, mut cancellation) = tokio::sync::watch::channel(false);
    {
        let mut current = state.chapter_image_task_cancel.lock().await;
        if current.is_some() {
            return false;
        }
        *current = Some(cancel);
    }

    tokio::spawn(async move {
        let start = now_unix();
        upsert_task_result(
            &state,
            "RefreshChapterImages",
            "Running",
            start,
            start,
            Some("Chapter image refresh is running"),
        )
        .await;
        let refresh =
            crate::chapters::refresh_all_chapter_images(&state.db, &state.sa_config.ffmpeg_path);
        tokio::pin!(refresh);
        let result = match max_runtime {
            Some(limit) => tokio::select! {
                result = &mut refresh => ChapterImageTaskOutcome::Finished(result),
                _ = cancellation.changed() => ChapterImageTaskOutcome::Cancelled,
                _ = tokio::time::sleep(limit) => ChapterImageTaskOutcome::TimedOut(limit),
            },
            None => tokio::select! {
                result = &mut refresh => ChapterImageTaskOutcome::Finished(result),
                _ = cancellation.changed() => ChapterImageTaskOutcome::Cancelled,
            },
        };
        let end = now_unix();
        let (status, message) = match result {
            ChapterImageTaskOutcome::Finished(Ok(stats)) if stats.failed == 0 => (
                "Completed",
                format!("Refreshed chapter images for {} item(s)", stats.processed),
            ),
            ChapterImageTaskOutcome::Finished(Ok(stats)) => (
                "Failed",
                format!(
                    "Refreshed {} item(s); {} item(s) failed",
                    stats.processed, stats.failed
                ),
            ),
            ChapterImageTaskOutcome::Finished(Err(error)) => ("Failed", format!("{error:#}")),
            ChapterImageTaskOutcome::Cancelled => (
                "Cancelled",
                "Chapter image refresh was cancelled".to_string(),
            ),
            ChapterImageTaskOutcome::TimedOut(limit) => (
                "Cancelled",
                format!(
                    "Chapter image refresh exceeded its maximum runtime of {} seconds",
                    limit.as_secs()
                ),
            ),
        };
        upsert_task_result(
            &state,
            "RefreshChapterImages",
            status,
            start,
            end,
            Some(&message),
        )
        .await;
        state.chapter_image_task_cancel.lock().await.take();
        log_activity(
            &state,
            "Refresh chapter images",
            "ChapterImages",
            None,
            None,
        )
        .await;
    });
    true
}

enum ChapterImageTaskOutcome {
    Finished(anyhow::Result<crate::chapters::ChapterImageRefreshStats>),
    Cancelled,
    TimedOut(Duration),
}

pub async fn repositories(State(state): State<Arc<AppState>>) -> Response {
    Json(plugin_repositories(&state.db).await).into_response()
}

pub async fn update_repositories(
    State(state): State<Arc<AppState>>,
    Json(repositories): Json<JsonValue>,
) -> Response {
    let repositories = match normalize_plugin_repositories(repositories) {
        Ok(repositories) => repositories,
        Err(error) => return validation_error_response(error),
    };

    match set_app_setting(&state.db, "PluginRepositories", &repositories.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn packages() -> Response {
    Json(package_list()).into_response()
}

pub async fn package_by_name(Path(name): Path<String>) -> Response {
    let target = name.trim();
    let Some(package) = package_list().into_iter().find(|package| {
        package
            .get("name")
            .and_then(JsonValue::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(target))
            || package
                .get("guid")
                .and_then(JsonValue::as_str)
                .is_some_and(|guid| guid.eq_ignore_ascii_case(target))
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Json(package).into_response()
}

pub async fn package_updates() -> Response {
    Json(package_update_list()).into_response()
}

pub async fn package_install_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Package installation is not available" })),
    )
        .into_response()
}

pub async fn party_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Party playback is not available" })),
    )
        .into_response()
}

pub async fn sync_play_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "SyncPlay is not available" })),
    )
        .into_response()
}

pub async fn sync_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Offline sync is not available" })),
    )
        .into_response()
}

pub async fn plugins() -> Response {
    Json(plugin_list()).into_response()
}

pub async fn channels() -> Response {
    Json(empty_query_result()).into_response()
}

pub async fn channel_items() -> Response {
    Json(empty_query_result()).into_response()
}

pub async fn live_tv_recording_folders() -> Response {
    Json(empty_query_result()).into_response()
}

pub async fn channel_features() -> Response {
    Json(channel_features_value()).into_response()
}

pub async fn all_channel_features() -> Response {
    Json(Vec::<JsonValue>::new()).into_response()
}

pub async fn plugin_not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

pub async fn scan_library_task(db: &DatabaseConnection) -> JsonValue {
    scan_library_task_value(db, false).await
}

async fn scan_library_task_for_state(state: &AppState) -> JsonValue {
    let is_running = state.scan_lock.try_lock().is_err();
    scan_library_task_value(&state.db, is_running).await
}

async fn chapter_images_task_for_state(state: &AppState) -> JsonValue {
    let result = last_task_result(&state.db, "RefreshChapterImages").await;
    let is_running = state.chapter_image_task_cancel.lock().await.is_some();
    json!({
        "Name": "Extract chapter images",
        "State": if is_running { "Running" } else { "Idle" },
        "Id": "RefreshChapterImages",
        "Key": "RefreshChapterImages",
        "Description": "Extracts chapter images for eligible video libraries.",
        "Category": "Library",
        "IsHidden": false,
        "LastExecutionResult": result,
        "Triggers": chapter_images_triggers(&state.db).await,
    })
}

async fn scan_library_task_value(db: &DatabaseConnection, is_running: bool) -> JsonValue {
    let scan_result = last_task_result(db, "scan-library").await;
    json!({
        "Name": "Scan media library",
        "State": if is_running { "Running" } else { "Idle" },
        "Id": "scan-library",
        "Key": "scan-library",
        "Description": "Scans configured media library paths for new and updated media files.",
        "Category": "Library",
        "IsHidden": false,
        "LastExecutionResult": scan_result,
        "Triggers": scan_library_triggers(db).await,
    })
}

async fn scan_library_triggers(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "ScheduledTask.scan-library.Triggers", "").await)
        .ok()
        .and_then(|value| normalize_scheduled_task_triggers(value).ok())
        .unwrap_or_else(default_scan_library_triggers)
}

async fn scheduled_task_triggers_value(db: &DatabaseConnection, task_id: &str) -> JsonValue {
    match task_id {
        "RefreshChapterImages" => chapter_images_triggers(db).await,
        _ => scan_library_triggers(db).await,
    }
}

async fn chapter_images_triggers(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "ScheduledTask.RefreshChapterImages.Triggers", "").await)
        .ok()
        .and_then(|value| normalize_scheduled_task_triggers(value).ok())
        .unwrap_or_else(|| {
            json!([{
                "Type": "DailyTrigger",
                "TimeOfDayTicks": 2 * 60 * 60 * 10_000_000_i64,
                "MaxRuntimeTicks": 4 * 60 * 60 * 10_000_000_i64,
            }])
        })
}

const SCHEDULE_RELOAD_INTERVAL: Duration = Duration::from_secs(60);
const TICKS_PER_DAY: i64 = 24 * 60 * 60 * TICKS_PER_SECOND;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DailyTaskSchedule {
    delay: Duration,
    max_runtime: Option<Duration>,
}

pub async fn start_scheduled_task_scheduler(state: Arc<AppState>) {
    recover_stale_chapter_image_task(&state).await;
    tokio::spawn(async move {
        loop {
            let triggers = chapter_images_triggers(&state.db).await;
            let Some(schedule) =
                next_daily_task_schedule(&triggers, chrono::Local::now().naive_local())
            else {
                tokio::time::sleep(SCHEDULE_RELOAD_INTERVAL).await;
                continue;
            };

            let wait = schedule.delay.min(SCHEDULE_RELOAD_INTERVAL);
            tokio::time::sleep(wait).await;
            if wait < schedule.delay {
                continue;
            }

            if !queue_chapter_images_task(state.clone(), schedule.max_runtime).await {
                tracing::info!(
                    "scheduled chapter image refresh skipped because the task is already running"
                );
            }
            // Move beyond an exact wall-clock match before calculating tomorrow's trigger.
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

async fn recover_stale_chapter_image_task(state: &AppState) {
    let was_running = last_task_result(&state.db, "RefreshChapterImages")
        .await
        .and_then(|result| {
            result
                .get("Status")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("Running");
    if was_running {
        let now = now_unix();
        upsert_task_result(
            state,
            "RefreshChapterImages",
            "Cancelled",
            now,
            now,
            Some("Chapter image refresh was interrupted by a server restart"),
        )
        .await;
    }
}

fn next_daily_task_schedule(
    triggers: &JsonValue,
    now: chrono::NaiveDateTime,
) -> Option<DailyTaskSchedule> {
    triggers
        .as_array()?
        .iter()
        .filter(|trigger| trigger.get("Type").and_then(JsonValue::as_str) == Some("DailyTrigger"))
        .filter_map(|trigger| {
            let time_of_day_ticks = trigger.get("TimeOfDayTicks")?.as_i64()?;
            let due = next_daily_trigger(now, time_of_day_ticks)?;
            let delay = (due - now).to_std().ok()?;
            let max_runtime = trigger
                .get("MaxRuntimeTicks")
                .and_then(JsonValue::as_i64)
                .and_then(ticks_to_duration);
            Some(DailyTaskSchedule { delay, max_runtime })
        })
        .min_by_key(|schedule| schedule.delay)
}

fn next_daily_trigger(
    now: chrono::NaiveDateTime,
    time_of_day_ticks: i64,
) -> Option<chrono::NaiveDateTime> {
    if !(0..TICKS_PER_DAY).contains(&time_of_day_ticks) {
        return None;
    }
    let whole_seconds = time_of_day_ticks / TICKS_PER_SECOND;
    let nanoseconds = (time_of_day_ticks % TICKS_PER_SECOND) * 100;
    let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        u32::try_from(whole_seconds).ok()?,
        u32::try_from(nanoseconds).ok()?,
    )?;
    let date = if now.time() > time {
        now.date().succ_opt()?
    } else {
        now.date()
    };
    Some(date.and_time(time))
}

fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    let nanoseconds = u64::try_from(ticks).ok()?.checked_mul(100)?;
    Some(Duration::from_nanos(nanoseconds))
}

fn default_scan_library_triggers() -> JsonValue {
    json!([{ "Type": "StartupTrigger" }])
}

fn normalize_scheduled_task_triggers(
    value: JsonValue,
) -> Result<JsonValue, (StatusCode, &'static str)> {
    let Some(triggers) = value.as_array() else {
        return Err((StatusCode::BAD_REQUEST, "Triggers must be an array"));
    };
    if triggers.len() > MAX_SCHEDULED_TASK_TRIGGERS {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many triggers"));
    }

    let mut normalized = Vec::with_capacity(triggers.len());
    for trigger in triggers {
        let Some(trigger) = trigger.as_object() else {
            return Err((StatusCode::BAD_REQUEST, "Trigger entries must be objects"));
        };
        let trigger_type = trigger_string_field(trigger, &["Type", "type"])
            .and_then(|value| canonical_trigger_type(&value))
            .or_else(|| inferred_trigger_type(trigger))
            .ok_or((StatusCode::BAD_REQUEST, "Invalid trigger type"))?;

        let mut object = serde_json::Map::new();
        object.insert("Type".to_string(), json!(trigger_type));
        if let Some(value) = trigger_ticks_field(trigger, &["TimeOfDayTicks", "timeOfDayTicks"])? {
            object.insert("TimeOfDayTicks".to_string(), json!(value));
        }
        if let Some(value) = trigger_ticks_field(trigger, &["IntervalTicks", "intervalTicks"])? {
            object.insert("IntervalTicks".to_string(), json!(value));
        }
        if let Some(value) = trigger_ticks_field(trigger, &["MaxRuntimeTicks", "maxRuntimeTicks"])?
        {
            object.insert("MaxRuntimeTicks".to_string(), json!(value));
        }
        if let Some(value) = trigger_string_field(trigger, &["DayOfWeek", "dayOfWeek"]) {
            object.insert(
                "DayOfWeek".to_string(),
                json!(canonical_day_of_week(&value)?),
            );
        }
        if let Some(value) = trigger_string_field(trigger, &["SystemEvent", "systemEvent"]) {
            object.insert(
                "SystemEvent".to_string(),
                json!(canonical_system_event(&value)?),
            );
        }
        normalized.push(JsonValue::Object(object));
    }

    let value = JsonValue::Array(normalized);
    if value.to_string().len() > MAX_SCHEDULED_TASK_TRIGGERS_JSON_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Triggers are too large"));
    }
    Ok(value)
}

fn trigger_string_field(
    object: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(JsonValue::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn trigger_ticks_field(
    object: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
) -> Result<Option<i64>, (StatusCode, &'static str)> {
    let Some(value) = keys.iter().find_map(|key| object.get(*key)) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_i64() {
        return (value >= 0)
            .then_some(Some(value))
            .ok_or((StatusCode::BAD_REQUEST, "Trigger ticks must be positive"));
    }
    if let Some(value) = value.as_u64().and_then(|value| i64::try_from(value).ok()) {
        return Ok(Some(value));
    }
    if let Some(value) = value
        .as_str()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|value| i64::try_from(value).ok())
    {
        return Ok(Some(value));
    }
    Err((StatusCode::BAD_REQUEST, "Invalid trigger ticks"))
}

fn canonical_trigger_type(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dailytrigger" => Some("DailyTrigger"),
        "weeklytrigger" => Some("WeeklyTrigger"),
        "intervaltrigger" => Some("IntervalTrigger"),
        "startuptrigger" => Some("StartupTrigger"),
        _ => None,
    }
}

fn inferred_trigger_type(trigger: &serde_json::Map<String, JsonValue>) -> Option<&'static str> {
    if trigger
        .keys()
        .any(|key| key.eq_ignore_ascii_case("IntervalTicks"))
    {
        Some("IntervalTrigger")
    } else if trigger
        .keys()
        .any(|key| key.eq_ignore_ascii_case("TimeOfDayTicks"))
    {
        Some(
            if trigger
                .keys()
                .any(|key| key.eq_ignore_ascii_case("DayOfWeek"))
            {
                "WeeklyTrigger"
            } else {
                "DailyTrigger"
            },
        )
    } else {
        None
    }
}

fn canonical_day_of_week(value: &str) -> Result<&'static str, (StatusCode, &'static str)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "sunday" => Ok("Sunday"),
        "monday" => Ok("Monday"),
        "tuesday" => Ok("Tuesday"),
        "wednesday" => Ok("Wednesday"),
        "thursday" => Ok("Thursday"),
        "friday" => Ok("Friday"),
        "saturday" => Ok("Saturday"),
        _ => Err((StatusCode::BAD_REQUEST, "Invalid trigger day")),
    }
}

fn canonical_system_event(value: &str) -> Result<&'static str, (StatusCode, &'static str)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "wakefromsleep" => Ok("WakeFromSleep"),
        "displayconfigurationchange" => Ok("DisplayConfigurationChange"),
        _ => Err((StatusCode::BAD_REQUEST, "Invalid trigger system event")),
    }
}

async fn plugin_repositories(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "PluginRepositories", "").await)
        .ok()
        .and_then(|value| normalize_plugin_repositories(value).ok())
        .unwrap_or_else(default_plugin_repositories)
}

fn default_plugin_repositories() -> JsonValue {
    JsonValue::Array(Vec::new())
}

fn normalize_plugin_repositories(
    value: JsonValue,
) -> Result<JsonValue, (StatusCode, &'static str)> {
    let Some(repositories) = value.as_array() else {
        return Err((StatusCode::BAD_REQUEST, "Repositories must be an array"));
    };
    if repositories.len() > MAX_PLUGIN_REPOSITORIES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many repositories"));
    }

    let mut normalized = Vec::with_capacity(repositories.len());
    for repository in repositories {
        let Some(repository) = repository.as_object() else {
            return Err((
                StatusCode::BAD_REQUEST,
                "Repository entries must be objects",
            ));
        };
        let name = repository_string_field(repository, &["Name", "name"])
            .unwrap_or_default()
            .trim()
            .to_string();
        validate_repository_text(&name, MAX_PLUGIN_REPOSITORY_NAME_LEN, "Repository name")?;

        let url = repository_string_field(repository, &["Url", "URL", "url"])
            .unwrap_or_default()
            .trim()
            .to_string();
        validate_repository_text(&url, MAX_PLUGIN_REPOSITORY_URL_LEN, "Repository URL")?;
        if !url.is_empty() && !is_supported_repository_url(&url) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Repository URL must use http or https",
            ));
        }

        normalized.push(json!({
            "Name": name,
            "Url": url,
            "Enabled": repository_bool_field(repository, &["Enabled", "enabled"]).unwrap_or(false)
        }));
    }

    Ok(JsonValue::Array(normalized))
}

fn repository_string_field(
    object: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn repository_bool_field(
    object: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
) -> Option<bool> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| {
            value.as_bool().or_else(|| {
                value
                    .as_str()
                    .and_then(|text| match text.trim().to_ascii_lowercase().as_str() {
                        "1" | "true" | "yes" => Some(true),
                        "0" | "false" | "no" => Some(false),
                        _ => None,
                    })
            })
        })
}

fn validate_repository_text(
    value: &str,
    max_len: usize,
    field: &'static str,
) -> Result<(), (StatusCode, &'static str)> {
    if value.chars().count() > max_len {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, field));
    }
    if value.contains('\0') || value.chars().any(char::is_control) {
        return Err((StatusCode::BAD_REQUEST, field));
    }
    Ok(())
}

fn is_supported_repository_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn validation_error_response(error: (StatusCode, &'static str)) -> Response {
    (
        error.0,
        Json(json!({
            "Error": error.1
        })),
    )
        .into_response()
}

fn package_list() -> Vec<JsonValue> {
    Vec::new()
}

fn package_update_list() -> Vec<JsonValue> {
    Vec::new()
}

fn plugin_list() -> Vec<JsonValue> {
    Vec::new()
}

fn empty_query_result() -> JsonValue {
    json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 })
}

fn channel_features_value() -> JsonValue {
    json!({
        "Name": "",
        "Id": "",
        "CanSearch": false,
        "MediaTypes": [],
        "ContentTypes": [],
        "DefaultSortFields": [],
        "SupportsSortOrderToggle": false,
        "SupportsLatestMedia": false,
        "SupportsContentDownloading": false,
        "AutoRefreshLevels": 0,
    })
}

async fn branding_options(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "BrandingOptions", "").await)
        .ok()
        .and_then(|value| normalize_branding_options(value).ok())
        .unwrap_or_else(default_branding_options)
}

fn branding_splashscreen_path() -> PathBuf {
    PathBuf::from(BRANDING_SPLASHSCREEN_PATH)
}

fn default_branding_options() -> JsonValue {
    json!({
        "LoginDisclaimer": "",
        "CustomCss": "",
        "SplashscreenEnabled": false
    })
}

fn normalize_branding_options(value: JsonValue) -> Result<JsonValue, (StatusCode, &'static str)> {
    let Some(object) = value.as_object() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Branding options must be an object",
        ));
    };
    let login_disclaimer = branding_string_field(
        object,
        "LoginDisclaimer",
        MAX_BRANDING_LOGIN_DISCLAIMER_BYTES,
    )?;
    let custom_css = branding_string_field(object, "CustomCss", MAX_BRANDING_CUSTOM_CSS_BYTES)?;

    Ok(json!({
        "LoginDisclaimer": login_disclaimer,
        "CustomCss": custom_css,
        "SplashscreenEnabled": object
            .get("SplashscreenEnabled")
            .and_then(json_bool_value)
            .unwrap_or(false),
    }))
}

fn branding_string_field(
    object: &serde_json::Map<String, JsonValue>,
    key: &'static str,
    max_bytes: usize,
) -> Result<String, (StatusCode, &'static str)> {
    let Some(value) = object.get(key) else {
        return Ok(String::new());
    };
    let Some(value) = value.as_str() else {
        return Err((StatusCode::BAD_REQUEST, key));
    };
    if value.len() > max_bytes {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, key));
    }
    if value.contains('\0')
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err((StatusCode::BAD_REQUEST, key));
    }
    Ok(value.to_string())
}

fn json_bool_value(value: &JsonValue) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .and_then(|text| match text.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => Some(true),
                "0" | "false" | "no" => Some(false),
                _ => None,
            })
    })
}

fn allowed_branding_image_content_type(content_type: &str) -> bool {
    matches!(
        content_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp"
    )
}

fn web_strings_value() -> JsonValue {
    json!({
        "AppName": "Jellyfin",
        "ButtonCancel": "Cancel",
        "ButtonDelete": "Delete",
        "ButtonSave": "Save",
        "Dashboard": "Dashboard",
        "HeaderLibraries": "Libraries",
        "HeaderLogin": "Login",
        "HeaderMetadata": "Metadata",
        "HeaderSettings": "Settings",
        "HeaderUsers": "Users",
        "LabelPassword": "Password",
        "LabelServerName": "Server name",
        "LabelUsername": "Username",
        "LoginDisclaimer": "",
        "MessageNoItemsAvailable": "No items available",
        "Name": "Name",
        "Password": "Password",
        "Settings": "Settings",
        "Users": "Users"
    })
}

fn usage_user_entry(user: &users::Model) -> JsonValue {
    json!({
        "Id": user.id,
        "Name": user.username,
        "UserName": user.username,
        "IsAdministrator": user.is_admin != 0,
        "IsDisabled": user.is_disabled != 0,
        "LastLoginDate": user.last_login_at.map(unix_to_jellyfin_date),
    })
}

fn items_access_value(user_id: &str, libraries: &[JsonValue]) -> JsonValue {
    let items = libraries
        .iter()
        .map(|library| {
            json!({
                "UserId": user_id,
                "ItemId": library["Id"],
                "ItemName": library["Name"],
                "CollectionType": library["CollectionType"],
                "HasAccess": true,
                "CanPlay": true,
                "CanDownload": true
            })
        })
        .collect::<Vec<_>>();
    json!({
        "UserId": user_id,
        "Items": items,
        "TotalRecordCount": items.len()
    })
}

fn user_view_grouping_options_value() -> JsonValue {
    json!([
        { "Name": "None", "Id": "none" },
        { "Name": "Folders", "Id": "folders" },
        { "Name": "Collections", "Id": "collections" }
    ])
}

fn image_by_name_info(name: &str, theme: &str, context: &str) -> JsonValue {
    json!({
        "Name": name,
        "Theme": theme,
        "Context": context,
        "FileLength": 68,
        "Format": "png"
    })
}

async fn game_system_summary_rows(db: &DatabaseConnection) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            "SELECT COALESCE(NULLIF(container, ''), 'unknown') AS system, COUNT(*) AS game_count FROM media_items WHERE item_type = 'Game' GROUP BY COALESCE(NULLIF(container, ''), 'unknown') ORDER BY system",
            vec![],
        ))
        .await
        .context("failed to load game system summaries")?;
    rows.iter()
        .map(|row| {
            let name = row.get_str("system")?;
            let count = row.get_i64("game_count").unwrap_or_default();
            Ok(json!({
                "Name": name,
                "DisplayName": game_system_display_name(&name),
                "GameCount": count,
                "GameFileExtensions": if name == "unknown" { json!([]) } else { json!([format!(".{name}")]) },
                "ClientInstalledGameCount": 0
            }))
        })
        .collect()
}

fn game_system_display_name(name: &str) -> String {
    if name == "unknown" {
        "Unknown".to_string()
    } else {
        name.to_ascii_uppercase()
    }
}

fn usage_stats_session_entry(session: &PlaybackSession) -> JsonValue {
    json!({
        "Id": session.id,
        "UserId": session.user_id,
        "Client": session.client,
        "DeviceName": session.device_name,
        "DeviceId": session.device_id,
        "ApplicationVersion": session.application_version,
        "NowPlayingItemId": session.item_id,
        "NowPlayingItemName": session.item_name,
        "IsActive": session.is_active,
        "LastActivityDate": session.last_activity_date,
        "PlayState": session.play_state,
    })
}

fn current_process_usage() -> JsonValue {
    let exe = std::env::current_exe().ok();
    let metadata = exe.as_ref().and_then(|path| path.metadata().ok());
    json!({
        "Id": std::process::id(),
        "Name": env!("CARGO_PKG_NAME"),
        "Path": exe.as_ref().and_then(|path| path.to_str()).unwrap_or_default(),
        "WorkingSetBytes": metadata.as_ref().map(|meta| meta.len()).unwrap_or_default(),
        "ThreadCount": crate::db::online_cpu_count(),
        "Date": unix_to_jellyfin_date(now_unix()),
    })
}

async fn play_activity_rows(
    db: &DatabaseConnection,
    item_types: &[&str],
    user_id: Option<&str>,
    day: Option<(i64, i64)>,
) -> anyhow::Result<Vec<JsonValue>> {
    let mut values: Vec<sea_orm::Value> = Vec::new();
    let type_filter = if item_types.is_empty() {
        String::new()
    } else {
        for item_type in item_types {
            values.push((*item_type).into());
        }
        format!(
            " AND mi.item_type IN ({})",
            item_types.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        )
    };
    let user_filter = if let Some(user_id) = user_id {
        values.push(user_id.to_string().into());
        " AND pwd.user_id = ?"
    } else {
        ""
    };
    let day_filter = if let Some((start, end)) = day {
        values.push(unix_day(start).into());
        values.push(unix_day(end.saturating_sub(1)).into());
        " AND pwd.day >= ? AND pwd.day <= ?"
    } else {
        ""
    };
    let sql = format!(
        r#"SELECT pwd.user_id, users.username, pwd.item_id, mi.title, mi.item_type, mi.runtime_ticks,
                  SUM(pwd.play_count)::BIGINT AS play_count,
                  SUM(pwd.watch_seconds)::BIGINT AS watch_seconds,
                  MAX(pwd.last_played_at) AS last_played_at
           FROM playback_watch_days pwd
           JOIN media_items mi ON mi.id = pwd.item_id
           LEFT JOIN users ON users.id = pwd.user_id
           WHERE (COALESCE(pwd.play_count, 0) > 0 OR COALESCE(pwd.watch_seconds, 0) > 0){type_filter}{user_filter}{day_filter}
           GROUP BY pwd.user_id, users.username, pwd.item_id, mi.title, mi.item_type, mi.runtime_ticks
           ORDER BY MAX(pwd.last_played_at) DESC
           LIMIT 500"#
    );
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await
        .context("failed to load playback activity")?;
    rows.iter()
        .map(|row| {
            let item_id = row.get_str("item_id")?;
            let user_id = row.get_str("user_id")?;
            let username = row.get_opt_str("username")?.unwrap_or_default();
            let item_name = row.get_str("title")?;
            let item_type = row.get_str("item_type")?;
            let runtime_ticks = row.get_opt_i64("runtime_ticks")?;
            let play_count = row.get_i64("play_count")?;
            let watch_seconds = row.get_i64("watch_seconds")?;
            let watch_ticks = watch_seconds.saturating_mul(TICKS_PER_SECOND);
            let last_played_at = row.get_opt_i64("last_played_at")?;
            Ok(json!({
                "UserId": user_id,
                "UserName": username,
                "ItemId": item_id,
                "ItemName": item_name,
                "Name": item_name,
                "ItemType": item_type,
                "RunTimeTicks": runtime_ticks,
                "PlayCount": play_count,
                "WatchSeconds": watch_seconds,
                "WatchMinutes": watch_seconds / 60,
                "DurationTicks": watch_ticks,
                "PlaybackPositionTicks": 0,
                "LastPlayedAt": last_played_at,
                "Date": last_played_at.map(unix_to_jellyfin_date),
                "LastPlayedDate": last_played_at.map(unix_to_jellyfin_date),
            }))
        })
        .collect()
}

async fn playback_stats_overview(db: &DatabaseConnection, days: i64) -> anyhow::Result<JsonValue> {
    let now = now_unix();
    let today_start = unix_day_start(now);
    let start_day = unix_day(today_start.saturating_sub((days - 1) * 86_400));
    let today = unix_day(today_start);

    let totals = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT COALESCE(SUM(watch_seconds), 0)::BIGINT AS watch_seconds,
                      COALESCE(SUM(play_count), 0)::BIGINT AS play_count,
                      COUNT(DISTINCT user_id) AS user_count,
                      COUNT(DISTINCT item_id) AS item_count
               FROM playback_watch_days"#,
            vec![],
        ))
        .await?
        .map(|row| {
            (
                row.get_i64("watch_seconds").unwrap_or_default(),
                row.get_i64("play_count").unwrap_or_default(),
                row.get_i64("user_count").unwrap_or_default(),
                row.get_i64("item_count").unwrap_or_default(),
            )
        })
        .unwrap_or_default();

    let today_watch_seconds = db
        .query_one_raw(crate::db::helpers::pg_statement(
            "SELECT COALESCE(SUM(watch_seconds), 0)::BIGINT AS watch_seconds FROM playback_watch_days WHERE day = ?",
            vec![today.clone().into()],
        ))
        .await?
        .and_then(|row| row.get_i64("watch_seconds").ok())
        .unwrap_or_default();

    Ok(json!({
        "TotalWatchSeconds": totals.0,
        "TotalWatchMinutes": totals.0 / 60,
        "TodayWatchSeconds": today_watch_seconds,
        "TodayWatchMinutes": today_watch_seconds / 60,
        "TotalPlayCount": totals.1,
        "UserCount": totals.2,
        "ItemCount": totals.3,
        "Days": days,
        "Daily": playback_stats_daily(db, &start_day, &today, days).await?,
        "Users": playback_stats_top_users(db, &start_day).await?,
        "Items": playback_stats_top_items(db, &start_day).await?,
        "Series": playback_stats_top_series(db, &start_day).await?,
    }))
}

async fn playback_stats_daily(
    db: &DatabaseConnection,
    start_day: &str,
    end_day: &str,
    days: i64,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT day, COALESCE(SUM(watch_seconds), 0)::BIGINT AS watch_seconds,
                      COALESCE(SUM(play_count), 0)::BIGINT AS play_count
               FROM playback_watch_days
               WHERE day >= ? AND day <= ?
               GROUP BY day
               ORDER BY day ASC"#,
            vec![start_day.into(), end_day.into()],
        ))
        .await?;
    let mut by_day: HashMap<String, (i64, i64)> = HashMap::new();
    for row in rows {
        by_day.insert(
            row.get_str("day")?,
            (
                row.get_i64("watch_seconds").unwrap_or_default(),
                row.get_i64("play_count").unwrap_or_default(),
            ),
        );
    }
    let start = usage_stats_day_range(start_day)
        .map(|(start, _)| start)
        .unwrap_or_default();
    Ok((0..days)
        .map(|offset| {
            let day = unix_day(start.saturating_add(offset * 86_400));
            let (watch_seconds, play_count) = by_day.get(&day).copied().unwrap_or_default();
            json!({
                "Date": day,
                "WatchSeconds": watch_seconds,
                "WatchMinutes": watch_seconds / 60,
                "PlayCount": play_count,
            })
        })
        .collect())
}

async fn playback_stats_top_users(
    db: &DatabaseConnection,
    start_day: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT pwd.user_id,
                      COALESCE(NULLIF(users.display_name, ''), NULLIF(users.username, ''), pwd.user_id) AS user_name,
                      COALESCE(SUM(pwd.watch_seconds), 0)::BIGINT AS watch_seconds,
                      COALESCE(SUM(pwd.play_count), 0)::BIGINT AS play_count
               FROM playback_watch_days pwd
               LEFT JOIN users ON users.id = pwd.user_id
               WHERE pwd.day >= ?
               GROUP BY pwd.user_id, users.display_name, users.username
               ORDER BY watch_seconds DESC, play_count DESC
               LIMIT 10"#,
            vec![start_day.into()],
        ))
        .await?;
    rows.iter()
        .map(|row| {
            let watch_seconds = row.get_i64("watch_seconds").unwrap_or_default();
            Ok(json!({
                "UserId": row.get_str("user_id")?,
                "UserName": row.get_str("user_name").unwrap_or_default(),
                "WatchSeconds": watch_seconds,
                "WatchMinutes": watch_seconds / 60,
                "PlayCount": row.get_i64("play_count").unwrap_or_default(),
            }))
        })
        .collect()
}

async fn playback_stats_top_items(
    db: &DatabaseConnection,
    start_day: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"SELECT pwd.item_id, mi.title, mi.item_type,
                          {series_id_sql} AS series_id,
                          {series_name_sql} AS series_name,
                          COALESCE(SUM(pwd.watch_seconds), 0)::BIGINT AS watch_seconds,
                          COALESCE(SUM(pwd.play_count), 0)::BIGINT AS play_count
                   FROM playback_watch_days pwd
                   LEFT JOIN media_items mi ON mi.id = pwd.item_id
                   {series_join_sql}
                   WHERE pwd.day >= ?
                   -- Use select-list ordinals for the derived series fields.  The
                   -- joined media_items table also has a series_name column, so
                   -- GROUP BY series_name is ambiguous in PostgreSQL.
                   GROUP BY pwd.item_id, mi.title, mi.item_type, 4, 5
                   ORDER BY watch_seconds DESC, play_count DESC
                   LIMIT 10"#,
                series_id_sql = playback_series_id_sql(),
                series_name_sql = playback_series_name_sql(),
                series_join_sql = playback_series_join_sql(),
            ),
            vec![start_day.into()],
        ))
        .await?;
    rows.iter()
        .map(|row| {
            let watch_seconds = row.get_i64("watch_seconds").unwrap_or_default();
            Ok(json!({
                "ItemId": row.get_str("item_id")?,
                "ItemName": row.get_opt_str("title")?.unwrap_or_else(|| row.get_str("item_id").unwrap_or_default()),
                "ItemType": row.get_opt_str("item_type")?.unwrap_or_default(),
                "SeriesId": row.get_opt_str("series_id")?,
                "SeriesName": row.get_opt_str("series_name")?,
                "WatchSeconds": watch_seconds,
                "WatchMinutes": watch_seconds / 60,
                "PlayCount": row.get_i64("play_count").unwrap_or_default(),
            }))
        })
        .collect()
}

async fn playback_stats_top_series(
    db: &DatabaseConnection,
    start_day: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"SELECT series_id, series_name,
                          COALESCE(SUM(watch_seconds), 0)::BIGINT AS watch_seconds,
                          COALESCE(SUM(play_count), 0)::BIGINT AS play_count,
                          COUNT(DISTINCT item_id) AS item_count
                   FROM (
                     SELECT pwd.item_id,
                            {series_id_sql} AS series_id,
                            {series_name_sql} AS series_name,
                            pwd.watch_seconds,
                            pwd.play_count
                     FROM playback_watch_days pwd
                     LEFT JOIN media_items mi ON mi.id = pwd.item_id
                     {series_join_sql}
                     WHERE pwd.day >= ?
                   ) stats
                   WHERE series_id IS NOT NULL
                   GROUP BY series_id, series_name
                   ORDER BY watch_seconds DESC, play_count DESC
                   LIMIT 10"#,
                series_id_sql = playback_series_id_sql(),
                series_name_sql = playback_series_name_sql(),
                series_join_sql = playback_series_join_sql(),
            ),
            vec![start_day.into()],
        ))
        .await?;
    rows.iter()
        .map(|row| {
            let watch_seconds = row.get_i64("watch_seconds").unwrap_or_default();
            Ok(json!({
                "SeriesId": row.get_str("series_id")?,
                "SeriesName": row.get_opt_str("series_name")?.unwrap_or_else(|| row.get_str("series_id").unwrap_or_default()),
                "WatchSeconds": watch_seconds,
                "WatchMinutes": watch_seconds / 60,
                "PlayCount": row.get_i64("play_count").unwrap_or_default(),
                "ItemCount": row.get_i64("item_count").unwrap_or_default(),
            }))
        })
        .collect()
}

fn playback_series_join_sql() -> &'static str {
    r#"LEFT JOIN media_items episode_season ON episode_season.id = mi.parent_id AND episode_season.item_type = 'Season'
       LEFT JOIN media_items episode_series ON episode_series.id = episode_season.parent_id AND episode_series.item_type = 'Series'
       LEFT JOIN media_items direct_series ON direct_series.id = mi.parent_id AND direct_series.item_type = 'Series'
       LEFT JOIN media_items video_parent ON video_parent.id = mi.parent_id
       LEFT JOIN media_items video_season ON video_season.id = video_parent.parent_id AND video_season.item_type = 'Season'
       LEFT JOIN media_items video_series ON video_series.id = video_season.parent_id AND video_series.item_type = 'Series'"#
}

fn playback_series_id_sql() -> &'static str {
    r#"CASE
         WHEN mi.item_type = 'Series' THEN mi.id
         WHEN mi.item_type = 'Season' THEN direct_series.id
         WHEN mi.item_type = 'Episode' THEN COALESCE(episode_series.id, direct_series.id)
         WHEN mi.item_type = 'Video' AND video_parent.item_type = 'Episode' THEN video_series.id
         ELSE NULL
       END"#
}

fn playback_series_name_sql() -> &'static str {
    r#"CASE
         WHEN mi.item_type = 'Series' THEN mi.title
         WHEN mi.item_type = 'Season' THEN direct_series.title
         WHEN mi.item_type = 'Episode' THEN COALESCE(episode_series.title, direct_series.title)
         WHEN mi.item_type = 'Video' AND video_parent.item_type = 'Episode' THEN video_series.title
         ELSE NULL
       END"#
}

async fn run_user_usage_custom_query(
    db: &DatabaseConnection,
    request: &CustomQueryRequest,
) -> anyhow::Result<JsonValue> {
    let sql = validate_user_usage_custom_query(&request.custom_query_string)?;
    let wrapped = format!(
        "SELECT row_to_json(custom_query)::text AS value FROM ({sql}) AS custom_query LIMIT 500"
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &wrapped,
            Vec::<sea_orm::Value>::new(),
        ))
        .await
        .context("failed to run custom usage query")?;
    let items = rows
        .iter()
        .map(|row| -> anyhow::Result<JsonValue> {
            let value: String = row.try_get("", "value")?;
            Ok(serde_json::from_str(&value)?)
        })
        .collect::<Result<Vec<_>, _>>()
        .context("failed to encode custom usage query")?;
    Ok(json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "ReplaceUserId": request.replace_user_id,
    }))
}

fn validate_user_usage_custom_query(sql: &str) -> anyhow::Result<String> {
    let trimmed = sql.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty()
        || trimmed.len() > 8192
        || !lower.starts_with("select")
        || trimmed.contains(';')
        || trimmed.contains('\0')
        || lower.contains("--")
        || lower.contains("/*")
        || lower.contains("*/")
    {
        anyhow::bail!("invalid custom query");
    }

    let words = sql_words(&lower);
    let forbidden = [
        "alter", "attach", "call", "copy", "create", "delete", "detach", "drop", "execute",
        "grant", "insert", "pragma", "reindex", "revoke", "truncate", "update", "vacuum",
    ];
    if forbidden
        .iter()
        .any(|keyword| words.iter().any(|word| word == keyword))
        || words.iter().any(|word| {
            word.starts_with("pg_")
                || matches!(
                    word.as_str(),
                    "access_tokens"
                        | "api_keys"
                        | "app_settings"
                        | "information_schema"
                        | "load_extension"
                        | "password_hash"
                        | "users"
                )
        })
    {
        anyhow::bail!("invalid custom query");
    }

    let mut referenced = false;
    for (index, word) in words.iter().enumerate() {
        if matches!(word.as_str(), "from" | "join") {
            let Some(table) = words.get(index + 1) else {
                anyhow::bail!("invalid custom query");
            };
            if !allowed_usage_stats_table(table) {
                anyhow::bail!("invalid custom query");
            }
            referenced = true;
        }
    }
    if !referenced {
        anyhow::bail!("invalid custom query");
    }
    Ok(trimmed.to_string())
}

fn allowed_usage_stats_table(table: &str) -> bool {
    matches!(
        table,
        "activity_log"
            | "genres"
            | "libraries"
            | "library_paths"
            | "media_genres"
            | "media_items"
            | "media_people"
            | "media_streams"
            | "media_studios"
            | "media_tags"
            | "people"
            | "playback_watch_days"
            | "playback_watch_sessions"
            | "provider_ids"
            | "studios"
            | "tags"
            | "user_data"
    )
}

fn sql_words(sql: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in sql.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

async fn user_usage_stats_save_backup_inner(db: &DatabaseConnection) -> anyhow::Result<JsonValue> {
    user_usage_stats_save_backup_to(db, PathBuf::from(USER_USAGE_BACKUP_PATH)).await
}

async fn user_usage_stats_save_backup_to(
    db: &DatabaseConnection,
    root: PathBuf,
) -> anyhow::Result<JsonValue> {
    let items = play_activity_rows(db, &[], None, None).await?;
    let file_name = format!("user_usage_stats-{}.json", now_unix());
    let path = root.join(&file_name);
    if let Some(directory) = path.parent() {
        tokio::fs::create_dir_all(directory).await?;
    }
    let backup = json!({
        "Version": 1,
        "CreatedAt": unix_to_jellyfin_date(now_unix()),
        "Items": items,
        "TotalRecordCount": items.len(),
    });
    tokio::fs::write(&path, serde_json::to_vec_pretty(&backup)?).await?;
    Ok(json!({
        "FileName": file_name,
        "Items": backup["Items"],
        "TotalRecordCount": backup["TotalRecordCount"]
    }))
}

async fn user_usage_stats_load_backup_inner(file_name: &str) -> anyhow::Result<Option<JsonValue>> {
    user_usage_stats_load_backup_from(PathBuf::from(USER_USAGE_BACKUP_PATH), file_name).await
}

async fn user_usage_stats_import_backup_inner(
    file_name: &str,
    body: Bytes,
) -> anyhow::Result<JsonValue> {
    user_usage_stats_import_backup_to(PathBuf::from(USER_USAGE_BACKUP_PATH), file_name, body).await
}

async fn user_usage_stats_import_backup_to(
    root: PathBuf,
    file_name: &str,
    body: Bytes,
) -> anyhow::Result<JsonValue> {
    if body.is_empty() || body.len() > MAX_USER_USAGE_BACKUP_BYTES {
        anyhow::bail!("invalid backup file");
    }
    let Some(file_name) = safe_user_usage_backup_file(file_name) else {
        anyhow::bail!("invalid backup file name");
    };
    let value: JsonValue = serde_json::from_slice(&body)?;
    let path = root.join(&file_name);
    if let Some(directory) = path.parent() {
        tokio::fs::create_dir_all(directory).await?;
    }
    tokio::fs::write(&path, serde_json::to_vec_pretty(&value)?).await?;
    Ok(json!({
        "FileName": file_name,
        "Imported": true,
        "TotalRecordCount": json_i64(&value, "TotalRecordCount")
    }))
}

async fn user_usage_stats_load_backup_from(
    root: PathBuf,
    file_name: &str,
) -> anyhow::Result<Option<JsonValue>> {
    let Some(file_name) = safe_user_usage_backup_file(file_name) else {
        anyhow::bail!("invalid backup file name");
    };
    let path = root.join(file_name);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn safe_user_usage_backup_file(file_name: &str) -> Option<String> {
    let path = std::path::Path::new(file_name);
    let name = path.file_name()?.to_str()?;
    (name == file_name && name.ends_with(".json")).then(|| name.to_string())
}

fn usage_stats_hourly_items(rows: &[JsonValue]) -> Vec<JsonValue> {
    let mut hours = vec![(0_i64, 0_i64, 0_i64); 24];
    for row in rows {
        let Some(timestamp) = row.get("LastPlayedAt").and_then(JsonValue::as_i64) else {
            continue;
        };
        let hour = (timestamp.rem_euclid(86_400) / 3_600) as usize;
        let play_count = json_i64(row, "PlayCount").max(1);
        hours[hour].0 += 1;
        hours[hour].1 += play_count;
        hours[hour].2 += usage_row_duration_ticks(row);
    }
    hours
        .into_iter()
        .enumerate()
        .map(|(hour, (count, play_count, duration_ticks))| {
            json!({
                "Hour": hour,
                "Name": format!("{hour:02}:00"),
                "Count": count,
                "PlayCount": play_count,
                "DurationTicks": duration_ticks
            })
        })
        .collect()
}

fn usage_stats_duration_histogram_items(rows: &[JsonValue]) -> Vec<JsonValue> {
    let mut buckets = vec![
        ("Unknown", None, None, 0_i64, 0_i64, 0_i64),
        ("<30m", Some(0_i64), Some(30_i64), 0_i64, 0_i64, 0_i64),
        ("30-60m", Some(30_i64), Some(60_i64), 0_i64, 0_i64, 0_i64),
        ("60-90m", Some(60_i64), Some(90_i64), 0_i64, 0_i64, 0_i64),
        ("90-120m", Some(90_i64), Some(120_i64), 0_i64, 0_i64, 0_i64),
        ("120m+", Some(120_i64), None, 0_i64, 0_i64, 0_i64),
    ];
    for row in rows {
        let duration_ticks = usage_row_duration_ticks(row);
        let minutes = duration_ticks / 600_000_000;
        let index = if duration_ticks <= 0 {
            0
        } else if minutes < 30 {
            1
        } else if minutes < 60 {
            2
        } else if minutes < 90 {
            3
        } else if minutes < 120 {
            4
        } else {
            5
        };
        let play_count = json_i64(row, "PlayCount").max(1);
        buckets[index].3 += 1;
        buckets[index].4 += play_count;
        buckets[index].5 += duration_ticks;
    }
    buckets
        .into_iter()
        .map(
            |(name, min_minutes, max_minutes, count, play_count, duration_ticks)| {
                json!({
                    "Name": name,
                    "MinMinutes": min_minutes,
                    "MaxMinutes": max_minutes,
                    "Count": count,
                    "PlayCount": play_count,
                    "DurationTicks": duration_ticks
                })
            },
        )
        .collect()
}

fn usage_stats_breakdown_items(rows: &[JsonValue], breakdown_type: &str) -> Option<Vec<JsonValue>> {
    let breakdown_type = breakdown_type.to_ascii_lowercase();
    if !matches!(
        breakdown_type.as_str(),
        "user"
            | "users"
            | "userid"
            | "username"
            | "item"
            | "items"
            | "itemid"
            | "itemname"
            | "type"
            | "itemtype"
            | "media"
            | "mediatype"
    ) {
        return None;
    }
    let mut groups: HashMap<String, (String, i64, i64, i64)> = HashMap::new();
    for row in rows {
        let (key, name) = match breakdown_type.as_str() {
            "user" | "users" | "userid" | "username" => (
                json_string(row, "UserId"),
                json_string(row, "UserName").or_else(|| json_string(row, "UserId")),
            ),
            "item" | "items" | "itemid" | "itemname" => (
                json_string(row, "ItemId"),
                json_string(row, "ItemName").or_else(|| json_string(row, "ItemId")),
            ),
            "type" | "itemtype" | "media" | "mediatype" => {
                (json_string(row, "ItemType"), json_string(row, "ItemType"))
            }
            _ => return None,
        };
        let key = key.unwrap_or_else(|| "Unknown".to_string());
        let name = name.unwrap_or_else(|| key.clone());
        let play_count = json_i64(row, "PlayCount").max(1);
        let entry = groups.entry(key).or_insert((name, 0, 0, 0));
        entry.1 += 1;
        entry.2 += play_count;
        entry.3 += usage_row_duration_ticks(row);
    }
    let mut items = groups
        .into_iter()
        .map(|(key, (name, count, play_count, duration_ticks))| {
            json!({
                "Id": key,
                "Name": name,
                "Count": count,
                "PlayCount": play_count,
                "DurationTicks": duration_ticks
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        json_i64(b, "PlayCount")
            .cmp(&json_i64(a, "PlayCount"))
            .then_with(|| {
                json_string(a, "Name")
                    .unwrap_or_default()
                    .cmp(&json_string(b, "Name").unwrap_or_default())
            })
    });
    Some(items)
}

fn json_i64(value: &JsonValue, key: &str) -> i64 {
    value
        .get(key)
        .and_then(JsonValue::as_i64)
        .unwrap_or_default()
}

fn usage_row_duration_ticks(row: &JsonValue) -> i64 {
    row.get("DurationTicks")
        .and_then(JsonValue::as_i64)
        .unwrap_or_else(|| {
            json_i64(row, "RunTimeTicks").saturating_mul(json_i64(row, "PlayCount").max(1))
        })
}

fn json_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn usage_stats_filter_types(filter: Option<String>) -> Vec<&'static str> {
    let Some(filter) = filter else {
        return Vec::new();
    };
    let mut item_types = Vec::new();
    for part in filter
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
    {
        match part.as_str() {
            "movie" | "movies" => item_types.push("Movie"),
            "series" | "show" | "shows" | "tv" | "tvshows" => {
                item_types.extend(["Series", "Season", "Episode"]);
            }
            "episode" | "episodes" => item_types.push("Episode"),
            _ => {}
        }
    }
    item_types.sort_unstable();
    item_types.dedup();
    item_types
}

fn usage_stats_day_range(date: &str) -> Option<(i64, i64)> {
    let start = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp();
    Some((start, start + 86_400))
}

fn unix_day(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp.max(0), 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

fn unix_day_start(timestamp: i64) -> i64 {
    timestamp.max(0).div_euclid(86_400) * 86_400
}

fn usage_stats_query_range(query: &HashMap<String, String>) -> Option<Option<(i64, i64)>> {
    let days = match query_value(query, "days").or_else(|| query_value(query, "Days")) {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(value.parse::<i64>().ok()?.clamp(1, 3650)),
        None => None,
    };
    let end = match query_value(query, "end_date").or_else(|| query_value(query, "EndDate")) {
        Some(value) if value.is_empty() => now_unix(),
        Some(value) => usage_stats_day_range(&value)?.1,
        None => now_unix(),
    };
    Some(days.map(|days| (end.saturating_sub(days * 86_400), end)))
}

pub async fn last_task_result(db: &DatabaseConnection, task_id: &str) -> Option<JsonValue> {
    let model = TaskResults::find_by_id(task_id).one(db).await.ok()??;
    Some(json!({
        "Id": model.task_id,
        "Key": model.task_id,
        "Name": scheduled_task_name(&model.task_id),
        "Status": model.status,
        "StartTimeUtc": model.start_time.map(crate::util::unix_to_jellyfin_date),
        "EndTimeUtc": model.end_time.map(crate::util::unix_to_jellyfin_date),
        "ErrorMessage": model.message,
        "LongErrorMessage": model.message,
    }))
}

fn scheduled_task_name(task_id: &str) -> &str {
    match task_id {
        "scan-library" => "Scan media library",
        "RefreshChapterImages" => "Extract chapter images",
        _ => task_id,
    }
}

pub async fn log_activity(
    state: &AppState,
    name: &str,
    log_type: &str,
    user_id: Option<&str>,
    item_id: Option<&str>,
) {
    let now = now_unix();
    let id = crate::util::stable_text_id(&format!("activity:{now}:{name}:{log_type}"));
    let active = activity_log::ActiveModel {
        id: Set(id),
        name: Set(name.to_string()),
        log_type: Set(log_type.to_string()),
        user_id: Set(user_id.map(ToString::to_string)),
        item_id: Set(item_id.map(ToString::to_string)),
        severity: Set("Info".to_string()),
        created_at: Set(now),
    };
    let _ = ActivityLog::insert(active).exec(&state.db).await;
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::ActivityCreated);
}

pub async fn upsert_task_result(
    state: &AppState,
    task_id: &str,
    status: &str,
    start_time: i64,
    end_time: i64,
    message: Option<&str>,
) {
    let existing = TaskResults::find_by_id(task_id)
        .one(&state.db)
        .await
        .ok()
        .flatten();
    let result = if let Some(model) = existing {
        let mut active: task_results::ActiveModel = model.into();
        active.status = Set(status.to_string());
        active.start_time = Set(Some(start_time));
        active.end_time = Set(Some(end_time));
        active.message = Set(message.map(ToString::to_string));
        active.update(&state.db).await.map(|_| ())
    } else {
        let active = task_results::ActiveModel {
            task_id: Set(task_id.to_string()),
            status: Set(status.to_string()),
            start_time: Set(Some(start_time)),
            end_time: Set(Some(end_time)),
            message: Set(message.map(ToString::to_string)),
        };
        TaskResults::insert(active)
            .exec(&state.db)
            .await
            .map(|_| ())
    };
    let _ = result;
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::TaskUpdated);
}

pub async fn shutdown_handler() -> Response {
    tracing::info!("shutdown requested; exiting in 1 second");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        std::process::exit(0);
    });
    axum::http::StatusCode::NO_CONTENT.into_response()
}

/// Stub for emby_ext_domains plugin — returns null when plugin is not installed.
pub async fn system_ext_server_domains() -> Response {
    Json(JsonValue::Null).into_response()
}

/// GET /System/ReleaseNotes — return release notes (stub)
pub async fn system_release_notes() -> Response {
    Json(json!({
        "Version": env!("CARGO_PKG_VERSION"),
        "ReleaseNotes": "jellyfin-rs media server",
        "ReleaseDate": "2025-01-01T00:00:00Z"
    }))
    .into_response()
}

/// GET /System/ReleaseNotes/Versions — return version list (stub)
pub async fn system_release_notes_versions() -> Response {
    Json(json!([
        {
            "Version": env!("CARGO_PKG_VERSION"),
            "ReleaseNotes": "jellyfin-rs media server",
            "ReleaseDate": "2025-01-01T00:00:00Z"
        }
    ]))
    .into_response()
}

/// GET /System/Logs/Query — paginated log query
pub async fn system_logs_query(Query(query): Query<HashMap<String, String>>) -> Response {
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .min(MAX_LOG_LINE_LIMIT);

    let files = log_files();

    let total = files.len();
    let items: Vec<_> = files.into_iter().skip(start_index).take(limit).collect();
    Json(json!({ "Items": items, "TotalRecordCount": total, "StartIndex": start_index }))
        .into_response()
}

fn log_files() -> Vec<JsonValue> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir("logs") {
        for entry in entries.flatten() {
            if let Some(file) = log_file_entry(&entry.path()) {
                files.push(file);
            }
        }
    }
    files
}

fn log_file_entry(path: &std::path::Path) -> Option<JsonValue> {
    let name = path.file_name()?.to_string_lossy().to_string();
    if !is_safe_log_name(&name) {
        return None;
    }
    let meta = path.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(json!({
        "Name": name,
        "Size": meta.len(),
        "DateModified": meta.modified().ok().map(|time| unix_to_jellyfin_date(system_time_to_unix(time))).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
    }))
}

/// POST /Items/{id}/MetadataEditor — update metadata via editor
pub async fn metadata_editor(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let body = match crate::jellyfin::items::normalize_item_update_body(body) {
        Ok(body) => body,
        Err(error) => return validation_error_response(error),
    };
    match crate::jellyfin::items::update_item_inner(&state.db, &item_id, body).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /System/WakeOnLanInfo — return WoL-capable MAC addresses (stub)
pub async fn system_wake_on_lan_info() -> Response {
    Json(json!([])).into_response()
}

/// GET /System/Logs/{name}/Lines — return tail of a log file
pub async fn system_log_lines(
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000)
        .min(MAX_LOG_LINE_LIMIT);

    match safe_log_path(&name).and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(data) => {
            let lines: Vec<&str> = data.lines().collect();
            let start = lines.len().saturating_sub(limit);
            let tail: String = lines[start..].join("\n");
            (StatusCode::OK, tail).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /System/Logs/{name} — download a log file
pub async fn system_log_download(Path(name): Path<String>) -> Response {
    log_file_response(&name)
}

fn log_file_response(name: &str) -> Response {
    match safe_log_path(name).and_then(|path| std::fs::read(path).ok()) {
        Some(data) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn sync_targets_result() -> JsonValue {
    json!([])
}

fn sync_options_result() -> JsonValue {
    json!({
        "Targets": sync_targets_result(),
        "Options": [],
        "QualityOptions": [],
        "ProfileOptions": []
    })
}

fn sync_data_result() -> JsonValue {
    json!({ "ItemIdsToRemove": [] })
}

async fn device_records(state: &AppState, user_id: Option<&str>) -> Vec<DeviceRecord> {
    let mut records = HashMap::<String, DeviceRecord>::new();

    match AccessTokens::find()
        .filter(access_tokens::Column::RevokedAt.is_null())
        .all(&state.db)
        .await
    {
        Ok(tokens) => {
            for token in tokens {
                if user_id.is_some_and(|id| id != token.user_id) {
                    continue;
                }
                let Some(device_id) = token
                    .device_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                records
                    .entry(device_id.to_string())
                    .or_insert_with(|| DeviceRecord::from_token(&token));
            }
        }
        Err(error) => tracing::warn!("failed to load device tokens: {error}"),
    }

    for session in state.playback_sessions.read().await.values() {
        if user_id.is_some_and(|id| id != session.user_id) {
            continue;
        }
        records
            .entry(device_record_id(
                &session.device_id,
                &session.device_name,
                &session.client,
            ))
            .and_modify(|record| record.merge_session(session))
            .or_insert_with(|| DeviceRecord::from_session(session));
    }

    for capabilities in state.session_capabilities.read().await.values() {
        if user_id.is_some_and(|id| id != capabilities.user_id) {
            continue;
        }
        records
            .entry(device_record_id(
                &capabilities.device_id,
                &capabilities.device_name,
                &capabilities.client,
            ))
            .and_modify(|record| record.merge_capabilities(capabilities))
            .or_insert_with(|| DeviceRecord::from_capabilities(capabilities));
    }

    records.into_values().collect()
}

fn device_record_id(device_id: &str, device_name: &str, client: &str) -> String {
    if !device_id.trim().is_empty() {
        return device_id.to_string();
    }
    stable_text_id(&format!("device:{client}:{device_name}"))
}

fn device_options_result(device_id: &str, custom_name: Option<String>) -> JsonValue {
    json!({ "Id": 0, "DeviceId": device_id, "CustomName": custom_name })
}

async fn save_camera_upload(
    db: &DatabaseConnection,
    headers: HeaderMap,
    query: CameraUploadQuery,
    body: Bytes,
) -> anyhow::Result<()> {
    save_camera_upload_to(db, PathBuf::from(CAMERA_UPLOADS_PATH), headers, query, body).await
}

async fn save_camera_upload_to(
    db: &DatabaseConnection,
    root: PathBuf,
    headers: HeaderMap,
    query: CameraUploadQuery,
    body: Bytes,
) -> anyhow::Result<()> {
    if body.len() > MAX_CAMERA_UPLOAD_BYTES {
        anyhow::bail!("camera upload is too large");
    }
    let device_id = normalize_device_id(query.device_id.as_deref())?;
    let album = required_upload_part(query.album, "Album")?;
    let name = required_upload_part(query.name, "Name")?;
    let id = required_upload_part(query.id, "Id")?;

    let path = root.join(sanitize_file_part(&device_id)).join(format!(
        "{}-{}",
        sanitize_file_part(&id),
        sanitize_file_part(&name)
    ));
    if let Some(directory) = path.parent() {
        tokio::fs::create_dir_all(directory).await?;
    }
    tokio::fs::write(&path, &body).await?;

    let mime_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    let mut history = camera_upload_history_value(db, &device_id).await?;
    if let Some(files) = history
        .get_mut("FilesUploaded")
        .and_then(JsonValue::as_array_mut)
    {
        files.retain(|file| file.get("Id").and_then(JsonValue::as_str) != Some(id.as_str()));
        files.push(json!({
            "Name": name,
            "Id": id,
            "Album": album,
            "MimeType": mime_type,
        }));
    }
    set_app_setting(
        db,
        &camera_upload_history_key(&device_id),
        &history.to_string(),
    )
    .await
}

async fn camera_upload_history_value(
    db: &DatabaseConnection,
    device_id: &str,
) -> anyhow::Result<JsonValue> {
    let value = app_setting(db, &camera_upload_history_key(device_id), "").await;
    if !value.trim().is_empty() {
        if let Ok(history) = serde_json::from_str::<JsonValue>(&value) {
            return Ok(history);
        }
    }
    Ok(json!({ "DeviceId": device_id, "FilesUploaded": [] }))
}

fn camera_upload_history_key(device_id: &str) -> String {
    format!("camera_uploads:{device_id}")
}

fn required_upload_part(value: Option<String>, name: &str) -> anyhow::Result<String> {
    let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        anyhow::bail!("{name} is required");
    };
    if value.chars().count() > MAX_CAMERA_UPLOAD_FIELD_LEN {
        anyhow::bail!("{name} is too long");
    }
    if value.contains('\0') || value.chars().any(char::is_control) {
        anyhow::bail!("{name} contains unsupported characters");
    }
    Ok(value.to_string())
}

fn sanitize_file_part(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect::<String>()
        .trim_matches('.')
        .to_string();
    if sanitized.is_empty() {
        "upload".to_string()
    } else {
        sanitized
    }
}

async fn device_custom_name(db: &DatabaseConnection, device_id: &str) -> Option<String> {
    let custom_name = app_setting(db, &device_options_key(device_id), "")
        .await
        .trim()
        .to_string();
    (!custom_name.is_empty()).then_some(custom_name)
}

fn device_options_key(device_id: &str) -> String {
    format!("device_options:{device_id}:custom_name")
}

fn normalize_device_ids(value: Option<&str>) -> Result<Vec<String>, (StatusCode, &'static str)> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err((StatusCode::BAD_REQUEST, "DeviceId is required"));
    };
    let mut ids = Vec::new();
    for id in value.split(',') {
        if id.trim().is_empty() {
            continue;
        }
        let id = normalize_device_id(Some(id))
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid DeviceId"))?;
        if ids.iter().any(|existing| existing == &id) {
            continue;
        }
        ids.push(id);
        if ids.len() > MAX_DEVICE_IDS_PER_REQUEST {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many device ids"));
        }
    }
    if ids.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "DeviceId is required"));
    }
    Ok(ids)
}

fn normalize_device_id(value: Option<&str>) -> anyhow::Result<String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        anyhow::bail!("DeviceId is required");
    };
    if value.len() > MAX_DEVICE_ID_LEN
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        anyhow::bail!("Invalid DeviceId");
    }
    Ok(value.to_string())
}

fn normalize_device_custom_name(value: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > MAX_DEVICE_CUSTOM_NAME_LEN {
        anyhow::bail!("CustomName is too long");
    }
    if value.contains('\0') || value.chars().any(|c| c.is_control() && c != '\t') {
        anyhow::bail!("CustomName contains unsupported characters");
    }
    Ok(Some(value.to_string()))
}

async fn revoke_device(state: &AppState, device_id: &str, now: i64) -> anyhow::Result<()> {
    let tokens = AccessTokens::find()
        .filter(access_tokens::Column::DeviceId.eq(device_id))
        .filter(access_tokens::Column::RevokedAt.is_null())
        .all(&state.db)
        .await
        .context("failed to find device tokens")?;
    for token in tokens {
        let mut active: access_tokens::ActiveModel = token.into();
        active.revoked_at = Set(Some(now));
        active
            .update(&state.db)
            .await
            .context("failed to revoke device tokens")?;
    }

    state
        .playback_sessions
        .write()
        .await
        .retain(|_, session| session.device_id != device_id);
    state
        .session_capabilities
        .write()
        .await
        .retain(|_, capabilities| capabilities.device_id != device_id);
    Ok(())
}

fn device_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "Error": "Device not found" })),
    )
        .into_response()
}

struct DeviceRecord {
    id: String,
    name: String,
    app_name: String,
    app_version: String,
    last_user_id: String,
    last_activity_unix: i64,
    capabilities: SessionCapabilities,
}

impl DeviceRecord {
    fn from_token(token: &access_tokens::Model) -> Self {
        let device_id = token.device_id.clone().unwrap_or_default();
        let app_name = token
            .name
            .clone()
            .unwrap_or_else(|| "jellyfin-rs".to_string());
        Self {
            id: device_id.clone(),
            name: device_id.clone(),
            app_name: app_name.clone(),
            app_version: String::new(),
            last_user_id: token.user_id.clone(),
            last_activity_unix: token.last_used_at.unwrap_or(token.created_at),
            capabilities: SessionCapabilities {
                user_id: token.user_id.clone(),
                client: app_name,
                device_name: device_id.clone(),
                device_id,
                application_version: String::new(),
                ..Default::default()
            },
        }
    }

    fn from_session(session: &PlaybackSession) -> Self {
        let capabilities = SessionCapabilities {
            user_id: session.user_id.clone(),
            client: session.client.clone(),
            device_name: session.device_name.clone(),
            device_id: session.device_id.clone(),
            application_version: session.application_version.clone(),
            playable_media_types: session.playable_media_types.clone(),
            supported_commands: session.supported_commands.clone(),
            supports_media_control: session.supports_media_control,
            supports_persistent_identifier: session.supports_persistent_identifier,
        };
        Self {
            id: device_record_id(&session.device_id, &session.device_name, &session.client),
            name: session.device_name.clone(),
            app_name: session.client.clone(),
            app_version: session.application_version.clone(),
            last_user_id: session.user_id.clone(),
            last_activity_unix: session.last_activity_unix,
            capabilities,
        }
    }

    fn from_capabilities(capabilities: &SessionCapabilities) -> Self {
        Self {
            id: device_record_id(
                &capabilities.device_id,
                &capabilities.device_name,
                &capabilities.client,
            ),
            name: capabilities.device_name.clone(),
            app_name: capabilities.client.clone(),
            app_version: capabilities.application_version.clone(),
            last_user_id: capabilities.user_id.clone(),
            last_activity_unix: now_unix(),
            capabilities: capabilities.clone(),
        }
    }

    fn merge_session(&mut self, session: &PlaybackSession) {
        if session.last_activity_unix >= self.last_activity_unix {
            *self = Self::from_session(session);
        }
    }

    fn merge_capabilities(&mut self, capabilities: &SessionCapabilities) {
        self.capabilities = capabilities.clone();
        if self.name.is_empty() {
            self.name = capabilities.device_name.clone();
        }
        if self.app_name.is_empty() {
            self.app_name = capabilities.client.clone();
        }
        if self.app_version.is_empty() {
            self.app_version = capabilities.application_version.clone();
        }
        if self.last_user_id.is_empty() {
            self.last_user_id = capabilities.user_id.clone();
        }
    }

    fn to_json(&self, custom_name: Option<String>) -> JsonValue {
        json!({
            "Name": self.name,
            "CustomName": custom_name,
            "AccessToken": null,
            "Id": self.id,
            "LastUserName": null,
            "AppName": self.app_name,
            "AppVersion": self.app_version,
            "LastUserId": self.last_user_id,
            "DateLastActivity": unix_to_jellyfin_date(self.last_activity_unix),
            "Capabilities": {
                "PlayableMediaTypes": self.capabilities.playable_media_types,
                "SupportedCommands": self.capabilities.supported_commands,
                "SupportsMediaControl": self.capabilities.supports_media_control,
                "SupportsPersistentIdentifier": self.capabilities.supports_persistent_identifier
            },
            "IconUrl": null
        })
    }
}

fn safe_log_path(name: &str) -> Option<std::path::PathBuf> {
    is_safe_log_name(name).then(|| std::path::PathBuf::from("logs").join(name))
}

fn is_safe_log_name(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    let path = std::path::Path::new(name);
    path.file_name().and_then(|part| part.to_str()) == Some(name)
        && (name.ends_with(".log") || name.ends_with("_err.log"))
}

/// POST /System/Configuration/Partial — partial configuration update
pub async fn update_server_configuration_partial(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JsonValue>,
) -> Response {
    let config = server_config_json(&state.db).await;
    let config = match configuration::merge_server_configuration_patch(config, body) {
        Ok(config) => config,
        Err(error) => {
            return (
                error.0,
                Json(json!({
                    "Error": error.1
                })),
            )
                .into_response();
        }
    };
    let value = serde_json::from_str::<JsonValue>(&config).unwrap_or_else(|_| json!({}));
    let runtime_settings = match configuration::runtime_server_settings(&value) {
        Ok(settings) => settings,
        Err(error) => {
            return (
                error.0,
                Json(json!({
                    "Error": error.1
                })),
            )
                .into_response();
        }
    };
    if let Err(error) =
        configuration::sync_runtime_server_settings(&state.db, runtime_settings).await
    {
        return internal_error(error);
    }

    match set_app_setting(&state.db, configuration::SERVER_CONFIG_SETTING_KEY, &config).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /Features — return server feature support
pub async fn features() -> Response {
    Json(json!({
        "Name": "jellyfin-rs",
        "Version": env!("CARGO_PKG_VERSION"),
        "Features": [
            "ContentUploading",
            "MediaPlayback",
            "ExternalContent",
            "FileOrganization",
            "UserData",
            "Sharing",
            "Playlists",
            "Collections"
        ]
    }))
    .into_response()
}

/// GET /Notifications/Types — return supported notification types
pub async fn notification_types() -> Response {
    Json(json!([
        {
            "Name": "TaskCompleted",
            "Category": "Task",
            "Enabled": true,
            "DisabledOnlineFeatures": []
        },
        {
            "Name": "LibraryChanged",
            "Category": "Library",
            "Enabled": true,
            "DisabledOnlineFeatures": []
        },
        {
            "Name": "UserActivity",
            "Category": "User",
            "Enabled": true,
            "DisabledOnlineFeatures": []
        }
    ]))
    .into_response()
}

/// GET /Notifications/Services/Defaults — return default notification services
pub async fn notification_services_defaults() -> Response {
    Json(notification_services_value()).into_response()
}

pub async fn notification_services() -> Response {
    Json(notification_services_value()).into_response()
}

fn notification_services_value() -> Vec<JsonValue> {
    vec![json!({
        "Name": "Email",
        "DefaultTitle": "jellyfin-rs Notification",
        "DefaultDescription": "A notification from jellyfin-rs",
        "DefaultUrl": "http://127.0.0.1:8096",
        "SupportedCommands": ["NotificationAdmin"]
    })]
}

pub async fn notification_services_test(State(state): State<Arc<AppState>>) -> Response {
    match insert_notification(
        &state,
        None,
        "Test Notification",
        "This is a test notification from jellyfin-rs.",
        "Normal",
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn smtp_notification_test(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match Users::find_by_id(&user_id).one(&state.db).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error.into()),
    }

    match insert_notification(
        &state,
        Some(&user_id),
        "SMTP Test",
        "SMTP delivery is not configured; the test notification was recorded locally.",
        "Normal",
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn send_admin_notification(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = match normalize_notification_text(
        query_value(&query, "Name").as_deref(),
        MAX_NOTIFICATION_NAME_LEN,
        true,
        "Name is required",
    ) {
        Ok(name) => name,
        Err(error) => return validation_error_response(error),
    };
    let description = match normalize_notification_text(
        query_value(&query, "Description").as_deref(),
        MAX_NOTIFICATION_DESCRIPTION_LEN,
        false,
        "Description is invalid",
    ) {
        Ok(description) => description,
        Err(error) => return validation_error_response(error),
    };
    let level = match normalize_notification_level(
        query_value(&query, "Level").as_deref().unwrap_or("Normal"),
    ) {
        Ok(level) => level,
        Err(error) => return validation_error_response(error),
    };
    match insert_notification(&state, None, &name, &description, &level).await {
        Ok(()) => Json(json!({ "Notifications": [], "TotalRecordCount": 0 })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn user_notifications(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match notification_items(&state.db, &user_id).await {
        Ok(mut items) => {
            if let Some(is_read) = query
                .get("IsRead")
                .or_else(|| query.get("isRead"))
                .and_then(|value| value.parse::<bool>().ok())
            {
                items.retain(|item| item["IsRead"] == is_read);
            }
            let start = query
                .get("StartIndex")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
                .min(items.len());
            let limit = query
                .get("Limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(items.len());
            let total = items.len();
            let page = items
                .into_iter()
                .skip(start)
                .take(limit)
                .collect::<Vec<_>>();
            Json(json!({ "Notifications": page, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_notifications_summary(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match notification_items(&state.db, &user_id).await {
        Ok(items) => {
            let unread = items
                .into_iter()
                .filter(|item| item["IsRead"] == false)
                .collect::<Vec<_>>();
            Json(json!({
                "UnreadCount": unread.len(),
                "MaxUnreadNotificationLevel": notification_max_level(&unread)
            }))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn mark_notifications_read(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    update_notification_read_state(&state.db, &user_id, &query, true).await
}

pub async fn mark_notifications_unread(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    update_notification_read_state(&state.db, &user_id, &query, false).await
}

async fn insert_notification(
    state: &AppState,
    user_id: Option<&str>,
    name: &str,
    description: &str,
    level: &str,
) -> anyhow::Result<()> {
    let name = normalize_notification_text(
        Some(name),
        MAX_NOTIFICATION_NAME_LEN,
        true,
        "Name is required",
    )
    .map_err(|(_, message)| anyhow::anyhow!(message))?;
    let description = normalize_notification_text(
        Some(description),
        MAX_NOTIFICATION_DESCRIPTION_LEN,
        false,
        "Description is invalid",
    )
    .map_err(|(_, message)| anyhow::anyhow!(message))?;
    let level =
        normalize_notification_level(level).map_err(|(_, message)| anyhow::anyhow!(message))?;
    let now = now_unix();
    let id = stable_text_id(&format!(
        "notification:{now}:{}:{name}:{description}:{level}",
        user_id.unwrap_or_default()
    ));
    let active = activity_log::ActiveModel {
        id: Set(id),
        name: Set(name.to_string()),
        log_type: Set("Notification".to_string()),
        user_id: Set(user_id.map(ToString::to_string)),
        item_id: Set(Some(format!("{level}|{description}"))),
        severity: Set(level.to_string()),
        created_at: Set(now),
    };
    ActivityLog::insert(active).exec(&state.db).await?;
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::ActivityCreated);
    Ok(())
}

async fn notification_items(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let read_ids = notification_read_ids(db, user_id).await;
    let models = ActivityLog::find()
        .filter(activity_log::Column::LogType.eq("Notification"))
        .filter(
            Condition::any()
                .add(activity_log::Column::UserId.is_null())
                .add(activity_log::Column::UserId.eq(user_id)),
        )
        .order_by_desc(activity_log::Column::CreatedAt)
        .all(db)
        .await?;
    Ok(models
        .into_iter()
        .map(|model| notification_json(model, &read_ids))
        .collect())
}

fn notification_json(model: activity_log::Model, read_ids: &[String]) -> JsonValue {
    let is_read = read_ids.iter().any(|id| id == &model.id);
    let (level, description) = model
        .item_id
        .as_deref()
        .and_then(|value| value.split_once('|'))
        .unwrap_or(("Normal", ""));
    json!({
        "Id": model.id,
        "UserId": model.user_id.unwrap_or_default(),
        "Date": unix_to_jellyfin_date(model.created_at),
        "IsRead": is_read,
        "Name": model.name,
        "Description": description,
        "Url": "",
        "Level": notification_level(level)
    })
}

async fn update_notification_read_state(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
    read: bool,
) -> Response {
    let ids = match normalize_notification_ids(
        query
            .get("Ids")
            .or_else(|| query.get("ids"))
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    ) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };
    if ids.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let visible_ids = match visible_notification_ids(db, user_id, &ids).await {
        Ok(ids) => ids,
        Err(error) => return internal_error(error),
    };
    if visible_ids.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let mut read_ids = notification_read_ids(db, user_id).await;
    if read {
        for id in visible_ids {
            if !read_ids.iter().any(|read_id| read_id == &id) {
                read_ids.push(id);
            }
        }
    } else {
        read_ids.retain(|read_id| !visible_ids.iter().any(|id| id == read_id));
    }

    match set_app_setting(
        db,
        &notification_read_key(user_id),
        &serde_json::to_string(&read_ids).unwrap_or_else(|_| "[]".to_string()),
    )
    .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn visible_notification_ids(
    db: &DatabaseConnection,
    user_id: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ActivityLog::find()
        .filter(activity_log::Column::LogType.eq("Notification"))
        .filter(activity_log::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await?;
    let visible_ids = rows
        .into_iter()
        .filter(|row| row.user_id.as_deref().map_or(true, |id| id == user_id))
        .map(|row| row.id)
        .collect::<HashSet<_>>();
    Ok(ids
        .iter()
        .filter(|id| visible_ids.contains(*id))
        .cloned()
        .collect())
}

async fn notification_read_ids(db: &DatabaseConnection, user_id: &str) -> Vec<String> {
    serde_json::from_str(&app_setting(db, &notification_read_key(user_id), "[]").await)
        .unwrap_or_default()
}

fn notification_read_key(user_id: &str) -> String {
    format!("notifications_read:{user_id}")
}

fn normalize_notification_text(
    value: Option<&str>,
    max_len: usize,
    required: bool,
    required_error: &'static str,
) -> Result<String, (StatusCode, &'static str)> {
    let value = value.unwrap_or_default().trim();
    if required && value.is_empty() {
        return Err((StatusCode::BAD_REQUEST, required_error));
    }
    if value.chars().count() > max_len {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, required_error));
    }
    if value.contains('\0')
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err((StatusCode::BAD_REQUEST, required_error));
    }
    Ok(value.to_string())
}

fn normalize_notification_level(level: &str) -> Result<String, (StatusCode, &'static str)> {
    match level.trim().to_ascii_lowercase().as_str() {
        "" | "normal" | "info" | "information" => Ok("Normal".to_string()),
        "warning" | "warn" => Ok("Warning".to_string()),
        "error" | "err" => Ok("Error".to_string()),
        _ => Err((StatusCode::BAD_REQUEST, "Invalid notification level")),
    }
}

fn normalize_notification_ids(ids: Vec<String>) -> Result<Vec<String>, (StatusCode, &'static str)> {
    if ids.len() > MAX_NOTIFICATION_IDS {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many notification ids"));
    }
    let mut normalized = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() || normalized.iter().any(|existing| existing == id) {
            continue;
        }
        if id.len() > MAX_NOTIFICATION_ID_LEN
            || id.contains('\0')
            || id.chars().any(char::is_control)
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid notification id"));
        }
        normalized.push(id.to_string());
    }
    Ok(normalized)
}

fn notification_level(level: &str) -> &str {
    match level {
        "Warning" | "Error" => level,
        _ => "Normal",
    }
}

fn notification_max_level(items: &[JsonValue]) -> &'static str {
    if items.iter().any(|item| item["Level"] == "Error") {
        "Error"
    } else if items.iter().any(|item| item["Level"] == "Warning") {
        "Warning"
    } else {
        "Normal"
    }
}

fn query_value(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .get(key)
        .or_else(|| query.get(&key.to_ascii_lowercase()))
        .map(|value| value.trim().to_string())
}

fn query_bool_any(query: &HashMap<String, String>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| query_value(query, key))
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        })
}

pub async fn fallback_fonts() -> Response {
    Json(fallback_font_entries()).into_response()
}

pub async fn fallback_font_file(Path(name): Path<String>) -> Response {
    let Some(path) = fallback_font_path(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(fallback_font_mime_type(&name)),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    (headers, bytes).into_response()
}

fn fallback_font_entries() -> Vec<JsonValue> {
    let mut entries = Vec::new();
    if let Ok(fonts) = std::fs::read_dir(FALLBACK_FONTS_PATH) {
        for entry in fonts.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !is_safe_fallback_font_name(name) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            entries.push(json!({
                "Name": name,
                "Size": metadata.len(),
                "DateCreated": metadata.created().ok().map(|time| unix_to_jellyfin_date(system_time_to_unix(time))).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
                "DateModified": metadata.modified().ok().map(|time| unix_to_jellyfin_date(system_time_to_unix(time))).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
            }));
        }
    }
    entries.sort_by(|left, right| {
        left.get("Name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .get("Name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
    });
    entries
}

fn fallback_font_path(name: &str) -> Option<std::path::PathBuf> {
    is_safe_fallback_font_name(name)
        .then(|| std::path::PathBuf::from(FALLBACK_FONTS_PATH).join(name))
}

fn is_safe_fallback_font_name(name: &str) -> bool {
    let path = std::path::Path::new(name);
    path.file_name().and_then(|part| part.to_str()) == Some(name)
        && matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .as_deref(),
            Some("ttf" | "otf" | "woff" | "woff2")
        )
}

fn fallback_font_mime_type(name: &str) -> &'static str {
    match std::path::Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub async fn news_product() -> Response {
    Json(Vec::<JsonValue>::new()).into_response()
}

pub async fn reports_headers(Query(query): Query<HashMap<String, String>>) -> Response {
    Json(report_headers_for_query(&query)).into_response()
}

pub async fn reports_activities(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match reports_activity_result(&state.db, &query).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn reports_items(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match reports_items_result(&state.db, &query).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn reports_items_download(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match reports_items_result(&state.db, &query).await {
        Ok(value) => {
            let headers = value["Headers"].as_array().cloned().unwrap_or_default();
            let rows = value["Rows"].as_array().cloned().unwrap_or_default();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                    (
                        header::CONTENT_DISPOSITION,
                        "attachment; filename=\"report.csv\"",
                    ),
                ],
                report_csv(&headers, &rows),
            )
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn reports_activity_result(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<JsonValue> {
    let headers = report_activity_headers_for_query(query);
    let fields = report_header_fields(&headers);
    let limit = report_limit(query);
    let start_index = report_start_index(query);
    let item_types = report_query_list(query, "IncludeItemTypes");
    let mut values: Vec<sea_orm::Value> = Vec::new();
    let mut where_parts = Vec::new();
    if !item_types.is_empty() {
        where_parts.push(format!(
            "mi.item_type IN ({})",
            item_types.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        ));
        values.extend(item_types.iter().map(|value| value.as_str().into()));
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    let count_rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                "SELECT COUNT(*) AS cnt FROM activity_log al LEFT JOIN media_items mi ON mi.id = al.item_id{where_sql}"
            ),
            values.clone(),
        ))
        .await
        .context("failed to count report activities")?;
    let total = count_rows
        .first()
        .and_then(|row| row.get_i64("cnt").ok())
        .unwrap_or_default();

    let mut row_values = values;
    row_values.push((limit as i64).into());
    row_values.push((start_index as i64).into());
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"SELECT al.id, al.name, al.log_type, al.user_id, al.item_id, al.severity, al.created_at, users.username, mi.title AS item_name
                   FROM activity_log al
                   LEFT JOIN users ON users.id = al.user_id
                   LEFT JOIN media_items mi ON mi.id = al.item_id
                   {where_sql}
                   ORDER BY al.created_at DESC
                   LIMIT ? OFFSET ?"#
            ),
            row_values,
        ))
        .await
        .context("failed to load report activities")?;

    let rows = rows
        .iter()
        .map(|row| {
            let id = row.get_str("id")?;
            let name = row.get_str("name")?;
            let log_type = row.get_str("log_type")?;
            let user_id = row.get_opt_str("user_id")?.unwrap_or_default();
            let username = row.get_opt_str("username")?.unwrap_or_default();
            let item_name = row.get_opt_str("item_name")?.unwrap_or_default();
            let severity = row
                .get_str("severity")
                .unwrap_or_else(|_| "Info".to_string());
            let created_at = row.get_i64("created_at").unwrap_or_default();
            Ok(report_row(
                id,
                "BaseItem",
                &user_id,
                false,
                report_activity_columns(
                    &fields, created_at, &name, &log_type, &severity, &username, &user_id,
                    &item_name,
                ),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(report_result(rows, headers, total))
}

async fn reports_items_result(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<JsonValue> {
    let headers = report_item_headers_for_query(query);
    let fields = report_header_fields(&headers);
    let limit = report_limit(query);
    let start_index = report_start_index(query);
    let (where_sql, values) = report_item_where(query);
    let order_sql = report_item_order(query);
    let count_rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!("SELECT COUNT(*) AS cnt FROM media_items mi{where_sql}"),
            values.clone(),
        ))
        .await
        .context("failed to count report items")?;
    let total = count_rows
        .first()
        .and_then(|row| row.get_i64("cnt").ok())
        .unwrap_or_default();

    let mut row_values = values;
    row_values.push((limit as i64).into());
    row_values.push((start_index as i64).into());
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"SELECT mi.id, mi.title, mi.path, mi.item_type, mi.overview, mi.official_rating, mi.production_year, mi.premiere_date,
                          mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number,
                          mi.community_rating, mi.critic_rating, mi.created_at,
                          (SELECT COUNT(*) FROM media_streams ms WHERE ms.item_id = mi.id AND ms.stream_type = 'Subtitle') AS subtitle_count
                   FROM media_items mi
                   {where_sql}
                   ORDER BY {order_sql}, mi.id ASC
                   LIMIT ? OFFSET ?"#
            ),
            row_values,
        ))
        .await
        .context("failed to load report items")?;

    let rows = rows
        .iter()
        .map(|row| {
            let id = row.get_str("id")?;
            let item_type = row.get_str("item_type")?;
            let user_id = query_value(query, "UserId").unwrap_or_default();
            let has_subtitles = row.get_i64("subtitle_count").unwrap_or_default() > 0;
            Ok(report_row(
                id,
                &item_type,
                &user_id,
                has_subtitles,
                report_item_columns(&fields, row)?,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(report_result(rows, headers, total))
}

fn report_item_where(query: &HashMap<String, String>) -> (String, Vec<sea_orm::Value>) {
    let mut where_parts = Vec::new();
    let mut values: Vec<sea_orm::Value> = Vec::new();

    let include_types = report_query_list(query, "IncludeItemTypes");
    if !include_types.is_empty() {
        where_parts.push(format!(
            "mi.item_type IN ({})",
            include_types
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",")
        ));
        values.extend(include_types.iter().map(|value| value.as_str().into()));
    }

    let exclude_types = report_query_list(query, "ExcludeItemTypes");
    if !exclude_types.is_empty() {
        where_parts.push(format!(
            "mi.item_type NOT IN ({})",
            exclude_types
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",")
        ));
        values.extend(exclude_types.iter().map(|value| value.as_str().into()));
    }

    if let Some(parent_id) = query_value(query, "ParentId").filter(|value| !value.is_empty()) {
        where_parts.push("mi.parent_id = ?".to_string());
        values.push(parent_id.into());
    }

    if let Some(search) = query_value(query, "SearchTerm").filter(|value| !value.is_empty()) {
        where_parts.push("LOWER(mi.title) LIKE ?".to_string());
        values.push(format!("%{}%", search.to_ascii_lowercase()).into());
    }

    if let Some(has_overview) = report_query_bool(query, "HasOverview") {
        where_parts.push(if has_overview {
            "mi.overview IS NOT NULL AND mi.overview <> ''".to_string()
        } else {
            "(mi.overview IS NULL OR mi.overview = '')".to_string()
        });
    }

    let years = report_query_list(query, "Years");
    if !years.is_empty() {
        let years = years
            .into_iter()
            .filter_map(|value| value.parse::<i64>().ok())
            .collect::<Vec<_>>();
        if !years.is_empty() {
            where_parts.push(format!(
                "mi.production_year IN ({})",
                years.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            ));
            values.extend(years.into_iter().map(Into::into));
        }
    }

    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    (where_sql, values)
}

fn report_item_order(query: &HashMap<String, String>) -> String {
    let sort_by = query_value(query, "SortBy").unwrap_or_else(|| "SortName".to_string());
    let field = sort_by
        .split(',')
        .next()
        .unwrap_or("SortName")
        .trim()
        .to_ascii_lowercase();
    let column = match field.as_str() {
        "path" => "mi.path",
        "datecreated" | "dateadded" => "mi.created_at",
        "runtime" => "COALESCE(mi.runtime_ticks, 0)",
        "productionyear" | "year" => "COALESCE(mi.production_year, 0)",
        "premieredate" => "COALESCE(mi.premiere_date, '9999-12-31')",
        "communityrating" => "COALESCE(mi.community_rating, 0)",
        "criticrating" => "COALESCE(mi.critic_rating, 0)",
        _ => "LOWER(mi.title)",
    };
    let direction = query_value(query, "SortOrder")
        .filter(|value| value.eq_ignore_ascii_case("Descending"))
        .map(|_| "DESC")
        .unwrap_or("ASC");
    format!("{column} {direction}")
}

fn report_result(rows: Vec<JsonValue>, headers: Vec<JsonValue>, total: i64) -> JsonValue {
    json!({
        "Rows": rows,
        "Headers": headers,
        "Groups": [],
        "TotalRecordCount": total,
        "IsGrouped": false
    })
}

fn report_row(
    id: String,
    row_type: &str,
    user_id: &str,
    has_subtitles: bool,
    columns: Vec<JsonValue>,
) -> JsonValue {
    json!({
        "Id": id,
        "HasImageTagsBackdrop": false,
        "HasImageTagsPrimary": false,
        "HasImageTagsLogo": false,
        "HasLocalTrailer": false,
        "HasLockData": false,
        "HasEmbeddedImage": false,
        "HasSubtitles": has_subtitles,
        "HasSpecials": false,
        "Columns": columns,
        "RowType": row_type,
        "UserId": user_id
    })
}

fn report_headers_for_query(query: &HashMap<String, String>) -> Vec<JsonValue> {
    if report_is_activity_view(query) {
        report_activity_headers_for_query(query)
    } else {
        report_item_headers_for_query(query)
    }
}

fn report_activity_headers_for_query(query: &HashMap<String, String>) -> Vec<JsonValue> {
    filter_report_headers(
        vec![
            report_header("Date", "Date", "DateTime"),
            report_header("Name", "Name", "String"),
            report_header("Type", "Type", "String"),
            report_header("Severity", "Severity", "String"),
            report_header("User", "User", "String"),
            report_header("UserId", "User Id", "String"),
            report_header("Item", "Item", "String"),
        ],
        query,
    )
}

fn report_item_headers_for_query(query: &HashMap<String, String>) -> Vec<JsonValue> {
    filter_report_headers(
        vec![
            report_header("Name", "Name", "String"),
            report_header("Path", "Path", "String"),
            report_header("Type", "Type", "String"),
            report_header("Year", "Year", "Int"),
            report_header("DateAdded", "Date Added", "DateTime"),
            report_header("Runtime", "Runtime", "Minutes"),
            report_header("ParentalRating", "Parental Rating", "String"),
            report_header("CommunityRating", "Community Rating", "String"),
            report_header("CriticRating", "Critic Rating", "String"),
            report_header("SeasonNumber", "Season", "Int"),
            report_header("EpisodeNumber", "Episode", "Int"),
            report_header("Overview", "Overview", "String"),
        ],
        query,
    )
}

fn report_header(field_name: &str, name: &str, field_type: &str) -> JsonValue {
    json!({
        "HeaderFieldType": field_type,
        "Name": name,
        "FieldName": field_name,
        "SortField": field_name,
        "Type": field_type,
        "ItemViewType": "None",
        "Visible": true,
        "DisplayType": "ScreenExport",
        "ShowHeaderLabel": true,
        "CanGroup": matches!(field_name, "Type" | "Year" | "Severity" | "User")
    })
}

fn filter_report_headers(
    headers: Vec<JsonValue>,
    query: &HashMap<String, String>,
) -> Vec<JsonValue> {
    let wanted = report_query_list(query, "ReportColumns");
    if wanted.is_empty() {
        return headers;
    }
    headers
        .into_iter()
        .filter(|header| {
            let field = header["FieldName"].as_str().unwrap_or_default();
            wanted.iter().any(|value| value.eq_ignore_ascii_case(field))
        })
        .collect()
}

fn report_header_fields(headers: &[JsonValue]) -> Vec<String> {
    headers
        .iter()
        .filter_map(|header| header["FieldName"].as_str().map(ToString::to_string))
        .collect()
}

fn report_activity_columns(
    fields: &[String],
    created_at: i64,
    name: &str,
    log_type: &str,
    severity: &str,
    username: &str,
    user_id: &str,
    item_name: &str,
) -> Vec<JsonValue> {
    fields
        .iter()
        .map(|field| {
            let value = match field.as_str() {
                "Date" => unix_to_jellyfin_date(created_at),
                "Name" => name.to_string(),
                "Type" => log_type.to_string(),
                "Severity" => severity.to_string(),
                "User" => username.to_string(),
                "UserId" => user_id.to_string(),
                "Item" => item_name.to_string(),
                _ => String::new(),
            };
            report_cell(field, value)
        })
        .collect()
}

fn report_item_columns(
    fields: &[String],
    row: &sea_orm::QueryResult,
) -> anyhow::Result<Vec<JsonValue>> {
    let runtime_minutes = row
        .get_opt_i64("runtime_ticks")?
        .map(|ticks| ticks / 600_000_000)
        .unwrap_or_default();
    let created_at = row.get_i64("created_at").unwrap_or_default();
    let values = [
        ("Name", row.get_str("title")?),
        ("Path", row.get_str("path")?),
        ("Type", row.get_str("item_type")?),
        (
            "Year",
            row.get_opt_i64("production_year")?
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("DateAdded", unix_to_jellyfin_date(created_at)),
        ("Runtime", runtime_minutes.to_string()),
        (
            "ParentalRating",
            row.get_opt_str("official_rating")?.unwrap_or_default(),
        ),
        (
            "CommunityRating",
            row.get_f64("community_rating")?
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "CriticRating",
            row.get_f64("critic_rating")?
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "SeasonNumber",
            row.get_opt_i64("season_number")?
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "EpisodeNumber",
            row.get_opt_i64("episode_number")?
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("Overview", row.get_opt_str("overview")?.unwrap_or_default()),
    ];
    Ok(fields
        .iter()
        .map(|field| {
            let value = values
                .iter()
                .find(|(name, _)| *name == field)
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            report_cell(field, value)
        })
        .collect())
}

fn report_cell(id: &str, name: String) -> JsonValue {
    json!({
        "Id": id,
        "Name": name,
        "Image": "",
        "CustomTag": ""
    })
}

fn report_csv(headers: &[JsonValue], rows: &[JsonValue]) -> String {
    let fields = report_header_fields(headers);
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(
        headers
            .iter()
            .map(|header| csv_escape(header["Name"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(","),
    );
    for row in rows {
        let cells = row["Columns"].as_array().cloned().unwrap_or_default();
        lines.push(
            fields
                .iter()
                .map(|field| {
                    let value = cells
                        .iter()
                        .find(|cell| cell["Id"].as_str().is_some_and(|id| id == field))
                        .and_then(|cell| cell["Name"].as_str())
                        .unwrap_or_default();
                    csv_escape(value)
                })
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\r\n")
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn report_is_activity_view(query: &HashMap<String, String>) -> bool {
    query_value(query, "ReportView")
        .is_some_and(|value| value.eq_ignore_ascii_case("ReportActivities"))
}

fn report_limit(query: &HashMap<String, String>) -> u64 {
    query_value(query, "Limit")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000)
}

fn report_start_index(query: &HashMap<String, String>) -> u64 {
    query_value(query, "StartIndex")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn report_query_bool(query: &HashMap<String, String>, key: &str) -> Option<bool> {
    query_value(query, key).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    })
}

fn report_query_list(query: &HashMap<String, String>, key: &str) -> Vec<String> {
    query_value(query, key)
        .map(|value| {
            value
                .split([',', '|'])
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub async fn live_stream_unavailable() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "Error": "Live streams are not available" })),
    )
        .into_response()
}

pub async fn live_stream_media_info() -> Response {
    Json(json!({ "MediaSources": [] })).into_response()
}

pub async fn live_tv_info() -> Response {
    Json(json!({
        "Services": [],
        "IsEnabled": false,
        "EnabledUsers": []
    }))
    .into_response()
}

pub async fn live_tv_guide_info() -> Response {
    Json(json!({
        "StartDate": null,
        "EndDate": null
    }))
    .into_response()
}

pub async fn live_tv_channel_mapping_options() -> Response {
    Json(live_tv_channel_mapping_options_value()).into_response()
}

pub async fn live_tv_default_listing_provider() -> Response {
    Json(live_tv_default_listing_provider_value()).into_response()
}

pub async fn live_tv_default_tuner_host(Path(tuner_type): Path<String>) -> Response {
    Json(live_tv_default_tuner_host_value(&tuner_type)).into_response()
}

pub async fn live_tv_timer_defaults() -> Response {
    Json(live_tv_timer_defaults_value()).into_response()
}

pub async fn live_tv_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Live TV is not available" })),
    )
        .into_response()
}

fn live_tv_timer_defaults_value() -> JsonValue {
    json!({
        "Id": "",
        "Type": "SeriesTimer",
        "ServerId": SERVER_NAME,
        "ExternalId": "",
        "ChannelId": "00000000-0000-0000-0000-000000000000",
        "ExternalChannelId": "",
        "ChannelName": "",
        "ProgramId": null,
        "ExternalProgramId": "",
        "Name": "",
        "Overview": "",
        "StartDate": "1970-01-01T00:00:00Z",
        "EndDate": "1970-01-01T00:00:00Z",
        "ServiceName": "",
        "Priority": 0,
        "PrePaddingSeconds": 0,
        "PostPaddingSeconds": 0,
        "IsPrePaddingRequired": false,
        "IsPostPaddingRequired": false,
        "KeepUntil": "UntilDeleted",
        "RecordAnyTime": false,
        "SkipEpisodesInLibrary": false,
        "RecordAnyChannel": false,
        "KeepUpTo": 0,
        "RecordNewOnly": false,
        "Days": [],
        "DayPattern": null,
        "ImageTags": {},
        "ParentBackdropImageTags": []
    })
}

fn live_tv_channel_mapping_options_value() -> JsonValue {
    json!({
        "TunerChannels": [],
        "ProviderChannels": [],
        "Mappings": [],
        "ProviderName": null
    })
}

fn live_tv_default_listing_provider_value() -> JsonValue {
    json!({
        "Name": "Schedules Direct",
        "SetupUrl": "",
        "Id": "",
        "Type": "SchedulesDirect",
        "Username": "",
        "Password": "",
        "ListingsId": "",
        "ZipCode": "",
        "Country": "",
        "Path": "",
        "EnabledTuners": [],
        "EnableAllTuners": false,
        "NewsCategories": [],
        "SportsCategories": [],
        "KidsCategories": [],
        "MovieCategories": [],
        "ChannelMappings": [],
        "MoviePrefix": "",
        "PreferredLanguage": "",
        "UserAgent": ""
    })
}

fn live_tv_default_tuner_host_value(tuner_type: &str) -> JsonValue {
    json!({
        "Id": "",
        "Url": "",
        "Type": tuner_type,
        "DeviceId": "",
        "FriendlyName": "",
        "ImportFavoritesOnly": false,
        "AllowHWTranscoding": false,
        "AllowFmp4TranscodingContainer": false,
        "AllowStreamSharing": false,
        "FallbackMaxStreamingBitrate": 0,
        "EnableStreamLooping": false,
        "Source": "",
        "TunerCount": 0,
        "UserAgent": "",
        "IgnoreDts": false,
        "ReadAtNativeFramerate": false
    })
}

pub async fn openapi_json() -> Response {
    match tokio::fs::read("docs/jellyfin-openapi-stable.json").await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn encoding_codec_configuration_defaults() -> Response {
    Json(json!({
        "IsEnabled": false,
        "Priority": 0,
        "Codec": "",
        "HardwareCodecs": [],
        "Profiles": []
    }))
    .into_response()
}

pub async fn encoding_video_codec_information() -> Response {
    Json(Vec::<JsonValue>::new()).into_response()
}

#[cfg(test)]
mod tests {
    use super::channel_features_value;
    use super::{
        CameraUploadQuery, CustomQueryRequest, DeviceRecord, FALLBACK_FONTS_PATH,
        TmdbApiKeyRequest, TmdbProxyUrlRequest, activity_log_entry_json, activity_log_query,
        camera_upload_history_value, chapter_images_task_for_state, client_log_document,
        client_log_file_name, default_branding_options, default_plugin_repositories,
        default_scan_library_triggers, device_info, device_options_result, empty_query_result,
        fallback_font_entries, fallback_font_mime_type, fallback_font_path,
        game_system_display_name, image_by_name_info, is_known_scheduled_task,
        is_safe_fallback_font_name, is_safe_log_name, items_access_value, last_task_result,
        live_tv_channel_mapping_options, live_tv_channel_mapping_options_value,
        live_tv_default_listing_provider, live_tv_default_listing_provider_value,
        live_tv_default_tuner_host, live_tv_default_tuner_host_value, live_tv_guide_info,
        live_tv_info, live_tv_recording_folders, live_tv_timer_defaults,
        live_tv_timer_defaults_value, live_tv_unavailable, log_file_entry,
        normalize_branding_options, normalize_device_custom_name, normalize_device_id,
        normalize_device_ids, normalize_notification_ids, normalize_notification_level,
        normalize_notification_text, normalize_plugin_repositories,
        normalize_scheduled_task_triggers, notification_items, notification_services_test,
        notification_services_value, package_install_unavailable, package_list,
        package_update_list, party_unavailable, play_activity_rows, plugin_list,
        report_activity_headers_for_query, report_csv, report_item_headers_for_query,
        reports_activity_result, reports_items_result, required_upload_part,
        run_user_usage_custom_query, safe_log_path, safe_user_usage_backup_file,
        sanitize_file_part, save_camera_upload_to, scan_library_task, smtp_notification_test,
        stop_scheduled_task, sync_data, sync_data_result, sync_empty_query_result,
        sync_empty_response, sync_options_result, sync_play_unavailable, sync_unavailable,
        system_info_value, system_log_file, system_log_lines, system_logs_query,
        tmdb_client_configuration_value, ui_command, update_notification_read_state,
        usage_stats_breakdown_items, usage_stats_duration_histogram_items,
        usage_stats_hourly_items, usage_stats_session_entry, usage_user_entry,
        user_usage_stats_load_backup_from, user_usage_stats_save_backup_to,
        user_usage_stats_user_manage, user_view_grouping_options_value,
        validate_path_request_from_inputs, web_strings_value,
    };
    use super::{
        DailyTaskSchedule, DeviceIdQuery, DirectoryContentsQuery, ParentPathQuery,
        ValidatePathRequest, device_options_key, next_daily_task_schedule, next_daily_trigger,
        queue_chapter_images_task, recover_stale_chapter_image_task, set_app_setting,
        upsert_task_result,
    };
    use crate::app::state::AppState;
    use crate::app::state::{PlaybackSession, PlaybackState, SessionCapabilities};
    use crate::entities::{
        access_tokens, activity_log, media_items, playback_watch_days, task_results, users,
    };
    use axum::{
        Json,
        body::Bytes,
        extract::{Extension, Path, Query, State},
        http::HeaderMap,
        response::IntoResponse,
    };
    use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait, Set};
    use std::{collections::HashMap, sync::Arc, time::Duration};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[test]
    fn sync_options_result_has_jellyfin_shape() {
        let options = sync_options_result();
        assert!(options["Targets"].as_array().is_some());
        assert!(options["Options"].as_array().is_some());
        assert!(options["QualityOptions"].as_array().is_some());
        assert!(options["ProfileOptions"].as_array().is_some());
        assert!(sync_data_result()["ItemIdsToRemove"].as_array().is_some());
    }

    #[tokio::test]
    async fn recording_folders_return_jellyfin_query_result_shape() {
        let response = live_tv_recording_folders().await.into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["Items"].as_array().unwrap().is_empty());
        assert_eq!(value["TotalRecordCount"], 0);
    }

    #[test]
    fn system_info_reports_server_ids() {
        let full = system_info_value(
            "Home".to_string(),
            true,
            true,
            Some("http://media.local:8096".to_string()),
        );
        assert_eq!(full["Id"], "jellyfin-rs");
        assert_eq!(full["ServerId"], "jellyfin-rs");
        assert_eq!(full["ServerName"], "Home");
        assert_eq!(full["ProductName"], "Emby Server");
        assert_eq!(full["PackageName"], "jellyfin-rs");
        assert_eq!(full["SystemUpdateLevel"], "Release");
        assert_eq!(full["Version"], "4.8.0.0");
        assert_eq!(full["LocalAddress"], "http://media.local:8096");
        assert_eq!(full["WanAddress"], "http://media.local:8096");
        assert_eq!(full["HttpServerPortNumber"], 8096);
        assert_eq!(full["WebSocketPortNumber"], 8096);
        assert_eq!(full["SupportsHttps"], false);
        assert_eq!(full["SupportsAutoRunAtStartup"], false);
        assert_eq!(full["HardwareAccelerationRequiresPremiere"], false);
        assert_eq!(full["HasPendingRestart"], false);
        assert_eq!(full["IsShuttingDown"], false);
        assert_eq!(full["EncoderLocation"], "System");

        let public = system_info_value(
            "Home".to_string(),
            false,
            false,
            Some("https://media.example".to_string()),
        );
        assert_eq!(public["Id"], "jellyfin-rs");
        assert_eq!(public["ServerId"], "jellyfin-rs");
        assert_eq!(public["ProductName"], "Emby Server");
        assert_eq!(public["LocalAddress"], "https://media.example");
        assert_eq!(public["WanAddress"], "https://media.example");
    }

    #[tokio::test]
    async fn sync_empty_query_result_has_start_index() {
        let response = sync_empty_query_result().await.into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["TotalRecordCount"], 0);
        assert_eq!(value["StartIndex"], 0);
        assert!(value["Items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn activity_log_entry_uses_model_severity() {
        let entry = activity_log::Model {
            id: "a1".to_string(),
            name: "Scanned".to_string(),
            log_type: "Task".to_string(),
            user_id: Some("u1".to_string()),
            item_id: None,
            severity: "Warning".to_string(),
            created_at: 1,
        };
        let value = activity_log_entry_json(&entry);
        assert_eq!(value["Severity"], "Warning");
        assert_eq!(value["UserId"], "u1");
    }

    #[tokio::test]
    async fn activity_log_query_count_uses_user_filter() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, user_id) in [("global", None), ("user", Some("u1"))] {
            activity_log::Entity::insert(activity_log::ActiveModel {
                id: Set(id.to_string()),
                name: Set(id.to_string()),
                log_type: Set("Task".to_string()),
                user_id: Set(user_id.map(ToString::to_string)),
                item_id: Set(None),
                severity: Set("Info".to_string()),
                created_at: Set(1),
            })
            .exec(&db)
            .await
            .unwrap();
        }

        assert_eq!(activity_log_query(false).count(&db).await.unwrap(), 2);
        assert_eq!(activity_log_query(true).count(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sync_data_reports_empty_response() {
        assert_eq!(
            sync_data().await.into_response().status(),
            axum::http::StatusCode::OK
        );
    }

    #[tokio::test]
    async fn sync_empty_response_reports_success() {
        assert_eq!(
            sync_empty_response().await.into_response().status(),
            axum::http::StatusCode::OK
        );
    }

    #[test]
    fn environment_requests_accept_jellyfin_and_emby_casing() {
        let directory: DirectoryContentsQuery = serde_json::from_value(serde_json::json!({
            "Path": "D:/Media",
            "IncludeFiles": true,
            "IncludeDirectories": true
        }))
        .unwrap();
        assert_eq!(directory.path, "D:/Media");
        assert!(directory.include_files);
        assert!(directory.include_directories);

        let parent: ParentPathQuery = serde_json::from_value(serde_json::json!({
            "Path": "D:/Media/Movie"
        }))
        .unwrap();
        assert_eq!(parent.path, "D:/Media/Movie");

        let validate: ValidatePathRequest = serde_json::from_value(serde_json::json!({
            "Path": "D:/Media",
            "ValidateWriteable": true
        }))
        .unwrap();
        assert_eq!(validate.path.as_deref(), Some("D:/Media"));
        assert!(validate.validate_writable);
    }

    #[test]
    fn validate_path_request_merges_emby_query_parameters() {
        let mut query = HashMap::new();
        query.insert("Path".to_string(), "D:/Media".to_string());
        query.insert("IsFile".to_string(), "true".to_string());
        query.insert("ValidateWriteable".to_string(), "yes".to_string());

        let request = validate_path_request_from_inputs(&query, ValidatePathRequest::default());
        assert_eq!(request.path.as_deref(), Some("D:/Media"));
        assert_eq!(request.is_file, Some(true));
        assert!(request.validate_writable);

        let body_request = ValidatePathRequest {
            path: Some("E:/Body".to_string()),
            is_file: Some(false),
            validate_writable: false,
        };
        let request = validate_path_request_from_inputs(&query, body_request);
        assert_eq!(request.path.as_deref(), Some("E:/Body"));
        assert_eq!(request.is_file, Some(false));
        assert!(request.validate_writable);
    }

    #[test]
    fn device_record_has_jellyfin_shape_without_token() {
        let session = PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
            user_name: "alice".to_string(),
            play_session_id: "p1".to_string(),
            item_id: "i1".to_string(),
            item_name: None,
            now_playing_queue: Vec::new(),
            additional_users: Vec::new(),
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
            playable_media_types: vec!["Audio".to_string(), "Video".to_string()],
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
                playable_media_types: vec!["Audio".to_string(), "Video".to_string()],
                supported_commands: vec!["Play".to_string()],
                supports_media_control: true,
                supports_persistent_identifier: true,
            },
        };
        let device = DeviceRecord::from_session(&session).to_json(None);
        assert_eq!(device["Id"], "device-1");
        assert_eq!(device["Name"], "Browser");
        assert_eq!(device["AppName"], "Web");
        assert!(device["AccessToken"].is_null());
        assert_eq!(device["Capabilities"]["PlayableMediaTypes"][0], "Audio");
    }

    #[test]
    fn device_options_result_has_jellyfin_shape() {
        let options = device_options_result("device-1", None);
        assert_eq!(options["Id"], 0);
        assert_eq!(options["DeviceId"], "device-1");
        assert!(options["CustomName"].is_null());
    }

    #[test]
    fn device_options_result_keeps_custom_name() {
        let options = device_options_result("device-1", Some("Living Room".to_string()));
        assert_eq!(options["CustomName"], "Living Room");
    }

    #[test]
    fn device_custom_name_is_trimmed_and_limited() {
        assert_eq!(
            normalize_device_custom_name(Some("  Living Room  "))
                .unwrap()
                .as_deref(),
            Some("Living Room")
        );
        assert!(normalize_device_custom_name(None).unwrap().is_none());
        assert!(normalize_device_custom_name(Some("   ")).unwrap().is_none());
        assert!(normalize_device_custom_name(Some("bad\nname")).is_err());
        assert!(normalize_device_custom_name(Some(&"x".repeat(129))).is_err());
    }

    #[test]
    fn device_ids_are_normalized_and_limited() {
        assert_eq!(
            normalize_device_id(Some("  device-1  ")).unwrap(),
            "device-1"
        );
        assert!(normalize_device_id(None).is_err());
        assert!(normalize_device_id(Some("bad\ndevice")).is_err());
        assert!(normalize_device_id(Some(&"x".repeat(super::MAX_DEVICE_ID_LEN + 1))).is_err());

        let ids = normalize_device_ids(Some(" d1, d2,,d1 ")).unwrap();
        assert_eq!(ids, vec!["d1".to_string(), "d2".to_string()]);
        assert!(normalize_device_ids(Some("bad\nid")).is_err());
        let too_many = (0..super::MAX_DEVICE_IDS_PER_REQUEST + 1)
            .map(|index| format!("d{index}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(normalize_device_ids(Some(&too_many)).is_err());
    }

    #[test]
    fn device_record_json_keeps_custom_name() {
        let token = access_tokens::Model {
            id: "t1".to_string(),
            user_id: "u1".to_string(),
            token_hash: "hash".to_string(),
            name: Some("Web".to_string()),
            device_id: Some("device-1".to_string()),
            created_at: 1,
            last_used_at: Some(2),
            expires_at: None,
            revoked_at: None,
        };
        let device = DeviceRecord::from_token(&token).to_json(Some("Living Room".to_string()));
        assert_eq!(device["CustomName"], "Living Room");
        assert_eq!(device["Id"], "device-1");
    }

    #[tokio::test]
    async fn device_info_reports_saved_custom_name() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        users::Entity::insert(users::ActiveModel {
            id: Set("u1".to_string()),
            username: Set("alice".to_string()),
            password_hash: Set(None),
            display_name: Set("Alice".to_string()),
            is_admin: Set(1),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            last_login_at: Set(None),
        })
        .exec(&db)
        .await
        .unwrap();
        access_tokens::Entity::insert(access_tokens::ActiveModel {
            id: Set("t1".to_string()),
            user_id: Set("u1".to_string()),
            token_hash: Set("hash".to_string()),
            name: Set(Some("Web".to_string())),
            device_id: Set(Some("device-1".to_string())),
            created_at: Set(1),
            last_used_at: Set(Some(2)),
            expires_at: Set(None),
            revoked_at: Set(None),
        })
        .exec(&db)
        .await
        .unwrap();
        set_app_setting(&db, &device_options_key("device-1"), "Living Room")
            .await
            .unwrap();
        let state = Arc::new(test_state(db));

        let response = device_info(
            State(state),
            Query(DeviceIdQuery {
                id: Some("device-1".to_string()),
            }),
        )
        .await
        .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["CustomName"], "Living Room");
    }

    #[tokio::test]
    async fn camera_upload_history_has_emby_shape() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let history = camera_upload_history_value(&db, "device-1").await.unwrap();
        assert_eq!(history["DeviceId"], "device-1");
        assert!(history["FilesUploaded"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn camera_upload_persists_history() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let upload_root =
            std::env::temp_dir().join(format!("jellyfin-rs-camera-test-{}", uuid::Uuid::new_v4()));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("image/jpeg"),
        );
        save_camera_upload_to(
            &db,
            upload_root.clone(),
            headers,
            CameraUploadQuery {
                device_id: Some("phone".to_string()),
                album: Some("Camera".to_string()),
                name: Some("../photo.jpg".to_string()),
                id: Some("p1".to_string()),
            },
            axum::body::Bytes::from_static(b"jpg"),
        )
        .await
        .unwrap();

        let history = camera_upload_history_value(&db, "phone").await.unwrap();
        assert_eq!(history["FilesUploaded"][0]["Id"], "p1");
        assert_eq!(history["FilesUploaded"][0]["MimeType"], "image/jpeg");
        assert_eq!(sanitize_file_part("../photo.jpg"), "photo.jpg");
        assert!(required_upload_part(None, "DeviceId").is_err());
        assert!(required_upload_part(Some("bad\nalbum".to_string()), "Album").is_err());
        assert!(
            required_upload_part(
                Some("x".repeat(super::MAX_CAMERA_UPLOAD_FIELD_LEN + 1)),
                "Name"
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(upload_root);
    }

    #[test]
    fn device_record_from_token_has_jellyfin_shape() {
        let token = access_tokens::Model {
            id: "t1".to_string(),
            user_id: "u1".to_string(),
            token_hash: "hash".to_string(),
            name: Some("Web".to_string()),
            device_id: Some("device-1".to_string()),
            created_at: 1,
            last_used_at: Some(2),
            expires_at: None,
            revoked_at: None,
        };
        let device = DeviceRecord::from_token(&token).to_json(None);
        assert_eq!(device["Id"], "device-1");
        assert_eq!(device["Name"], "device-1");
        assert_eq!(device["AppName"], "Web");
        assert_eq!(device["LastUserId"], "u1");
    }

    #[test]
    fn scan_library_default_triggers_have_jellyfin_shape() {
        let triggers = default_scan_library_triggers();
        assert_eq!(triggers[0]["Type"], "StartupTrigger");
        assert!(triggers.as_array().is_some());
    }

    #[test]
    fn scheduled_task_triggers_are_normalized_and_limited() {
        let triggers = normalize_scheduled_task_triggers(serde_json::json!([
            { "type": "intervaltrigger", "intervalTicks": "3000000000", "Ignored": true },
            { "Type": "WeeklyTrigger", "TimeOfDayTicks": 36000000000_i64, "DayOfWeek": "monday" },
            { "Type": "StartupTrigger", "SystemEvent": "WakeFromSleep" }
        ]))
        .unwrap();
        assert_eq!(triggers[0]["Type"], "IntervalTrigger");
        assert_eq!(triggers[0]["IntervalTicks"], 3000000000_i64);
        assert!(triggers[0]["Ignored"].is_null());
        assert_eq!(triggers[1]["DayOfWeek"], "Monday");
        assert_eq!(triggers[2]["SystemEvent"], "WakeFromSleep");

        assert!(normalize_scheduled_task_triggers(serde_json::json!({})).is_err());
        assert!(normalize_scheduled_task_triggers(serde_json::json!([true])).is_err());
        assert!(
            normalize_scheduled_task_triggers(serde_json::json!([
                { "Type": "UnknownTrigger" }
            ]))
            .is_err()
        );
        assert!(
            normalize_scheduled_task_triggers(serde_json::json!([
                { "Type": "DailyTrigger", "TimeOfDayTicks": -1 }
            ]))
            .is_err()
        );
        assert!(
            normalize_scheduled_task_triggers(serde_json::json!([
                { "Type": "WeeklyTrigger", "DayOfWeek": "Funday" }
            ]))
            .is_err()
        );
    }

    #[test]
    fn empty_repository_list_has_jellyfin_shape() {
        let repositories = default_plugin_repositories();
        assert!(repositories.as_array().is_some());
    }

    #[test]
    fn plugin_repositories_are_normalized_and_limited() {
        let repositories = normalize_plugin_repositories(serde_json::json!([
            {
                "name": " Stable ",
                "url": "https://repo.jellyfin.org/releases/plugin/manifest-stable.json",
                "enabled": "true",
                "Ignored": "field"
            }
        ]))
        .unwrap();
        assert_eq!(repositories[0]["Name"], "Stable");
        assert_eq!(
            repositories[0]["Url"],
            "https://repo.jellyfin.org/releases/plugin/manifest-stable.json"
        );
        assert_eq!(repositories[0]["Enabled"], true);
        assert!(repositories[0]["Ignored"].is_null());

        assert!(normalize_plugin_repositories(serde_json::json!({})).is_err());
        assert!(
            normalize_plugin_repositories(serde_json::json!([
                { "Name": "bad", "Url": "file:///tmp/repo.json", "Enabled": true }
            ]))
            .is_err()
        );
        assert!(
            normalize_plugin_repositories(serde_json::json!([
                { "Name": "bad\nname", "Url": "https://example.com/repo.json" }
            ]))
            .is_err()
        );
    }

    #[test]
    fn package_list_has_jellyfin_shape() {
        assert!(package_list().is_empty());
    }

    #[test]
    fn package_updates_have_emby_shape() {
        assert!(package_update_list().is_empty());
    }

    #[test]
    fn disabled_channels_have_jellyfin_shapes() {
        let channels = empty_query_result();
        assert!(channels["Items"].as_array().unwrap().is_empty());
        assert_eq!(channels["TotalRecordCount"], 0);
        assert_eq!(channels["StartIndex"], 0);

        let features = channel_features_value();
        assert_eq!(features["CanSearch"], false);
        assert!(features["MediaTypes"].as_array().unwrap().is_empty());
        assert!(features["ContentTypes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn package_install_reports_unavailable() {
        let response = package_install_unavailable().await.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn compatibility_write_stubs_report_unavailable() {
        assert_eq!(
            party_unavailable().await.into_response().status(),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            sync_play_unavailable().await.into_response().status(),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            sync_unavailable().await.into_response().status(),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
    }

    #[tokio::test]
    async fn live_tv_reports_disabled_or_unavailable() {
        assert_eq!(
            live_tv_info().await.into_response().status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            live_tv_guide_info().await.into_response().status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            live_tv_channel_mapping_options()
                .await
                .into_response()
                .status(),
            axum::http::StatusCode::OK
        );
        let mappings = live_tv_channel_mapping_options_value();
        assert!(mappings["TunerChannels"].as_array().unwrap().is_empty());
        assert!(mappings["ProviderChannels"].as_array().unwrap().is_empty());
        assert!(mappings["Mappings"].as_array().unwrap().is_empty());
        assert_eq!(
            live_tv_default_listing_provider()
                .await
                .into_response()
                .status(),
            axum::http::StatusCode::OK
        );
        let provider = live_tv_default_listing_provider_value();
        assert_eq!(provider["Type"], "SchedulesDirect");
        assert!(provider["ChannelMappings"].as_array().unwrap().is_empty());
        assert_eq!(
            live_tv_default_tuner_host(Path("M3U".to_string()))
                .await
                .into_response()
                .status(),
            axum::http::StatusCode::OK
        );
        let tuner = live_tv_default_tuner_host_value("M3U");
        assert_eq!(tuner["Type"], "M3U");
        assert_eq!(tuner["TunerCount"], 0);
        assert_eq!(
            live_tv_timer_defaults().await.into_response().status(),
            axum::http::StatusCode::OK
        );
        let defaults = live_tv_timer_defaults_value();
        assert_eq!(defaults["Type"], "SeriesTimer");
        assert_eq!(defaults["KeepUntil"], "UntilDeleted");
        assert_eq!(defaults["RecordAnyChannel"], false);
        assert_eq!(
            live_tv_unavailable().await.into_response().status(),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn start_scheduled_task_rejects_unknown_task() {
        assert!(is_known_scheduled_task("scan-library"));
        assert!(is_known_scheduled_task("RefreshChapterImages"));
        assert!(!is_known_scheduled_task("missing"));
    }

    #[tokio::test]
    async fn scheduled_task_has_jellyfin_shape() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        let task = scan_library_task(&db).await;
        assert_eq!(task["Id"], "scan-library");
        assert_eq!(task["Key"], "scan-library");
        assert_eq!(task["Name"], "Scan media library");
        assert_eq!(task["State"], "Idle");
        assert!(task["Triggers"].as_array().is_some());
        assert!(task["LastExecutionResult"].is_null());

        let state = test_state(db);
        let chapter_task = chapter_images_task_for_state(&state).await;
        assert_eq!(chapter_task["Id"], "RefreshChapterImages");
        assert_eq!(chapter_task["Name"], "Extract chapter images");
        assert_eq!(chapter_task["Triggers"][0]["Type"], "DailyTrigger");
        assert_eq!(
            chapter_task["Triggers"][0]["TimeOfDayTicks"],
            72_000_000_000_i64
        );
    }

    #[test]
    fn daily_trigger_uses_local_wall_clock_and_rolls_to_tomorrow() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
        let two_am_ticks = 2 * 60 * 60 * 10_000_000_i64;

        let before = date.and_hms_opt(1, 30, 0).unwrap();
        assert_eq!(
            next_daily_trigger(before, two_am_ticks),
            Some(date.and_hms_opt(2, 0, 0).unwrap())
        );

        let exact = date.and_hms_opt(2, 0, 0).unwrap();
        assert_eq!(next_daily_trigger(exact, two_am_ticks), Some(exact));

        let after = date.and_hms_opt(2, 0, 1).unwrap();
        assert_eq!(
            next_daily_trigger(after, two_am_ticks),
            Some(date.succ_opt().unwrap().and_hms_opt(2, 0, 0).unwrap())
        );
    }

    #[test]
    fn daily_schedule_selects_nearest_trigger_and_its_runtime_limit() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 7, 21)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        let triggers = serde_json::json!([
            {
                "Type": "DailyTrigger",
                "TimeOfDayTicks": 4 * 60 * 60 * 10_000_000_i64,
                "MaxRuntimeTicks": 8 * 60 * 60 * 10_000_000_i64
            },
            {
                "Type": "DailyTrigger",
                "TimeOfDayTicks": 2 * 60 * 60 * 10_000_000_i64,
                "MaxRuntimeTicks": 4 * 60 * 60 * 10_000_000_i64
            },
            { "Type": "StartupTrigger" }
        ]);

        assert_eq!(
            next_daily_task_schedule(&triggers, now),
            Some(DailyTaskSchedule {
                delay: Duration::from_secs(60 * 60),
                max_runtime: Some(Duration::from_secs(4 * 60 * 60)),
            })
        );
    }

    #[tokio::test]
    async fn chapter_image_task_rejects_concurrent_execution() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = Arc::new(test_state(db));
        let (cancel, _cancellation) = tokio::sync::watch::channel(false);
        *state.chapter_image_task_cancel.lock().await = Some(cancel);

        assert!(!queue_chapter_images_task(state.clone(), None).await);
        assert_eq!(
            chapter_images_task_for_state(&state).await["State"],
            "Running"
        );
    }

    #[tokio::test]
    async fn stale_chapter_image_task_is_cancelled_on_startup() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = test_state(db);
        upsert_task_result(
            &state,
            "RefreshChapterImages",
            "Running",
            1,
            1,
            Some("running"),
        )
        .await;

        recover_stale_chapter_image_task(&state).await;

        let result = last_task_result(&state.db, "RefreshChapterImages")
            .await
            .unwrap();
        assert_eq!(result["Status"], "Cancelled");
        assert_eq!(
            result["ErrorMessage"],
            "Chapter image refresh was interrupted by a server restart"
        );
    }

    #[tokio::test]
    async fn task_result_uses_jellyfin_error_fields() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        task_results::Entity::insert(task_results::ActiveModel {
            task_id: Set("scan-library".to_string()),
            status: Set("Failed".to_string()),
            start_time: Set(Some(1)),
            end_time: Set(Some(2)),
            message: Set(Some("boom".to_string())),
        })
        .exec(&db)
        .await
        .unwrap();

        let result = last_task_result(&db, "scan-library").await.unwrap();
        assert_eq!(result["Id"], "scan-library");
        assert_eq!(result["Key"], "scan-library");
        assert_eq!(result["Name"], "Scan media library");
        assert_eq!(result["ErrorMessage"], "boom");
        assert_eq!(result["LongErrorMessage"], "boom");
        assert!(!result.as_object().unwrap().contains_key("Message"));
    }

    #[tokio::test]
    async fn stop_scheduled_task_records_cancelled_result() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = Arc::new(test_state(db));

        let response =
            stop_scheduled_task(State(state.clone()), Path("scan-library".to_string())).await;

        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        let result = last_task_result(&state.db, "scan-library").await.unwrap();
        assert_eq!(result["Status"], "Cancelled");
    }

    #[tokio::test]
    async fn ui_command_is_safe_noop() {
        assert_eq!(
            ui_command().await.status(),
            axum::http::StatusCode::NO_CONTENT
        );
    }

    #[test]
    fn plugin_list_has_jellyfin_shape() {
        assert!(plugin_list().is_empty());
    }

    #[test]
    fn default_branding_options_have_jellyfin_shape() {
        let options = default_branding_options();
        assert_eq!(options["LoginDisclaimer"], "");
        assert_eq!(options["CustomCss"], "");
        assert_eq!(options["SplashscreenEnabled"], false);
    }

    #[test]
    fn branding_options_are_normalized_and_limited() {
        let options = normalize_branding_options(serde_json::json!({
            "LoginDisclaimer": "Welcome\n",
            "CustomCss": "body { color: red; }\t",
            "SplashscreenEnabled": "true",
            "Ignored": true
        }))
        .unwrap();
        assert_eq!(options["LoginDisclaimer"], "Welcome\n");
        assert_eq!(options["CustomCss"], "body { color: red; }\t");
        assert_eq!(options["SplashscreenEnabled"], true);
        assert!(options["Ignored"].is_null());

        assert!(normalize_branding_options(serde_json::json!([])).is_err());
        assert!(
            normalize_branding_options(serde_json::json!({
                "LoginDisclaimer": "bad\0text"
            }))
            .is_err()
        );
        assert!(
            normalize_branding_options(serde_json::json!({
                "CustomCss": "x".repeat(super::MAX_BRANDING_CUSTOM_CSS_BYTES + 1)
            }))
            .is_err()
        );

        assert!(super::allowed_branding_image_content_type("image/png"));
        assert!(super::allowed_branding_image_content_type("image/jpeg"));
        assert!(!super::allowed_branding_image_content_type("image/svg+xml"));
        assert!(!super::allowed_branding_image_content_type("image/unknown"));
    }

    #[test]
    fn web_strings_are_not_empty() {
        let strings = web_strings_value();
        assert_eq!(strings["HeaderLogin"], "Login");
        assert_eq!(strings["ButtonSave"], "Save");
    }

    #[test]
    fn user_view_grouping_options_are_not_empty() {
        let options = user_view_grouping_options_value();
        assert!(options.as_array().is_some_and(|items| !items.is_empty()));
        assert_eq!(options[0]["Id"], "none");
    }

    #[test]
    fn image_by_name_info_has_emby_shape() {
        let info = image_by_name_info("logo", "", "General");
        assert_eq!(info["Name"], "logo");
        assert_eq!(info["Context"], "General");
        assert_eq!(info["Format"], "png");
        assert!(info["FileLength"].as_i64().is_some_and(|len| len > 0));
    }

    #[test]
    fn game_system_display_name_is_stable() {
        assert_eq!(game_system_display_name("nes"), "NES");
        assert_eq!(game_system_display_name("unknown"), "Unknown");
    }

    #[test]
    fn usage_user_entry_has_plugin_shape() {
        let user = users::Model {
            id: "u1".to_string(),
            username: "alice".to_string(),
            password_hash: None,
            display_name: "Alice".to_string(),
            is_admin: 1,
            is_disabled: 0,
            created_at: 0,
            updated_at: 0,
            last_login_at: Some(0),
        };
        let entry = usage_user_entry(&user);
        assert_eq!(entry["Id"], "u1");
        assert_eq!(entry["Name"], "alice");
        assert_eq!(entry["IsAdministrator"], true);
        assert_eq!(entry["IsDisabled"], false);
        assert!(entry["LastLoginDate"].as_str().is_some());
    }

    #[test]
    fn items_access_value_reports_accessible_libraries() {
        let value = items_access_value(
            "u1",
            &[serde_json::json!({
                "Id": "lib1",
                "Name": "Movies",
                "CollectionType": "movies"
            })],
        );
        assert_eq!(value["UserId"], "u1");
        assert_eq!(value["TotalRecordCount"], 1);
        assert_eq!(value["Items"][0]["ItemId"], "lib1");
        assert_eq!(value["Items"][0]["HasAccess"], true);
        assert_eq!(value["Items"][0]["CanPlay"], true);
    }

    #[tokio::test]
    async fn items_access_rejects_body_user_for_non_admin() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        users::Entity::insert(users::ActiveModel {
            id: Set("u1".to_string()),
            username: Set("alice".to_string()),
            password_hash: Set(None),
            display_name: Set("Alice".to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            last_login_at: Set(None),
        })
        .exec(&db)
        .await
        .unwrap();
        let state = Arc::new(test_state(db));

        let response = super::items_access(
            State(state),
            Extension("u1".to_string()),
            Query(HashMap::new()),
            Some(Json(serde_json::json!({ "UserId": "u2" }))),
        )
        .await
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn items_access_allows_body_user_for_admin() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        users::Entity::insert(users::ActiveModel {
            id: Set("admin".to_string()),
            username: Set("admin".to_string()),
            password_hash: Set(None),
            display_name: Set("Admin".to_string()),
            is_admin: Set(1),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            last_login_at: Set(None),
        })
        .exec(&db)
        .await
        .unwrap();
        let state = Arc::new(test_state(db));

        let response = super::items_access(
            State(state),
            Extension("admin".to_string()),
            Query(HashMap::new()),
            Some(Json(serde_json::json!({ "UserId": "u2" }))),
        )
        .await
        .into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(value["UserId"], "u2");
    }

    #[test]
    fn tmdb_client_configuration_reports_compatible_enabled_fields() {
        let enabled = tmdb_client_configuration_value(true, true, Some("https://tmdb.qb.edu.kg"));
        assert_eq!(enabled["ProviderEnabled"], true);
        assert_eq!(enabled["Configured"], true);
        assert_eq!(enabled["IsTmdbEnabled"], true);
        assert_eq!(enabled["IsEnabled"], true);
        assert_eq!(enabled["Enabled"], true);
        assert_eq!(enabled["HasApiKey"], true);
        assert_eq!(enabled["HasProxy"], true);
        assert_eq!(enabled["ProxyUrl"], "https://tmdb.qb.edu.kg");
        assert_eq!(enabled["TmdbProxyUrl"], "https://tmdb.qb.edu.kg");

        let disabled = tmdb_client_configuration_value(false, true, None);
        assert_eq!(disabled["ProviderEnabled"], false);
        assert_eq!(disabled["Configured"], false);
        assert_eq!(disabled["IsTmdbEnabled"], false);
        assert_eq!(disabled["IsEnabled"], false);
        assert_eq!(disabled["Enabled"], false);
        assert_eq!(disabled["HasApiKey"], true);
        assert_eq!(disabled["HasProxy"], false);
        assert_eq!(disabled["ProxyUrl"], "");

        let unconfigured = tmdb_client_configuration_value(true, false, None);
        assert_eq!(unconfigured["ProviderEnabled"], true);
        assert_eq!(unconfigured["Configured"], false);
        assert_eq!(unconfigured["HasApiKey"], false);
    }

    #[test]
    fn douban_client_configuration_respects_provider_switch() {
        let enabled = super::douban_client_configuration_value(true, true);
        assert_eq!(enabled["ProviderEnabled"], true);
        assert_eq!(enabled["Configured"], true);
        assert_eq!(enabled["IsDoubanEnabled"], true);

        let disabled = super::douban_client_configuration_value(false, true);
        assert_eq!(disabled["ProviderEnabled"], false);
        assert_eq!(disabled["Configured"], false);
        assert_eq!(disabled["HasCookie"], true);
        assert_eq!(disabled["IsDoubanEnabled"], false);
    }

    #[test]
    fn tmdb_api_key_request_accepts_common_field_names() {
        let request: TmdbApiKeyRequest =
            serde_json::from_value(serde_json::json!({ "ApiKey": "abc" })).unwrap();
        assert_eq!(request.tmdb_api_key, "abc");

        let request: TmdbApiKeyRequest =
            serde_json::from_value(serde_json::json!({ "tmdbApiKey": "def" })).unwrap();
        assert_eq!(request.tmdb_api_key, "def");
    }

    #[test]
    fn tmdb_proxy_url_request_accepts_common_field_names() {
        let request: TmdbProxyUrlRequest =
            serde_json::from_value(serde_json::json!({ "ProxyUrl": "https://tmdb.qb.edu.kg" }))
                .unwrap();
        assert_eq!(request.tmdb_proxy_url, "https://tmdb.qb.edu.kg");

        let request: TmdbProxyUrlRequest =
            serde_json::from_value(serde_json::json!({ "tmdbProxyUrl": "tmdb.qb.edu.kg" }))
                .unwrap();
        assert_eq!(request.tmdb_proxy_url, "tmdb.qb.edu.kg");
    }

    #[test]
    fn usage_stats_session_entry_has_activity_shape() {
        let session = PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
            user_name: "alice".to_string(),
            play_session_id: "p1".to_string(),
            item_id: "m1".to_string(),
            item_name: Some("Movie".to_string()),
            now_playing_queue: Vec::new(),
            additional_users: Vec::new(),
            client: "Web".to_string(),
            device_name: "Browser".to_string(),
            device_id: "device-1".to_string(),
            application_version: "1.0".to_string(),
            is_active: true,
            last_activity_date: "1970-01-01T00:00:00Z".to_string(),
            last_playback_check_in: "1970-01-01T00:00:00Z".to_string(),
            last_activity_unix: 0,
            play_state: PlaybackState {
                position_ticks: 42,
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
        };
        let entry = usage_stats_session_entry(&session);
        assert_eq!(entry["Id"], "s1");
        assert_eq!(entry["UserId"], "u1");
        assert_eq!(entry["NowPlayingItemName"], "Movie");
        assert_eq!(entry["PlayState"]["PositionTicks"], 42);
    }

    #[tokio::test]
    async fn play_activity_uses_precise_watch_days() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_user(&db, "u1", "alice", "Alice", 0, None).await;
        insert_test_user(&db, "u2", "bob", "Bob", 0, None).await;
        insert_test_item(
            &db,
            "m1",
            "Movie",
            "D:/movie.mkv",
            "Movie",
            0,
            None,
            None,
            None,
            1,
            1,
            1,
        )
        .await;
        insert_watch_day(&db, "1970-01-01", "u1", "m1", 120, 2, Some(10)).await;
        insert_test_item(
            &db,
            "e1",
            "Episode",
            "D:/episode.mkv",
            "Episode",
            0,
            None,
            None,
            None,
            1,
            1,
            1,
        )
        .await;
        insert_watch_day(&db, "1970-01-02", "u2", "e1", 60, 1, Some(90010)).await;

        let rows = play_activity_rows(&db, &[], None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        let movie = rows
            .iter()
            .find(|row| row["ItemName"] == "Movie")
            .expect("movie activity row");
        assert_eq!(movie["UserName"], "alice");
        assert_eq!(movie["PlayCount"], 2);
        assert_eq!(
            play_activity_rows(&db, &["Movie"], Some("u1"), Some((0, 86_400)))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            play_activity_rows(&db, &["Movie"], Some("u2"), None)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            play_activity_rows(&db, &["Movie"], None, None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            play_activity_rows(&db, &["Series", "Season", "Episode"], None, None)
                .await
                .unwrap()
                .len(),
            1
        );

        let hourly = usage_stats_hourly_items(&rows);
        assert_eq!(hourly.len(), 24);
        assert_eq!(hourly[0]["PlayCount"], 2);

        let durations = usage_stats_duration_histogram_items(&rows);
        assert_eq!(durations[1]["Name"], "<30m");
        assert_eq!(durations[1]["Count"], 2);

        let by_user = usage_stats_breakdown_items(&rows, "User").unwrap();
        assert_eq!(by_user.len(), 2);
        assert_eq!(by_user[0]["PlayCount"], 2);
        assert_eq!(by_user[0]["DurationTicks"], 1_200_000_000_i64);
        assert!(usage_stats_breakdown_items(&[], "Bad").is_none());
    }

    #[tokio::test]
    async fn usage_stats_backup_round_trips_json_safely() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-usage-test-{}", uuid::Uuid::new_v4()));

        let saved = user_usage_stats_save_backup_to(&db, root.clone())
            .await
            .unwrap();
        let file_name = saved["FileName"].as_str().unwrap();
        let loaded = user_usage_stats_load_backup_from(root.clone(), file_name)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(loaded["Version"], 1);
        assert_eq!(loaded["TotalRecordCount"], 0);
        assert!(safe_user_usage_backup_file("../bad.json").is_none());
        assert!(safe_user_usage_backup_file("backup.txt").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn usage_stats_import_saves_valid_backup_json() {
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-usage-import-{}", uuid::Uuid::new_v4()));
        let body =
            axum::body::Bytes::from_static(br#"{"Version":1,"Items":[],"TotalRecordCount":0}"#);

        let imported = super::user_usage_stats_import_backup_to(root.clone(), "import.json", body)
            .await
            .unwrap();
        let loaded = user_usage_stats_load_backup_from(root.clone(), "import.json")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(imported["FileName"], "import.json");
        assert_eq!(imported["Imported"], true);
        assert_eq!(loaded["Version"], 1);
        assert!(
            super::user_usage_stats_import_backup_to(
                root.clone(),
                "../bad.json",
                axum::body::Bytes::from_static(b"{}"),
            )
            .await
            .is_err()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn custom_usage_query_is_read_only_and_scoped() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_item(
            &db,
            "m1",
            "Movie",
            "D:/movie.mkv",
            "Movie",
            0,
            None,
            None,
            None,
            1,
            1,
            1,
        )
        .await;

        let ok = run_user_usage_custom_query(
            &db,
            &CustomQueryRequest {
                custom_query_string: "SELECT id, title FROM media_items".to_string(),
                replace_user_id: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(ok["TotalRecordCount"], 1);
        assert_eq!(ok["Items"][0]["title"], "Movie");

        for sql in [
            "DELETE FROM media_items",
            "SELECT * FROM users",
            "SELECT * FROM media_items; SELECT * FROM user_data",
            "SELECT * FROM media_items -- nope",
        ] {
            assert!(
                run_user_usage_custom_query(
                    &db,
                    &CustomQueryRequest {
                        custom_query_string: sql.to_string(),
                        replace_user_id: false,
                    },
                )
                .await
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn usage_user_manage_returns_safe_user_shape() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_user(&db, "u1", "alice", "Alice", 1, Some("secret-hash")).await;
        let state = Arc::new(test_state(db));

        let response =
            user_usage_stats_user_manage(State(state), Path(("get".to_string(), "u1".to_string())))
                .await
                .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn reports_items_use_media_rows_and_filters() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_item(
            &db,
            "m1",
            "Movie",
            "D:/movie.mkv",
            "Movie",
            0,
            Some("overview"),
            Some(2024),
            Some(1_200_000_000),
            1,
            2,
            3,
        )
        .await;
        insert_test_item(
            &db,
            "e1",
            "Episode",
            "D:/episode.mkv",
            "Episode",
            0,
            None,
            None,
            None,
            1,
            2,
            3,
        )
        .await;
        let mut query = HashMap::new();
        query.insert("IncludeItemTypes".to_string(), "Movie".to_string());
        query.insert("ReportColumns".to_string(), "Name,Year,Runtime".to_string());

        let result = reports_items_result(&db, &query).await.unwrap();
        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["Rows"][0]["Id"], "m1");
        assert_eq!(result["Rows"][0]["Columns"][0]["Name"], "Movie");
        assert_eq!(result["Rows"][0]["Columns"][1]["Name"], "2024");
        assert_eq!(report_item_headers_for_query(&query).len(), 3);
        assert!(
            report_csv(
                result["Headers"].as_array().unwrap(),
                result["Rows"].as_array().unwrap()
            )
            .contains("Movie")
        );
    }

    #[tokio::test]
    async fn reports_activities_use_activity_log_rows() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        activity_log::Entity::insert(activity_log::ActiveModel {
            id: Set("a1".to_string()),
            name: Set("Scanned".to_string()),
            log_type: Set("Task".to_string()),
            user_id: Set(Some("u1".to_string())),
            item_id: Set(None),
            severity: Set("Info".to_string()),
            created_at: Set(1),
        })
        .exec(&db)
        .await
        .unwrap();
        let mut query = HashMap::new();
        query.insert("ReportView".to_string(), "ReportActivities".to_string());
        query.insert(
            "ReportColumns".to_string(),
            "Name,Type,Severity".to_string(),
        );

        let result = reports_activity_result(&db, &query).await.unwrap();
        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["Rows"][0]["Id"], "a1");
        assert_eq!(result["Rows"][0]["Columns"][0]["Name"], "Scanned");
        assert_eq!(report_activity_headers_for_query(&query).len(), 3);
    }

    #[tokio::test]
    async fn notification_read_state_ignores_invisible_ids() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        activity_log::Entity::insert(activity_log::ActiveModel {
            id: Set("n1".to_string()),
            name: Set("Visible".to_string()),
            log_type: Set("Notification".to_string()),
            user_id: Set(Some("u1".to_string())),
            item_id: Set(Some("Normal|ok".to_string())),
            severity: Set("Normal".to_string()),
            created_at: Set(1),
        })
        .exec(&db)
        .await
        .unwrap();
        activity_log::Entity::insert(activity_log::ActiveModel {
            id: Set("n2".to_string()),
            name: Set("Other".to_string()),
            log_type: Set("Notification".to_string()),
            user_id: Set(Some("u2".to_string())),
            item_id: Set(Some("Normal|no".to_string())),
            severity: Set("Normal".to_string()),
            created_at: Set(1),
        })
        .exec(&db)
        .await
        .unwrap();

        let mut query = HashMap::new();
        query.insert("Ids".to_string(), "n1,n2,missing".to_string());
        assert_eq!(
            update_notification_read_state(&db, "u1", &query, true)
                .await
                .status(),
            axum::http::StatusCode::OK
        );
        let saved = super::notification_read_ids(&db, "u1").await;
        assert_eq!(saved, vec!["n1".to_string()]);

        query.insert("Ids".to_string(), "missing".to_string());
        assert_eq!(
            update_notification_read_state(&db, "u1", &query, true)
                .await
                .status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn notification_inputs_are_normalized_and_limited() {
        assert_eq!(
            normalize_notification_text(
                Some("  Hello\t"),
                super::MAX_NOTIFICATION_NAME_LEN,
                true,
                "Name is required"
            )
            .unwrap(),
            "Hello"
        );
        assert!(
            normalize_notification_text(
                Some("bad\0name"),
                super::MAX_NOTIFICATION_NAME_LEN,
                true,
                "Name is required"
            )
            .is_err()
        );
        assert!(
            normalize_notification_text(
                Some(&"x".repeat(super::MAX_NOTIFICATION_NAME_LEN + 1)),
                super::MAX_NOTIFICATION_NAME_LEN,
                true,
                "Name is required"
            )
            .is_err()
        );
        assert_eq!(normalize_notification_level("warn").unwrap(), "Warning");
        assert_eq!(
            normalize_notification_level("information").unwrap(),
            "Normal"
        );
        assert!(normalize_notification_level("critical").is_err());
        assert_eq!(
            normalize_notification_ids(vec![
                " n1 ".to_string(),
                "n1".to_string(),
                "".to_string(),
                "n2".to_string()
            ])
            .unwrap(),
            vec!["n1".to_string(), "n2".to_string()]
        );
        assert!(normalize_notification_ids(vec!["bad\nid".to_string()]).is_err());
        assert!(
            normalize_notification_ids(vec!["x".to_string(); super::MAX_NOTIFICATION_IDS + 1])
                .is_err()
        );
    }

    #[tokio::test]
    async fn notification_service_test_records_global_notification() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = Arc::new(test_state(db));

        assert_eq!(
            notification_services_test(State(state.clone()))
                .await
                .status(),
            axum::http::StatusCode::OK
        );
        let items = notification_items(&state.db, "u1").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["Name"], "Test Notification");
        assert_eq!(items[0]["UserId"], "");
    }

    #[test]
    fn notification_services_are_advertised() {
        let services = notification_services_value();
        assert!(!services.is_empty());
        assert_eq!(services[0]["Name"], "Email");
        assert!(services[0]["SupportedCommands"].as_array().is_some());
    }

    #[tokio::test]
    async fn smtp_notification_test_records_user_notification() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_user(&db, "u1", "alice", "Alice", 0, None).await;
        let state = Arc::new(test_state(db));

        assert_eq!(
            smtp_notification_test(State(state.clone()), Path("u1".to_string()))
                .await
                .status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            smtp_notification_test(State(state.clone()), Path("missing".to_string()))
                .await
                .status(),
            axum::http::StatusCode::NOT_FOUND
        );

        let alice = notification_items(&state.db, "u1").await.unwrap();
        let other = notification_items(&state.db, "u2").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0]["Name"], "SMTP Test");
        assert!(other.is_empty());
    }

    async fn insert_test_user(
        db: &DatabaseConnection,
        id: &str,
        username: &str,
        display_name: &str,
        is_admin: i64,
        password_hash: Option<&str>,
    ) {
        users::Entity::insert(users::ActiveModel {
            id: Set(id.to_string()),
            username: Set(username.to_string()),
            password_hash: Set(password_hash.map(ToString::to_string)),
            display_name: Set(display_name.to_string()),
            is_admin: Set(is_admin),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            last_login_at: Set(None),
        })
        .exec(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_test_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        item_type: &str,
        is_folder: i64,
        overview: Option<&str>,
        production_year: Option<i64>,
        runtime_ticks: Option<i64>,
        modified_at: i64,
        created_at: i64,
        updated_at: i64,
    ) {
        media_items::Entity::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(String::new()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(1),
            overview: Set(overview.map(ToString::to_string)),
            production_year: Set(production_year),
            runtime_ticks: Set(runtime_ticks),
            modified_at: Set(modified_at),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
            ..Default::default()
        })
        .exec(db)
        .await
        .unwrap();
    }

    async fn insert_watch_day(
        db: &DatabaseConnection,
        day: &str,
        user_id: &str,
        item_id: &str,
        watch_seconds: i64,
        play_count: i64,
        last_played_at: Option<i64>,
    ) {
        playback_watch_days::Entity::insert(playback_watch_days::ActiveModel {
            day: Set(day.to_string()),
            user_id: Set(user_id.to_string()),
            item_id: Set(item_id.to_string()),
            watch_seconds: Set(watch_seconds),
            play_count: Set(play_count),
            last_played_at: Set(last_played_at),
        })
        .exec(db)
        .await
        .unwrap();
    }

    #[test]
    fn log_names_reject_paths() {
        assert!(is_safe_log_name("server.log"));
        assert!(is_safe_log_name("server_err.log"));
        assert!(!is_safe_log_name("../server.log"));
        assert!(!is_safe_log_name("logs/server.log"));
        assert!(!is_safe_log_name(r"logs\server.log"));
        assert!(safe_log_path("../server.log").is_none());
    }

    #[test]
    fn log_file_entry_rejects_non_logs() {
        assert!(log_file_entry(std::path::Path::new("notes.txt")).is_none());
    }

    #[test]
    fn client_log_file_names_are_unique_and_safe() {
        let first = client_log_file_name();
        let second = client_log_file_name();
        assert_ne!(first, second);
        assert!(is_safe_log_name(&first));
        assert!(is_safe_log_name(&second));
    }

    #[tokio::test]
    async fn client_log_document_writes_unique_safe_log_file() {
        let root = std::path::PathBuf::from("logs");
        let existed = root.exists();
        std::fs::create_dir_all(&root).unwrap();

        let response = client_log_document(HeaderMap::new(), Bytes::from_static(b"hello"))
            .await
            .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let name = value["FileName"].as_str().unwrap();
        assert!(is_safe_log_name(name));
        assert!(root.join(name).is_file());

        std::fs::remove_file(root.join(name)).unwrap();
        if !existed {
            let _ = std::fs::remove_dir(root);
        }
    }

    #[tokio::test]
    async fn log_query_reports_start_index_and_limits_lines() {
        let root = std::path::PathBuf::from("logs");
        let existed = root.exists();
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("system-test.log");
        let second_log = root.join("system-test-2.log");
        std::fs::write(&log, b"one\ntwo\nthree").unwrap();
        std::fs::write(&second_log, b"other").unwrap();

        let mut query = HashMap::new();
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = system_logs_query(Query(query)).await.into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["StartIndex"], 1);
        assert!(value["TotalRecordCount"].as_u64().unwrap_or_default() >= 1);
        assert_eq!(value["Items"].as_array().unwrap().len(), 1);

        let mut query = HashMap::new();
        query.insert("Limit".to_string(), "2".to_string());
        let response = system_log_lines(Path("system-test.log".to_string()), Query(query))
            .await
            .into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "two\nthree");

        std::fs::remove_file(log).unwrap();
        std::fs::remove_file(second_log).unwrap();
        if !existed {
            let _ = std::fs::remove_dir(root);
        }
    }

    #[test]
    fn fallback_fonts_list_only_safe_font_files() {
        let root = std::path::PathBuf::from(FALLBACK_FONTS_PATH);
        let existed = root.exists();
        std::fs::create_dir_all(&root).unwrap();
        let font = root.join("TestFont.ttf");
        let ignored = root.join("notes.txt");
        std::fs::write(&font, b"font").unwrap();
        std::fs::write(&ignored, b"no").unwrap();

        let entries = fallback_font_entries();
        assert!(entries.iter().any(|entry| entry["Name"] == "TestFont.ttf"));
        assert!(!entries.iter().any(|entry| entry["Name"] == "notes.txt"));
        assert!(is_safe_fallback_font_name("TestFont.ttf"));
        assert!(!is_safe_fallback_font_name("../TestFont.ttf"));
        assert!(fallback_font_path("../TestFont.ttf").is_none());
        assert_eq!(fallback_font_mime_type("TestFont.ttf"), "font/ttf");
        assert_eq!(
            fallback_font_path("TestFont.ttf")
                .unwrap()
                .file_name()
                .and_then(|value| value.to_str()),
            Some("TestFont.ttf")
        );

        std::fs::remove_file(font).unwrap();
        std::fs::remove_file(ignored).unwrap();
        if !existed {
            let _ = std::fs::remove_dir(root);
        }
    }

    #[tokio::test]
    async fn system_log_file_requires_safe_name() {
        assert_eq!(
            system_log_file(Query(HashMap::new())).await.status(),
            axum::http::StatusCode::BAD_REQUEST
        );

        let mut query = HashMap::new();
        query.insert("name".to_string(), "../server.log".to_string());
        assert_eq!(
            system_log_file(Query(query)).await.status(),
            axum::http::StatusCode::NOT_FOUND
        );
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
            tmdb_proxy_url: Arc::new(RwLock::new(None)),
            tmdb_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            douban_cookie: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            chapter_image_task_cancel: tokio::sync::Mutex::new(None),
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            admin_http_log_seq: std::sync::atomic::AtomicU64::new(0),
            admin_http_logs: RwLock::new(std::collections::VecDeque::new()),
            playback_distribution: RwLock::new(crate::app::state::PlaybackDistribution::default()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
