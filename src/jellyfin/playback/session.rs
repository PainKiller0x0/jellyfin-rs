use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, PlaybackSession, PlaybackState},
    entities::user_data::{self, Entity as UserData},
    jellyfin::{
        auth::request_user_id_or_default, common::{internal_error, strip_nulls}, dlna, items::find_media_item,
    },
    library::models::media_source_json_with_streams,
    util::{now_unix, unix_to_jellyfin_date},
};

pub async fn playback_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => {
            let profile = dlna::request_device_profile(body.as_ref().map(|Json(value)| value));

            // For Movie/Episode folders with child Video files (multi-version), return their media sources
            let media_sources = if item.is_folder && (item.item_type == "Movie" || item.item_type == "Episode") {
                match super::child_video_sources(&state.db, &item.id).await {
                    Ok(sources) if !sources.is_empty() => sources,
                    Ok(_) => {
                        // No child videos, return the item itself (e.g. Episode folder is the video)
                        match super::media_streams_for_item(&state.db, &item.id).await {
                            Ok(streams) => {
                                let mut ms = media_source_json_with_streams(&item, streams.clone());
                                dlna::apply_playback_profile(&mut ms, &profile, &streams, &query);
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
            } else {
                match super::media_streams_for_item(&state.db, &item.id).await {
                    Ok(streams) => {
                        let mut media_source = media_source_json_with_streams(&item, streams.clone());
                        dlna::apply_playback_profile(&mut media_source, &profile, &streams, &query);
                        vec![media_source]
                    }
                    Err(error) => return internal_error(error),
                }
            };

            Json(strip_nulls(json!({ "MediaSources": media_sources, "PlaySessionId": uuid::Uuid::new_v4().simple().to_string(), "ErrorCode": null }))).into_response()
        }
        Ok(None) => Json(
            json!({ "MediaSources": [], "PlaySessionId": uuid::Uuid::new_v4().simple().to_string() }),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn playback_start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(state, headers, query, body, PlaybackEvent::Start).await
}

pub async fn playback_progress(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(state, headers, query, body, PlaybackEvent::Progress).await
}

pub async fn playback_stopped(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    playback_progress_inner(state, headers, query, body, PlaybackEvent::Stopped).await
}

async fn playback_progress_inner(
    state: Arc<AppState>,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: JsonValue,
    event: PlaybackEvent,
) -> Response {
    let Some(item_id) = body.get("ItemId").and_then(JsonValue::as_str) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let position_ticks = body
        .get("PositionTicks")
        .and_then(JsonValue::as_i64)
        .unwrap_or_default();
    let user_id = if let Some(user_id) = body.get("UserId").and_then(JsonValue::as_str) {
        user_id.to_string()
    } else {
        request_user_id_or_default(&state, &headers, &query).await
    };
    let result = upsert_playback_position(&state.db, &user_id, item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    // Save RunTimeTicks from client report if media_items doesn't have it yet
    if let Some(runtime_ticks) = body.get("RunTimeTicks").and_then(JsonValue::as_i64).filter(|v| *v > 0) {
        let _ = state.db.execute(crate::db::helpers::portable_statement(
            state.db.get_database_backend(),
            "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
            vec![runtime_ticks.into(), item_id.into()],
        )).await;
    }

    // Hook intro_skip behavior detection
    if state.sa_config.intro_skip_enabled {
        let is_paused = body.get("IsPaused").and_then(JsonValue::as_bool).unwrap_or(false);
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
                let runtime_ticks = body.get("RunTimeTicks").and_then(JsonValue::as_i64).unwrap_or(0);

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
                            position_ticks,
                        );
                    }
                }

                state.intro_detector.on_playback_progress(
                    play_session_id,
                    position_ticks,
                    is_paused,
                );
            }
            PlaybackEvent::Stopped => {
                state.intro_detector.on_playback_stopped(
                    play_session_id,
                    position_ticks,
                );
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
        position_ticks,
        &body,
        event,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
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
    let session = PlaybackSession {
        id: play_session_id.clone(),
        user_id: user_id.to_string(),
        play_session_id: play_session_id.clone(),
        item_id: item_id.to_string(),
        item_name,
        now_playing_queue: body
            .get("NowPlayingQueue")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default(),
        client: session_info.client,
        device_name: body
            .get("DeviceName")
            .and_then(JsonValue::as_str)
            .unwrap_or(&session_info.device_name)
            .to_string(),
        device_id: body
            .get("DeviceId")
            .and_then(JsonValue::as_str)
            .unwrap_or(&session_info.device_id)
            .to_string(),
        application_version: session_info.application_version,
        is_active: true,
        last_activity_date: unix_to_jellyfin_date(now),
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
        playable_media_types: session_info.playable_media_types,
        supports_media_control_commands: session_info.supported_commands.clone(),
        supported_commands: session_info.supported_commands,
        supports_media_control: session_info.supports_media_control,
        supports_persistent_identifier: session_info.supports_persistent_identifier,
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

pub(crate) async fn upsert_playback_position(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    position_ticks: i64,
) -> anyhow::Result<()> {
    let now = now_unix();
    let existing = UserData::find_by_id((user_id.to_string(), item_id.to_string()))
        .one(db)
        .await?;
    if let Some(model) = existing {
        let mut active: user_data::ActiveModel = model.into();
        active.playback_position_ticks = Set(position_ticks.max(0));
        active.updated_at = Set(now);
        active.last_played_at = Set(Some(now));
        active.update(db).await?;
    } else {
        let active = user_data::ActiveModel {
            user_id: Set(user_id.to_string()),
            item_id: Set(item_id.to_string()),
            playback_position_ticks: Set(position_ticks.max(0)),
            updated_at: Set(now),
            last_played_at: Set(Some(now)),
            ..Default::default()
        };
        UserData::insert(active).exec(db).await?;
    }
    Ok(())
}

/// POST /Users/{user_id}/PlayingItems/{item_id} — report playback start
pub async fn playing_item_start(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    _headers: HeaderMap,
    Query(_query): Query<HashMap<String, String>>,
    body: Option<Json<JsonValue>>,
) -> Response {
    let position_ticks = body
        .as_ref()
        .and_then(|b| b.get("PositionTicks").and_then(JsonValue::as_i64))
        .unwrap_or(0);

    let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    // Save RunTimeTicks if provided
    if let Some(rt) = body.as_ref().and_then(|b| b.get("RunTimeTicks").and_then(JsonValue::as_i64)).filter(|v| *v > 0) {
        let _ = state.db.execute(crate::db::helpers::portable_statement(
            state.db.get_database_backend(),
            "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
            vec![rt.into(), item_id.into()],
        )).await;
    }

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
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    // Update play count and mark as played if near the end
    let now = now_unix();
    let backend = state.db.get_database_backend();
    let _ = state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE user_data SET play_count = play_count + 1, played = 1, updated_at = ? WHERE user_id = ? AND item_id = ?",
        vec![now.into(), user_id.into(), item_id.into()],
    )).await;

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
        .or_else(|| body.as_ref().and_then(|b| b.get("PositionTicks").and_then(JsonValue::as_i64)))
        .unwrap_or(0);

    let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    StatusCode::NO_CONTENT.into_response()
}
