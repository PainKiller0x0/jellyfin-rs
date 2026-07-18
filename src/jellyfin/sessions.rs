use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, EntityTrait};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::state::{
        AppState, PlaybackSession, PlaybackState, SessionCapabilities, SessionUserInfo,
        session_timeout_seconds,
    },
    entities::users::Entity as Users,
    jellyfin::{
        auth::{auth_header_value_field, request_token, request_user_id_or_default},
        common::internal_error,
    },
    util::{now_unix, stable_text_id, unix_to_jellyfin_date},
};

const MAX_SESSION_TEXT_LEN: usize = 256;
const MAX_SESSION_ID_LEN: usize = 256;
const MAX_SESSION_ARRAY_ITEMS: usize = 64;
const MAX_SESSION_ARRAY_VALUE_LEN: usize = 64;

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

#[derive(Default, Deserialize)]
pub struct SessionsQuery {
    #[serde(rename = "controllableByUserId", alias = "ControllableByUserId")]
    controllable_by_user_id: Option<String>,
    #[serde(rename = "deviceId", alias = "DeviceId")]
    device_id: Option<String>,
    #[serde(rename = "activeWithinSeconds", alias = "ActiveWithinSeconds")]
    active_within_seconds: Option<i64>,
}

pub async fn sessions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SessionsQuery>,
) -> Response {
    let now = now_unix();
    let timeout = session_timeout_seconds();
    let mut sessions_guard = state.playback_sessions.write().await;
    sessions_guard.retain(|_, session| now - session.last_activity_unix <= timeout);
    let sessions = sessions_guard.values().cloned().collect::<Vec<_>>();
    Json(filter_sessions(sessions, &query, now)).into_response()
}

