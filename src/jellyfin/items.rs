use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::people::Entity as People,
    jellyfin::{
        common::{internal_error, strip_nulls},
        item_queries::{find_library_as_item, latest_media_items, library_views, list_media_items, resume_media_items},
    },
    library::{models::MediaItem, scanner::scan_media_library},
    util::now_unix,
};

mod discovery;
mod item_operations;
mod remote_metadata;

pub use crate::jellyfin::item_queries::find_media_item;
pub use discovery::{search_hints, shows_missing, shows_next_up, similar_items};
pub use item_operations::{delete_info, delete_items, update_item};
pub use remote_metadata::{apply_remote_search, remote_search};

/// Deduplicate episodes by (parent_id, season_number, episode_number).
/// When multiple video files exist for the same episode (multi-version),
/// keep the one with the largest size_bytes (highest quality).
fn deduplicate_episodes(items: Vec<MediaItem>) -> Vec<MediaItem> {
    let mut map: HashMap<(String, i64, i64), MediaItem> = HashMap::new();
    for item in items {
        let key = (
            item.parent_id.clone(),
            item.season_number.unwrap_or(0),
            item.episode_number.unwrap_or(0),
        );
        let should_replace = match map.get(&key) {
            Some(existing) => item.size_bytes > existing.size_bytes,
            None => true,
        };
        if should_replace {
            map.insert(key, item);
        }
    }
    let mut result: Vec<_> = map.into_values().collect();
    result.sort_by(|a, b| {
        a.season_number.unwrap_or(0).cmp(&b.season_number.unwrap_or(0))
            .then_with(|| a.episode_number.unwrap_or(0).cmp(&b.episode_number.unwrap_or(0)))
            .then_with(|| a.title.cmp(&b.title))
    });
    result
}

