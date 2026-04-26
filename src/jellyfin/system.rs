use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME, SERVER_NAME, VERSION},
    jellyfin::common::internal_error,
    util::{hash_password, now_unix},
};

#[derive(Deserialize)]
pub struct StartupConfigurationRequest {
    #[serde(rename = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "UICulture")]
    ui_culture: Option<String>,
    #[serde(rename = "MetadataCountryCode")]
    metadata_country_code: Option<String>,
    #[serde(rename = "PreferredMetadataLanguage")]
    preferred_metadata_language: Option<String>,
}

#[derive(Deserialize)]
pub struct StartupUserRequest {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Password")]
    password: String,
}

#[derive(Deserialize)]
pub struct RemoteAccessRequest {
    #[serde(rename = "EnableRemoteAccess")]
    enable_remote_access: bool,
}

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

pub async fn startup_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ServerName": app_setting(&state.db, "ServerName", SERVER_NAME).await,
        "UICulture": app_setting(&state.db, "UICulture", "zh-CN").await,
        "MetadataCountryCode": app_setting(&state.db, "MetadataCountryCode", "CN").await,
        "PreferredMetadataLanguage": app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        "EnableRemoteAccess": app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
    }))
    .into_response()
}

pub async fn update_startup_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartupConfigurationRequest>,
) -> Response {
    for (key, value) in [
        ("ServerName", request.server_name),
        ("UICulture", request.ui_culture),
        ("MetadataCountryCode", request.metadata_country_code),
        (
            "PreferredMetadataLanguage",
            request.preferred_metadata_language,
        ),
    ] {
        if let Some(value) = value {
            if let Err(error) = set_app_setting(&state.db, key, value.trim()).await {
                return internal_error(error);
            }
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn startup_user(State(state): State<Arc<AppState>>) -> Response {
    match first_admin_user(&state.db).await {
        Ok(Some((id, name))) => {
            Json(json!({ "Id": id, "Name": name, "Password": "" })).into_response()
        }
        Ok(None) => Json(json!({ "Name": DEFAULT_USER_NAME, "Password": "" })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_startup_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartupUserRequest>,
) -> Response {
    let username = request.name.trim();
    if username.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Name is required" })),
        )
            .into_response();
    }

    let now = now_unix();
    let password_hash = match hash_password(&request.password) {
        Ok(hash) => hash,
        Err(error) => return internal_error(error),
    };
    let user_id = state.user_id.to_string();

    match sqlx::query(r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 1, ?, ?) ON CONFLICT(id) DO UPDATE SET username = excluded.username, password_hash = excluded.password_hash, display_name = excluded.display_name, is_admin = 1, is_disabled = 0, updated_at = excluded.updated_at"#)
        .bind(&user_id)
        .bind(username)
        .bind(&password_hash)
        .bind(username)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
    {
        Ok(_) => Json(json!({ "Id": user_id, "Name": username })).into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_remote_access(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RemoteAccessRequest>,
) -> Response {
    match set_app_setting(
        &state.db,
        "EnableRemoteAccess",
        if request.enable_remote_access {
            "true"
        } else {
            "false"
        },
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn complete_startup(State(state): State<Arc<AppState>>) -> Response {
    match set_app_setting(&state.db, "StartupWizardCompleted", "true").await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn localization_options() -> impl IntoResponse {
    Json(vec![
        json!({ "Name": "Chinese Simplified", "Value": "zh-CN" }),
        json!({ "Name": "English", "Value": "en-US" }),
    ])
}

pub async fn localization_cultures() -> impl IntoResponse {
    Json(vec![
        json!({ "DisplayName": "Chinese Simplified", "Name": "zh-CN", "TwoLetterISOLanguageName": "zh-CN" }),
        json!({ "DisplayName": "English", "Name": "en-US", "TwoLetterISOLanguageName": "en" }),
    ])
}

pub async fn localization_countries() -> impl IntoResponse {
    Json(vec![
        json!({ "DisplayName": "China", "Name": "China", "TwoLetterISORegionName": "CN" }),
        json!({ "DisplayName": "United States", "Name": "United States", "TwoLetterISORegionName": "US" }),
    ])
}

pub async fn parental_ratings() -> impl IntoResponse {
    Json(Vec::<Value>::new())
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

pub async fn server_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ServerName": app_setting(&state.db, "ServerName", SERVER_NAME).await,
        "UICulture": app_setting(&state.db, "UICulture", "zh-CN").await,
        "MetadataCountryCode": app_setting(&state.db, "MetadataCountryCode", "CN").await,
        "PreferredMetadataLanguage": app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        "EnableRemoteAccess": app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
        "CastReceiverApplications": [
            {
                "Id": "",
                "Name": "Disabled"
            }
        ]
    }))
    .into_response()
}

pub async fn update_server_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Value>,
) -> Response {
    for key in [
        "ServerName",
        "UICulture",
        "MetadataCountryCode",
        "PreferredMetadataLanguage",
    ] {
        if let Some(value) = request.get(key).and_then(Value::as_str) {
            if let Err(error) = set_app_setting(&state.db, key, value.trim()).await {
                return internal_error(error);
            }
        }
    }

    if let Some(value) = request.get("EnableRemoteAccess").and_then(Value::as_bool) {
        if let Err(error) = set_app_setting(
            &state.db,
            "EnableRemoteAccess",
            if value { "true" } else { "false" },
        )
        .await
        {
            return internal_error(error);
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn default_metadata_options() -> impl IntoResponse {
    Json(json!({
        "ItemType": "Movie",
        "DisabledMetadataSavers": [],
        "LocalMetadataReaderOrder": [],
        "DisabledMetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "DisabledImageFetchers": [],
        "ImageFetcherOrder": []
    }))
}

pub async fn named_configuration(Path(key): Path<String>) -> impl IntoResponse {
    let value = match key.as_str() {
        "encoding" => json!({
            "EnableHardwareEncoding": false,
            "EnableThrottling": false,
            "EncodingThreadCount": -1
        }),
        "network" => json!({
            "EnableRemoteAccess": false,
            "LocalNetworkSubnets": [],
            "LocalNetworkAddresses": [],
            "KnownProxies": [],
            "PublishedServerUriBySubnet": []
        }),
        "livetv" => json!({
            "ListingProviders": [],
            "TunerHosts": []
        }),
        _ => json!({}),
    };
    Json(value)
}

pub async fn update_named_configuration() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub async fn configuration_pages(
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let enable_in_main_menu = query
        .get("enableInMainMenu")
        .or_else(|| query.get("EnableInMainMenu"))
        .is_none_or(|value| value.eq_ignore_ascii_case("true"));
    let pages: Vec<Value> = if enable_in_main_menu {
        Vec::new()
    } else {
        Vec::new()
    };
    Json(pages)
}

pub async fn dashboard_configuration_page() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}

pub async fn devices() -> impl IntoResponse {
    Json(json!({ "Items": [], "TotalRecordCount": 0 }))
}

pub async fn device_options() -> impl IntoResponse {
    Json(json!({}))
}

pub async fn default_directory_browser() -> impl IntoResponse {
    Json(json!(
        std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string))
            .unwrap_or_else(|| ".".to_string())
    ))
}

pub async fn directory_contents() -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

pub async fn drives() -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

pub async fn parent_path() -> impl IntoResponse {
    Json(json!(""))
}

pub async fn validate_path() -> impl IntoResponse {
    StatusCode::NO_CONTENT
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

pub async fn tmdb_client_configuration() -> impl IntoResponse {
    Json(json!({
        "IsTmdbEnabled": false
    }))
}

pub async fn system_logs() -> impl IntoResponse {
    Json(Vec::<Value>::new())
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

    let sql = if has_user_id {
        "SELECT name, log_type, created_at, user_id FROM activity_log WHERE user_id IS NOT NULL ORDER BY created_at DESC LIMIT ? OFFSET ?"
            .to_string()
    } else {
        "SELECT name, log_type, created_at, user_id FROM activity_log ORDER BY created_at DESC LIMIT ? OFFSET ?"
            .to_string()
    };

    match sqlx::query(&sql)
        .bind(limit)
        .bind(start_index)
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let created_at: i64 = row.try_get("created_at").unwrap_or_default();
                    json!({
                        "Name": row.try_get::<String, _>("name").unwrap_or_default(),
                        "Type": row.try_get::<String, _>("log_type").unwrap_or_default(),
                        "Date": crate::util::unix_to_jellyfin_date(created_at),
                        "UserId": row.try_get::<Option<String>, _>("user_id").unwrap_or_default(),
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

async fn app_setting(db: &AnyPool, key: &str, default: &str) -> String {
    sqlx::query("SELECT value FROM app_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get::<String, _>("value").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

async fn app_setting_bool(db: &AnyPool, key: &str, default: bool) -> bool {
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

async fn set_app_setting(db: &AnyPool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at")
        .bind(key)
        .bind(value)
        .bind(now_unix())
        .execute(db)
        .await?;
    Ok(())
}

async fn first_admin_user(db: &AnyPool) -> anyhow::Result<Option<(String, String)>> {
    let Some(row) = sqlx::query(
        "SELECT id, username FROM users WHERE is_admin = 1 ORDER BY created_at ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };

    Ok(Some((row.try_get("id")?, row.try_get("username")?)))
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

async fn last_task_result(db: &sqlx::AnyPool, task_id: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT status, start_time, end_time, message FROM task_results WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .ok()??;
    let status: String = row.try_get("status").ok()?;
    let start_time: Option<i64> = row.try_get("start_time").ok().flatten();
    let end_time: Option<i64> = row.try_get("end_time").ok().flatten();
    Some(json!({
        "Status": status,
        "StartTimeUtc": start_time.map(crate::util::unix_to_jellyfin_date),
        "EndTimeUtc": end_time.map(crate::util::unix_to_jellyfin_date),
        "Message": row.try_get::<Option<String>, _>("message").ok().flatten(),
    }))
}

pub async fn log_activity(
    db: &sqlx::AnyPool,
    name: &str,
    log_type: &str,
    user_id: Option<&str>,
    item_id: Option<&str>,
) {
    let now = now_unix();
    let id = crate::util::stable_text_id(&format!("activity:{now}:{name}:{log_type}"));
    let _ = sqlx::query(
        "INSERT INTO activity_log (id, name, log_type, user_id, item_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(log_type)
    .bind(user_id)
    .bind(item_id)
    .bind(now)
    .execute(db)
    .await;
}

pub async fn upsert_task_result(
    db: &sqlx::AnyPool,
    task_id: &str,
    status: &str,
    start_time: i64,
    end_time: i64,
    message: Option<&str>,
) {
    let _ = sqlx::query(
        "INSERT INTO task_results (task_id, status, start_time, end_time, message) VALUES (?, ?, ?, ?, ?) ON CONFLICT(task_id) DO UPDATE SET status = excluded.status, start_time = excluded.start_time, end_time = excluded.end_time, message = excluded.message",
    )
    .bind(task_id)
    .bind(status)
    .bind(start_time)
    .bind(end_time)
    .bind(message)
    .execute(db)
    .await;
}

pub async fn shutdown_handler() -> Response {
    tracing::info!("shutdown requested; exiting in 1 second");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        std::process::exit(0);
    });
    axum::http::StatusCode::NO_CONTENT.into_response()
}
