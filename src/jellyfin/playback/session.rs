use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, PlaybackSession, PlaybackState, SessionCapabilities},
    db::row_ext::QueryResultExt,
    entities::user_data::{self, Entity as UserData},
    jellyfin::{
        auth::{
            query_user_id_or_request, request_token, request_user_id_and_admin_or_default,
            request_user_id_or_default,
        },
        common::{internal_error, strip_nulls},
        dlna,
        items::{find_media_item, find_media_item_for_admin},
    },
    library::models::media_source_json_with_streams,
    util::{now_unix, unix_to_jellyfin_date},
};

const PLAYED_PERCENT_THRESHOLD: f64 = 0.9;
const TICKS_PER_SECOND: i64 = 10_000_000;
const WATCH_POSITION_TOLERANCE_SECONDS: i64 = 30;

pub async fn playback_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let (request_user_id, is_admin) =
        request_user_id_and_admin_or_default(&state, &headers, &query).await;
    let user_id = if is_admin {
        query_user_id_or_request(&query, &request_user_id)
    } else {
        request_user_id
    };
    let item_result = if is_admin {
        find_media_item_for_admin(&state.db, &user_id, &item_id).await
    } else {
        find_media_item(&state.db, &user_id, &item_id).await
    };
    match item_result {
        Ok(Some(item)) => {
            let body_value = body.as_ref().map(|Json(value)| value);
            let profile = dlna::request_device_profile(body_value);
            let max_streaming_bitrate = playback_max_streaming_bitrate(&query, body_value);

            // For Movie/Episode folders with child Video files (multi-version), return their media sources
            let mut media_sources = if item.is_folder && (item.item_type == "Movie" || item.item_type == "Episode") {
                match super::child_video_sources(&state.db, &item.id, is_admin).await {
                    Ok(mut sources) if !sources.is_empty() => {
                        apply_playback_profile_to_sources(&mut sources, &profile, &query);
                        sources
                    }
                    Ok(_) => {
                        // No child videos, return the item itself (e.g. Episode folder is the video)
                        match super::media_streams_for_item(&state.db, &item.id).await {
                            Ok(streams) => {
                                let mut ms = media_source_json_with_streams(&item, streams);
                                let playback_streams = media_source_streams(&ms);
                                dlna::apply_playback_profile(&mut ms, &profile, &playback_streams, &query);
                                vec![ms]
                            }
                            Err(error) => return internal_error(error),
                        }
                    }
                    Err(e) => return internal_error(e),
                }
            } else if item.is_folder {
                // Season/Series/other folders: return empty sources, client should pick a specific item
                vec![]
            } else if item.item_type == "Episode" {
                match super::episode_version_sources(&state.db, &item, is_admin).await {
                    Ok(mut sources) if !sources.is_empty() => {
                        apply_playback_profile_to_sources(&mut sources, &profile, &query);
                        sources
                    }
                    Ok(_) => match super::media_streams_for_item(&state.db, &item.id).await {
                        Ok(streams) => {
                            let mut media_source =
                                media_source_json_with_streams(&item, streams);
                            let playback_streams = media_source_streams(&media_source);
                            dlna::apply_playback_profile(
                                &mut media_source,
                                &profile,
                                &playback_streams,
                                &query,
                            );
                            vec![media_source]
                        }
                        Err(error) => return internal_error(error),
                    },
                    Err(error) => return internal_error(error),
                }
            } else {
                match super::media_streams_for_item(&state.db, &item.id).await {
                    Ok(streams) => {
                        let mut media_source = media_source_json_with_streams(&item, streams);
                        let playback_streams = media_source_streams(&media_source);
                        dlna::apply_playback_profile(&mut media_source, &profile, &playback_streams, &query);
                        vec![media_source]
                    }
                    Err(error) => return internal_error(error),
                }
            };
            if let Some(max_streaming_bitrate) = max_streaming_bitrate {
                apply_max_streaming_bitrate_to_sources(&mut media_sources, max_streaming_bitrate);
            }
            if let Some(token) = request_token(&headers, &query) {
                append_access_token_to_media_sources(&mut media_sources, &token);
            }

            Json(strip_nulls(json!({ "MediaSources": media_sources, "PlaySessionId": uuid::Uuid::new_v4().simple().to_string(), "ErrorCode": null }))).into_response()
        }
        Ok(None) => Json(
            json!({ "MediaSources": [], "PlaySessionId": uuid::Uuid::new_v4().simple().to_string() }),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

fn playback_max_streaming_bitrate(
    query: &HashMap<String, String>,
    body: Option<&JsonValue>,
) -> Option<i64> {
    body.and_then(|value| query_json_i64(value, "MaxStreamingBitrate"))
        .or_else(|| query_i64(query, "MaxStreamingBitrate"))
        .filter(|value| *value > 0)
}

fn apply_max_streaming_bitrate_to_sources(media_sources: &mut [JsonValue], bitrate: i64) {
    for source in media_sources {
        if let Some(object) = source.as_object_mut() {
            object.insert("FallbackMaxStreamingBitrate".to_string(), json!(bitrate));
        }
    }
}

fn apply_playback_profile_to_sources(
    media_sources: &mut [JsonValue],
    profile: &JsonValue,
    query: &HashMap<String, String>,
) {
    for media_source in media_sources {
        let playback_streams = media_source_streams(media_source);
        dlna::apply_playback_profile(media_source, profile, &playback_streams, query);
    }
}

fn media_source_streams(media_source: &JsonValue) -> Vec<JsonValue> {
    media_source
        .get("MediaStreams")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
}

fn append_access_token_to_media_sources(media_sources: &mut [JsonValue], token: &str) {
    if token.trim().is_empty() {
        return;
    }
    for source in media_sources {
        append_access_token_to_value(source, token);
    }
}

fn append_access_token_to_value(value: &mut JsonValue, token: &str) {
    match value {
        JsonValue::Object(map) => {
            for (key, value) in map.iter_mut() {
                if matches!(
                    key.as_str(),
                    "DirectStreamUrl" | "TranscodingUrl" | "DeliveryUrl"
                ) {
                    if let Some(url) = value
                        .as_str()
                        .and_then(|url| stream_url_with_token(url, token))
                    {
                        *value = JsonValue::String(url);
                    }
                } else {
                    append_access_token_to_value(value, token);
                }
            }
        }
        JsonValue::Array(values) => {
            for value in values {
                append_access_token_to_value(value, token);
            }
        }
        _ => {}
    }
}

fn query_i64(query: &HashMap<String, String>, key: &str) -> Option<i64> {
    query
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, value)| value.trim().parse::<i64>().ok())
}

