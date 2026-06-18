use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{app::state::AppState, db::row_ext::QueryResultExt, jellyfin::common::internal_error};

/// GET /UserSettings/{user_id} — get user settings
pub async fn get_user_settings(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT key, value FROM app_settings WHERE key LIKE ?",
            vec![format!("user_settings:{}:%", user_id).into()],
        ))
        .await;

    match rows {
        Ok(rows) => {
            let mut settings = serde_json::Map::new();
            for row in &rows {
                if let (Ok(key), Ok(value)) = (row.get_str("key"), row.get_str("value")) {
                    let short_key = key
                        .strip_prefix(&format!("user_settings:{}:", user_id))
                        .unwrap_or(&key);
                    settings.insert(short_key.to_string(), json!(value));
                }
            }
            Json(Value::Object(settings)).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

/// POST /UserSettings/{user_id} — update user settings
pub async fn update_user_settings(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let backend = state.db.get_database_backend();
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            let full_key = format!("user_settings:{}:{}", user_id, key);
            let value_str = match value.as_str() {
                Some(s) => s.to_string(),
                None => value.to_string(),
            };
            let now = crate::util::now_unix();
            let _ = state.db.execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![full_key.into(), value_str.as_str().into(), now.into()],
            )).await;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// GET /Sessions/PlayQueue — get current play queue
pub async fn play_queue(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let session_id = query.get("Id").or_else(|| query.get("PlaySessionId"));
    let sessions = state.playback_sessions.read().await;

    if let Some(sid) = session_id {
        if let Some(session) = sessions.get(sid) {
            return Json(json!({
                "PlaySessionId": session.id,
                "NowPlayingQueue": session.now_playing_queue,
            }))
            .into_response();
        }
    }

    Json(json!({
        "PlaySessionId": null,
        "NowPlayingQueue": [],
    }))
    .into_response()
}

/// POST /Playlists/{playlist_id}/Items/{item_id}/Move/{new_index} — reorder playlist
pub async fn playlist_move_item(
    State(_state): State<Arc<AppState>>,
    Path((_playlist_id, _item_id, _new_index)): Path<(String, String, usize)>,
) -> Response {
    // Not implemented yet - would need sort_order in linked_children
    StatusCode::NO_CONTENT.into_response()
}

/// GET /Playlists/{id}/AddToPlaylistInfo — info for adding to playlist
pub async fn add_to_playlist_info(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let _user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let ids = query
        .get("Ids")
        .map(|v| v.split(',').collect::<Vec<_>>())
        .unwrap_or_default();

    Json(json!({
        "PlaylistId": playlist_id,
        "Items": ids.iter().map(|id| json!({"Id": id})).collect::<Vec<_>>(),
    }))
    .into_response()
}
