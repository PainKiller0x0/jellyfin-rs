use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, PlaybackSession, SERVER_NAME, SessionCapabilities, VERSION},
    db::row_ext::QueryResultExt,
    entities::{
        access_tokens, access_tokens::Entity as AccessTokens, activity_log,
        activity_log::Entity as ActivityLog, app_settings, app_settings::Entity as AppSettings,
        task_results, task_results::Entity as TaskResults, users, users::Entity as Users,
    },
    jellyfin::common::{image as placeholder_image, internal_error},
    jellyfin::item_queries::library_views,
    library::path_utils,
    util::{now_unix, stable_text_id, unix_to_jellyfin_date},
};

const BRANDING_SPLASHSCREEN_PATH: &str = "data/images/branding-splashscreen";
const BRANDING_SPLASHSCREEN_CONTENT_TYPE_KEY: &str = "BrandingSplashscreenContentType";
const MAX_BRANDING_SPLASHSCREEN_BYTES: usize = 10 * 1024 * 1024;
const MAX_CLIENT_LOG_BYTES: usize = 1024 * 1024;
const MAX_CAMERA_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
const MAX_USER_USAGE_BACKUP_BYTES: usize = 20 * 1024 * 1024;
const CAMERA_UPLOADS_PATH: &str = "data/camera_uploads";
const USER_USAGE_BACKUP_PATH: &str = "data/user_usage_stats";

mod configuration;
mod localization;

pub use configuration::{
    complete_startup, configuration_pages, dashboard_configuration_page, default_metadata_options,
    named_configuration, server_configuration, startup_configuration, startup_user,
    update_named_configuration, update_remote_access, update_server_configuration,
    update_startup_configuration, update_startup_user,
};
pub use localization::{
    localization_countries, localization_cultures, localization_options, parental_ratings,
};

pub async fn system_info(State(state): State<Arc<AppState>>) -> Response {
    let server_name = app_setting(&state.db, "ServerName", SERVER_NAME).await;
    let startup_completed = app_setting_bool(&state.db, "StartupWizardCompleted", false).await;
    Json(json!({
        "ServerName": server_name,
        "Version": VERSION,
        "LocalAddress": "http://127.0.0.1:8096",
        "WanAddress": "http://127.0.0.1:8096",
        "OperatingSystem": std::env::consts::OS,
        "StartupWizardCompleted": startup_completed,
        "HasUpdateAvailable": false,
    }))
    .into_response()
}

pub async fn public_system_info(State(state): State<Arc<AppState>>) -> Response {
    let server_name = app_setting(&state.db, "ServerName", SERVER_NAME).await;
    let startup_completed = app_setting_bool(&state.db, "StartupWizardCompleted", false).await;
    Json(json!({
        "ServerName": server_name,
        "Version": VERSION,
        "Id": "jellyfin-rs",
        "StartupWizardCompleted": startup_completed,
    }))
    .into_response()
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

pub async fn quick_connect_enabled() -> impl IntoResponse {
    Json(true)
}

pub async fn connect_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Connect is not available on this server" })),
    )
        .into_response()
}

pub async fn branding_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(branding_options(&state.db).await).into_response()
}