fn query_json_i64(value: &JsonValue, key: &str) -> Option<i64> {
    value.as_object().and_then(|object| {
        object
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| {
                value
                    .as_i64()
                    .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                    .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
            })
    })
}

fn stream_url_with_token(url: &str, token: &str) -> Option<String> {
    if !url.starts_with('/') || stream_url_has_token(url) {
        return None;
    }
    let (url_without_fragment, fragment) = url.split_once('#').unwrap_or((url, ""));
    let separator = if url_without_fragment.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut updated = format!(
        "{url_without_fragment}{separator}api_key={}",
        percent_encode_query_value(token)
    );
    if !fragment.is_empty() {
        updated.push('#');
        updated.push_str(fragment);
    }
    Some(updated)
}

fn stream_url_has_token(url: &str) -> bool {
    let query = url
        .split_once('?')
        .map(|(_, query)| query.split('#').next().unwrap_or_default())
        .unwrap_or_default();
    query.split('&').any(|part| {
        let key = part.split_once('=').map(|(key, _)| key).unwrap_or(part);
        matches!(
            key.to_ascii_lowercase().as_str(),
            "api_key" | "apikey" | "token" | "access_token" | "accesstoken"
        )
    })
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub async fn playback_start(
    State(state): State<Arc<AppState>>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(
        state,
        Some(remote_addr),
        headers,
        query,
        body,
        PlaybackEvent::Start,
    )
    .await
}

pub async fn playback_progress(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(state, None, headers, query, body, PlaybackEvent::Progress).await
}

pub async fn playback_stopped(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(state, None, headers, query, body, PlaybackEvent::Stopped).await
}

async fn playback_progress_inner(
    state: Arc<AppState>,
    remote_addr: Option<SocketAddr>,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: JsonValue,
    event: PlaybackEvent,
) -> Response {
    let Some(item_id) = playback_body_item_id(&body) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let position_ticks = body.get("PositionTicks").and_then(JsonValue::as_i64);
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    let persistence_item_id = playback_body_persistence_item_id(&body).unwrap_or(item_id);
    let runtime_ticks = body.get("RunTimeTicks").and_then(JsonValue::as_i64);
    let result = match event {
        PlaybackEvent::Start => {
            if position_ticks.is_some_and(|value| value > 0) {
                upsert_playback_position(
                    &state.db,
                    &user_id,
                    persistence_item_id,
                    position_ticks.unwrap(),
                )
                .await
            } else {
                Ok(())
            }
        }
        PlaybackEvent::Progress => match position_ticks {
            Some(position_ticks) => {
                upsert_playback_position(&state.db, &user_id, persistence_item_id, position_ticks)
                    .await
            }
            None => Ok(()),
        },
        PlaybackEvent::Stopped => {
            finish_playback_position(
                &state.db,
                &user_id,
                persistence_item_id,
                position_ticks,
                runtime_ticks,
            )
            .await
        }
    };
    if let Err(error) = result {
        return internal_error(error);
    }

    let play_session_id = playback_body_play_session_id(&body, &user_id, item_id);
    let is_paused = body
        .get("IsPaused")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    if let Err(error) = record_playback_watch_event(
        &state.db,
        &user_id,
        persistence_item_id,
        body.get("MediaSourceId").and_then(JsonValue::as_str),
        &play_session_id,
        body.get("Client").and_then(JsonValue::as_str),
        body.get("DeviceName").and_then(JsonValue::as_str),
        position_ticks,
        runtime_ticks,
        is_paused,
        event,
    )
    .await
    {
        return internal_error(error);
    }

    if event == PlaybackEvent::Start {
        let session_info = crate::jellyfin::sessions::session_info(&state, &headers, &query).await;
        let item_name = find_media_item(&state.db, &user_id, item_id)
            .await
            .ok()
            .flatten()
            .map(|item| item.title);
        let device_name = body
            .get("DeviceName")
            .and_then(JsonValue::as_str)
            .unwrap_or(&session_info.device_name);
        let remote_address = playback_remote_address(&headers, remote_addr);
        state
            .record_playback_start(
                remote_address.as_deref(),
                &user_id,
                &session_info.client,
                device_name,
                item_id,
                item_name,
            )
            .await;
    }

    // Save RunTimeTicks from client report if media_items doesn't have it yet
    if let Some(runtime_ticks) = body
        .get("RunTimeTicks")
        .and_then(JsonValue::as_i64)
        .filter(|v| *v > 0)
    {
        let _ = state
            .db
            .execute(crate::db::helpers::pg_statement(
                "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
                vec![runtime_ticks.into(), item_id.into()],
            ))
            .await;
        if let Some(media_source_id) = body.get("MediaSourceId").and_then(JsonValue::as_str) {
            let _ = state
                .db
                .execute(crate::db::helpers::pg_statement(
                    "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
                    vec![runtime_ticks.into(), media_source_id.into()],
                ))
                .await;
        }
    }

    // Hook intro_skip behavior detection
    if state.sa_config.intro_skip_enabled {
        let is_paused = body
            .get("IsPaused")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let default_session_id = format!("{user_id}:{item_id}");
        let play_session_id = body
            .get("PlaySessionId")
            .and_then(JsonValue::as_str)
            .unwrap_or(&default_session_id);
        let client = body
            .get("Client")
            .and_then(JsonValue::as_str)
            .unwrap_or("Unknown");

        match event {
            PlaybackEvent::Progress => {
                // Get runtime ticks for the session
                let runtime_ticks = body
                    .get("RunTimeTicks")
                    .and_then(JsonValue::as_i64)
                    .unwrap_or(0);

                // Check if session exists, if not create it
                {
                    let sessions = state.playback_sessions.read().await;
                    if !sessions.contains_key(play_session_id) {
                        drop(sessions);
                        state.intro_detector.on_playback_start(
                            play_session_id,
                            item_id,
                            &user_id,
                            client,
                            runtime_ticks,
                            position_ticks.unwrap_or_default(),
                        );
                    }
                }

                state.intro_detector.on_playback_progress(
                    play_session_id,
                    position_ticks.unwrap_or_default(),
                    is_paused,
                );
            }
            PlaybackEvent::Stopped => {
                state
                    .intro_detector
                    .on_playback_stopped(play_session_id, position_ticks.unwrap_or_default());
            }
            _ => {}
        }
    }

    match update_playback_session(
        &state,
        &headers,
        &query,
        &user_id,
        item_id,
        position_ticks.unwrap_or_default(),
        &body,
        event,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

fn playback_body_item_id(body: &JsonValue) -> Option<&str> {
    body.get("ItemId")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            body.get("MediaSourceId")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
        })
}

fn playback_body_persistence_item_id(body: &JsonValue) -> Option<&str> {
    body.get("ItemId")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            body.get("MediaSourceId")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
        })
}

