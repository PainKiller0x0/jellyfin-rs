use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        common::internal_error,
        item_queries,
        playback::upsert_playback_position,
    },
};

/// GET /Items/Filters2 — return available filter values for a query
pub async fn filters2(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match filters2_inner(&state.db, &query).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn filters2_inner(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let parent_id = query.get("ParentId").map(String::as_str);
    let include_types = query.get("IncludeItemTypes").map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    // Build WHERE clause based on ParentId and IncludeItemTypes
    let mut conditions = vec!["1=1".to_string()];
    let mut values: Vec<sea_orm::Value> = Vec::new();

    if let Some(pid) = parent_id {
        conditions.push("media_items.parent_id = ?".to_string());
        values.push(pid.into());
    }
    if let Some(types) = &include_types {
        if !types.is_empty() {
            let ph = types.iter().map(|_| "media_items.item_type = ?").collect::<Vec<_>>().join(" OR ");
            conditions.push(format!("({})", ph));
            for t in types {
                values.push((*t).into());
            }
        }
    }

    let where_clause = conditions.join(" AND ");

    // Get genres
    let genres_sql = format!(
        "SELECT DISTINCT g.name FROM genres g JOIN media_genres mg ON mg.genre_id = g.id JOIN media_items ON media_items.id = mg.item_id WHERE {} ORDER BY g.name ASC",
        where_clause
    );
    let genres: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &genres_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get years
    let years_sql = format!(
        "SELECT DISTINCT media_items.production_year FROM media_items WHERE {} AND media_items.production_year IS NOT NULL ORDER BY media_items.production_year DESC",
        where_clause
    );
    let years: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &years_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_i64("production_year").ok().flatten().map(|y| json!(y)))
        .collect();

    // Get tags
    let tags_sql = format!(
        "SELECT DISTINCT t.name FROM tags t JOIN media_tags mt ON mt.tag_id = t.id JOIN media_items ON media_items.id = mt.item_id WHERE {} ORDER BY t.name ASC",
        where_clause
    );
    let tags: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &tags_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get studios
    let studios_sql = format!(
        "SELECT DISTINCT s.name FROM studios s JOIN media_studios ms ON ms.studio_id = s.id JOIN media_items ON media_items.id = ms.item_id WHERE {} ORDER BY s.name ASC",
        where_clause
    );
    let studios: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &studios_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get official ratings
    let ratings_sql = format!(
        "SELECT DISTINCT media_items.official_rating FROM media_items WHERE {} AND media_items.official_rating IS NOT NULL AND media_items.official_rating <> '' ORDER BY media_items.official_rating ASC",
        where_clause
    );
    let ratings: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &ratings_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("official_rating").ok().flatten().map(|n| json!(n)))
        .collect();

    Ok(json!({
        "Genres": genres,
        "Years": years,
        "Tags": tags,
        "Studios": studios,
        "OfficialRatings": ratings,
        "VideoTypes": ["VideoFile", "Iso", "Dvd", "BluRay"],
    }))
}

