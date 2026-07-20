use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    entities::{
        linked_children::{self, Entity as LinkedChildren},
        media_items::Entity as MediaItems,
    },
    jellyfin::{
        auth::request_user_id_and_admin_or_default, collect::playlist_write_access,
        common::internal_error,
    },
};

const MAX_USER_SETTING_KEY_LEN: usize = 128;
const MAX_USER_SETTING_VALUE_BYTES: usize = 64 * 1024;
const MAX_USER_SETTINGS_PER_REQUEST: usize = 128;
const MAX_TYPED_SETTING_KEY_LEN: usize = 128;
const MAX_TYPED_SETTING_VALUE_BYTES: usize = 64 * 1024;

/// GET /UserSettings/{user_id} — get user settings
pub async fn get_user_settings(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    let prefix = format!("user_settings:{}:", user_id);
    let rows = crate::db::settings::find_by_prefix(&state.db, &prefix).await;

    match rows {
        Ok(rows) => {
            let mut settings = serde_json::Map::new();
            for row in rows {
                let short_key = row.key.strip_prefix(&prefix).unwrap_or(&row.key);
                settings.insert(short_key.to_string(), json!(row.value));
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
    let Some(obj) = body.as_object() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if obj.len() > MAX_USER_SETTINGS_PER_REQUEST {
        return validation_error_response(StatusCode::PAYLOAD_TOO_LARGE, "Too many settings");
    }

    for (key, value) in obj {
        let key = key.trim();
        if !setting_key_allowed(key, MAX_USER_SETTING_KEY_LEN) {
            return validation_error_response(StatusCode::BAD_REQUEST, "Invalid setting key");
        }
        let Ok(value_str) = user_setting_value(value) else {
            return validation_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Setting is too large",
            );
        };
        let full_key = format!("user_settings:{}:{}", user_id, key);
        if let Err(error) = crate::db::settings::set(&state.db, &full_key, &value_str).await {
            return internal_error(error.into());
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_typed_setting(
    State(state): State<Arc<AppState>>,
    Path((user_id, key)): Path<(String, String)>,
) -> Response {
    if !setting_key_allowed(&key, MAX_TYPED_SETTING_KEY_LEN) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match crate::db::settings::get(&state.db, &typed_setting_key(&user_id, &key)).await {
        Ok(Some(stored)) => {
            let value = serde_json::from_str(&stored).unwrap_or(Value::Null);
            Json(value).into_response()
        }
        Ok(None) => Json(json!({})).into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_typed_setting(
    State(state): State<Arc<AppState>>,
    Path((user_id, key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    if !setting_key_allowed(&key, MAX_TYPED_SETTING_KEY_LEN) {
        return validation_error_response(StatusCode::BAD_REQUEST, "Invalid setting key");
    }
    let Ok(value) = serialize_json_value(&body, MAX_TYPED_SETTING_VALUE_BYTES) else {
        return validation_error_response(StatusCode::PAYLOAD_TOO_LARGE, "Setting is too large");
    };
    match crate::db::settings::set(&state.db, &typed_setting_key(&user_id, &key), &value).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

fn typed_setting_key(user_id: &str, key: &str) -> String {
    format!("typed_user_settings:{}:{}:{}", user_id.len(), user_id, key)
}

fn user_setting_value(value: &Value) -> Result<String, ()> {
    let value = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    if value.len() > MAX_USER_SETTING_VALUE_BYTES
        || value.contains('\0')
        || value.chars().any(|c| c.is_control() && c != '\t')
    {
        return Err(());
    }
    Ok(value)
}

fn serialize_json_value(value: &Value, max_bytes: usize) -> Result<String, ()> {
    let value = value.to_string();
    if value.len() > max_bytes {
        return Err(());
    }
    Ok(value)
}

fn setting_key_allowed(key: &str, max_len: usize) -> bool {
    let key = key.trim();
    !key.is_empty()
        && key.len() <= max_len
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validation_error_response(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({ "Error": message }))).into_response()
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
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((playlist_id, item_id, new_index)): Path<(String, String, usize)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (user_id, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match playlist_write_access(&state.db, &playlist_id, &user_id, is_admin).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }
    match move_playlist_item_inner(&state.db, &playlist_id, &item_id, new_index).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

fn playlist_forbidden_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "Error": "Playlist access is denied" })),
    )
        .into_response()
}

async fn move_playlist_item_inner(
    db: &sea_orm::DatabaseConnection,
    playlist_id: &str,
    item_id: &str,
    new_index: usize,
) -> anyhow::Result<bool> {
    let Some(playlist) = MediaItems::find_by_id(playlist_id.to_string())
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if playlist.item_type != "Playlist" {
        return Ok(false);
    }
    let children = LinkedChildren::find()
        .filter(linked_children::Column::ParentId.eq(playlist_id))
        .order_by_asc(linked_children::Column::SortOrder)
        .all(db)
        .await?;
    let mut ids = children
        .iter()
        .map(|child| child.item_id.clone())
        .collect::<Vec<_>>();
    let Some(index) = ids.iter().position(|id| id == item_id) else {
        return Ok(false);
    };
    let moved = ids.remove(index);
    ids.insert(new_index.min(ids.len()), moved);
    for (index, id) in ids.iter().enumerate() {
        let Some(child) = LinkedChildren::find_by_id((playlist_id.to_string(), id.to_string()))
            .one(db)
            .await?
        else {
            continue;
        };
        let mut active: linked_children::ActiveModel = child.into();
        active.sort_order = Set(i64::try_from(index).unwrap_or(i64::MAX));
        active.update(db).await?;
    }
    Ok(true)
}

/// GET /Playlists/{id}/AddToPlaylistInfo — info for adding to playlist
pub async fn add_to_playlist_info(
    State(_state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::PlaybackSession;
    use crate::entities::media_items;
    use sea_orm::{DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[test]
    fn user_setting_value_preserves_strings_and_json_values() {
        assert_eq!(user_setting_value(&json!("dark")).unwrap(), "dark");
        assert_eq!(
            user_setting_value(&json!({"enabled": true})).unwrap(),
            "{\"enabled\":true}"
        );
        assert!(user_setting_value(&json!("bad\nvalue")).is_err());
    }

    #[test]
    fn typed_setting_key_separates_user_and_key() {
        assert_ne!(typed_setting_key("ab", "c"), typed_setting_key("a", "bc"));
    }

    #[test]
    fn user_setting_keys_are_limited() {
        assert!(setting_key_allowed(
            "home.layout-1",
            MAX_USER_SETTING_KEY_LEN
        ));
        assert!(!setting_key_allowed("", MAX_USER_SETTING_KEY_LEN));
        assert!(!setting_key_allowed("../secret", MAX_USER_SETTING_KEY_LEN));
        assert!(!setting_key_allowed(
            &"x".repeat(MAX_USER_SETTING_KEY_LEN + 1),
            MAX_USER_SETTING_KEY_LEN
        ));
        assert!(
            serialize_json_value(&json!({"theme": "dark"}), MAX_TYPED_SETTING_VALUE_BYTES).is_ok()
        );
        assert!(
            serialize_json_value(
                &json!({"value": "x".repeat(MAX_TYPED_SETTING_VALUE_BYTES)}),
                MAX_TYPED_SETTING_VALUE_BYTES
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn user_settings_trim_keys_before_saving() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = Arc::new(test_state(db));

        let response = update_user_settings(
            State(state.clone()),
            Path("u1".to_string()),
            Json(json!({ " theme ": "dark" })),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let saved = crate::db::settings::get(&state.db, "user_settings:u1:theme")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved, "dark");
    }

    #[tokio::test]
    async fn playlist_move_item_reorders_linked_children() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, item_type) in [
            ("playlist", "Playlist"),
            ("a", "Audio"),
            ("b", "Audio"),
            ("c", "Audio"),
        ] {
            MediaItems::insert(media_items::ActiveModel {
                id: Set(id.to_string()),
                title: Set(id.to_string()),
                path: Set(id.to_string()),
                library_id: Set(String::new()),
                parent_id: Set(String::new()),
                item_type: Set(item_type.to_string()),
                is_folder: Set(0),
                is_public: Set(1),
                modified_at: Set(1),
                created_at: Set(1),
                updated_at: Set(1),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }
        for (index, id) in ["a", "b", "c"].iter().enumerate() {
            LinkedChildren::insert(linked_children::ActiveModel {
                parent_id: Set("playlist".to_string()),
                item_id: Set((*id).to_string()),
                sort_order: Set(i64::try_from(index).unwrap()),
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }

        assert!(
            move_playlist_item_inner(&db, "playlist", "c", 0)
                .await
                .unwrap()
        );
        assert!(
            !move_playlist_item_inner(&db, "playlist", "missing", 0)
                .await
                .unwrap()
        );

        let rows = LinkedChildren::find()
            .filter(linked_children::Column::ParentId.eq("playlist"))
            .order_by_asc(linked_children::Column::SortOrder)
            .all(&db)
            .await
            .unwrap();
        let ids = rows
            .iter()
            .map(|row| row.item_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    fn test_state(db: DatabaseConnection) -> AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"user-prefs-test"),
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