fn playback_body_play_session_id(body: &JsonValue, user_id: &str, item_id: &str) -> String {
    body.get("PlaySessionId")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{user_id}:{item_id}"))
}

#[allow(clippy::too_many_arguments)]
async fn update_playback_session(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    user_id: &str,
    item_id: &str,
    position_ticks: i64,
    body: &JsonValue,
    event: PlaybackEvent,
) -> anyhow::Result<()> {
    let play_session_id = body
        .get("PlaySessionId")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{user_id}:{item_id}"));

    if event == PlaybackEvent::Stopped {
        state
            .playback_sessions
            .write()
            .await
            .remove(&play_session_id);
        let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
        return Ok(());
    }

    let item_name = find_media_item(&state.db, user_id, item_id)
        .await?
        .map(|item| item.title);
    let session_info = crate::jellyfin::sessions::session_info(state, headers, query).await;
    let now = now_unix();
    let last_activity_date = unix_to_jellyfin_date(now);
    let client = session_info.client;
    let device_name = body
        .get("DeviceName")
        .and_then(JsonValue::as_str)
        .unwrap_or(&session_info.device_name)
        .to_string();
    let device_id = body
        .get("DeviceId")
        .and_then(JsonValue::as_str)
        .unwrap_or(&session_info.device_id)
        .to_string();
    let application_version = session_info.application_version;
    let playable_media_types = session_info.playable_media_types;
    let supported_commands = session_info.supported_commands;
    let supports_media_control = session_info.supports_media_control;
    let supports_persistent_identifier = session_info.supports_persistent_identifier;
    let user_name = crate::jellyfin::sessions::session_user_name(state, user_id).await;
    let session = PlaybackSession {
        id: play_session_id.clone(),
        user_id: user_id.to_string(),
        user_name,
        play_session_id: play_session_id.clone(),
        item_id: item_id.to_string(),
        item_name,
        now_playing_queue: body
            .get("NowPlayingQueue")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default(),
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
            position_ticks: position_ticks.max(0),
            is_paused: body
                .get("IsPaused")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            can_seek: body
                .get("CanSeek")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
        },
        playable_media_types: playable_media_types.clone(),
        supports_media_control_commands: supported_commands.clone(),
        supported_commands: supported_commands.clone(),
        supports_media_control,
        supports_remote_control: supports_media_control,
        supports_persistent_identifier,
        capabilities: SessionCapabilities {
            user_id: user_id.to_string(),
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
        .insert(play_session_id, session);
    let _ = state.ws_event_tx.send(crate::ws::WsEvent::SessionsChanged);
    Ok(())
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum PlaybackEvent {
    Start,
    Progress,
    Stopped,
}

struct WatchSessionRow {
    item_id: String,
    position_ticks: i64,
    is_paused: bool,
    last_event_at: i64,
    ended_at: Option<i64>,
    watch_seconds: i64,
}

#[allow(clippy::too_many_arguments)]
async fn record_playback_watch_event(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    media_source_id: Option<&str>,
    play_session_id: &str,
    client: Option<&str>,
    device_name: Option<&str>,
    position_ticks: Option<i64>,
    runtime_ticks: Option<i64>,
    is_paused: bool,
    event: PlaybackEvent,
) -> anyhow::Result<()> {
    let now = now_unix();
    let item_id = canonical_watch_item_id(db, item_id).await?;
    let existing = watch_session_row(db, play_session_id).await?;
    if existing
        .as_ref()
        .is_some_and(|session| session.ended_at.is_some())
        && event != PlaybackEvent::Start
    {
        return Ok(());
    }
    let starts_new_session = existing
        .as_ref()
        .map(|session| session.ended_at.is_some() || session.item_id != item_id)
        .unwrap_or(true);

    if let Some(session) = existing
        .as_ref()
        .filter(|session| session.ended_at.is_none() && !starts_new_session)
    {
        let delta_seconds = watch_delta_seconds(
            session.last_event_at,
            now,
            session.position_ticks,
            position_ticks,
            session.is_paused,
        );
        if delta_seconds > 0 {
            add_watch_segment(
                db,
                user_id,
                &session.item_id,
                session.last_event_at,
                delta_seconds,
            )
            .await?;
        }
    }

    if starts_new_session && event == PlaybackEvent::Stopped {
        return Ok(());
    }

    if starts_new_session {
        increment_watch_play_count(db, user_id, &item_id, now).await?;
    }

    let accumulated_seconds = if starts_new_session {
        0
    } else {
        existing
            .as_ref()
            .map(|session| {
                session.watch_seconds.saturating_add(watch_delta_seconds(
                    session.last_event_at,
                    now,
                    session.position_ticks,
                    position_ticks,
                    session.is_paused,
                ))
            })
            .unwrap_or_default()
    };
    let ended_at = (event == PlaybackEvent::Stopped).then_some(now);
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO playback_watch_sessions
           (play_session_id, user_id, item_id, media_source_id, client, device_name, position_ticks, runtime_ticks, is_paused, started_at, last_event_at, ended_at, watch_seconds, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT(play_session_id) DO UPDATE SET
             user_id = excluded.user_id,
             item_id = excluded.item_id,
             media_source_id = excluded.media_source_id,
             client = COALESCE(excluded.client, playback_watch_sessions.client),
             device_name = COALESCE(excluded.device_name, playback_watch_sessions.device_name),
             position_ticks = excluded.position_ticks,
             runtime_ticks = COALESCE(excluded.runtime_ticks, playback_watch_sessions.runtime_ticks),
             is_paused = excluded.is_paused,
             started_at = CASE WHEN playback_watch_sessions.ended_at IS NOT NULL AND excluded.ended_at IS NULL THEN excluded.started_at ELSE playback_watch_sessions.started_at END,
             last_event_at = excluded.last_event_at,
             ended_at = excluded.ended_at,
             watch_seconds = excluded.watch_seconds,
             updated_at = excluded.updated_at"#,
        vec![
            play_session_id.into(),
            user_id.into(),
            item_id.into(),
            media_source_id.map(str::to_string).into(),
            client.map(str::to_string).into(),
            device_name.map(str::to_string).into(),
            position_ticks.unwrap_or_default().max(0).into(),
            runtime_ticks.filter(|value| *value > 0).into(),
            if is_paused || event == PlaybackEvent::Stopped {
                1_i64
            } else {
                0
            }
            .into(),
            now.into(),
            now.into(),
            ended_at.into(),
            accumulated_seconds.into(),
            now.into(),
        ],
    ))
    .await?;

    Ok(())
}

async fn watch_session_row(
    db: &DatabaseConnection,
    play_session_id: &str,
) -> anyhow::Result<Option<WatchSessionRow>> {
    db.query_one(crate::db::helpers::pg_statement(
        "SELECT item_id, position_ticks, is_paused, last_event_at, ended_at, watch_seconds FROM playback_watch_sessions WHERE play_session_id = ?",
        vec![play_session_id.into()],
    ))
    .await?
    .map(|row| {
        Ok(WatchSessionRow {
            item_id: row.get_str("item_id")?,
            position_ticks: row.get_i64("position_ticks").unwrap_or_default(),
            is_paused: row.get_i64("is_paused").unwrap_or_default() != 0,
            last_event_at: row.get_i64("last_event_at").unwrap_or_default(),
            ended_at: row.get_opt_i64("ended_at")?,
            watch_seconds: row.get_i64("watch_seconds").unwrap_or_default(),
        })
    })
    .transpose()
}

async fn canonical_watch_item_id(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<String> {
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT mi.item_type, mi.parent_id, parent.item_type AS parent_item_type, parent.is_folder AS parent_is_folder FROM media_items mi LEFT JOIN media_items parent ON parent.id = mi.parent_id WHERE mi.id = ?",
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(item_id.to_string());
    };
    let item_type = row.get_str("item_type").unwrap_or_default();
    let parent_id = row.get_str("parent_id").unwrap_or_default();
    let parent_item_type = row.get_opt_str("parent_item_type")?.unwrap_or_default();
    let parent_is_folder = row.get_i64("parent_is_folder").unwrap_or_default() != 0;
    if item_type == "Video"
        && !parent_id.is_empty()
        && parent_is_folder
        && matches!(parent_item_type.as_str(), "Movie" | "Episode")
    {
        Ok(parent_id)
    } else {
        Ok(item_id.to_string())
    }
}

fn watch_delta_seconds(
    previous_event_at: i64,
    now: i64,
    previous_position_ticks: i64,
    position_ticks: Option<i64>,
    was_paused: bool,
) -> i64 {
    if was_paused || now <= previous_event_at {
        return 0;
    }
    let wall_seconds = (now - previous_event_at).min(playback_watch_max_gap_seconds());
    let Some(position_ticks) = position_ticks else {
        return wall_seconds.max(0);
    };
    let position_delta_ticks = position_ticks.saturating_sub(previous_position_ticks);
    if position_delta_ticks < 0 {
        return wall_seconds.max(0);
    }
    let position_seconds = ticks_to_seconds_ceil(position_delta_ticks);
    wall_seconds
        .min(position_seconds.saturating_add(WATCH_POSITION_TOLERANCE_SECONDS))
        .max(0)
}

fn ticks_to_seconds_ceil(ticks: i64) -> i64 {
    if ticks <= 0 {
        0
    } else {
        ticks.saturating_add(TICKS_PER_SECOND - 1) / TICKS_PER_SECOND
    }
}

fn playback_watch_max_gap_seconds() -> i64 {
    std::env::var("JELLYFIN_RS_MAX_WATCH_DELTA_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(12 * 60 * 60)
}

async fn increment_watch_play_count(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    timestamp: i64,
) -> anyhow::Result<()> {
    let day = unix_day(timestamp);
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO playback_watch_days (day, user_id, item_id, watch_seconds, play_count, last_played_at)
           VALUES (?, ?, ?, 0, 1, ?)
           ON CONFLICT(day, user_id, item_id) DO UPDATE SET
             play_count = playback_watch_days.play_count + 1,
             last_played_at = GREATEST(COALESCE(playback_watch_days.last_played_at, 0), excluded.last_played_at)"#,
        vec![day.into(), user_id.into(), item_id.into(), timestamp.into()],
    ))
    .await?;
    Ok(())
}

