use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Value};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, SERVER_NAME, VERSION},
    db::row_ext::QueryResultExt,
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
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(15)
        .clamp(1, 100);
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let has_user_id = query
        .get("hasUserId")
        .or_else(|| query.get("HasUserId"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));

    let backend = state.db.get_database_backend();
    let sql = if has_user_id {
        "SELECT name, log_type, created_at, user_id FROM activity_log WHERE user_id IS NOT NULL ORDER BY created_at DESC LIMIT ? OFFSET ?"
    } else {
        "SELECT name, log_type, created_at, user_id FROM activity_log ORDER BY created_at DESC LIMIT ? OFFSET ?"
    };

    match state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            sql,
            vec![limit.into(), start_index.into()],
        ))
        .await
    {
        Ok(rows) => {
            let items: Vec<JsonValue> = rows
                .iter()
                .map(|row| {
                    let created_at: i64 = row.get_i64("created_at").unwrap_or_default();
                    json!({
                        "Name": row.get_str("name").unwrap_or_default(),
                        "Type": row.get_str("log_type").unwrap_or_default(),
                        "Date": crate::util::unix_to_jellyfin_date(created_at),
                        "UserId": row.get_opt_str("user_id").unwrap_or_default(),
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
    let backend = db.get_database_backend();
    db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT value FROM app_settings WHERE key = ?",
        vec![key.into()],
    ))
    .await
    .ok()
    .flatten()
    .and_then(|row| row.get_str("value").ok())
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
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![key.into(), value.into(), now_unix().into()],
    ))
    .await?;
    Ok(())
}

pub(super) async fn first_admin_user(
    db: &DatabaseConnection,
) -> anyhow::Result<Option<(String, String)>> {
    let backend = db.get_database_backend();
    let Some(row) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, username FROM users WHERE is_admin = 1 ORDER BY created_at ASC LIMIT 1",
            vec![],
        ))
        .await?
    else {
        return Ok(None);
    };
    Ok(Some((row.get_str("id")?, row.get_str("username")?)))
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
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT status, start_time, end_time, message FROM task_results WHERE task_id = ?",
            vec![task_id.into()],
        ))
        .await
        .ok()??;
    let status: String = row.get_str("status").ok()?;
    let start_time: Option<i64> = row.get_opt_i64("start_time").ok().flatten();
    let end_time: Option<i64> = row.get_opt_i64("end_time").ok().flatten();
    Some(json!({
        "Status": status,
        "StartTimeUtc": start_time.map(crate::util::unix_to_jellyfin_date),
        "EndTimeUtc": end_time.map(crate::util::unix_to_jellyfin_date),
        "Message": row.get_opt_str("message").ok().flatten(),
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
    let backend = state.db.get_database_backend();
    let _ = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO activity_log (id, name, log_type, user_id, item_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                id.into(),
                name.into(),
                log_type.into(),
                Value::from(user_id.map(ToString::to_string)),
                Value::from(item_id.map(ToString::to_string)),
                now.into(),
            ],
        ))
        .await;
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
    let backend = state.db.get_database_backend();
    let _ = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO task_results (task_id, status, start_time, end_time, message) VALUES (?, ?, ?, ?, ?) ON CONFLICT(task_id) DO UPDATE SET status = excluded.status, start_time = excluded.start_time, end_time = excluded.end_time, message = excluded.message",
            vec![
                task_id.into(),
                status.into(),
                start_time.into(),
                end_time.into(),
                message.into(),
            ],
        ))
        .await;
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