fn filter_sessions(
    mut sessions: Vec<PlaybackSession>,
    query: &SessionsQuery,
    now: i64,
) -> Vec<PlaybackSession> {
    if let Some(device_id) = query
        .device_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sessions.retain(|session| session.device_id.eq_ignore_ascii_case(device_id));
    }
    if let Some(active_within_seconds) = query.active_within_seconds.filter(|value| *value >= 0) {
        sessions.retain(|session| now - session.last_activity_unix <= active_within_seconds);
    }
    if let Some(user_id) = query
        .controllable_by_user_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sessions.retain(|session| {
            session.supports_remote_control
                && (session.user_id == user_id
                    || session
                        .additional_users
                        .iter()
                        .any(|user| user.user_id == user_id))
        });
    }
    sessions
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(token) = request_token(&headers, &query) else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let now = now_unix();
    let backend = state.db.get_database_backend();
    if let Err(error) = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE access_tokens SET revoked_at = ? WHERE token_hash = ? AND revoked_at IS NULL",
            vec![now.into(), stable_text_id(&token).into()],
        ))
        .await
    {
        return internal_error(error.into());
    }

    state.playback_sessions.write().await.remove(&token);
    state.session_capabilities.write().await.remove(&token);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    StatusCode::NO_CONTENT.into_response()
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
        user_id: user_id.clone(),
        client: device.client,
        device_name: device.device_name,
        device_id: device.device_id.clone(),
        application_version: device.version,
        playable_media_types: default_if_empty(
            normalize_string_array(
                capabilities.playable_media_types,
                MAX_SESSION_ARRAY_ITEMS,
                MAX_SESSION_ARRAY_VALUE_LEN,
            ),
            ["Audio", "Video"].map(ToString::to_string).to_vec(),
        ),
        supported_commands: default_if_empty(
            normalize_string_array(
                capabilities.supported_commands,
                MAX_SESSION_ARRAY_ITEMS,
                MAX_SESSION_ARRAY_VALUE_LEN,
            ),
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
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
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
            .map(|values| {
                string_array(values, MAX_SESSION_ARRAY_ITEMS, MAX_SESSION_ARRAY_VALUE_LEN)
            })
            .unwrap_or_default(),
        supported_commands: body
            .get("SupportedCommands")
            .and_then(Value::as_array)
            .map(|values| {
                string_array(values, MAX_SESSION_ARRAY_ITEMS, MAX_SESSION_ARRAY_VALUE_LEN)
            })
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

pub async fn playback_ping(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(session_id) = query
        .get("playSessionId")
        .or_else(|| query.get("PlaySessionId"))
        .map(String::as_str)
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    touch_session(&state, session_id).await
}

pub async fn touch_session_by_id(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    touch_session(&state, &session_id).await
}

pub async fn touch_session_command(
    State(state): State<Arc<AppState>>,
    Path((session_id, _command)): Path<(String, String)>,
) -> Response {
    touch_session(&state, &session_id).await
}

pub async fn playstate_command(
    State(state): State<Arc<AppState>>,
    Path((session_id, command)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let seek_position_ticks = match seek_position_ticks(&query) {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "Error": message }))).into_response();
        }
    };
    let now = now_unix();
    let mut sessions = state.playback_sessions.write().await;
    let remove_session = {
        let Some(session) = sessions.get_mut(&session_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let remove_session = match apply_playstate_command(session, &command, seek_position_ticks) {
            Ok(remove_session) => remove_session,
            Err(message) => {
                return (StatusCode::BAD_REQUEST, Json(json!({ "Error": message })))
                    .into_response();
            }
        };
        if !remove_session {
            touch_session_time(session, now);
        }
        remove_session
    };
    if remove_session {
        sessions.remove(&session_id);
    }
    drop(sessions);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn report_viewing(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(item_id) = query_text(&query, &["itemId", "ItemId"], MAX_SESSION_ID_LEN) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    let device = device_info_from_headers(&headers);
    let session_id = query
        .get("sessionId")
        .or_else(|| query.get("SessionId"))
        .and_then(|value| normalize_session_text(value, MAX_SESSION_ID_LEN))
        .unwrap_or_else(|| session_key(&headers, &query, &device.device_id));
    let session_info = session_info(&state, &headers, &query).await;
    let now = now_unix();
    let last_activity_date = unix_to_jellyfin_date(now);
    let client = session_info.client;
    let device_name = device.device_name;
    let device_id = device.device_id;
    let application_version = session_info.application_version;
    let playable_media_types = session_info.playable_media_types;
    let supported_commands = session_info.supported_commands;
    let supports_media_control = session_info.supports_media_control;
    let supports_persistent_identifier = session_info.supports_persistent_identifier;

    let session = PlaybackSession {
        id: session_id.clone(),
        user_id: user_id.clone(),
        play_session_id: session_id.clone(),
        item_id,
        item_name: query_text(&query, &["itemName", "ItemName"], MAX_SESSION_TEXT_LEN),
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
            user_id: user_id.clone(),
            client,
            device_name,
            device_id,
            application_version,
            playable_media_types,
            supported_commands,
            supports_media_control,
            supports_persistent_identifier,
        },
    };
    state
        .playback_sessions
        .write()
        .await
        .insert(session_id, session);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    StatusCode::NO_CONTENT.into_response()
}

async fn touch_session(state: &AppState, session_id: &str) -> Response {
    let now = now_unix();
    let mut sessions = state.playback_sessions.write().await;
    let Some(session) = sessions.get_mut(session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    touch_session_time(session, now);
    drop(sessions);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    StatusCode::NO_CONTENT.into_response()
}

fn touch_session_time(session: &mut PlaybackSession, now: i64) {
    session.last_activity_unix = now;
    session.last_activity_date = unix_to_jellyfin_date(now);
    session.last_playback_check_in = session.last_activity_date.clone();
}

fn apply_playstate_command(
    session: &mut PlaybackSession,
    command: &str,
    seek_position_ticks: Option<i64>,
) -> Result<bool, &'static str> {
    match command.to_ascii_lowercase().as_str() {
        "stop" => Ok(true),
        "pause" => {
            session.play_state.is_paused = true;
            Ok(false)
        }
        "unpause" => {
            session.play_state.is_paused = false;
            Ok(false)
        }
        "playpause" => {
            session.play_state.is_paused = !session.play_state.is_paused;
            Ok(false)
        }
        "seek" => {
            let Some(position) = seek_position_ticks else {
                return Err("seekPositionTicks is required");
            };
            session.play_state.position_ticks = position.max(0);
            Ok(false)
        }
        "rewind" => {
            session.play_state.position_ticks =
                (session.play_state.position_ticks - 10_000_000).max(0);
            Ok(false)
        }
        "fastforward" => {
            session.play_state.position_ticks += 10_000_000;
            Ok(false)
        }
        "nexttrack" | "previoustrack" => {
            // ponytail: queue mutation needs a real queue model; for now this keeps remote clients compatible.
            Ok(false)
        }
        _ => Err("unknown playstate command"),
    }
}