/// GET /Items/{item_id}/Ancestors — breadcrumb navigation
pub async fn item_ancestors(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match item_ancestors_inner(&state.db, &user_id, &item_id).await {
        Ok(ancestors) => Json(ancestors).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_ancestors_inner(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let mut ancestors = Vec::new();
    let mut current_id = Some(item_id.to_string());

    // Walk up the parent chain
    while let Some(id) = current_id {
        let row = db
            .query_one(crate::db::helpers::portable_statement(
                backend,
                &item_queries::media_item_select_sql("WHERE media_items.id = ?"),
                vec![user_id.into(), id.clone().into()],
            ))
            .await?;

        match row {
            Some(r) => {
                let item = crate::library::models::MediaItem::from_query_result(&r)?;
                current_id = if item.parent_id.is_empty() || item.parent_id == item.id {
                    None
                } else {
                    Some(item.parent_id.clone())
                };
                ancestors.push(json!({
                    "Name": item.title,
                    "Id": item.id,
                    "Type": item.item_type,
                    "IsFolder": item.is_folder,
                    "Path": item.path,
                }));
            }
            None => break,
        }
    }

    ancestors.reverse();
    Ok(ancestors)
}

/// POST /Users/{user_id}/PlayingItems/{item_id} — report playback start
pub async fn playing_item_start(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    _headers: HeaderMap,
    Query(_query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Response {
    let position_ticks = body
        .as_ref()
        .and_then(|b| b.get("PositionTicks").and_then(Value::as_i64))
        .unwrap_or(0);

    let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    // Save RunTimeTicks if provided
    if let Some(rt) = body.as_ref().and_then(|b| b.get("RunTimeTicks").and_then(Value::as_i64)).filter(|v| *v > 0) {
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
    let now = crate::util::now_unix();
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
    body: Option<Json<Value>>,
) -> Response {
    let position_ticks = query
        .get("PositionTicks")
        .and_then(|v| v.parse::<i64>().ok())
        .or_else(|| body.as_ref().and_then(|b| b.get("PositionTicks").and_then(Value::as_i64)))
        .unwrap_or(0);

    let result = upsert_playback_position(&state.db, &user_id, &item_id, position_ticks).await;
    if let Err(error) = result {
        return internal_error(error);
    }

    // Save RunTimeTicks if provided
    if let Some(rt) = query
        .get("RunTimeTicks")
        .and_then(|v| v.parse::<i64>().ok())
        .or_else(|| body.as_ref().and_then(|b| b.get("RunTimeTicks").and_then(Value::as_i64)))
        .filter(|v| *v > 0)
    {
        let _ = state.db.execute(crate::db::helpers::portable_statement(
            state.db.get_database_backend(),
            "UPDATE media_items SET runtime_ticks = ? WHERE id = ? AND runtime_ticks IS NULL",
            vec![rt.into(), item_id.into()],
        )).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// GET /UserSettings/{user_id} — get user settings
pub async fn get_user_settings(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state.db
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
                    let short_key = key.strip_prefix(&format!("user_settings:{}:", user_id)).unwrap_or(&key);
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

/// GET /Items/Suggestions — alternative suggestions path
pub async fn items_suggestions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    crate::jellyfin::items::user_suggestions(State(state), Path(user_id), Query(query)).await
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

/// GET /Items/{item_id}/Intros — get intros (returns empty, not supported)
pub async fn item_intros() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Items/{item_id}/LocalTrailers — get local trailers
pub async fn item_local_trailers(
    State(_state): State<Arc<AppState>>,
    Path(_item_id): Path<String>,
) -> Response {
    Json(json!([])).into_response()
}

/// GET /Items/{item_id}/SpecialFeatures — get special features
pub async fn item_special_features(
    State(_state): State<Arc<AppState>>,
    Path(_item_id): Path<String>,
) -> Response {
    Json(json!([])).into_response()
}

/// DELETE /Users/{user_id}/TrackSelections/{track_type} — clear track selections
pub async fn clear_track_selections(
    State(_state): State<Arc<AppState>>,
    Path((_user_id, _track_type)): Path<(String, String)>,
) -> Response {
    // No-op for now - track selections are client-side
    StatusCode::NO_CONTENT.into_response()
}

/// GET /Artists/{name}/Images/{image_type} — serve artist image
pub async fn artist_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((name, image_type)): Path<(String, String)>,
) -> Response {
    // Look up artist by name and serve their image
    let backend = state.db.get_database_backend();
    let row = state.db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM people WHERE name = ?",
            vec![name.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            if let Ok(person_id) = r.get_str("id") {
                return crate::jellyfin::persons::person_image(
                    State(state),
                    headers,
                    Path((person_id, image_type)),
                )
                .await;
            }
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// POST /Playlists/{playlist_id}/Items/{item_id}/Move/{new_index} — reorder playlist
pub async fn playlist_move_item(
    State(_state): State<Arc<AppState>>,
    Path((_playlist_id, _item_id, _new_index)): Path<(String, String, usize)>,
) -> Response {
    // Not implemented yet - would need sort_order in linked_children
    StatusCode::NO_CONTENT.into_response()
}

/// GET /Items/{item_id}/RemoteSearch/Subtitles/{language} — search remote subtitles
pub async fn remote_subtitle_search(
    State(state): State<Arc<AppState>>,
    Path((item_id, language)): Path<(String, String)>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    // Return empty - remote subtitle providers not implemented
    Json(json!([])).into_response()
}

/// POST /Items/{item_id}/RemoteSearch/Subtitles/{subtitle_id} — download remote subtitle
pub async fn download_remote_subtitle(
    State(_state): State<Arc<AppState>>,
    Path((item_id, subtitle_id)): Path<(String, String)>,
) -> Response {
    // Not implemented - would need subtitle provider integration
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Videos/{item_id}/{media_source_id}/Attachments/{index}/Stream — font attachments
pub async fn attachment_stream(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index)): Path<(String, String, String)>,
) -> Response {
    // Look for external subtitle attachment files
    let backend = state.db.get_database_backend();
    let row = state.db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT path FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' AND is_external = 1 AND stream_index = ?",
            vec![item_id.into(), index.parse::<i64>().unwrap_or(0).into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            if let Ok(path) = r.get_str("path") {
                match tokio::fs::read(&path).await {
                    Ok(bytes) => {
                        let content_type = if path.ends_with(".ttf") || path.ends_with(".otf") {
                            "application/octet-stream"
                        } else {
                            "application/octet-stream"
                        };
                        return (
                            [(axum::http::header::CONTENT_TYPE, content_type)],
                            bytes,
                        )
                            .into_response();
                    }
                    Err(_) => return StatusCode::NOT_FOUND.into_response(),
                }
            }
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// HEAD /Items/{item_id}/Images/{image_type} — HEAD request for image (ETag caching)
pub async fn item_image_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
) -> Response {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use crate::entities::image_assets::{Entity as ImageAssets, Column};

    let model = ImageAssets::find()
        .filter(Column::ItemId.eq(&item_id))
        .filter(Column::ImageType.eq(&image_type))
        .filter(Column::ImageIndex.eq(0))
        .one(&state.db)
        .await;

    match model {
        Ok(Some(m)) => {
            let etag = m.etag.unwrap_or_default();
            if headers
                .get(axum::http::header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim_matches('"') == etag)
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            let path = m.path.unwrap_or_default();
            match tokio::fs::metadata(&path).await {
                Ok(meta) => {
                    let mut resp_headers = axum::http::HeaderMap::new();
                    resp_headers.insert(
                        axum::http::header::CONTENT_LENGTH,
                        axum::http::HeaderValue::from_str(&meta.len().to_string()).unwrap(),
                    );
                    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
                        resp_headers.insert(axum::http::header::ETAG, v);
                    }
                    (resp_headers, StatusCode::OK).into_response()
                }
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// HEAD /Items/{item_id}/Images/{image_type}/{index} — HEAD for indexed image
pub async fn item_image_index_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type, index)): Path<(String, String, i64)>,
) -> Response {
    item_image_head(State(state), headers, Path((item_id, image_type))).await
}
