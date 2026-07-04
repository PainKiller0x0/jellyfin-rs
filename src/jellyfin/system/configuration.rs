use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME, SERVER_NAME},
    entities::users::{self, Entity as Users},
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

type Response = axum::response::Response;

pub(super) const SERVER_CONFIG_SETTING_KEY: &str = "server_config";
const MAX_SERVER_CONFIGURATION_JSON_BYTES: usize = 256 * 1024;
const MAX_NAMED_CONFIGURATION_KEY_LEN: usize = 64;
const MAX_NAMED_CONFIGURATION_JSON_BYTES: usize = 128 * 1024;

pub async fn startup_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ServerName": super::app_setting(&state.db, "ServerName", SERVER_NAME).await,
        "UICulture": super::app_setting(&state.db, "UICulture", "zh-CN").await,
        "MetadataCountryCode": super::app_setting(&state.db, "MetadataCountryCode", "CN").await,
        "PreferredMetadataLanguage": super::app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        "EnableRemoteAccess": super::app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
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
            if let Err(error) = super::set_app_setting(&state.db, key, value.trim()).await {
                return internal_error(error);
            }
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn startup_user(State(state): State<Arc<AppState>>) -> Response {
    match super::first_admin_user(&state.db).await {
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

    let result = match Users::find_by_id(&user_id).one(&state.db).await {
        Ok(Some(model)) => {
            let mut active: users::ActiveModel = model.into();
            active.username = Set(username.to_string());
            active.password_hash = Set(Some(password_hash));
            active.display_name = Set(username.to_string());
            active.is_admin = Set(1);
            active.is_disabled = Set(0);
            active.updated_at = Set(now);
            active.update(&state.db).await.map(|_| ())
        }
        Ok(None) => {
            let active = users::ActiveModel {
                id: Set(user_id.clone()),
                username: Set(username.to_string()),
                password_hash: Set(Some(password_hash)),
                display_name: Set(username.to_string()),
                is_admin: Set(1),
                is_disabled: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            Users::insert(active).exec(&state.db).await.map(|_| ())
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(_) => Json(json!({ "Id": user_id, "Name": username })).into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_remote_access(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RemoteAccessRequest>,
) -> Response {
    match super::set_app_setting(
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
    match super::set_app_setting(&state.db, "StartupWizardCompleted", "true").await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn server_configuration(State(state): State<Arc<AppState>>) -> Response {
    let saved_config = super::server_config_json(&state.db).await;
    Json(server_configuration_value(
        saved_config,
        super::app_setting(&state.db, "ServerName", SERVER_NAME).await,
        super::app_setting(&state.db, "UICulture", "zh-CN").await,
        super::app_setting(&state.db, "MetadataCountryCode", "CN").await,
        super::app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        super::app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
    ))
    .into_response()
}

pub async fn update_server_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Value>,
) -> Response {
    let serialized = match serialize_server_configuration(&request) {
        Ok(serialized) => serialized,
        Err(error) => return validation_error_response(error),
    };

    if let Err(error) = sync_runtime_server_settings(&state.db, &request).await {
        return internal_error(error);
    }

    match super::set_app_setting(&state.db, SERVER_CONFIG_SETTING_KEY, &serialized).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
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

pub async fn named_configuration(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Response {
    let Some(key) = normalize_named_configuration_key(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let value = super::app_setting(&state.db, &named_configuration_setting_key(&key), "").await;
    let value = if value.trim().is_empty() {
        default_named_configuration(&key)
    } else {
        serde_json::from_str(&value).unwrap_or_else(|_| default_named_configuration(&key))
    };
    Json(value).into_response()
}

pub async fn update_named_configuration(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(request): Json<Value>,
) -> Response {
    let Some(key) = normalize_named_configuration_key(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let serialized = match serialize_named_configuration(&request) {
        Ok(serialized) => serialized,
        Err(error) => return validation_error_response(error),
    };
    match super::set_app_setting(
        &state.db,
        &named_configuration_setting_key(&key),
        &serialized,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn configuration_pages(
    Query(_query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

pub async fn dashboard_configuration_page() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}

pub(super) fn merge_server_configuration_patch(
    current: Value,
    patch: Value,
) -> Result<String, (StatusCode, &'static str)> {
    if !patch.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Configuration must be a JSON object",
        ));
    }

    let mut merged = if current.is_object() {
        current
    } else {
        Value::Object(Map::new())
    };
    if let (Some(target), Some(patch)) = (merged.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
    serialize_server_configuration(&merged)
}

pub(super) async fn sync_runtime_server_settings(
    db: &DatabaseConnection,
    request: &Value,
) -> anyhow::Result<()> {
    for key in [
        "ServerName",
        "UICulture",
        "MetadataCountryCode",
        "PreferredMetadataLanguage",
    ] {
        if let Some(value) = request.get(key).and_then(Value::as_str) {
            super::set_app_setting(db, key, value.trim()).await?;
        }
    }

    if let Some(value) = request.get("EnableRemoteAccess").and_then(Value::as_bool) {
        super::set_app_setting(
            db,
            "EnableRemoteAccess",
            if value { "true" } else { "false" },
        )
        .await?;
    }

    Ok(())
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

fn server_configuration_value(
    saved_config: Value,
    server_name: String,
    ui_culture: String,
    metadata_country_code: String,
    preferred_metadata_language: String,
    enable_remote_access: bool,
) -> Value {
    let mut config = default_server_configuration();
    if let (Some(config), Some(saved)) = (config.as_object_mut(), saved_config.as_object()) {
        for (key, value) in saved {
            config.insert(key.clone(), value.clone());
        }
    }

    let object = config.as_object_mut().expect("default config is an object");
    object.insert("ServerName".to_string(), json!(server_name));
    object.insert("UICulture".to_string(), json!(ui_culture));
    object.insert(
        "MetadataCountryCode".to_string(),
        json!(metadata_country_code),
    );
    object.insert(
        "PreferredMetadataLanguage".to_string(),
        json!(preferred_metadata_language),
    );
    object.insert(
        "EnableRemoteAccess".to_string(),
        json!(enable_remote_access),
    );
    if !object
        .get("ContentTypes")
        .is_some_and(|value| value.is_array())
    {
        object.insert("ContentTypes".to_string(), json!([]));
    }
    if !object
        .get("CastReceiverApplications")
        .is_some_and(|value| value.is_array())
    {
        object.insert(
            "CastReceiverApplications".to_string(),
            default_cast_receiver_applications(),
        );
    }
    config
}

fn default_server_configuration() -> Value {
    json!({
        "ContentTypes": [],
        "CastReceiverApplications": default_cast_receiver_applications(),
        "PluginRepositories": [],
        "LocalNetworkSubnets": [],
        "LocalNetworkAddresses": [],
        "KnownProxies": [],
        "PublishedServerUriBySubnet": []
    })
}

fn default_cast_receiver_applications() -> Value {
    json!([
        {
            "Id": "",
            "Name": "Disabled"
        }
    ])
}

fn serialize_server_configuration(value: &Value) -> Result<String, (StatusCode, &'static str)> {
    serialize_json_object(value, MAX_SERVER_CONFIGURATION_JSON_BYTES, "Configuration")
}

fn serialize_named_configuration(value: &Value) -> Result<String, (StatusCode, &'static str)> {
    serialize_json_object(value, MAX_NAMED_CONFIGURATION_JSON_BYTES, "Configuration")
}

fn serialize_json_object(
    value: &Value,
    max_bytes: usize,
    _name: &'static str,
) -> Result<String, (StatusCode, &'static str)> {
    if !value.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Configuration must be a JSON object",
        ));
    }
    let serialized = value.to_string();
    if serialized.len() > max_bytes {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Configuration is too large"));
    }
    Ok(serialized)
}

fn normalize_named_configuration_key(key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_NAMED_CONFIGURATION_KEY_LEN {
        return None;
    }
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(key.to_ascii_lowercase())
}

fn named_configuration_setting_key(key: &str) -> String {
    format!("named_config:{key}")
}

fn default_named_configuration(key: &str) -> Value {
    match key {
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
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_NAMED_CONFIGURATION_JSON_BYTES, default_named_configuration,
        merge_server_configuration_patch, named_configuration_setting_key,
        normalize_named_configuration_key, serialize_named_configuration,
        server_configuration_value,
    };
    use axum::http::StatusCode;
    use serde_json::json;

    #[test]
    fn named_configuration_keys_are_normalized_safely() {
        assert_eq!(
            normalize_named_configuration_key("Encoding").as_deref(),
            Some("encoding")
        );
        assert_eq!(
            normalize_named_configuration_key("Network-Map.1").as_deref(),
            Some("network-map.1")
        );
        assert!(normalize_named_configuration_key("").is_none());
        assert!(normalize_named_configuration_key("../network").is_none());
        assert!(normalize_named_configuration_key(&"x".repeat(65)).is_none());
        assert_eq!(
            named_configuration_setting_key("encoding"),
            "named_config:encoding"
        );
        assert_eq!(
            default_named_configuration("network")["EnableRemoteAccess"],
            false
        );
    }

    #[test]
    fn named_configuration_payload_must_be_object_and_limited() {
        assert_eq!(
            serialize_named_configuration(&json!([])).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        let oversized = json!({ "Value": "x".repeat(MAX_NAMED_CONFIGURATION_JSON_BYTES) });
        assert_eq!(
            serialize_named_configuration(&oversized).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[test]
    fn server_configuration_merges_saved_and_runtime_settings() {
        let value = server_configuration_value(
            json!({
                "ContentTypes": [{ "Name": "/media", "Value": "movies" }],
                "KnownProxies": ["127.0.0.1"],
                "EnableRemoteAccess": true
            }),
            "Home Server".to_string(),
            "en-US".to_string(),
            "US".to_string(),
            "en".to_string(),
            false,
        );

        assert_eq!(value["ServerName"], "Home Server");
        assert_eq!(value["EnableRemoteAccess"], false);
        assert_eq!(value["ContentTypes"][0]["Value"], "movies");
        assert_eq!(value["KnownProxies"][0], "127.0.0.1");
        assert!(value["CastReceiverApplications"].as_array().is_some());
    }

    #[test]
    fn partial_server_configuration_requires_object_patch() {
        assert_eq!(
            merge_server_configuration_patch(json!({}), json!([]))
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        let merged = merge_server_configuration_patch(
            json!({ "ServerName": "old", "ContentTypes": [] }),
            json!({ "ServerName": "new" }),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["ServerName"], "new");
        assert!(value["ContentTypes"].as_array().is_some());
    }
}