fn seek_position_ticks(query: &HashMap<String, String>) -> Result<Option<i64>, &'static str> {
    query
        .get("seekPositionTicks")
        .or_else(|| query.get("SeekPositionTicks"))
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| "seekPositionTicks is invalid")
        })
        .transpose()
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
    request_token(headers, query)
        .and_then(|token| normalize_session_text(&token, MAX_SESSION_ID_LEN))
        .unwrap_or_else(|| {
            normalize_session_text(device_id, MAX_SESSION_ID_LEN)
                .map(|device_id| format!("device:{device_id}"))
                .unwrap_or_else(|| "device:unknown".to_string())
        })
}

fn device_info_from_headers(headers: &HeaderMap) -> DeviceInfo {
    DeviceInfo {
        client: auth_value_from_headers(headers, "Client")
            .and_then(|value| normalize_session_text(&value, MAX_SESSION_TEXT_LEN))
            .unwrap_or_else(|| "jellyfin-rs".to_string()),
        device_name: auth_value_from_headers(headers, "Device")
            .and_then(|value| normalize_session_text(&value, MAX_SESSION_TEXT_LEN))
            .unwrap_or_else(|| "Unknown Device".to_string()),
        device_id: auth_value_from_headers(headers, "DeviceId")
            .and_then(|value| normalize_session_text(&value, MAX_SESSION_ID_LEN))
            .unwrap_or_default(),
        version: auth_value_from_headers(headers, "Version")
            .and_then(|value| normalize_session_text(&value, MAX_SESSION_TEXT_LEN))
            .unwrap_or_else(|| "0.1.0".to_string()),
    }
}

fn auth_value_from_headers(headers: &HeaderMap, key: &str) -> Option<String> {
    [
        "Authorization",
        "X-Emby-Authorization",
        "X-MediaBrowser-Authorization",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| auth_value(value, key))
    })
}

fn auth_value(header: &str, key: &str) -> Option<String> {
    auth_header_value_field(header, key)
}

fn default_if_empty(values: Vec<String>, default: Vec<String>) -> Vec<String> {
    if values.is_empty() { default } else { values }
}

fn string_array(values: &[Value], max_items: usize, max_len: usize) -> Vec<String> {
    normalize_string_array(
        values
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string),
        max_items,
        max_len,
    )
}

fn normalize_string_array<I>(values: I, max_items: usize, max_len: usize) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut normalized = Vec::new();
    for value in values {
        let Some(value) = normalize_session_text(&value, max_len) else {
            continue;
        };
        if normalized.iter().any(|existing| existing == &value) {
            continue;
        }
        normalized.push(value);
        if normalized.len() >= max_items {
            break;
        }
    }
    normalized
}

fn query_text(query: &HashMap<String, String>, keys: &[&str], max_len: usize) -> Option<String> {
    keys.iter()
        .find_map(|key| query.get(*key))
        .and_then(|value| normalize_session_text(value, max_len))
}

fn normalize_session_text(value: &str, max_len: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let text = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_len)
        .collect::<String>()
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
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