pub async fn views(State(state): State<Arc<AppState>>) -> Response {
    match library_views(&state.db).await {
        Ok(items) => {
            let items: Vec<_> = items.into_iter().map(strip_nulls).collect();
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_media_items(&state.db, &user_id, &query).await {
        Ok((items, total)) => media_list_response_with_total(items, total),
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let parent_id = query.get("ParentId").map(String::as_str);
    match latest_media_items(&state.db, &user_id, parent_id).await {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|item| strip_nulls(item.to_jellyfin_json()))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn resume_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match resume_media_items(&state.db, &user_id).await {
        Ok(items) => {
            let total = items.len();
            let enriched = enrich_resume_items(&state.db, items).await;
            Json(json!({ "Items": enriched, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn enrich_resume_items(db: &DatabaseConnection, items: Vec<MediaItem>) -> Vec<Value> {
    use std::collections::HashMap;
    let backend = db.get_database_backend();

    // Collect parent_ids for Episode items to look up series info
    let parent_ids: Vec<&str> = items.iter()
        .filter(|i| i.item_type == "Episode")
        .map(|i| i.parent_id.as_str())
        .collect();

    // Batch lookup: parent_id -> (season_title, series_id, series_title)
    let mut season_map: HashMap<String, (String, String, String)> = HashMap::new();
    if !parent_ids.is_empty() {
        let placeholders = parent_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT s.id AS season_id, s.title AS season_title, ser.id AS series_id, ser.title AS series_title \
             FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id \
             WHERE s.id IN ({placeholders})",
        );
        let mut vals: Vec<sea_orm::Value> = parent_ids.iter().map(|p| (*p).into()).collect();
        if let Ok(rows) = db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals)).await {
            for row in &rows {
                if let (Ok(sid), Ok(st), Ok(srid), Ok(srt)) = (
                    row.get_str("season_id"),
                    row.get_str("season_title"),
                    row.get_str("series_id"),
                    row.get_str("series_title"),
                ) {
                    season_map.insert(sid, (st, srid, srt));
                }
            }
        }
    }

    // Also batch load media sources for resume items (to get RunTimeTicks, MediaStreams)
    let item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let mut source_map: HashMap<String, Vec<Value>> = HashMap::new();
    if !item_ids.is_empty() {
        for item in &items {
            if item.is_folder {
                // Folder items (Movie/Episode) - load child video sources
                if let Ok(sources) = super::playback::child_video_sources(db, &item.id).await {
                    if !sources.is_empty() {
                        source_map.insert(item.id.clone(), sources);
                    }
                }
            } else {
                // Non-folder items - build MediaSource directly from the item itself
                let stream_jsons = super::playback::media_streams_for_item(db, &item.id).await.unwrap_or_default();
                let container = item.container.as_deref().unwrap_or("bin");
                let source = json!({
                    "Id": item.id,
                    "Name": item.title,
                    "Path": item.path,
                    "Type": "Default",
                    "Container": container,
                    "Size": item.size_bytes,
                    "RunTimeTicks": item.runtime_ticks,
                    "SupportsDirectPlay": true,
                    "SupportsDirectStream": true,
                    "SupportsTranscoding": false,
                    "IsInfiniteStream": false,
                    "MediaStreams": stream_jsons,
                    "Formats": [],
                    "RequiredHttpHeaders": {},
                    "DirectStreamUrl": format!("/Videos/{}/stream.{}", item.id, container),
                });
                source_map.insert(item.id.clone(), vec![source]);
            }
        }
    }

    items.into_iter().map(|item| {
        let mut value = item.to_jellyfin_json();

        // Enrich Episode items with series/season info
        if item.item_type == "Episode" {
            if let Some((season_title, series_id, series_title)) = season_map.get(&item.parent_id) {
                value["SeriesName"] = json!(series_title);
                value["SeriesId"] = json!(series_id);
                value["SeasonName"] = json!(season_title);
                value["SeasonId"] = json!(item.parent_id);
            }
            value["SupportsResume"] = json!(true);
        }

        // Add MediaSources if available
        if let Some(sources) = source_map.get(&item.id) {
            if !sources.is_empty() {
                value["MediaSources"] = Value::Array(sources.clone());
                // Flatten streams to top-level
                let mut all_streams = Vec::new();
                for source in sources {
                    if let Some(streams) = source.get("MediaStreams").and_then(Value::as_array) {
                        all_streams.extend(streams.clone());
                    }
                }
                value["MediaStreams"] = Value::Array(all_streams);

                // Get RunTimeTicks from first source if item doesn't have it
                if item.runtime_ticks.is_none() {
                    if let Some(rt) = sources.first().and_then(|s| s.get("RunTimeTicks")).and_then(Value::as_i64).filter(|v| *v > 0) {
                        value["RunTimeTicks"] = json!(rt);
                    }
                }
            }
        }

        // Ensure RunTimeTicks is set from item if available
        if item.runtime_ticks.is_some() && value.get("RunTimeTicks").and_then(Value::as_i64).is_none() {
            value["RunTimeTicks"] = json!(item.runtime_ticks);
        }

        // Calculate PlayedPercentage if not set
        if item.played_percentage.is_none() && item.playback_position_ticks > 0 {
            if let Some(rt) = value.get("RunTimeTicks").and_then(Value::as_i64).filter(|v| *v > 0) {
                let pct = (item.playback_position_ticks as f64 / rt as f64 * 100.0).min(100.0);
                if let Some(ud) = value.get_mut("UserData") {
                    ud["PlayedPercentage"] = json!(pct);
                }
            }
        }

        strip_nulls(value)
    }).collect()
}

pub async fn show_seasons(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match child_items_by_type(&state.db, &user_id, &show_id, "Season").await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn show_episodes(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let result = if let Some(season_id) = query.get("SeasonId") {
        child_items_by_type(&state.db, &user_id, season_id, "Episode").await
    } else {
        descendant_episodes(&state.db, &user_id, &show_id).await
    };
    match result {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub(super) fn media_list_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

pub(super) fn media_list_response_with_total(items: Vec<MediaItem>, total: usize) -> Response {
    Json(json!({ "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

async fn child_items_by_type(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: &str,
    item_type: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let order = if item_type == "Episode" {
        "ORDER BY media_items.season_number ASC, media_items.episode_number ASC"
    } else {
        "ORDER BY media_items.title ASC"
    };
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &crate::jellyfin::item_queries::media_item_select_sql(
                &format!("WHERE media_items.parent_id = ? AND media_items.item_type = ? {order}"),
            ),
            vec![user_id.into(), parent_id.into(), item_type.into()],
        ))
        .await
        .with_context(|| format!("failed to list {item_type} children for: {parent_id}"))?;
    let mut items = rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show child items")?;
    // Deduplicate episodes by (parent_id, season_number, episode_number), keep largest file
    if item_type == "Episode" && items.len() > 1 {
        items = deduplicate_episodes(items);
    }
    // Batch load image tags
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        if let Ok(tags_map) = crate::jellyfin::item_queries::batch_item_image_tags(db, &ids).await {
            for item in &mut items {
                if let Some(tags) = tags_map.get(&item.id) {
                    item.image_tags = Some(tags.clone());
                }
            }
        }
    }
    Ok(items)
}

async fn descendant_episodes(
    db: &DatabaseConnection,
    user_id: &str,
    show_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.item_type = 'Episode' ORDER BY media_items.title ASC"#,
                crate::jellyfin::item_queries::media_item_select_sql("").trim()
            ),
            vec![show_id.into(), user_id.into(), show_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list episodes for show: {show_id}"))?;
    let mut items = rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show episodes")?;
    // Deduplicate episodes by (parent_id, season_number, episode_number), keep largest file
    if items.len() > 1 {
        items = deduplicate_episodes(items);
    }
    // Batch load image tags
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        if let Ok(tags_map) = crate::jellyfin::item_queries::batch_item_image_tags(db, &ids).await {
            for item in &mut items {
                if let Some(tags) = tags_map.get(&item.id) {
                    item.image_tags = Some(tags.clone());
                }
            }
        }
    }
    Ok(items)
}

pub async fn item_by_id(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, &user_id, item).await {
            Ok(item) => Json(strip_nulls(item)).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => {
            // Check if it's a library
            if let Ok(Some(lib_item)) = find_library_as_item(&state.db, &item_id).await {
                return Json(lib_item).into_response();
            }
            // Check if it's a person
            if let Ok(Some(person_item)) = find_person_as_item(&state.db, &user_id, &item_id).await {
                return Json(person_item).into_response();
            }
            Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn item_by_id_public(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = state.user_id.to_string();
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, &user_id, item).await {
            Ok(item) => Json(strip_nulls(item)).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => {
            // Check if it's a library
            if let Ok(Some(lib_item)) = find_library_as_item(&state.db, &item_id).await {
                return Json(lib_item).into_response();
            }
            // Check if it's a person
            if let Ok(Some(person_item)) = find_person_as_item(&state.db, &user_id, &item_id).await {
                return Json(person_item).into_response();
            }
            Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// Look up a person by ID and return as a BaseItemDto-like JSON object.
async fn find_person_as_item(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    person_id: &str,
) -> anyhow::Result<Option<Value>> {
    let model = People::find()
        .filter(crate::entities::people::Column::Id.eq(person_id))
        .one(db)
        .await?;
    match model {
        Some(m) => {
            let image_tags = crate::jellyfin::persons::person_images(db, &m.id).await?;
            // Get favorite status from user_data
            let backend = db.get_database_backend();
            let is_favorite = db
                .query_one(crate::db::helpers::portable_statement(
                    backend,
                    "SELECT is_favorite FROM user_data WHERE user_id = ? AND item_id = ?",
                    vec![user_id.into(), m.id.clone().into()],
                ))
                .await?
                .map(|r| r.get_i64("is_favorite").unwrap_or(0) != 0)
                .unwrap_or(false);
            Ok(Some(json!({
                "Name": m.name,
                "Id": m.id,
                "ServerId": "jellyfin-rs",
                "Type": "Person",
                "Etag": null,
                "Path": null,
                "Overview": m.overview,
                "ProductionYear": null,
                "PremiereDate": null,
                "EndDate": null,
                "SortName": m.name,
                "ProviderIds": {},
                "CanDelete": false,
                "CanDownload": false,
                "PlayAccess": "Full",
                "IsFolder": false,
                "LocationType": null,
                "MediaSources": [],
                "ImageTags": image_tags,
                "BackdropImageTags": [],
                "ImageBlurHashes": {},
                "Genres": [],
                "GenreItems": [],
                "Tags": [],
                "Studios": [],
                "UserData": {
                    "ItemId": m.id,
                    "Key": m.id,
                    "Played": false,
                    "IsFavorite": is_favorite,
                    "PlayCount": 0,
                    "PlaybackPositionTicks": 0,
                    "PlayedPercentage": null,
                    "Rating": null,
                    "LastPlayedDate": null,
                    "Likes": null,
                    "UnplayedItemCount": null,
                },
                "LockData": false,
                "LockedFields": [],
                "ExternalUrls": [],
            })))
        }
        None => Ok(None),
    }
}

pub async fn items_root(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match list_media_items(&state.db, &user_id, &query).await {
        Ok((items, total)) => media_list_response_with_total(items, total),
        Err(error) => internal_error(error),
    }
}

async fn item_json_with_provider_ids(
    db: &DatabaseConnection,
    user_id: &str,
    item: MediaItem,
) -> anyhow::Result<Value> {
    let mut value = item.to_jellyfin_json();
    let backend = db.get_database_backend();

    // Combine provider_ids and image_assets into one query
    let meta_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT 'provider' AS src, provider AS key, provider_item_id AS val, NULL AS etag FROM provider_ids WHERE item_id = ? UNION ALL SELECT 'image' AS src, image_type AS key, NULL AS val, etag FROM image_assets WHERE item_id = ?",
            vec![item.id.clone().into(), item.id.clone().into()],
        ))
        .await
        .with_context(|| format!("failed to load metadata for item: {}", item.id))?;

    let mut provider_ids = serde_json::Map::new();
    let mut image_map = serde_json::Map::new();
    for row in &meta_rows {
        let src = row.get_str("src").unwrap_or_default();
        let key = row.get_str("key").unwrap_or_default();
        if src == "provider" {
            if let Ok(val) = row.get_str("val") {
                provider_ids.insert(key, Value::String(val));
            }
        } else if src == "image" {
            if let Ok(etag) = row.get_str("etag") {
                if !etag.is_empty() {
                    image_map.insert(key, json!(etag));
                }
            }
        }
    }
    value["ProviderIds"] = Value::Object(provider_ids);
    let image_tags: Value = image_map.into();
    let backdrop_tags: Vec<Value> = image_tags
        .get("Backdrop")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|etag| vec![Value::String(etag.to_string())])
        .unwrap_or_default();
    value["BackdropImageTags"] = Value::Array(backdrop_tags);
    value["ImageTags"] = image_tags;

    // Combine genres, tags, studios into one UNION ALL query
    let rel_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT 'genre' AS kind, g.id, g.name FROM genres g JOIN media_genres mg ON mg.genre_id = g.id WHERE mg.item_id = ?
               UNION ALL
               SELECT 'tag' AS kind, t.id, t.name FROM tags t JOIN media_tags mt ON mt.tag_id = t.id WHERE mt.item_id = ?
               UNION ALL
               SELECT 'studio' AS kind, s.id, s.name FROM studios s JOIN media_studios ms ON ms.studio_id = s.id WHERE ms.item_id = ?"#,
            vec![item.id.clone().into(), item.id.clone().into(), item.id.clone().into()],
        ))
        .await
        .with_context(|| format!("failed to load relations for item: {}", item.id))?;

    let mut genres = Vec::new();
    let mut tags = Vec::new();
    let mut studios = Vec::new();
    for row in &rel_rows {
        let kind = row.get_str("kind").unwrap_or_default();
        let id = row.get_str("id").unwrap_or_default();
        let name = row.get_str("name").unwrap_or_default();
        let entry = json!({"Name": name, "Id": id});
        match kind.as_str() {
            "genre" => genres.push(entry),
            "tag" => tags.push(entry),
            "studio" => studios.push(entry),
            _ => {}
        }
    }
    value["GenreItems"] = Value::Array(genres);
    value["TagItems"] = Value::Array(tags);
    value["Studios"] = Value::Array(studios);
    value["People"] = Value::Array(people_values(db, &item.id, Some(user_id)).await?);

    // For Movie/Episode folders, load child video media sources (multi-version support)
    if (item.item_type == "Movie" || item.item_type == "Episode") && item.is_folder {
        if let Ok(sources) = super::playback::child_video_sources(db, &item.id).await {
            if !sources.is_empty() {
                value["MediaSources"] = Value::Array(sources.clone());
                // Also flatten streams to top-level MediaStreams for clients that expect it
                let mut all_streams = Vec::new();
                for source in &sources {
                    if let Some(streams) = source.get("MediaStreams").and_then(Value::as_array) {
                        all_streams.extend(streams.clone());
                    }
                }
                value["MediaStreams"] = Value::Array(all_streams);
            }
        }
    } else if !item.is_folder {
        // For non-folder items (Episode/Video files), load media streams from DB
        let streams = super::playback::media_streams_for_item(db, &item.id).await.unwrap_or_default();
        if !streams.is_empty() {
            // Rebuild MediaSources with streams included
            let container = item.container.as_deref().unwrap_or("bin");
            let source = json!({
                "Id": item.id,
                "Name": item.title,
                "Path": item.path,
                "Type": "Default",
                "Protocol": "File",
                "Container": container,
                "Size": item.size_bytes,
                "RunTimeTicks": item.runtime_ticks,
                "SupportsDirectPlay": true,
                "SupportsDirectStream": true,
                "SupportsTranscoding": false,
                "SupportsProbing": true,
                "IsInfiniteStream": false,
                "IsRemote": false,
                "RequiresOpening": false,
                "RequiresClosing": false,
                "MediaStreams": streams,
                "Formats": [],
                "RequiredHttpHeaders": {},
                "DirectStreamUrl": format!("/Videos/{}/stream.{}", item.id, container),
                "VideoType": "VideoFile",
            });
            value["MediaSources"] = json!([source]);
            value["MediaStreams"] = Value::Array(streams);
        }
    }

    Ok(value)
}

async fn relation_values(
    db: &DatabaseConnection,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT {table}.id, {table}.name FROM {table} JOIN {relation_table} ON {relation_table}.{relation_column} = {table}.id WHERE {relation_table}.item_id = ? ORDER BY {table}.name ASC"
    );
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list {table} for item: {item_id}"))?;
    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Id": row.get_str("id")?,
                "Name": row.get_str("name")?,
            }))
        })
        .collect()
}

async fn people_values(db: &DatabaseConnection, item_id: &str, user_id: Option<&str>) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT people.id, people.name, media_people.role, media_people.person_type, ia.etag AS primary_image_tag FROM people JOIN media_people ON media_people.person_id = people.id LEFT JOIN image_assets ia ON ia.item_id = people.id AND ia.image_type = 'Primary' WHERE media_people.item_id = ? ORDER BY media_people.sort_order ASC, people.name ASC",
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list people for item: {item_id}"))?;

    // Batch load user data for all people
    let person_ids: Vec<String> = rows.iter()
        .filter_map(|r| r.get_opt_str("id").ok().flatten())
        .collect();
    let fav_map = if user_id.is_some() && !person_ids.is_empty() {
        let uid = user_id.unwrap();
        let ph = person_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT item_id, is_favorite FROM user_data WHERE user_id = ? AND item_id IN ({})", ph);
        let mut vals: Vec<sea_orm::Value> = vec![uid.into()];
        for pid in &person_ids { vals.push(pid.as_str().into()); }
        let fav_rows = db
            .query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
            .await?;
        let mut m: HashMap<String, bool> = HashMap::new();
        for r in &fav_rows {
            if let (Ok(pid), Ok(fav)) = (r.get_str("item_id"), r.get_i64("is_favorite")) {
                m.insert(pid, fav != 0);
            }
        }
        m
    } else {
        HashMap::new()
    };

    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            let id = row.get_str("id")?;
            let is_favorite = fav_map.get(&id).copied().unwrap_or(false);
            let mut value = json!({
                "Id": id,
                "Name": row.get_str("name")?,
                "Role": row.get_opt_str("role")?,
                "Type": row.get_opt_str("person_type")?,
                "UserData": {
                    "IsFavorite": is_favorite,
                },
            });
            if let Some(tag) = row.get_opt_str("primary_image_tag")? {
                if !tag.is_empty() {
                    value["PrimaryImageTag"] = json!(tag);
                }
            }
            Ok(value)
        })
        .collect()
}

