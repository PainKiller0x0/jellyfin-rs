use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, SERVER_NAME, VERSION},
    entities::{
        activity_log, activity_log::Entity as ActivityLog, app_settings,
        app_settings::Entity as AppSettings, task_results, task_results::Entity as TaskResults,
        users, users::Entity as Users,
    },
    jellyfin::common::internal_error,
    library::path_utils,
    util::now_unix,
};

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

pub async fn quick_connect_enabled() -> impl IntoResponse {
    Json(false)
}

pub async fn quick_connect_result() -> impl IntoResponse {
    Json(json!({
        "Authenticated": false,
        "Secret": "",
        "Code": "",
        "DeviceId": "",
        "DeviceName": "",
        "AppName": "",
        "AppVersion": "",
        "DateAdded": "1970-01-01T00:00:00Z"
    }))
}

pub async fn branding_configuration() -> impl IntoResponse {
    Json(json!({
        "LoginDisclaimer": "",
        "CustomCss": "",
        "SplashscreenEnabled": false
    }))
}

pub async fn devices() -> impl IntoResponse {
    Json(json!({ "Items": [], "TotalRecordCount": 0 }))
}

pub async fn device_options() -> impl IntoResponse {
    Json(json!({}))
}

#[derive(Deserialize)]
pub struct DirectoryContentsQuery {
    #[serde(rename = "path")]
    path: String,
    #[serde(rename = "includeFiles", default)]
    include_files: bool,
    #[serde(rename = "includeDirectories", default)]
    include_directories: bool,
}

#[derive(Deserialize)]
pub struct ParentPathQuery {
    #[serde(rename = "path")]
    path: String,
}

#[derive(Deserialize)]
pub struct ValidatePathRequest {
    #[serde(rename = "Path")]
    path: Option<String>,
    #[serde(rename = "IsFile")]
    is_file: Option<bool>,
    #[serde(rename = "ValidateWritable", default)]
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
        .as_deref()
        .is_some_and(|key| !key.is_empty());
    Json(json!({
        "IsTmdbEnabled": enabled
    }))
}

pub async fn system_logs() -> impl IntoResponse {
    Json(Vec::<JsonValue>::new())
}

pub async fn system_log_file() -> impl IntoResponse {
    ""
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
    let scan_result = last_task_result(&state.db, "scan-library").await;
    let task = json!({
        "Name": "Scan media library",
        "State": "Idle",
        "Id": "scan-library",
        "Description": "Scans configured media library paths for new and updated media files.",
        "Category": "Library",
        "IsHidden": false,
        "LastExecutionResult": scan_result,
    });
    Json(vec![task])
}

pub async fn last_task_result(db: &DatabaseConnection, task_id: &str) -> Option<JsonValue> {
    let model = TaskResults::find_by_id(task_id).one(db).await.ok()??;
    Some(json!({
        "Status": model.status,
        "StartTimeUtc": model.start_time.map(crate::util::unix_to_jellyfin_date),
        "EndTimeUtc": model.end_time.map(crate::util::unix_to_jellyfin_date),
        "Message": model.message,
    }))
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
pub async fn system_logs_query(
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let start_index = query.get("StartIndex").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let limit = query.get("Limit").and_then(|v| v.parse::<usize>().ok()).unwrap_or(50);

    let log_dir = std::path::PathBuf::from("logs");
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    files.push(json!({
                        "Name": entry.file_name().to_string_lossy(),
                        "Size": meta.len(),
                        "DateModified": meta.modified().ok().and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
                    }));
                }
            }
        }
    }
    // Also check current directory for log files
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".log") || name.ends_with("_err.log") {
                if let Ok(meta) = entry.metadata() {
                    files.push(json!({
                        "Name": name,
                        "Size": meta.len(),
                        "DateModified": meta.modified().ok().and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
                    }));
                }
            }
        }
    }

    let total = files.len();
    let items: Vec<_> = files.into_iter().skip(start_index).take(limit).collect();
    Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
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
