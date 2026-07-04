use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::people::Entity as People,
    jellyfin::{
        auth::request_user_id_and_admin_or_default,
        common::{internal_error, strip_nulls},
        item_queries::find_library_as_item,
    },
    library::models::{MediaItem, media_source_json_with_streams},
};

pub async fn item_by_id(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match super::find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, &user_id, item, false).await
        {
            Ok(item) => Json(strip_nulls(item)).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => {
            // Check if it's a library
            if let Ok(Some(lib_item)) = find_library_as_item(&state.db, &item_id).await {
                return Json(lib_item).into_response();
            }
            // Check if it's a person
            if let Ok(Some(person_item)) = find_person_as_item(&state.db, &user_id, &item_id).await
            {
                return Json(person_item).into_response();
            }
            Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn item_by_id_public(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (user_id, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    let item_result = if is_admin {
        super::find_media_item_for_admin(&state.db, &user_id, &item_id).await
    } else {
        super::find_media_item(&state.db, &user_id, &item_id).await
    };
    match item_result {
        Ok(Some(item)) => {
            match item_json_with_provider_ids(&state.db, &user_id, item, is_admin).await {
                Ok(item) => Json(strip_nulls(item)).into_response(),
                Err(error) => internal_error(error),
            }
        }
        Ok(None) => {
            // Check if it's a library
            if let Ok(Some(lib_item)) = find_library_as_item(&state.db, &item_id).await {
                return Json(lib_item).into_response();
            }
            // Check if it's a person
            if let Ok(Some(person_item)) = find_person_as_item(&state.db, &user_id, &item_id).await
            {
                return Json(person_item).into_response();
            }
            Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// Look up a person by ID and return as a BaseItemDto-like JSON object.
async fn find_person_as_item(
    db: &DatabaseConnection,
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

async fn item_json_with_provider_ids(
    db: &DatabaseConnection,
    user_id: &str,
    item: MediaItem,
    include_private_sources: bool,
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
        if let Ok(sources) =
            crate::jellyfin::playback::child_video_sources(db, &item.id, include_private_sources)
                .await
        {
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
        let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
            .await
            .unwrap_or_default();
        if !streams.is_empty() {
            // Rebuild MediaSources with streams included
            let source = media_source_json_with_streams(&item, streams.clone());
            let top_level_streams = source
                .get("MediaStreams")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            value["MediaSources"] = json!([source]);
            value["MediaStreams"] = Value::Array(top_level_streams);
        }
    }

    // Enrich Episode items with series/season info
    if item.item_type == "Episode" {
        if let Some((series_name, series_id, season_name, season_id)) =
            get_episode_parent_info(db, &item.parent_id).await
        {
            value["SeriesName"] = json!(series_name);
            value["SeriesId"] = json!(series_id);
            value["SeasonName"] = json!(season_name);
            value["SeasonId"] = json!(season_id);
        }
    }

    // Enrich Season items with series info
    if item.item_type == "Season" {
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT id, title FROM media_items WHERE id = ? AND is_public = 1",
                vec![item.parent_id.clone().into()],
            ))
            .await
        {
            if let (Ok(id), Ok(title)) = (row.get_str("id"), row.get_str("title")) {
                value["SeriesName"] = json!(title);
                value["SeriesId"] = json!(id);
            }
        }
        // RecursiveItemCount for Season = episode count
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT COUNT(*) AS cnt FROM media_items WHERE parent_id = ? AND item_type = 'Episode' AND is_public = 1",
                vec![item.id.clone().into()],
            ))
            .await
        {
            let cnt = row.get_i64("cnt").unwrap_or(0);
            value["RecursiveItemCount"] = json!(cnt);
            // UnplayedItemCount = total - played episodes in this season
            if let Ok(Some(ud_row)) = db
                .query_one(crate::db::helpers::portable_statement(
                    db.get_database_backend(),
                    "SELECT COUNT(*) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id = ? AND mi.item_type = 'Episode' AND mi.is_public = 1 AND ud.user_id = ? AND ud.played = 1",
                    vec![item.id.clone().into(), user_id.into()],
                ))
                .await
            {
                let played_cnt = ud_row.get_i64("cnt").unwrap_or(0);
                value["UserData"]["UnplayedItemCount"] = json!(cnt - played_cnt);
            }
        }
    }

    // Enrich Series items with child counts
    if item.item_type == "Series" {
        // ChildCount = number of seasons
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT COUNT(*) AS cnt FROM media_items WHERE parent_id = ? AND item_type = 'Season' AND is_public = 1",
                vec![item.id.clone().into()],
            ))
            .await
        {
            let season_cnt = row.get_i64("cnt").unwrap_or(0);
            value["ChildCount"] = json!(season_cnt);
            value["SeasonCount"] = json!(season_cnt);
        }
        // RecursiveItemCount = total episodes under all seasons of this series
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT COUNT(*) AS cnt FROM media_items WHERE item_type = 'Episode' AND is_public = 1 AND parent_id IN (SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Season' AND is_public = 1)",
                vec![item.id.clone().into()],
            ))
            .await
        {
            let total_eps = row.get_i64("cnt").unwrap_or(0);
            value["RecursiveItemCount"] = json!(total_eps);
            value["RecursiveUnplayedItemCount"] = json!(total_eps); // will be overridden below
            // UnplayedItemCount = episodes not played by user
            if let Ok(Some(ud_row)) = db
                .query_one(crate::db::helpers::portable_statement(
                    db.get_database_backend(),
                    "SELECT COUNT(*) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.item_type = 'Episode' AND mi.is_public = 1 AND mi.parent_id IN (SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Season' AND is_public = 1) AND ud.user_id = ? AND ud.played = 1",
                    vec![item.id.clone().into(), user_id.into()],
                ))
                .await
            {
                let played_eps = ud_row.get_i64("cnt").unwrap_or(0);
                let unplayed = total_eps - played_eps;
                value["UserData"]["UnplayedItemCount"] = json!(unplayed);
                value["RecursiveUnplayedItemCount"] = json!(unplayed);
            }
        }
    }

    // Load chapters (intro/credits markers)
    if let Ok(chapters) = crate::chapters::get_chapters(db, &item.id).await {
        if !chapters.is_empty() {
            let chapter_values: Vec<Value> = chapters
                .iter()
                .map(|ch| {
                    json!({
                        "StartPositionTicks": ch.start_position_ticks,
                        "Name": ch.name,
                        "MarkerType": ch.marker_type,
                    })
                })
                .collect();
            value["Chapters"] = Value::Array(chapter_values);
            value["HasSegments"] = json!(true);
        }
    }

    Ok(value)
}

