use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    app::state::{AppState, SessionCapabilities, session_timeout_seconds},
    jellyfin::auth::{header_token, request_token, request_user_id_or_default},
    util::now_unix,
};

#[derive(Deserialize)]
pub struct CapabilitiesRequest {
    #[serde(rename = "PlayableMediaTypes", default)]
    playable_media_types: Vec<String>,
    #[serde(rename = "SupportedCommands", default)]
    supported_commands: Vec<String>,
    #[serde(rename = "SupportsMediaControl", default)]
    supports_media_control: bool,
    #[serde(rename = "SupportsPersistentIdentifier", default)]
    supports_persistent_identifier: bool,
}

pub async fn sessions(State(state): State<Arc<AppState>>) -> Response {
    let now = now_unix();
    let timeout = session_timeout_seconds();
    let mut sessions_guard = state.playback_sessions.write().await;
    sessions_guard.retain(|_, session| now - session.last_activity_unix <= timeout);
    let sessions = sessions_guard.values().cloned().collect::<Vec<_>>();
    Json(sessions).into_response()
}

pub async fn capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<CapabilitiesRequest>>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    let device = device_info_from_headers(&headers);
    let capabilities = body.map(|Json(body)| body).unwrap_or_default();
    let session_capabilities = SessionCapabilities {
        user_id,
        client: device.client,
        device_name: device.device_name,
        device_id: device.device_id.clone(),
        application_version: device.version,
        playable_media_types: default_if_empty(
            capabilities.playable_media_types,
            ["Audio", "Video"].map(ToString::to_string).to_vec(),
        ),
        supported_commands: default_if_empty(
            capabilities.supported_commands,
            ["Play", "Pause", "Stop", "Seek", "SetVolume", "ToggleMute"]
                .map(ToString::to_string)
                .to_vec(),
        ),
        supports_media_control: capabilities.supports_media_control,
        supports_persistent_identifier: capabilities.supports_persistent_identifier,
    };

    state.session_capabilities.write().await.insert(
        session_key(&headers, &query, &device.device_id),
        session_capabilities,
    );
    StatusCode::NO_CONTENT.into_response()
}

pub async fn capabilities_full(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    let request = CapabilitiesRequest {
        playable_media_types: body
            .get("PlayableMediaTypes")
            .and_then(Value::as_array)
            .map(|values| string_array(values))
            .unwrap_or_default(),
        supported_commands: body
            .get("SupportedCommands")
            .and_then(Value::as_array)
            .map(|values| string_array(values))
            .unwrap_or_default(),
        supports_media_control: body
            .get("SupportsMediaControl")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        supports_persistent_identifier: body
            .get("SupportsPersistentIdentifier")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    capabilities(State(state), headers, Query(query), Some(Json(request))).await
}

pub async fn session_info(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> SessionCapabilities {
    let device = device_info_from_headers(headers);
    let key = session_key(headers, query, &device.device_id);
    state
        .session_capabilities
        .read()
        .await
        .get(&key)
        .cloned()
        .unwrap_or_else(|| SessionCapabilities {
            user_id: String::new(),
            client: device.client,
            device_name: device.device_name,
            device_id: device.device_id,
            application_version: device.version,
            playable_media_types: vec!["Audio".to_string(), "Video".to_string()],
            supported_commands: vec![
                "Play".to_string(),
                "Pause".to_string(),
                "Stop".to_string(),
                "Seek".to_string(),
            ],
            supports_media_control: true,
            supports_persistent_identifier: true,
        })
}

fn session_key(headers: &HeaderMap, query: &HashMap<String, String>, device_id: &str) -> String {
    request_token(headers, query).unwrap_or_else(|| format!("device:{device_id}"))
}

fn device_info_from_headers(headers: &HeaderMap) -> DeviceInfo {
    let authorization = header_token(headers, "X-Emby-Authorization")
        .or_else(|| header_token(headers, "X-Emby-authorization"))
        .unwrap_or_default();
    DeviceInfo {
        client: auth_value(&authorization, "Client").unwrap_or_else(|| "jellyfin-rs".to_string()),
        device_name: auth_value(&authorization, "Device")
            .unwrap_or_else(|| "Unknown Device".to_string()),
        device_id: auth_value(&authorization, "DeviceId").unwrap_or_default(),
        version: auth_value(&authorization, "Version").unwrap_or_else(|| "0.1.0".to_string()),
    }
}

fn auth_value(header: &str, key: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let part = part
            .trim()
            .strip_prefix("MediaBrowser ")
            .unwrap_or(part.trim());
        part.strip_prefix(&format!("{key}="))
            .or_else(|| part.strip_prefix(&format!("{key}=\"")))
            .map(|value| value.trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
    })
}

fn default_if_empty(values: Vec<String>, default: Vec<String>) -> Vec<String> {
    if values.is_empty() { default } else { values }
}

fn string_array(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

struct DeviceInfo {
    client: String,
    device_name: String,
    device_id: String,
    version: String,
}

impl Default for CapabilitiesRequest {
    fn default() -> Self {
        Self {
            playable_media_types: Vec::new(),
            supported_commands: Vec::new(),
            supports_media_control: true,
            supports_persistent_identifier: true,
        }
    }
}