pub async fn update_branding_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<JsonValue>,
) -> Response {
    let options = json!({
        "LoginDisclaimer": request.get("LoginDisclaimer").and_then(JsonValue::as_str).unwrap_or(""),
        "CustomCss": request.get("CustomCss").and_then(JsonValue::as_str).unwrap_or(""),
        "SplashscreenEnabled": request.get("SplashscreenEnabled").and_then(JsonValue::as_bool).unwrap_or(false),
    });

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
    if css.trim().is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

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
    if !content_type.starts_with("image/") {
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
    if let Some(directory) = path.parent()
        && let Err(error) = tokio::fs::create_dir_all(directory).await
    {
        return internal_error(error.into());
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

    let file_name = format!("client-{}.log", now_unix());
    let path = std::path::PathBuf::from("logs").join(&file_name);
    if let Err(error) = tokio::fs::create_dir_all("logs").await {
        return internal_error(error.into());
    }
    if let Err(error) = tokio::fs::write(&path, &body).await {
        return internal_error(error.into());
    }
    Json(json!({ "FileName": file_name })).into_response()
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
    let mut items = device_records(&state, query.user_id.as_deref())
        .await
        .into_iter()
        .map(|record| record.to_json())
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
    let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let ids = id
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let now = now_unix();
    for id in ids {
        if let Err(error) = revoke_device(&state, id, now).await {
            return internal_error(error);
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn camera_uploads(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(device_id) = query_value(&query, "DeviceId")
        .or_else(|| query_value(&query, "deviceId"))
        .filter(|value| !value.trim().is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
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
    let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return device_not_found();
    };
    device_records(&state, None)
        .await
        .into_iter()
        .find(|record| record.id == id)
        .map(|record| Json(record.to_json()).into_response())
        .unwrap_or_else(device_not_found)
}

pub async fn device_options(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DeviceIdQuery>,
) -> Response {
    let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return device_not_found();
    };
    let exists = device_records(&state, None)
        .await
        .into_iter()
        .any(|record| record.id == id);
    if exists {
        Json(device_options_result(
            id,
            device_custom_name(&state.db, id).await,
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
    let Some(id) = query
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return device_not_found();
    };
    let exists = device_records(&state, None)
        .await
        .into_iter()
        .any(|record| record.id == id);
    if !exists {
        return device_not_found();
    }
    match set_app_setting(
        &state.db,
        &device_options_key(id),
        request.custom_name.as_deref().unwrap_or_default(),
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
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
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

#[derive(Deserialize)]
pub struct ValidatePathRequest {
    #[serde(rename = "Path")]
    path: Option<String>,
    #[serde(rename = "IsFile")]
    is_file: Option<bool>,
    #[serde(rename = "ValidateWritable", alias = "ValidateWriteable", default)]
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

pub async fn validate_path(Json(request): Json<ValidatePathRequest>) -> Response {
    let path = request.path.unwrap_or_default();
    match path_utils::validate_path(&path, request.is_file, request.validate_writable) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error)
            if error.to_string().contains("not found")
                || error.to_string().contains("required") =>
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

pub async fn system_endpoint() -> impl IntoResponse {
    Json(json!({
        "IsLocal": true,
        "IsInNetwork": true
    }))
}

pub async fn system_ping() -> impl IntoResponse {
    "Jellyfin Server is running"
}

pub async fn utc_time() -> impl IntoResponse {
    Json(json!({
        "RequestReceptionTime": crate::util::unix_to_jellyfin_date(now_unix()),
        "ResponseTransmissionTime": crate::util::unix_to_jellyfin_date(now_unix())
    }))
}

pub async fn tmdb_client_configuration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let enabled = state
        .tmdb_api_key
        .read()
        .await
        .as_deref()
        .is_some_and(|key| !key.is_empty());
    Json(tmdb_client_configuration_value(enabled))
}

fn tmdb_client_configuration_value(enabled: bool) -> JsonValue {
    json!({
        "IsTmdbEnabled": enabled,
        "Enabled": enabled,
        "HasApiKey": enabled
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

    let mut select = ActivityLog::find()
        .order_by_desc(activity_log::Column::CreatedAt)
        .limit(limit)
        .offset(start_index);

    if has_user_id {
        select = select.filter(activity_log::Column::UserId.is_not_null());
    }

    match select.all(&state.db).await {
        Ok(models) => {
            let items: Vec<JsonValue> = models
                .iter()
                .map(|m| {
                    json!({
                        "Name": m.name,
                        "Type": m.log_type,
                        "Date": crate::util::unix_to_jellyfin_date(m.created_at),
                        "UserId": m.user_id.as_deref().unwrap_or_default(),
                        "Severity": "Info",
                    })
                })
                .collect();
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

pub async fn users_item_access(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_value(&query, "UserId").unwrap_or_else(|| state.user_id.to_string());
    match library_views(&state.db).await {
        Ok(libraries) => Json(items_access_value(&user_id, &libraries)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn items_access(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let user_id = query_value(&query, "UserId")
        .or_else(|| {
            body.as_ref()
                .and_then(|Json(body)| json_string(body, "UserId"))
        })
        .unwrap_or_else(|| state.user_id.to_string());
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
    let backend = state.db.get_database_backend();
    match state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
    AppSettings::find_by_id(key)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|model| model.value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub(crate) async fn server_config_json(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "server_config", "{}").await)
        .unwrap_or_else(|_| json!({}))
}

pub(super) async fn app_setting_bool(db: &DatabaseConnection, key: &str, default: bool) -> bool {
    match app_setting(db, key, if default { "true" } else { "false" })
        .await
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" => true,
        "0" | "false" | "no" => false,
        _ => default,
    }
}

pub(super) async fn set_app_setting(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
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

pub(super) async fn first_admin_user(
    db: &DatabaseConnection,
) -> anyhow::Result<Option<(String, String)>> {
    let Some(model) = Users::find()
        .filter(users::Column::IsAdmin.eq(1))
        .order_by_asc(users::Column::CreatedAt)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((model.id, model.username)))
}

pub async fn scheduled_tasks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(vec![scan_library_task(&state.db).await])
}

pub async fn scheduled_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    Json(scan_library_task(&state.db).await).into_response()
}

pub async fn scheduled_task_triggers(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    Json(scan_library_triggers(&state.db).await).into_response()
}

pub async fn update_scheduled_task_triggers(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    Json(triggers): Json<JsonValue>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !triggers.is_array() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match set_app_setting(
        &state.db,
        "ScheduledTask.scan-library.Triggers",
        &triggers.to_string(),
    )
    .await
    {
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
    crate::jellyfin::items::scan_handler(state).await
}

pub async fn stop_scheduled_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Response {
    if !is_known_scheduled_task(&task_id) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let now = now_unix();
    upsert_task_result(&state, &task_id, "Cancelled", now, now, None).await;
    StatusCode::NO_CONTENT.into_response()
}

fn is_known_scheduled_task(task_id: &str) -> bool {
    task_id == "scan-library"
}

pub async fn repositories(State(state): State<Arc<AppState>>) -> Response {
    Json(plugin_repositories(&state.db).await).into_response()
}

pub async fn update_repositories(
    State(state): State<Arc<AppState>>,
    Json(repositories): Json<JsonValue>,
) -> Response {
    if !repositories.is_array() {
        return StatusCode::BAD_REQUEST.into_response();
    }

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
    let scan_result = last_task_result(db, "scan-library").await;
    json!({
        "Name": "Scan media library",
        "State": "Idle",
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
        .unwrap_or_else(|_| default_scan_library_triggers())
}

fn default_scan_library_triggers() -> JsonValue {
    json!([{ "Type": "StartupTrigger" }])
}

async fn plugin_repositories(db: &DatabaseConnection) -> JsonValue {
    serde_json::from_str(&app_setting(db, "PluginRepositories", "").await)
        .unwrap_or_else(|_| default_plugin_repositories())
}

fn default_plugin_repositories() -> JsonValue {
    JsonValue::Array(Vec::new())
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
        .unwrap_or_else(|_| default_branding_options())
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
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
        "ThreadCount": std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1),
        "Date": unix_to_jellyfin_date(now_unix()),
    })
}

async fn play_activity_rows(
    db: &DatabaseConnection,
    item_types: &[&str],
    user_id: Option<&str>,
    day: Option<(i64, i64)>,
) -> anyhow::Result<Vec<JsonValue>> {
    let backend = db.get_database_backend();
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
        " AND ud.user_id = ?"
    } else {
        ""
    };
    let day_filter = if let Some((start, end)) = day {
        values.push(start.into());
        values.push(end.into());
        " AND ud.last_played_at >= ? AND ud.last_played_at < ?"
    } else {
        ""
    };
    let sql = format!(
        r#"SELECT ud.user_id, users.username, ud.item_id, mi.title, mi.item_type, mi.runtime_ticks, ud.play_count, ud.playback_position_ticks, ud.last_played_at
           FROM user_data ud
           JOIN media_items mi ON mi.id = ud.item_id
           LEFT JOIN users ON users.id = ud.user_id
           WHERE (COALESCE(ud.play_count, 0) > 0 OR ud.last_played_at IS NOT NULL){type_filter}{user_filter}{day_filter}
           ORDER BY COALESCE(ud.last_played_at, ud.updated_at) DESC
           LIMIT 200"#
    );
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
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
            let play_count = row.get_i64("play_count").unwrap_or_default();
            let position_ticks = row.get_i64("playback_position_ticks").unwrap_or_default();
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
                "PlaybackPositionTicks": position_ticks,
                "LastPlayedAt": last_played_at,
                "Date": last_played_at.map(unix_to_jellyfin_date),
                "LastPlayedDate": last_played_at.map(unix_to_jellyfin_date),
            }))
        })
        .collect()
}

async fn run_user_usage_custom_query(
    db: &DatabaseConnection,
    request: &CustomQueryRequest,
) -> anyhow::Result<JsonValue> {
    let sql = validate_user_usage_custom_query(&request.custom_query_string)?;
    let backend = db.get_database_backend();
    let wrapped = format!("SELECT * FROM ({sql}) AS custom_query LIMIT 500");
    let rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &wrapped,
            Vec::<sea_orm::Value>::new(),
        ))
        .await
        .context("failed to run custom usage query")?;
    let items = rows
        .iter()
        .map(|row| serde_json::Value::from_query_result(row, ""))
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
                || word.starts_with("sqlite_")
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
        hours[hour].2 += json_i64(row, "RunTimeTicks").saturating_mul(play_count);
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
        let runtime_ticks = json_i64(row, "RunTimeTicks");
        let minutes = runtime_ticks / 600_000_000;
        let index = if runtime_ticks <= 0 {
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
        buckets[index].5 += runtime_ticks.saturating_mul(play_count);
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
        entry.3 += json_i64(row, "RunTimeTicks").saturating_mul(play_count);
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
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);

    let files = log_files();

    let total = files.len();
    let items: Vec<_> = files.into_iter().skip(start_index).take(limit).collect();
    Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
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
        "DateModified": meta.modified().ok().and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
    }))
}

/// POST /Items/{id}/MetadataEditor — update metadata via editor
pub async fn metadata_editor(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    // Delegate to update_item_inner
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
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000);

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
    match safe_log_path(&name).and_then(|path| std::fs::read(path).ok()) {
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
    let device_id = required_upload_part(query.device_id, "DeviceId")?;
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
    if !value.trim().is_empty()
        && let Ok(history) = serde_json::from_str::<JsonValue>(&value)
    {
        return Ok(history);
    }
    Ok(json!({ "DeviceId": device_id, "FilesUploaded": [] }))
}

fn camera_upload_history_key(device_id: &str) -> String {
    format!("camera_uploads:{device_id}")
}

fn required_upload_part(value: Option<String>, name: &str) -> anyhow::Result<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{name} is required"))
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

async fn revoke_device(state: &AppState, device_id: &str, now: i64) -> anyhow::Result<()> {
    let backend = state.db.get_database_backend();
    state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE access_tokens SET revoked_at = ? WHERE device_id = ? AND revoked_at IS NULL",
            vec![now.into(), device_id.into()],
        ))
        .await
        .context("failed to revoke device tokens")?;

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

    fn to_json(&self) -> JsonValue {
        json!({
            "Name": self.name,
            "CustomName": null,
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
    let path = std::path::Path::new(name);
    path.file_name().and_then(|part| part.to_str()) == Some(name)
        && (name.ends_with(".log") || name.ends_with("_err.log"))
}

/// POST /System/Configuration/Partial — partial configuration update
pub async fn update_server_configuration_partial(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JsonValue>,
) -> Response {
    // Merge with existing config
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT value FROM app_settings WHERE key = 'server_config'",
            vec![],
        ))
        .await;

    let mut config: JsonValue = match row {
        Ok(Some(r)) => {
            let v: String = r.get_str("value").unwrap_or_else(|_| "{}".to_string());
            serde_json::from_str(&v).unwrap_or(json!({}))
        }
        _ => json!({}),
    };

    // Merge body into config
    if let (Some(obj), Some(patch)) = (config.as_object_mut(), body.as_object()) {
        for (k, v) in patch {
            obj.insert(k.clone(), v.clone());
        }
    }

    let now = crate::util::now_unix();
    let _ = state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO app_settings (key, value, updated_at) VALUES ('server_config', ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![config.to_string().into(), now.into()],
    )).await;

    StatusCode::NO_CONTENT.into_response()
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
    let Some(name) = query_value(&query, "Name").filter(|value| !value.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let description = query_value(&query, "Description").unwrap_or_default();
    let level = query_value(&query, "Level").unwrap_or_else(|| "Normal".to_string());
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
            activity_log::Column::UserId
                .is_null()
                .or(activity_log::Column::UserId.eq(user_id)),
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
    let ids = query
        .get("Ids")
        .or_else(|| query.get("ids"))
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
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
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id FROM activity_log WHERE log_type = 'Notification' AND (user_id IS NULL OR user_id = ?) AND id IN ({placeholders})"
    );
    let mut values = vec![user_id.into()];
    values.extend(ids.iter().map(|id| id.as_str().into()));
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &sql,
            values,
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get_str("id").ok())
        .collect())
}