pub async fn item_subtitles(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match subtitle_list_inner(&state.db, &item_id).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn subtitle_list_inner(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT stream_index, codec, language, title, is_external FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' ORDER BY stream_index ASC",
            vec![item_id.into()],
        ))
        .await
        .context("failed to list subtitles")?;

    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Index": row.get_i64("stream_index")?,
                "Codec": row.get_opt_str("codec")?,
                "Language": row.get_opt_str("language")?,
                "DisplayTitle": row.get_opt_str("title")?,
                "IsExternal": row.get_i64("is_external").unwrap_or_default() != 0,
            }))
        })
        .collect()
}

pub async fn metadata_reset(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let item_ids: Vec<&str> = body
        .get("Ids")
        .and_then(Value::as_str)
        .map(|ids| {
            ids.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if item_ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let now = now_unix();
    let backend = state.db.get_database_backend();
    for item_id in &item_ids {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET overview = NULL, production_year = NULL, updated_at = ? WHERE id = ?",
                vec![now.into(), (*item_id).into()],
            ))
            .await;

        for table in [
            "media_people",
            "media_genres",
            "media_tags",
            "media_studios",
            "provider_ids",
        ] {
            let _ = state
                .db
                .execute(crate::db::helpers::portable_statement(
                    backend,
                    &format!("DELETE FROM {table} WHERE item_id = ?"),
                    vec![(*item_id).into()],
                ))
                .await;
        }

        crate::jellyfin::system::log_activity(
            &state,
            "Metadata reset",
            "MetadataReset",
            None,
            Some(item_id),
        )
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
) -> Response {
    match display_preferences_inner(&state.db, &prefs_id).await {
        Ok(Some(prefs)) => Json(prefs).into_response(),
        Ok(None) => Json(json!({ "Id": prefs_id, "CustomPrefs": {} })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let now = now_unix();
    let prefs_json = body.to_string();
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let default_user_id = state.user_id.to_string();
    let user_id = body
        .get("UserId")
        .and_then(Value::as_str)
        .unwrap_or(&default_user_id);
    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO display_preferences (id, user_id, preferences_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET preferences_json = excluded.preferences_json, user_id = excluded.user_id, updated_at = excluded.updated_at"#,
            vec![id.into(), user_id.into(), prefs_json.into(), now.into(), now.into()],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

async fn display_preferences_inner(
    db: &DatabaseConnection,
    prefs_id: &str,
) -> anyhow::Result<Option<Value>> {
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT preferences_json FROM display_preferences WHERE id = ?",
            vec![id.into()],
        ))
        .await
        .context("failed to load display preferences")?;
    match row {
        Some(row) => {
            let json_str: String = row.get_str("preferences_json")?;
            Ok(Some(serde_json::from_str(&json_str)?))
        }
        None => Ok(None),
    }
}

pub async fn item_counts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match item_counts_inner(&state.db, &query).await {
        Ok(counts) => Json(counts).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_counts_inner(
    db: &DatabaseConnection,
    _query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT library_id, item_type, COUNT(*) AS count FROM media_items WHERE is_folder = 0 GROUP BY library_id, item_type"#,
            vec![],
        ))
        .await
        .context("failed to count items")?;

    let mut counts = serde_json::Map::new();
    for row in &rows {
        let library_id: String = row.get_str("library_id")?;
        let item_type: String = row.get_str("item_type")?;
        let count: i64 = row.get_i64("count")?;
        if let Some(obj) = counts
            .entry(library_id)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            let existing = obj.get(&item_type).and_then(Value::as_i64).unwrap_or(0);
            obj.insert(item_type, json!(existing + count));
        }
    }

    Ok(Value::Object(counts))
}

pub async fn scan_handler(State(state): State<Arc<AppState>>) -> Response {
    tokio::spawn(async move {
        let start = now_unix();
        let result = scan_media_library(&state).await;
        let end = now_unix();
        let (status, message) = match &result {
            Ok(count) => ("Completed", Some(format!("Scanned {count} items"))),
            Err(error) => ("Failed", Some(format!("{error:#}"))),
        };
        crate::jellyfin::system::upsert_task_result(
            &state,
            "scan-library",
            status,
            start,
            end,
            message.as_deref(),
        )
        .await;
        crate::jellyfin::system::log_activity(&state, "Library scan", "LibraryScan", None, None)
            .await;
    });
    Json(json!({ "Scanning": true })).into_response()
}

pub async fn external_id_infos() -> Response {
    Json(json!([
        {
            "Name": "TheMovieDb",
            "Key": "Tmdb",
            "Website": "https://www.themoviedb.org/",
            "UrlFormatString": "https://www.themoviedb.org/movie/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "TheTVDB",
            "Key": "Tvdb",
            "Website": "https://thetvdb.com/",
            "UrlFormatString": "https://thetvdb.com/?id={0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "IMDb",
            "Key": "IMDB",
            "Website": "https://www.imdb.com/",
            "UrlFormatString": "https://www.imdb.com/title/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album",
            "Key": "MusicBrainzAlbum",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album Artist",
            "Key": "MusicBrainzAlbumArtist",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/artist/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Release Group",
            "Key": "MusicBrainzReleaseGroup",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release-group/{0}",
            "IsSupportedAsIdentifier": true
        }
    ]))
    .into_response()
}

pub async fn movie_recommendations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let parent_id = query.get("ParentId").map(String::as_str);
    match movie_recommendations_inner(&state.db, &user_id, parent_id).await {
        Ok(categories) => Json(categories).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn movie_recommendations_inner(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let item_limit = 8;
    let mut category_counter: i64 = 1;

    // Get recently played Movie folders (is_folder=1, item_type='Movie')
    let recent_movies = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type = 'Movie' AND COALESCE(user_data.played, 0) = 1 AND COALESCE(user_data.play_count, 0) > 0 {} ORDER BY user_data.last_played_at DESC LIMIT 12",
                crate::jellyfin::item_queries::media_item_select_sql(""),
                parent_id.map(|_p| "AND media_items.library_id = ?").unwrap_or(""),
            ),
            if parent_id.is_some() {
                vec![user_id.into(), parent_id.unwrap().into()]
            } else {
                vec![user_id.into()]
            },
        ))
        .await
        .context("failed to get recent movies")?;

    let recent_movies = crate::jellyfin::item_queries::decode_media_items(&recent_movies)?;
    if recent_movies.is_empty() {
        return Ok(Vec::new());
    }

    let mut categories = Vec::new();

    // Category 1: Similar by genre to recently played
    let mut similar_items = Vec::new();
    for movie in &recent_movies[..recent_movies.len().min(4)] {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                r#"SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id WHERE mg_src.item_id = ? GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT 8"#,
                vec![movie.id.clone().into()],
            ))
            .await?;
        for row in &rows {
            if let Ok(id) = row.get_str("item_id") {
                if !similar_items.contains(&id) {
                    similar_items.push(id);
                }
            }
        }
    }

    if !similar_items.is_empty() {
        let ph = similar_items.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let items = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &format!("{} WHERE media_items.id IN ({}) ORDER BY media_items.production_year DESC LIMIT {item_limit}", crate::jellyfin::item_queries::media_item_select_sql(""), ph),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &similar_items { vals.push(id.as_str().into()); }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            categories.push(json!({
                "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                "RecommendationType": "SimilarToRecentlyPlayed",
                "BaselineItemName": recent_movies[0].title.clone(),
                "CategoryId": category_counter,
            }));
            category_counter += 1;
        }
    }

    // Category 2: Movies with same actors
    let mut actor_items = Vec::new();
    for movie in &recent_movies[..recent_movies.len().min(3)] {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                r#"SELECT mp2.item_id FROM media_people mp1 JOIN media_people mp2 ON mp1.person_id = mp2.person_id AND mp1.item_id <> mp2.item_id WHERE mp1.item_id = ? AND mp2.item_id NOT IN (SELECT id FROM media_items WHERE item_type IN ('Video', 'Episode')) GROUP BY mp2.item_id LIMIT 4"#,
                vec![movie.id.clone().into()],
            ))
            .await?;
        for row in &rows {
            if let Ok(id) = row.get_str("item_id") {
                if !actor_items.contains(&id) {
                    actor_items.push(id);
                }
            }
        }
    }

    if !actor_items.is_empty() {
        let ph = actor_items.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let items = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &format!("{} WHERE media_items.id IN ({}) LIMIT {item_limit}", crate::jellyfin::item_queries::media_item_select_sql(""), ph),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &actor_items { vals.push(id.as_str().into()); }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            categories.push(json!({
                "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                "RecommendationType": "HasActorFromRecentlyPlayed",
                "BaselineItemName": recent_movies[0].title.clone(),
                "CategoryId": category_counter,
            }));
            category_counter += 1;
        }
    }

    Ok(categories)
}