/// POST /Sessions/{session_id}/Users/{user_id} — add user to session (stub)
pub async fn session_add_user(
    State(state): State<Arc<AppState>>,
    Path((session_id, user_id)): Path<(String, String)>,
) -> Response {
    let (session_id, user_id) = match session_user_params(&session_id, &user_id) {
        Ok(params) => params,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response();
        }
    };
    let Some(user) = session_user_info(&state, &user_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut sessions = state.playback_sessions.write().await;
    let Some(session) = sessions.get_mut(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if session.additional_users.len() >= MAX_SESSION_ARRAY_ITEMS
        && !session
            .additional_users
            .iter()
            .any(|existing| existing.user_id == user.user_id)
    {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Too many session users" })),
        )
            .into_response();
    }
    let mut changed = false;
    if !session
        .additional_users
        .iter()
        .any(|existing| existing.user_id == user.user_id)
    {
        session.additional_users.push(user);
        touch_session_time(session, now_unix());
        changed = true;
    }
    drop(sessions);
    if changed {
        let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /Sessions/{session_id}/Users/{user_id} — remove user from session (stub)
pub async fn session_remove_user(
    State(state): State<Arc<AppState>>,
    Path((session_id, user_id)): Path<(String, String)>,
) -> Response {
    let (session_id, user_id) = match session_user_params(&session_id, &user_id) {
        Ok(params) => params,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response();
        }
    };
    let mut sessions = state.playback_sessions.write().await;
    let Some(session) = sessions.get_mut(&session_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let before = session.additional_users.len();
    session
        .additional_users
        .retain(|user| user.user_id != user_id);
    let changed = session.additional_users.len() != before;
    if changed {
        touch_session_time(session, now_unix());
    }
    drop(sessions);
    if changed {
        let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    }
    StatusCode::NO_CONTENT.into_response()
}

fn session_user_params(session_id: &str, user_id: &str) -> anyhow::Result<(String, String)> {
    Ok((
        validate_session_path_id(session_id, "SessionId")?,
        validate_session_path_id(user_id, "UserId")?,
    ))
}

fn validate_session_path_id(value: &str, label: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("{label} is required");
    }
    if value.len() > MAX_SESSION_ID_LEN
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        anyhow::bail!("Invalid {label}");
    }
    Ok(value.to_string())
}