async fn notification_read_ids(db: &DatabaseConnection, user_id: &str) -> Vec<String> {
    serde_json::from_str(&app_setting(db, &notification_read_key(user_id), "[]").await)
        .unwrap_or_default()
}

fn notification_read_key(user_id: &str) -> String {
    format!("notifications_read:{user_id}")
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

pub async fn fallback_fonts() -> Response {
    Json(Vec::<JsonValue>::new()).into_response()
}

pub async fn fallback_font_file() -> Response {
    StatusCode::NOT_FOUND.into_response()
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
    let backend = db.get_database_backend();
    let count_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
    let backend = db.get_database_backend();
    let count_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                r#"SELECT mi.id, mi.title, mi.path, mi.item_type, mi.overview, mi.official_rating, mi.production_year,
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
                id.clone(),
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
        "productionyear" | "premieredate" | "year" => "COALESCE(mi.production_year, 0)",
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

pub async fn emby_openapi_json() -> Response {
    match tokio::fs::read("docs/emby-openapi.json").await {
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
        CameraUploadQuery, CustomQueryRequest, DeviceRecord, TmdbApiKeyRequest,
        camera_upload_history_value, connect_unavailable, default_branding_options,
        default_plugin_repositories, default_scan_library_triggers, device_options_result,
        empty_query_result, game_system_display_name, image_by_name_info, is_known_scheduled_task,
        is_safe_log_name, items_access_value, last_task_result, live_tv_channel_mapping_options,
        live_tv_channel_mapping_options_value, live_tv_default_listing_provider,
        live_tv_default_listing_provider_value, live_tv_default_tuner_host,
        live_tv_default_tuner_host_value, live_tv_guide_info, live_tv_info, live_tv_timer_defaults,
        live_tv_timer_defaults_value, live_tv_unavailable, log_file_entry, notification_items,
        notification_services_test, notification_services_value, package_install_unavailable,
        package_list, package_update_list, party_unavailable, play_activity_rows, plugin_list,
        report_activity_headers_for_query, report_csv, report_item_headers_for_query,
        reports_activity_result, reports_items_result, required_upload_part,
        run_user_usage_custom_query, safe_log_path, safe_user_usage_backup_file,
        sanitize_file_part, save_camera_upload_to, scan_library_task, smtp_notification_test,
        stop_scheduled_task, sync_data, sync_data_result, sync_empty_response, sync_options_result,
        sync_play_unavailable, sync_unavailable, system_log_file, tmdb_client_configuration_value,
        ui_command, update_notification_read_state, usage_stats_breakdown_items,
        usage_stats_duration_histogram_items, usage_stats_hourly_items, usage_stats_session_entry,
        usage_user_entry, user_usage_stats_load_backup_from, user_usage_stats_save_backup_to,
        user_usage_stats_user_manage, user_view_grouping_options_value, web_strings_value,
    };
    use super::{DirectoryContentsQuery, ParentPathQuery, ValidatePathRequest};
    use crate::app::state::AppState;
    use crate::app::state::{PlaybackSession, PlaybackState, SessionCapabilities};
    use crate::entities::{access_tokens, activity_log, users};
    use axum::{
        extract::{Path, Query, State},
        response::IntoResponse,
    };
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, EntityTrait, Set};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[test]
    fn sync_options_result_has_emby_shape() {
        let options = sync_options_result();
        assert!(options["Targets"].as_array().is_some());
        assert!(options["Options"].as_array().is_some());
        assert!(options["QualityOptions"].as_array().is_some());
        assert!(options["ProfileOptions"].as_array().is_some());
        assert!(sync_data_result()["ItemIdsToRemove"].as_array().is_some());
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
    fn device_record_has_jellyfin_shape_without_token() {
        let session = PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
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
        let device = DeviceRecord::from_session(&session).to_json();
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

    #[tokio::test]
    async fn camera_upload_history_has_emby_shape() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let history = camera_upload_history_value(&db, "device-1").await.unwrap();
        assert_eq!(history["DeviceId"], "device-1");
        assert!(history["FilesUploaded"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn camera_upload_persists_history() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let device = DeviceRecord::from_token(&token).to_json();
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
    fn empty_repository_list_has_jellyfin_shape() {
        let repositories = default_plugin_repositories();
        assert!(repositories.as_array().is_some());
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
    async fn connect_reports_unavailable() {
        let response = connect_unavailable().await.into_response();
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
        assert!(!is_known_scheduled_task("missing"));
    }

    #[tokio::test]
    async fn scheduled_task_has_jellyfin_shape() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();

        let task = scan_library_task(&db).await;
        assert_eq!(task["Id"], "scan-library");
        assert_eq!(task["Key"], "scan-library");
        assert_eq!(task["Name"], "Scan media library");
        assert_eq!(task["State"], "Idle");
        assert!(task["Triggers"].as_array().is_some());
        assert!(task["LastExecutionResult"].is_null());
    }

    #[tokio::test]
    async fn task_result_uses_jellyfin_error_fields() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO task_results (task_id, status, start_time, end_time, message) VALUES ('scan-library', 'Failed', 1, 2, 'boom')",
            vec![],
        ))
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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

    #[test]
    fn tmdb_client_configuration_reports_compatible_enabled_fields() {
        let enabled = tmdb_client_configuration_value(true);
        assert_eq!(enabled["IsTmdbEnabled"], true);
        assert_eq!(enabled["Enabled"], true);
        assert_eq!(enabled["HasApiKey"], true);

        let disabled = tmdb_client_configuration_value(false);
        assert_eq!(disabled["IsTmdbEnabled"], false);
        assert_eq!(disabled["Enabled"], false);
        assert_eq!(disabled["HasApiKey"], false);
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
    fn usage_stats_session_entry_has_activity_shape() {
        let session = PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
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
    async fn play_activity_uses_user_data() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec!["u1".into(), "alice".into(), "Alice".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec!["u2".into(), "bob".into(), "Bob".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, 1, 1, 1)",
            vec!["m1".into(), "Movie".into(), "D:/movie.mkv".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, last_played_at, updated_at) VALUES (?, ?, 1, 123, 2, 10, 10)",
            vec!["u1".into(), "m1".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Episode', 0, 1, 1, 1)",
            vec!["e1".into(), "Episode".into(), "D:/episode.mkv".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, last_played_at, updated_at) VALUES (?, ?, 1, 456, 1, 90010, 90010)",
            vec!["u2".into(), "e1".into()],
        ))
        .await
        .unwrap();

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
        assert_eq!(durations[0]["Count"], 2);

        let by_user = usage_stats_breakdown_items(&rows, "User").unwrap();
        assert_eq!(by_user.len(), 2);
        assert_eq!(by_user[0]["PlayCount"], 2);
        assert!(usage_stats_breakdown_items(&[], "Bad").is_none());
    }

    #[tokio::test]
    async fn usage_stats_backup_round_trips_json_safely() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES ('m1', 'Movie', 'D:/movie.mkv', '', '', 'Movie', 0, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();

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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, password_hash, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'Alice', 'secret-hash', 1, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        let state = Arc::new(test_state(db));

        let response =
            user_usage_stats_user_manage(State(state), Path(("get".to_string(), "u1".to_string())))
                .await
                .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn reports_items_use_media_rows_and_filters() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, overview, production_year, runtime_ticks, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, 'overview', 2024, 1200000000, 1, 2, 3)",
            vec!["m1".into(), "Movie".into(), "D:/movie.mkv".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Episode', 0, 1, 2, 3)",
            vec!["e1".into(), "Episode".into(), "D:/episode.mkv".into()],
        ))
        .await
        .unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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

    #[tokio::test]
    async fn notification_service_test_records_global_notification() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'alice', 'Alice', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
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
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
