use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set,
};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::{AppState, PlaybackSession, PlaybackState},
    entities::{
        media_streams::Entity as MediaStreams,
        user_data::{self, Entity as UserData},
    },
    jellyfin::{
        auth::request_user_id_or_default, common::internal_error, dlna, items::find_media_item,
    },
    library::models::{MediaStreamRow, media_source_json_with_streams},
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
        Ok(Some(item)) => match media_streams_for_item(&state.db, &item.id).await {
            Ok(streams) => {
                let profile = dlna::request_device_profile(body.as_ref().map(|Json(value)| value));
                let mut media_source = media_source_json_with_streams(&item, streams.clone());
                dlna::apply_playback_profile(&mut media_source, &profile, &streams, &query);
                Json(json!({ "MediaSources": [media_source], "PlaySessionId": uuid::Uuid::new_v4().simple().to_string(), "ErrorCode": null })).into_response()
            }
            Err(error) => internal_error(error),
        },
        Ok(None) => Json(
            json!({ "MediaSources": [], "PlaySessionId": uuid::Uuid::new_v4().simple().to_string() }),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

async fn media_streams_for_item(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let models = MediaStreams::find()
        .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
        .order_by_asc(crate::entities::media_streams::Column::StreamIndex)
        .all(db)
        .await
        .with_context(|| format!("failed to list media streams for item: {item_id}"))?;
    let streams: Vec<_> = models
        .iter()
        .map(|m| MediaStreamRow {
            stream_index: m.stream_index,
            stream_type: m.stream_type.clone(),
            codec: m.codec.clone(),
            language: m.language.clone(),
            title: m.title.clone(),
            bit_rate: m.bit_rate,
            width: m.width,
            height: m.height,
            channels: m.channels,
            sample_rate: m.sample_rate,
            is_external: m.is_external != 0,
        })
        .collect();
    Ok(streams
        .into_iter()
        .map(|s| s.to_jellyfin_json(item_id))
        .collect())
}

pub async fn subtitle_stream_path(
    db: &DatabaseConnection,
    item_id: &str,
    stream_index: i64,
) -> anyhow::Result<Option<String>> {
    let model = MediaStreams::find()
        .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
        .filter(crate::entities::media_streams::Column::StreamIndex.eq(stream_index))
        .filter(crate::entities::media_streams::Column::StreamType.eq("Subtitle"))
        .filter(crate::entities::media_streams::Column::IsExternal.eq(1))
        .one(db)
        .await
        .with_context(|| format!("failed to find subtitle stream: {item_id}:{stream_index}"))?;
    Ok(model.and_then(|m| m.path))
}

pub async fn favorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "is_favorite", true).await
}

pub async fn unfavorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "is_favorite", false).await
}

pub async fn mark_played(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "played", true).await
}

pub async fn mark_unplayed(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "played", false).await
}

pub async fn hide_from_resume(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.playback_position_ticks = Set(0);
    })
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn set_rating(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    let rating = body
        .get("Likes")
        .and_then(JsonValue::as_bool)
        .map(|likes| if likes { 1.0 } else { -1.0 })
        .or_else(|| body.get("Rating").and_then(JsonValue::as_f64))
        .unwrap_or(0.0);
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.rating = Set(Some(rating));
    })
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_rating(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.rating = Set(None);
    })
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn set_user_data_flag(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> Response {
    match upsert_user_data_flag(db, user_id, item_id, field, value).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn upsert_user_data_flag(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> anyhow::Result<()> {
    let value_int = if value { 1 } else { 0 };

    match field {
        "is_favorite" => {
            upsert_user_data_simple(db, user_id, item_id, |active| {
                active.is_favorite = Set(value_int);
            })
            .await
        }
        "played" => {
            let now = now_unix();
            let backend = db.get_database_backend();
            db.execute(crate::db::helpers::portable_statement(
                backend,
                r#"INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, played_percentage, play_count, last_played_at, updated_at) VALUES (?, ?, ?, 0, NULL, COALESCE((SELECT play_count FROM user_data WHERE user_id = ? AND item_id = ?), 0) + 1, ?, ?) ON CONFLICT(user_id, item_id) DO UPDATE SET played = excluded.played, playback_position_ticks = excluded.playback_position_ticks, played_percentage = excluded.played_percentage, play_count = COALESCE(user_data.play_count, 0) + 1, last_played_at = excluded.last_played_at, updated_at = excluded.updated_at"#,
                vec![
                    user_id.into(),
                    item_id.into(),
                    value_int.into(),
                    user_id.into(),
                    item_id.into(),
                    now.into(),
                    now.into(),
                ],
            ))
            .await
            .with_context(|| format!("failed to update user data flag for item: {item_id}"))?;
            Ok(())
        }
        _ => anyhow::bail!("unsupported user data flag: {field}"),
    }
}

async fn upsert_user_data_simple(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    apply: impl FnOnce(&mut user_data::ActiveModel),
) -> anyhow::Result<()> {
    let now = now_unix();
    match UserData::find_by_id((user_id.to_string(), item_id.to_string()))
        .one(db)
        .await?
    {
        Some(model) => {
            let mut active: user_data::ActiveModel = model.into();
            apply(&mut active);
            active.updated_at = Set(now);
            active.update(db).await?;
        }
        None => {
            let mut active = user_data::ActiveModel {
                user_id: Set(user_id.to_string()),
                item_id: Set(item_id.to_string()),
                is_favorite: Set(0),
                played: Set(0),
                playback_position_ticks: Set(0),
                play_count: Set(0),
                updated_at: Set(now),
                ..Default::default()
            };
            apply(&mut active);
            UserData::insert(active).exec(db).await?;
        }
    }
    Ok(())
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

async fn upsert_playback_position(
    db: &DatabaseConnection,
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