async fn session_user_info(state: &AppState, user_id: &str) -> Option<SessionUserInfo> {
    Users::find_by_id(user_id)
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .filter(|user| user.is_disabled == 0)
        .map(|user| SessionUserInfo {
            user_id: user.id,
            user_name: user.username,
        })
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SESSION_ARRAY_ITEMS, MAX_SESSION_ARRAY_VALUE_LEN, MAX_SESSION_ID_LEN,
        MAX_SESSION_TEXT_LEN, SessionsQuery, apply_playstate_command, auth_value,
        device_info_from_headers, filter_sessions, normalize_string_array, query_text,
        session_add_user, session_key, session_remove_user, touch_session_time,
        validate_session_path_id,
    };
    use crate::app::state::{
        AppState, PlaybackSession, PlaybackState, SessionCapabilities, SessionUserInfo,
    };
    use axum::{
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode},
    };
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use serde_json::json;
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[test]
    fn auth_value_reads_comma_separated_jellyfin_header() {
        let header =
            r#"MediaBrowser Client="Web", Device="Browser", DeviceId="dev1", Version="1.0""#;
        assert_eq!(auth_value(header, "Client").as_deref(), Some("Web"));
        assert_eq!(auth_value(header, "Device").as_deref(), Some("Browser"));
        assert_eq!(auth_value(header, "DeviceId").as_deref(), Some("dev1"));
        assert_eq!(auth_value(header, "Version").as_deref(), Some("1.0"));
    }

    #[test]
    fn session_key_uses_token_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            r#"MediaBrowser Client="Web", Token="abc""#.parse().unwrap(),
        );
        let query = HashMap::new();
        assert_eq!(session_key(&headers, &query, "device-1"), "abc");
    }

    #[test]
    fn device_info_uses_authorization_header_not_token_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static(
                r#"MediaBrowser Client="Web", Device="Browser", DeviceId="dev1", Version="1.0", Token="abc""#,
            ),
        );
        let device = device_info_from_headers(&headers);
        assert_eq!(device.client, "Web");
        assert_eq!(device.device_name, "Browser");
        assert_eq!(device.device_id, "dev1");
        assert_eq!(device.version, "1.0");
    }

    #[test]
    fn device_info_accepts_jellyfin_compat_authorization_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-MediaBrowser-Authorization",
            HeaderValue::from_static(
                r#"MediaBrowser Client="Tsukimi", Device="Desktop", DeviceId="dev2", Version="1.2.3""#,
            ),
        );
        let device = device_info_from_headers(&headers);
        assert_eq!(device.client, "Tsukimi");
        assert_eq!(device.device_name, "Desktop");
        assert_eq!(device.device_id, "dev2");
        assert_eq!(device.version, "1.2.3");

        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Emby-Authorization",
            HeaderValue::from_static(
                "Emby Client=Tsukimi,Device=linux,DeviceId=dev3,Version=26.7.3",
            ),
        );
        let device = device_info_from_headers(&headers);
        assert_eq!(device.client, "Tsukimi");
        assert_eq!(device.device_name, "linux");
        assert_eq!(device.device_id, "dev3");
        assert_eq!(device.version, "26.7.3");
    }

    #[test]
    fn session_inputs_are_normalized_and_limited() {
        let mut headers = HeaderMap::new();
        let long_device_id = "d".repeat(MAX_SESSION_ID_LEN + 20);
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!(
                r#"MediaBrowser Client=" Web ", Device="{}", DeviceId="{long_device_id}", Version="1.0""#,
                "B".repeat(MAX_SESSION_TEXT_LEN + 20)
            ))
            .unwrap(),
        );
        let device = device_info_from_headers(&headers);
        assert_eq!(device.client, "Web");
        assert_eq!(device.device_name.len(), MAX_SESSION_TEXT_LEN);
        assert_eq!(device.device_id.len(), MAX_SESSION_ID_LEN);
        assert_eq!(device.version, "1.0");

        let values = normalize_string_array(
            (0..MAX_SESSION_ARRAY_ITEMS + 10).map(|index| {
                if index % 2 == 0 {
                    "Play".to_string()
                } else {
                    format!("{}{}", "x".repeat(MAX_SESSION_ARRAY_VALUE_LEN + 5), index)
                }
            }),
            MAX_SESSION_ARRAY_ITEMS,
            MAX_SESSION_ARRAY_VALUE_LEN,
        );
        assert!(values.len() <= MAX_SESSION_ARRAY_ITEMS);
        assert!(
            values
                .iter()
                .all(|value| value.len() <= MAX_SESSION_ARRAY_VALUE_LEN)
        );
        assert_eq!(
            values
                .iter()
                .filter(|value| value.as_str() == "Play")
                .count(),
            1
        );

        let mut query = HashMap::new();
        query.insert("ItemName".to_string(), " Movie\nName ".to_string());
        assert_eq!(
            query_text(&query, &["ItemName"], MAX_SESSION_TEXT_LEN).as_deref(),
            Some("MovieName")
        );
    }

    #[test]
    fn session_serializes_additional_users() {
        let session = test_session();
        let value = serde_json::to_value(session).unwrap();
        assert_eq!(
            value["AdditionalUsers"],
            json!([{ "UserId": "u2", "UserName": "guest" }])
        );
        assert_eq!(value["LastPlaybackCheckIn"], "1970-01-01T00:00:00Z");
        assert_eq!(value["SupportsRemoteControl"], true);
        assert_eq!(
            value["Capabilities"]["PlayableMediaTypes"],
            json!(["Video"])
        );
    }

    #[test]
    fn playstate_command_updates_session_state() {
        let mut session = test_session();
        assert_eq!(
            apply_playstate_command(&mut session, "Pause", None),
            Ok(false)
        );
        assert!(session.play_state.is_paused);
        assert_eq!(
            apply_playstate_command(&mut session, "PlayPause", None),
            Ok(false)
        );
        assert!(!session.play_state.is_paused);
        assert_eq!(
            apply_playstate_command(&mut session, "Seek", Some(42)),
            Ok(false)
        );
        assert_eq!(session.play_state.position_ticks, 42);
        assert_eq!(
            apply_playstate_command(&mut session, "FastForward", None),
            Ok(false)
        );
        assert_eq!(session.play_state.position_ticks, 10_000_042);
        assert_eq!(
            apply_playstate_command(&mut session, "Rewind", None),
            Ok(false)
        );
        assert_eq!(session.play_state.position_ticks, 42);
        assert_eq!(
            apply_playstate_command(&mut session, "NextTrack", None),
            Ok(false)
        );
        assert_eq!(
            apply_playstate_command(&mut session, "PreviousTrack", None),
            Ok(false)
        );
        assert_eq!(
            apply_playstate_command(&mut session, "Stop", None),
            Ok(true)
        );
        assert!(apply_playstate_command(&mut session, "Seek", None).is_err());
    }

    #[test]
    fn touch_session_time_keeps_session_dates_in_sync() {
        let mut session = test_session();
        touch_session_time(&mut session, 1);
        assert_eq!(session.last_activity_unix, 1);
        assert_eq!(session.last_playback_check_in, session.last_activity_date);
    }

    #[test]
    fn session_query_filters_by_device_activity_and_control_user() {
        let mut stale = test_session();
        stale.id = "old".to_string();
        stale.device_id = "other".to_string();
        stale.last_activity_unix = 1;

        let query = SessionsQuery {
            controllable_by_user_id: Some("u2".to_string()),
            device_id: Some("device-1".to_string()),
            active_within_seconds: Some(10),
        };
        let sessions = filter_sessions(vec![test_session(), stale], &query, 5);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s1");
    }

    #[test]
    fn session_path_ids_are_validated() {
        assert_eq!(
            validate_session_path_id("  s1  ", "SessionId").unwrap(),
            "s1"
        );
        assert!(validate_session_path_id("", "SessionId").is_err());
        assert!(validate_session_path_id("bad\nid", "SessionId").is_err());
        assert!(
            validate_session_path_id(&"x".repeat(MAX_SESSION_ID_LEN + 1), "SessionId").is_err()
        );
    }

    #[tokio::test]
    async fn session_user_routes_add_dedupe_and_remove_users() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u2', 'guest', 'guest', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        let state = Arc::new(test_state(db));
        let mut session = test_session();
        session.additional_users.clear();
        session.last_activity_unix = 1;
        state
            .playback_sessions
            .write()
            .await
            .insert("s1".to_string(), session);

        let response = session_add_user(
            State(state.clone()),
            axum::extract::Path(("s1".to_string(), "u2".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let sessions = state.playback_sessions.read().await;
        let session = sessions.get("s1").unwrap();
        assert_eq!(session.additional_users.len(), 1);
        assert_eq!(session.additional_users[0].user_name, "guest");
        assert!(session.last_activity_unix >= 1);
        drop(sessions);

        let response = session_add_user(
            State(state.clone()),
            axum::extract::Path(("s1".to_string(), "u2".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            state
                .playback_sessions
                .read()
                .await
                .get("s1")
                .unwrap()
                .additional_users
                .len(),
            1
        );

        let response = session_remove_user(
            State(state.clone()),
            axum::extract::Path(("s1".to_string(), "u2".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            state
                .playback_sessions
                .read()
                .await
                .get("s1")
                .unwrap()
                .additional_users
                .is_empty()
        );
    }

    #[tokio::test]
    async fn session_user_routes_validate_and_limit_inputs() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('new-user', 'new-user', 'new-user', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        let state = Arc::new(test_state(db));
        let mut session = test_session();
        session.additional_users = (0..MAX_SESSION_ARRAY_ITEMS)
            .map(|index| SessionUserInfo {
                user_id: format!("u{index}"),
                user_name: format!("User {index}"),
            })
            .collect();
        state
            .playback_sessions
            .write()
            .await
            .insert("s1".to_string(), session);

        let response = session_add_user(
            State(state.clone()),
            axum::extract::Path(("s1".to_string(), "new-user".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let response = session_add_user(
            State(state),
            axum::extract::Path(("bad\nsession".to_string(), "new-user".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    fn test_session() -> PlaybackSession {
        PlaybackSession {
            id: "s1".to_string(),
            user_id: "u1".to_string(),
            play_session_id: "p1".to_string(),
            item_id: "i1".to_string(),
            item_name: None,
            now_playing_queue: Vec::new(),
            additional_users: vec![SessionUserInfo {
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
}