async fn add_watch_segment(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    segment_start: i64,
    seconds: i64,
) -> anyhow::Result<()> {
    if seconds <= 0 {
        return Ok(());
    }
    let segment_end = segment_start.saturating_add(seconds);
    for (day, day_seconds, last_played_at) in watch_segment_day_slices(segment_start, segment_end) {
        db.execute(crate::db::helpers::pg_statement(
            r#"INSERT INTO playback_watch_days (day, user_id, item_id, watch_seconds, play_count, last_played_at)
               VALUES (?, ?, ?, ?, 0, ?)
               ON CONFLICT(day, user_id, item_id) DO UPDATE SET
                 watch_seconds = playback_watch_days.watch_seconds + excluded.watch_seconds,
                 last_played_at = GREATEST(COALESCE(playback_watch_days.last_played_at, 0), excluded.last_played_at)"#,
            vec![
                day.into(),
                user_id.into(),
                item_id.into(),
                day_seconds.into(),
                last_played_at.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

fn watch_segment_day_slices(start: i64, end: i64) -> Vec<(String, i64, i64)> {
    let mut slices = Vec::new();
    let mut cursor = start.max(0);
    let end = end.max(cursor);
    while cursor < end {
        let next_day = unix_day_start(cursor).saturating_add(86_400);
        let slice_end = end.min(next_day);
        slices.push((unix_day(cursor), slice_end - cursor, slice_end));
        cursor = slice_end;
    }
    slices
}

fn unix_day(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp.max(0), 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

fn unix_day_start(timestamp: i64) -> i64 {
    timestamp.max(0).div_euclid(86_400) * 86_400
}

pub(crate) async fn upsert_playback_position(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    position_ticks: i64,
) -> anyhow::Result<()> {
    if position_ticks <= 0 {
        return Ok(());
    }
    let item_ids = super::user_data::playback_user_data_item_ids(db, item_id).await?;
    let now = now_unix();
    for target_id in item_ids {
        let existing = UserData::find_by_id((user_id.to_string(), target_id.clone()))
            .one(db)
            .await?;
        if let Some(model) = existing {
            let mut active: user_data::ActiveModel = model.into();
            active.played = Set(0);
            active.playback_position_ticks = Set(position_ticks.max(0));
            active.played_percentage = Set(None);
            active.updated_at = Set(now);
            active.last_played_at = Set(Some(now));
            active.update(db).await?;
        } else {
            let active = user_data::ActiveModel {
                user_id: Set(user_id.to_string()),
                item_id: Set(target_id),
                played: Set(0),
                playback_position_ticks: Set(position_ticks.max(0)),
                updated_at: Set(now),
                last_played_at: Set(Some(now)),
                ..Default::default()
            };
            UserData::insert(active).exec(db).await?;
        }
    }
    Ok(())
}

async fn finish_playback_position(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    position_ticks: Option<i64>,
    runtime_ticks: Option<i64>,
) -> anyhow::Result<()> {
    let Some(position_ticks) = position_ticks.filter(|value| *value > 0) else {
        return Ok(());
    };
    let runtime_ticks = match runtime_ticks.filter(|value| *value > 0) {
        Some(runtime_ticks) => Some(runtime_ticks),
        None => playback_runtime_ticks(db, item_id).await?,
    };
    if should_mark_played(position_ticks, runtime_ticks) {
        super::user_data::upsert_played_flag(db, user_id, item_id, true).await
    } else {
        upsert_playback_position(db, user_id, item_id, position_ticks).await
    }
}

async fn playback_runtime_ticks(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<i64>> {
    let item_ids = super::user_data::playback_user_data_item_ids(db, item_id).await?;
    let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT MAX(runtime_ticks) AS runtime_ticks FROM media_items WHERE id IN ({placeholders})"
    );
    let values: Vec<sea_orm::Value> = item_ids.iter().map(|id| id.as_str().into()).collect();
    Ok(db
        .query_one(crate::db::helpers::pg_statement(&sql, values))
        .await?
        .and_then(|row| row.get_opt_i64("runtime_ticks").ok().flatten()))
}

fn should_mark_played(position_ticks: i64, runtime_ticks: Option<i64>) -> bool {
    let Some(runtime_ticks) = runtime_ticks.filter(|value| *value > 0) else {
        return false;
    };
    (position_ticks as f64 / runtime_ticks as f64) >= PLAYED_PERCENT_THRESHOLD
}

fn playback_remote_address(headers: &HeaderMap, remote_addr: Option<SocketAddr>) -> Option<String> {
    playback_forwarded_for(headers).or_else(|| remote_addr.map(|addr| addr.to_string()))
}

fn playback_forwarded_for(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Forwarded-For")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// POST /Users/{user_id}/PlayingItems/{item_id} — report playback start
pub async fn playing_item_start(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let position_ticks = body
        .as_ref()
        .and_then(|b| b.get("PositionTicks").and_then(JsonValue::as_i64))
        .filter(|value| *value > 0);

    if let Some(position_ticks) = position_ticks {
        let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
        if let Err(error) = result {
            return internal_error(error);
        }
    }

    // Save RunTimeTicks if provided
    if let Some(rt) = body
        .as_ref()
        .and_then(|b| b.get("RunTimeTicks").and_then(JsonValue::as_i64))
        .filter(|v| *v > 0)
    {
        let _ = state
            .db
            .execute(crate::db::helpers::pg_statement(
                "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
                vec![rt.into(), item_id.clone().into()],
            ))
            .await;
    }

    let body_value = body.as_ref().map(|Json(value)| value);
    let play_session_id = legacy_play_session_id(&query, body_value, &user_id, &item_id);
    let is_paused = body_value
        .and_then(|value| value.get("IsPaused"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    if let Err(error) = record_playback_watch_event(
        &state.db,
        &user_id,
        &item_id,
        body_value
            .and_then(|value| value.get("MediaSourceId"))
            .and_then(JsonValue::as_str),
        &play_session_id,
        body_value
            .and_then(|value| value.get("Client"))
            .and_then(JsonValue::as_str),
        body_value
            .and_then(|value| value.get("DeviceName"))
            .and_then(JsonValue::as_str),
        position_ticks,
        body_value
            .and_then(|value| value.get("RunTimeTicks"))
            .and_then(JsonValue::as_i64),
        is_paused,
        PlaybackEvent::Start,
    )
    .await
    {
        return internal_error(error);
    }

    let session_info = crate::jellyfin::sessions::session_info(&state, &headers, &query).await;
    let item_name = find_media_item(&state.db, &user_id, &item_id)
        .await
        .ok()
        .flatten()
        .map(|item| item.title);
    state
        .record_playback_start(
            playback_forwarded_for(&headers).as_deref(),
            &user_id,
            &session_info.client,
            &session_info.device_name,
            &item_id,
            item_name,
        )
        .await;

    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /Users/{user_id}/PlayingItems/{item_id} — report playback stop
pub async fn playing_item_stop(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let position_ticks = query
        .get("PositionTicks")
        .and_then(|v| v.parse::<i64>().ok());
    let runtime_ticks = query
        .get("RunTimeTicks")
        .and_then(|v| v.parse::<i64>().ok());

    if let Err(error) =
        finish_playback_position(&state.db, &user_id, &item_id, position_ticks, runtime_ticks).await
    {
        return internal_error(error);
    }
    let play_session_id = legacy_play_session_id(&query, None, &user_id, &item_id);
    if let Err(error) = record_playback_watch_event(
        &state.db,
        &user_id,
        &item_id,
        query
            .get("MediaSourceId")
            .or_else(|| query.get("mediaSourceId"))
            .map(String::as_str),
        &play_session_id,
        None,
        None,
        position_ticks,
        runtime_ticks,
        true,
        PlaybackEvent::Stopped,
    )
    .await
    {
        return internal_error(error);
    }

    StatusCode::NO_CONTENT.into_response()
}

/// POST /Users/{user_id}/PlayingItems/{item_id}/Progress — report playback progress
pub async fn playing_item_progress(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let position_ticks = query
        .get("PositionTicks")
        .and_then(|v| v.parse::<i64>().ok())
        .or_else(|| {
            body.as_ref()
                .and_then(|b| b.get("PositionTicks").and_then(JsonValue::as_i64))
        });

    if let Some(position_ticks) = position_ticks.filter(|value| *value > 0) {
        let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
        if let Err(error) = result {
            return internal_error(error);
        }
    }

    let body_value = body.as_ref().map(|Json(value)| value);
    let play_session_id = legacy_play_session_id(&query, body_value, &user_id, &item_id);
    let is_paused = query
        .get("IsPaused")
        .or_else(|| query.get("isPaused"))
        .and_then(|value| value.parse::<bool>().ok())
        .or_else(|| {
            body_value
                .and_then(|value| value.get("IsPaused"))
                .and_then(JsonValue::as_bool)
        })
        .unwrap_or(false);
    if let Err(error) = record_playback_watch_event(
        &state.db,
        &user_id,
        &item_id,
        query
            .get("MediaSourceId")
            .or_else(|| query.get("mediaSourceId"))
            .map(String::as_str)
            .or_else(|| {
                body_value
                    .and_then(|value| value.get("MediaSourceId"))
                    .and_then(JsonValue::as_str)
            }),
        &play_session_id,
        body_value
            .and_then(|value| value.get("Client"))
            .and_then(JsonValue::as_str),
        body_value
            .and_then(|value| value.get("DeviceName"))
            .and_then(JsonValue::as_str),
        position_ticks,
        body_value
            .and_then(|value| value.get("RunTimeTicks"))
            .and_then(JsonValue::as_i64),
        is_paused,
        PlaybackEvent::Progress,
    )
    .await
    {
        return internal_error(error);
    }

    StatusCode::NO_CONTENT.into_response()
}

fn legacy_play_session_id(
    query: &HashMap<String, String>,
    body: Option<&JsonValue>,
    user_id: &str,
    item_id: &str,
) -> String {
    body.and_then(|value| value.get("PlaySessionId"))
        .and_then(JsonValue::as_str)
        .or_else(|| query.get("PlaySessionId").map(String::as_str))
        .or_else(|| query.get("playSessionId").map(String::as_str))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{user_id}:{item_id}"))
}

pub async fn current_user_playing_item_start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    playing_item_start(
        State(state),
        Path((user_id, item_id)),
        headers,
        Query(query),
        body,
    )
    .await
}

pub async fn current_user_playing_item_stop(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    playing_item_stop(State(state), Path((user_id, item_id)), Query(query)).await
}

pub async fn current_user_playing_item_progress(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    playing_item_progress(State(state), Path((user_id, item_id)), Query(query), body).await
}

#[cfg(test)]
mod tests {
    use super::{
        append_access_token_to_media_sources, apply_max_streaming_bitrate_to_sources,
        current_user_playing_item_start, playback_max_streaming_bitrate, watch_delta_seconds,
        watch_segment_day_slices,
    };
    use crate::app::state::{AppState, PlaybackSession};
    use crate::db::row_ext::QueryResultExt;
    use axum::{
        Json,
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
    };
    use sea_orm::{ConnectionTrait, DatabaseConnection};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[test]
    fn playback_info_urls_include_access_token_for_clients_that_drop_auth_headers() {
        let mut sources = vec![serde_json::json!({
            "DirectStreamUrl": "/Videos/video/stream",
            "TranscodingUrl": null,
            "MediaAttachments": [
                { "DeliveryUrl": "/Videos/video/video/Attachments/5" }
            ],
        })];

        append_access_token_to_media_sources(&mut sources, "token 1");

        assert_eq!(
            sources[0]["DirectStreamUrl"],
            "/Videos/video/stream?api_key=token%201"
        );
        assert_eq!(
            sources[0]["MediaAttachments"][0]["DeliveryUrl"],
            "/Videos/video/video/Attachments/5?api_key=token%201"
        );
    }

    #[test]
    fn playback_info_accepts_emby_max_streaming_bitrate_shapes() {
        let mut query = HashMap::new();
        query.insert("maxStreamingBitrate".to_string(), "12000000".to_string());
        assert_eq!(playback_max_streaming_bitrate(&query, None), Some(12000000));

        let body = serde_json::json!({ "MaxStreamingBitrate": "24000000" });
        assert_eq!(
            playback_max_streaming_bitrate(&query, Some(&body)),
            Some(24000000)
        );

        let mut sources = vec![serde_json::json!({ "Id": "source" })];
        apply_max_streaming_bitrate_to_sources(&mut sources, 36000000);
        assert_eq!(sources[0]["FallbackMaxStreamingBitrate"], 36000000);
    }

    #[test]
    fn watch_delta_counts_wall_time_but_skips_paused_time() {
        assert_eq!(
            watch_delta_seconds(100, 130, 0, Some(300_000_000), false),
            30
        );
        assert_eq!(watch_delta_seconds(100, 130, 0, Some(300_000_000), true), 0);
        assert_eq!(watch_delta_seconds(100, 190, 0, Some(0), false), 30);
    }

    #[test]
    fn watch_segments_split_across_utc_days() {
        let slices = watch_segment_day_slices(86_390, 86_410);
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0], ("1970-01-01".to_string(), 10, 86_400));
        assert_eq!(slices[1], ("1970-01-02".to_string(), 10, 86_410));
    }

    #[tokio::test]
    async fn current_user_playing_item_start_uses_single_path_item_id() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = Arc::new(test_state(db));
        seed_user_and_item(&state.db, &state.user_id.to_string(), "m1").await;

        let response = current_user_playing_item_start(
            State(state.clone()),
            HeaderMap::new(),
            Query(HashMap::new()),
            Path("m1".to_string()),
            Some(Json(serde_json::json!({ "PositionTicks": 42 }))),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let row = state
            .db
            .query_one(crate::db::helpers::pg_statement(
                "SELECT playback_position_ticks FROM user_data WHERE user_id = ? AND item_id = ?",
                vec![state.user_id.to_string().into(), "m1".into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("playback_position_ticks").unwrap(), 42);
    }

    async fn seed_user_and_item(db: &DatabaseConnection, user_id: &str, item_id: &str) {
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, 'test', 'Test', 0, 0, 1, 1)",
            vec![user_id.into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, 'Movie', 'D:/movie.mkv', '', '', 'Movie', 0, 1, 1, 1, 1)",
            vec![item_id.into()],
        ))
        .await
        .unwrap();
    }

    fn test_state(db: DatabaseConnection) -> AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"playback-test"),
            access_token: "test-token".to_string(),
            db,
            media_dirs: Vec::new(),
            http_client: reqwest::Client::new(),
            tmdb_api_key: RwLock::new(None),
            tmdb_proxy_url: Arc::new(RwLock::new(None)),
            tmdb_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            douban_cookie: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            playback_sessions: RwLock::new(HashMap::<String, PlaybackSession>::new()),
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