/// Get series name, series ID, season name, season ID for an episode's parent
async fn get_episode_parent_info(
    db: &DatabaseConnection,
    season_id: &str,
) -> Option<(String, String, String, String)> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT s.title AS season_title, s.parent_id AS series_id, ser.title AS series_title FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND ser.is_public = 1 WHERE s.id = ? AND s.is_public = 1",
            vec![season_id.into()],
        ))
        .await
        .ok()??;
    let season_title = row.get_str("season_title").ok()?;
    let series_id = row.get_str("series_id").ok()?;
    let series_title = row.get_str("series_title").ok()?;
    Some((series_title, series_id, season_title, season_id.to_string()))
}

#[allow(dead_code)]
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

async fn people_values(
    db: &DatabaseConnection,
    item_id: &str,
    user_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
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
    let person_ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get_opt_str("id").ok().flatten())
        .collect();
    let fav_map = if let Some(uid) = user_id {
        if !person_ids.is_empty() {
            let ph = person_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT item_id, is_favorite FROM user_data WHERE user_id = ? AND item_id IN ({})",
                ph
            );
            let mut vals: Vec<sea_orm::Value> = vec![uid.into()];
            for pid in &person_ids {
                vals.push(pid.as_str().into());
            }
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
        }
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

/// Batch-enrich episode items in a list with SeriesId/SeriesName/SeasonId/SeasonName.
pub async fn enrich_episode_list(db: &DatabaseConnection, items: Vec<MediaItem>) -> Vec<Value> {
    // Collect unique parent_ids (season IDs) for episodes
    let season_ids: Vec<&str> = items
        .iter()
        .filter(|i| i.item_type == "Episode")
        .map(|i| i.parent_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query: season_id -> (series_id, series_name, season_name)
    let mut season_map: HashMap<String, (String, String, String)> = HashMap::new();
    if !season_ids.is_empty() {
        let backend = db.get_database_backend();
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT s.id AS season_id, s.title AS season_name, s.parent_id AS series_id, ser.title AS series_name FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND ser.is_public = 1 WHERE s.id IN ({placeholders}) AND s.is_public = 1"
        );
        let values: Vec<sea_orm::Value> = season_ids.iter().map(|id| (*id).into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
        {
            for row in &rows {
                if let (Ok(sid), Ok(sname), Ok(serid), Ok(sername)) = (
                    row.get_str("season_id"),
                    row.get_str("season_name"),
                    row.get_str("series_id"),
                    row.get_str("series_name"),
                ) {
                    season_map.insert(sid, (serid, sername, sname));
                }
            }
        }
    }

    // Collect unique series IDs and batch-query their logo/backdrop image tags
    let series_ids: Vec<String> = season_map
        .values()
        .map(|(serid, _, _)| serid.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    // series_id -> (logo_etag, backdrop_etag)
    let mut logo_map: HashMap<String, (String, String)> = HashMap::new();
    if !series_ids.is_empty() {
        let backend = db.get_database_backend();
        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT item_id, image_type, etag FROM image_assets WHERE item_id IN ({placeholders}) AND image_type IN ('Logo', 'Backdrop')"
        );
        let values: Vec<sea_orm::Value> = series_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
        {
            for row in &rows {
                if let (Ok(item_id), Ok(img_type), Ok(etag)) = (
                    row.get_str("item_id"),
                    row.get_str("image_type"),
                    row.get_str("etag"),
                ) {
                    let entry = logo_map
                        .entry(item_id)
                        .or_insert((String::new(), String::new()));
                    if img_type == "Logo" {
                        entry.0 = etag;
                    } else if img_type == "Backdrop" {
                        entry.1 = etag;
                    }
                }
            }
        }
    }

    items
        .into_iter()
        .map(|item| {
            let mut val = item.to_jellyfin_json();
            if item.item_type == "Episode" {
                if let Some((series_id, series_name, season_name)) = season_map.get(&item.parent_id)
                {
                    val["SeriesId"] = json!(series_id);
                    val["SeriesName"] = json!(series_name);
                    val["SeasonId"] = json!(item.parent_id);
                    val["SeasonName"] = json!(season_name);
                    // Add parent logo/backdrop for playback UI
                    val["ParentLogoItemId"] = json!(series_id);
                    if let Some((logo_etag, _)) = logo_map.get(series_id) {
                        val["ParentLogoImageTag"] = json!(logo_etag);
                    }
                    val["ParentBackdropItemId"] = json!(series_id);
                    if let Some((_, backdrop_etag)) = logo_map.get(series_id) {
                        if !backdrop_etag.is_empty() {
                            val["ParentBackdropImageTags"] = json!([backdrop_etag]);
                        }
                    }
                }
            }
            strip_nulls(val)
        })
        .collect()
}

pub async fn enrich_resume_items(db: &DatabaseConnection, items: Vec<MediaItem>) -> Vec<Value> {
    use std::collections::HashMap;
    let backend = db.get_database_backend();

    // Collect parent_ids for Episode items to look up series info
    let parent_ids: Vec<&str> = items
        .iter()
        .filter(|i| i.item_type == "Episode")
        .map(|i| i.parent_id.as_str())
        .collect();

    // Batch lookup: parent_id -> (season_title, series_id, series_title)
    let mut season_map: HashMap<String, (String, String, String)> = HashMap::new();
    if !parent_ids.is_empty() {
        let placeholders = parent_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT s.id AS season_id, s.title AS season_title, ser.id AS series_id, ser.title AS series_title \
             FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND ser.is_public = 1 \
             WHERE s.id IN ({placeholders}) AND s.is_public = 1",
        );
        let vals: Vec<sea_orm::Value> = parent_ids.iter().map(|p| (*p).into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
            .await
        {
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
                if let Ok(sources) =
                    crate::jellyfin::playback::child_video_sources(db, &item.id, false).await
                {
                    if !sources.is_empty() {
                        source_map.insert(item.id.clone(), sources);
                    }
                }
            } else {
                // Non-folder items - build MediaSource directly from the item itself
                let stream_jsons = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
                    .await
                    .unwrap_or_default();
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

    items
        .into_iter()
        .map(|item| {
            let mut value = item.to_jellyfin_json();

            // Enrich Episode items with series/season info
            if item.item_type == "Episode" {
                if let Some((season_title, series_id, series_title)) =
                    season_map.get(&item.parent_id)
                {
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
                        if let Some(streams) = source.get("MediaStreams").and_then(Value::as_array)
                        {
                            all_streams.extend(streams.clone());
                        }
                    }
                    value["MediaStreams"] = Value::Array(all_streams);

                    // Get RunTimeTicks from first source if item doesn't have it
                    if item.runtime_ticks.is_none() {
                        if let Some(rt) = sources
                            .first()
                            .and_then(|s| s.get("RunTimeTicks"))
                            .and_then(Value::as_i64)
                            .filter(|v| *v > 0)
                        {
                            value["RunTimeTicks"] = json!(rt);
                        }
                    }
                }
            }

            // Ensure RunTimeTicks is set from item if available
            if item.runtime_ticks.is_some()
                && value.get("RunTimeTicks").and_then(Value::as_i64).is_none()
            {
                value["RunTimeTicks"] = json!(item.runtime_ticks);
            }

            // Calculate PlayedPercentage if not set
            if item.played_percentage.is_none() && item.playback_position_ticks > 0 {
                if let Some(rt) = value
                    .get("RunTimeTicks")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
                {
                    let pct = (item.playback_position_ticks as f64 / rt as f64 * 100.0).min(100.0);
                    if let Some(ud) = value.get_mut("UserData") {
                        ud["PlayedPercentage"] = json!(pct);
                    }
                }
            }

            strip_nulls(value)
        })
        .collect()
}