/// GET /Users/{user_id}/Suggestions — personalized suggestions
pub async fn user_suggestions(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);
    let parent_id = query.get("ParentId").map(String::as_str);

    // Return recently added unplayed items as suggestions
    let backend = state.db.get_database_backend();
    let (sql, vals) = if let Some(pid) = parent_id {
        (
            format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type IN ('Movie', 'Series') AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC LIMIT ?",
                super::item_queries::media_item_select_sql("AND media_items.library_id = ?")
            ),
            vec![user_id.clone().into(), pid.into(), (limit as i64).into()],
        )
    } else {
        (
            format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type IN ('Movie', 'Series') AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC LIMIT ?",
                super::item_queries::media_item_select_sql("")
            ),
            vec![user_id.clone().into(), (limit as i64).into()],
        )
    };

    match state.db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals)).await {
        Ok(rows) => {
            let items = super::item_queries::decode_media_items(&rows).unwrap_or_default();
            let total = items.len();
            Json(json!({ "Items": items.into_iter().map(|i| strip_nulls(i.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

/// GET /Users/{user_id}/HomeSections — home screen layout
pub async fn home_sections(
    State(_state): State<Arc<AppState>>,
    Path(_user_id): Path<String>,
) -> Response {
    // Return a standard home screen layout matching ContentSection SDK model
    let sections = json!([
        { "Name": "Continue Watching", "SectionType": "Resume", "ViewType": "Resume", "Id": "resume", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Next Up", "SectionType": "NextUp", "ViewType": "NextUp", "Id": "nextup", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Latest Movies", "SectionType": "Latest", "ViewType": "Latest", "CollectionType": "movies", "Id": "latest-movies", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Latest TV Shows", "SectionType": "Latest", "ViewType": "Latest", "CollectionType": "tvshows", "Id": "latest-tvshows", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Suggestions", "SectionType": "Suggestions", "ViewType": "Suggestions", "Id": "suggestions", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
    ]);
    Json(sections).into_response()
}

/// GET /Users/{user_id}/Sections/{section_id}/Items — items for a home section
pub async fn home_section_items(
    State(state): State<Arc<AppState>>,
    Path((user_id, section_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);

    match section_id.as_str() {
        "resume" => {
            match resume_media_items(&state.db, &user_id).await {
                Ok(items) => {
                    let total = items.len();
                    let enriched = enrich_resume_items(&state.db, items).await;
                    Json(json!({ "Items": enriched, "TotalRecordCount": total })).into_response()
                }
        Err(error) => internal_error(error.into()),
            }
        }
        "nextup" => {
            let user_id_ref = query
                .get("UserId")
                .cloned()
                .unwrap_or_else(|| user_id.clone());
            match super::items::discovery::shows_next_up(State(state), Query(query)).await {
                resp => resp,
            }
        }
        "latest-movies" | "latest-tvshows" => {
            let collection_type = if section_id == "latest-movies" { "movies" } else { "tvshows" };
            let backend = state.db.get_database_backend();
            // Find libraries matching the collection type
            let lib_rows = state.db
                .query_all(crate::db::helpers::portable_statement(
                    backend,
                    "SELECT id FROM libraries WHERE collection_type = ?",
                    vec![collection_type.into()],
                ))
                .await
                .unwrap_or_default();

            let mut all_items = Vec::new();
            for row in &lib_rows {
                if let Ok(lib_id) = row.get_str("id") {
                    if let Ok(items) = super::item_queries::latest_media_items(&state.db, &user_id, Some(&lib_id)).await {
                        all_items.extend(items);
                    }
                }
            }
            all_items.sort_by_key(|i| std::cmp::Reverse(i.modified_at));
            all_items.truncate(limit);

            if !all_items.is_empty() {
                let ids: Vec<String> = all_items.iter().map(|i| i.id.clone()).collect();
                if let Ok(tags_map) = super::item_queries::batch_item_image_tags(&state.db, &ids).await {
                    for item in &mut all_items {
                        if let Some(tags) = tags_map.get(&item.id) {
                            item.image_tags = Some(tags.clone());
                        }
                    }
                }
            }

            Json(json!(all_items.into_iter().map(|i| strip_nulls(i.to_jellyfin_json())).collect::<Vec<_>>())).into_response()
        }
        "suggestions" => {
            user_suggestions(State(state), Path(user_id), Query(query)).await
        }
        _ => Json(json!([])).into_response(),
    }
}
