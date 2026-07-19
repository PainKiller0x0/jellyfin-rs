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
        auth::{query_user_id_or_request, request_user_id_and_admin_or_default},
        common::{internal_error, strip_nulls},
        item_queries::find_library_as_item,
    },
    library::models::{MediaItem, media_source_json_with_streams},
};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

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
    let (request_user_id, is_admin) =
        request_user_id_and_admin_or_default(&state, &headers, &query).await;
    let user_id = if is_admin {
        query_user_id_or_request(&query, &request_user_id)
    } else {
        request_user_id
    };
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
            let is_favorite = db
                .query_one(crate::db::helpers::pg_statement(
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

    // Combine provider_ids and image_assets into one query
    let meta_rows = db
        .query_all(crate::db::helpers::pg_statement(
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
    crate::jellyfin::images::add_art_tag_fallback(&mut image_map);
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
        .query_all(crate::db::helpers::pg_statement(
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
                attach_media_sources(&mut value, sources);
            }
        }
    } else if item.item_type == "Episode" {
        let sources =
            crate::jellyfin::playback::episode_version_sources(db, &item, include_private_sources)
                .await?;
        if !sources.is_empty() {
            attach_media_sources(&mut value, sources);
        } else {
            let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
                .await
                .unwrap_or_default();
            attach_media_sources(
                &mut value,
                vec![media_source_json_with_streams(&item, streams)],
            );
        }
    } else if !item.is_folder {
        // For non-folder items (Episode/Video files), load media streams from DB
        let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
            .await
            .unwrap_or_default();
        if !streams.is_empty() {
            // Rebuild MediaSources with streams included
            let source = media_source_json_with_streams(&item, streams.clone());
            attach_media_sources(&mut value, vec![source]);
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

            let parent_ids = vec![season_id.clone(), series_id.clone()];
            let parent_image_tags =
                crate::jellyfin::item_queries::batch_item_image_tags(db, &parent_ids)
                    .await
                    .unwrap_or_default();
            apply_episode_parent_images(&mut value, &series_id, &season_id, &parent_image_tags);
        }
    }

    // Enrich Season items with series info
    if item.item_type == "Season" {
        let parent_visible = visible_media_item_sql("media_items");
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::pg_statement(
                &format!("SELECT id, title FROM media_items WHERE id = ? AND {parent_visible}"),
                vec![item.parent_id.clone().into()],
            ))
            .await
        {
            if let (Ok(id), Ok(title)) = (row.get_str("id"), row.get_str("title")) {
                value["SeriesName"] = json!(title);
                value["SeriesId"] = json!(id);
                let parent_ids = vec![id.clone()];
                let parent_image_tags =
                    crate::jellyfin::item_queries::batch_item_image_tags(db, &parent_ids)
                        .await
                        .unwrap_or_default();
                apply_season_parent_images(&mut value, &id, &parent_image_tags);
            }
        }
        // RecursiveItemCount for Season = episode count
        let episode_visible = visible_media_item_sql("media_items");
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::pg_statement(
                &format!("SELECT COUNT(DISTINCT (COALESCE(season_number, 0), COALESCE(episode_number, 0))) AS cnt FROM media_items WHERE parent_id = ? AND item_type = 'Episode' AND {episode_visible}"),
                vec![item.id.clone().into()],
            ))
            .await
        {
            let cnt = row.get_i64("cnt").unwrap_or(0);
            value["ChildCount"] = json!(cnt);
            value["RecursiveItemCount"] = json!(cnt);
            value["EpisodeCount"] = json!(cnt);
            // UnplayedItemCount = total - played episodes in this season
            if let Ok(Some(ud_row)) = db
                .query_one(crate::db::helpers::pg_statement(
                    &format!("SELECT COUNT(DISTINCT (COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id = ? AND mi.item_type = 'Episode' AND {} AND ud.user_id = ? AND ud.played = 1", visible_media_item_sql("mi")),
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
        let season_visible = visible_media_item_sql("media_items");
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::pg_statement(
                &format!("SELECT COUNT(*) AS cnt FROM media_items WHERE parent_id = ? AND item_type = 'Season' AND {season_visible}"),
                vec![item.id.clone().into()],
            ))
            .await
        {
            let season_cnt = row.get_i64("cnt").unwrap_or(0);
            value["ChildCount"] = json!(season_cnt);
            value["SeasonCount"] = json!(season_cnt);
        }
        // RecursiveItemCount = total episodes under all seasons of this series
        let episode_visible = visible_media_item_sql("media_items");
        let season_visible = visible_media_item_sql("s");
        if let Ok(Some(row)) = db
            .query_one(crate::db::helpers::pg_statement(
                &format!("SELECT COUNT(DISTINCT (parent_id, COALESCE(season_number, 0), COALESCE(episode_number, 0))) AS cnt FROM media_items WHERE item_type = 'Episode' AND {episode_visible} AND parent_id IN (SELECT s.id FROM media_items s WHERE s.parent_id = ? AND s.item_type = 'Season' AND {season_visible})"),
                vec![item.id.clone().into()],
            ))
            .await
        {
            let total_eps = row.get_i64("cnt").unwrap_or(0);
            value["RecursiveItemCount"] = json!(total_eps);
            value["RecursiveUnplayedItemCount"] = json!(total_eps); // will be overridden below
            value["EpisodeCount"] = json!(total_eps);
            // UnplayedItemCount = episodes not played by user
            if let Ok(Some(ud_row)) = db
                .query_one(crate::db::helpers::pg_statement(
                    &format!("SELECT COUNT(DISTINCT (mi.parent_id, COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.item_type = 'Episode' AND {} AND mi.parent_id IN (SELECT s.id FROM media_items s WHERE s.parent_id = ? AND s.item_type = 'Season' AND {}) AND ud.user_id = ? AND ud.played = 1", visible_media_item_sql("mi"), visible_media_item_sql("s")),
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

fn attach_media_sources(value: &mut Value, sources: Vec<Value>) {
    if sources.is_empty() {
        return;
    }

    let source_count = sources.len() as i64;
    let mut all_streams = Vec::new();
    for source in &sources {
        if let Some(streams) = source.get("MediaStreams").and_then(Value::as_array) {
            all_streams.extend(streams.clone());
        }
    }
    let has_subtitles = all_streams.iter().any(|stream| {
        stream
            .get("Type")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("Subtitle"))
    });

    value["MediaSources"] = Value::Array(sources);
    value["MediaSourceCount"] = json!(source_count);
    value["PartCount"] = json!(source_count);
    value["EnableMediaSourceDisplay"] = json!(source_count > 1);
    if !all_streams.is_empty() {
        value["MediaStreams"] = Value::Array(all_streams);
        value["HasSubtitles"] = json!(has_subtitles);
    }
}

fn apply_episode_parent_images(
    value: &mut Value,
    series_id: &str,
    season_id: &str,
    parent_image_tags: &HashMap<String, Value>,
) {
    if let Some(series_primary) = image_tag(parent_image_tags, series_id, "Primary") {
        value["SeriesPrimaryImageTag"] = json!(series_primary);
    }

    if let Some(season_primary) = image_tag(parent_image_tags, season_id, "Primary") {
        value["ParentPrimaryImageItemId"] = json!(season_id);
        value["ParentPrimaryImageTag"] = json!(season_primary);
    } else if let Some(series_primary) = image_tag(parent_image_tags, series_id, "Primary") {
        value["ParentPrimaryImageItemId"] = json!(series_id);
        value["ParentPrimaryImageTag"] = json!(series_primary);
    }

    apply_parent_single_image(
        value,
        parent_image_tags,
        "Logo",
        "ParentLogoItemId",
        "ParentLogoImageTag",
        &[season_id, series_id],
    );
    apply_parent_single_image(
        value,
        parent_image_tags,
        "Art",
        "ParentArtItemId",
        "ParentArtImageTag",
        &[season_id, series_id],
    );
    apply_parent_single_image(
        value,
        parent_image_tags,
        "Thumb",
        "ParentThumbItemId",
        "ParentThumbImageTag",
        &[season_id, series_id],
    );
    apply_parent_backdrop(value, parent_image_tags, &[season_id, series_id]);
}

fn apply_season_parent_images(
    value: &mut Value,
    series_id: &str,
    parent_image_tags: &HashMap<String, Value>,
) {
    if let Some(series_primary) = image_tag(parent_image_tags, series_id, "Primary") {
        value["SeriesPrimaryImageTag"] = json!(series_primary);
        value["ParentPrimaryImageItemId"] = json!(series_id);
        value["ParentPrimaryImageTag"] = json!(series_primary);
    }
    apply_parent_single_image(
        value,
        parent_image_tags,
        "Logo",
        "ParentLogoItemId",
        "ParentLogoImageTag",
        &[series_id],
    );
    apply_parent_single_image(
        value,
        parent_image_tags,
        "Art",
        "ParentArtItemId",
        "ParentArtImageTag",
        &[series_id],
    );
    apply_parent_single_image(
        value,
        parent_image_tags,
        "Thumb",
        "ParentThumbItemId",
        "ParentThumbImageTag",
        &[series_id],
    );
    apply_parent_backdrop(value, parent_image_tags, &[series_id]);
}

fn apply_parent_single_image(
    value: &mut Value,
    parent_image_tags: &HashMap<String, Value>,
    image_type: &str,
    item_id_field: &str,
    tag_field: &str,
    parent_ids: &[&str],
) {
    for parent_id in parent_ids {
        if let Some(tag) = image_tag(parent_image_tags, parent_id, image_type) {
            value[item_id_field] = json!(parent_id);
            value[tag_field] = json!(tag);
            break;
        }
    }
}

fn apply_parent_backdrop(
    value: &mut Value,
    parent_image_tags: &HashMap<String, Value>,
    parent_ids: &[&str],
) {
    for parent_id in parent_ids {
        if let Some(tag) = image_tag(parent_image_tags, parent_id, "Backdrop") {
            value["ParentBackdropItemId"] = json!(parent_id);
            value["ParentBackdropImageTags"] = json!([tag]);
            break;
        }
    }
}

fn image_tag<'a>(
    image_tags: &'a HashMap<String, Value>,
    item_id: &str,
    image_type: &str,
) -> Option<&'a str> {
    image_tags
        .get(item_id)
        .and_then(Value::as_object)
        .and_then(|tags| tags.get(image_type))
        .and_then(Value::as_str)
        .filter(|tag| !tag.is_empty())
}

/// Get series name, series ID, season name, season ID for an episode's parent
async fn get_episode_parent_info(
    db: &DatabaseConnection,
    season_id: &str,
) -> Option<(String, String, String, String)> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            &format!("SELECT s.title AS season_title, s.parent_id AS series_id, ser.title AS series_title FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND {} WHERE s.id = ? AND {}", visible_media_item_sql("ser"), visible_media_item_sql("s")),
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
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![item_id.into()]))
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
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
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
                .query_all(crate::db::helpers::pg_statement(&sql, vals))
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
                    "ItemId": id,
                    "Key": id,
                    "IsFavorite": is_favorite,
                    "Played": false,
                    "PlayCount": 0,
                    "PlaybackPositionTicks": 0,
                    "PlayedPercentage": null,
                    "Rating": null,
                    "LastPlayedDate": null,
                    "Likes": null,
                    "UnplayedItemCount": null,
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

/// Batch-enrich episode, season, and series list items with Jellyfin-compatible
/// parent links, counts, and inherited image tags.
pub async fn enrich_episode_list(
    db: &DatabaseConnection,
    user_id: &str,
    items: Vec<MediaItem>,
) -> Vec<Value> {
    // Collect unique season IDs for episode parent lookups and season list enrichment.
    let season_ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i.item_type.as_str() {
            "Episode" => Some(i.parent_id.as_str()),
            "Season" => Some(i.id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query: season_id -> (series_id, series_name, season_name)
    let mut season_map: HashMap<String, (String, String, String)> = HashMap::new();
    if !season_ids.is_empty() {
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let season_visible = visible_media_item_sql("s");
        let series_visible = visible_media_item_sql("ser");
        let sql = format!(
            "SELECT s.id AS season_id, s.title AS season_name, s.parent_id AS series_id, ser.title AS series_name FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND {series_visible} WHERE s.id IN ({placeholders}) AND {season_visible}"
        );
        let values: Vec<sea_orm::Value> = season_ids.iter().map(|id| (*id).into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
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

    // Collect unique series IDs and batch-query inherited image tags.
    let mut series_id_set = season_map
        .values()
        .map(|(serid, _, _)| serid.clone())
        .collect::<std::collections::HashSet<_>>();
    series_id_set.extend(
        items
            .iter()
            .filter(|item| item.item_type == "Series")
            .map(|item| item.id.clone()),
    );
    let series_ids: Vec<String> = series_id_set.into_iter().collect();
    let mut parent_image_ids = series_ids.clone();
    parent_image_ids.extend(season_ids.iter().map(|id| (*id).to_string()));
    parent_image_ids.sort();
    parent_image_ids.dedup();
    let parent_image_tags =
        crate::jellyfin::item_queries::batch_item_image_tags(db, &parent_image_ids)
            .await
            .unwrap_or_default();

    let item_ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
    let provider_map = crate::jellyfin::item_queries::batch_item_provider_ids(db, &item_ids)
        .await
        .unwrap_or_default();

    let mut season_episode_count_map: HashMap<String, i64> = HashMap::new();
    let mut season_played_episode_count_map: HashMap<String, i64> = HashMap::new();
    if !season_ids.is_empty() {
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let visible = visible_media_item_sql("media_items");
        let sql = format!(
            "SELECT parent_id, COUNT(DISTINCT (COALESCE(season_number, 0), COALESCE(episode_number, 0))) AS cnt FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Episode' AND {visible} GROUP BY parent_id"
        );
        let values: Vec<sea_orm::Value> = season_ids.iter().map(|id| (*id).into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(parent_id), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    season_episode_count_map.insert(parent_id, cnt);
                }
            }
        }

        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let visible = visible_media_item_sql("mi");
        let sql = format!(
            "SELECT mi.parent_id, COUNT(DISTINCT (COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id IN ({placeholders}) AND mi.item_type = 'Episode' AND {visible} AND ud.user_id = ? AND ud.played = 1 GROUP BY mi.parent_id"
        );
        let mut values: Vec<sea_orm::Value> = season_ids.iter().map(|id| (*id).into()).collect();
        values.push(user_id.into());
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(parent_id), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    season_played_episode_count_map.insert(parent_id, cnt);
                }
            }
        }
    }

    let mut series_season_count_map: HashMap<String, i64> = HashMap::new();
    let mut series_episode_count_map: HashMap<String, i64> = HashMap::new();
    let mut series_played_episode_count_map: HashMap<String, i64> = HashMap::new();
    if !series_ids.is_empty() {
        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let season_visible = visible_media_item_sql("media_items");
        let sql = format!(
            "SELECT parent_id AS series_id, COUNT(*) AS cnt FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Season' AND {season_visible} GROUP BY parent_id"
        );
        let values: Vec<sea_orm::Value> = series_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(series_id), Ok(cnt)) = (row.get_str("series_id"), row.get_i64("cnt")) {
                    series_season_count_map.insert(series_id, cnt);
                }
            }
        }

        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let episode_visible = visible_media_item_sql("episode");
        let season_visible = visible_media_item_sql("season");
        let sql = format!(
            "SELECT season.parent_id AS series_id, COUNT(DISTINCT (episode.parent_id, COALESCE(episode.season_number, 0), COALESCE(episode.episode_number, 0))) AS cnt FROM media_items episode JOIN media_items season ON season.id = episode.parent_id WHERE season.parent_id IN ({placeholders}) AND season.item_type = 'Season' AND episode.item_type = 'Episode' AND {episode_visible} AND {season_visible} GROUP BY season.parent_id"
        );
        let values: Vec<sea_orm::Value> = series_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(series_id), Ok(cnt)) = (row.get_str("series_id"), row.get_i64("cnt")) {
                    series_episode_count_map.insert(series_id, cnt);
                }
            }
        }

        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let episode_visible = visible_media_item_sql("episode");
        let season_visible = visible_media_item_sql("season");
        let sql = format!(
            "SELECT season.parent_id AS series_id, COUNT(DISTINCT (episode.parent_id, COALESCE(episode.season_number, 0), COALESCE(episode.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items episode ON episode.id = ud.item_id JOIN media_items season ON season.id = episode.parent_id WHERE season.parent_id IN ({placeholders}) AND season.item_type = 'Season' AND episode.item_type = 'Episode' AND {episode_visible} AND {season_visible} AND ud.user_id = ? AND ud.played = 1 GROUP BY season.parent_id"
        );
        let mut values: Vec<sea_orm::Value> =
            series_ids.iter().map(|id| id.as_str().into()).collect();
        values.push(user_id.into());
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(series_id), Ok(cnt)) = (row.get_str("series_id"), row.get_i64("cnt")) {
                    series_played_episode_count_map.insert(series_id, cnt);
                }
            }
        }
    }

    let episode_source_map =
        crate::jellyfin::playback::batch_episode_version_sources(db, &items, false, false)
            .await
            .unwrap_or_default();

    let mut enriched_items = Vec::with_capacity(items.len());
    for item in items {
        let mut val = item.to_jellyfin_json();
        if let Some(provider_ids) = provider_map.get(&item.id) {
            val["ProviderIds"] = provider_ids.clone();
        }
        if item.item_type == "Episode" {
            if let Some((series_id, series_name, season_name)) = season_map.get(&item.parent_id) {
                val["SeriesId"] = json!(series_id);
                val["SeriesName"] = json!(series_name);
                val["SeasonId"] = json!(item.parent_id);
                val["SeasonName"] = json!(season_name);
                apply_episode_parent_images(
                    &mut val,
                    series_id,
                    &item.parent_id,
                    &parent_image_tags,
                );
            }
            if let Some(sources) = episode_source_map.get(&item.id) {
                attach_media_sources(&mut val, sources.clone());
            }
        } else if item.item_type == "Season" {
            if let Some((series_id, series_name, _)) = season_map.get(&item.id) {
                val["SeriesId"] = json!(series_id);
                val["SeriesName"] = json!(series_name);
                apply_season_parent_images(&mut val, series_id, &parent_image_tags);
            }
            let episode_count = season_episode_count_map.get(&item.id).copied().unwrap_or(0);
            let played_count = season_played_episode_count_map
                .get(&item.id)
                .copied()
                .unwrap_or(0);
            let unplayed_count = (episode_count - played_count).max(0);
            val["ChildCount"] = json!(episode_count);
            val["RecursiveItemCount"] = json!(episode_count);
            val["EpisodeCount"] = json!(episode_count);
            val["UserData"]["UnplayedItemCount"] = json!(unplayed_count);
        } else if item.item_type == "Series" {
            let season_count = series_season_count_map.get(&item.id).copied().unwrap_or(0);
            let episode_count = series_episode_count_map.get(&item.id).copied().unwrap_or(0);
            let played_count = series_played_episode_count_map
                .get(&item.id)
                .copied()
                .unwrap_or(0);
            let unplayed_count = (episode_count - played_count).max(0);
            val["ChildCount"] = json!(season_count);
            val["SeasonCount"] = json!(season_count);
            val["RecursiveItemCount"] = json!(episode_count);
            val["RecursiveUnplayedItemCount"] = json!(unplayed_count);
            val["EpisodeCount"] = json!(episode_count);
            val["UserData"]["UnplayedItemCount"] = json!(unplayed_count);
        }
        enriched_items.push(strip_nulls(val));
    }
    enriched_items
}

pub async fn enrich_resume_items(db: &DatabaseConnection, items: Vec<MediaItem>) -> Vec<Value> {
    use std::collections::HashMap;

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
        let season_visible = visible_media_item_sql("s");
        let series_visible = visible_media_item_sql("ser");
        let sql = format!(
            "SELECT s.id AS season_id, s.title AS season_title, ser.id AS series_id, ser.title AS series_title \
             FROM media_items s LEFT JOIN media_items ser ON ser.id = s.parent_id AND {series_visible} \
             WHERE s.id IN ({placeholders}) AND {season_visible}",
        );
        let vals: Vec<sea_orm::Value> = parent_ids.iter().map(|p| (*p).into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::pg_statement(&sql, vals))
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
                    attach_media_sources(&mut value, sources.clone());

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

#[cfg(test)]
mod tests {
    use super::{
        attach_media_sources, enrich_episode_list, get_episode_parent_info,
        item_json_with_provider_ids,
    };
    use sea_orm::{ConnectionTrait, DatabaseConnection};
    use serde_json::json;

    #[test]
    fn attach_media_sources_exposes_subtitle_streams_on_item_detail() {
        let mut value = json!({
            "Id": "episode",
            "Type": "Episode",
            "HasSubtitles": null,
            "MediaStreams": null,
        });
        let sources = vec![json!({
            "Id": "version-a",
            "MediaStreams": [
                { "Index": 0, "Type": "Video", "Codec": "h264" },
                { "Index": 1, "Type": "Audio", "Codec": "aac" },
                {
                    "Index": 2,
                    "Type": "Subtitle",
                    "Codec": "ass",
                    "IsExternal": true,
                    "DeliveryMethod": "External",
                    "DeliveryUrl": "/Videos/episode/episode/Subtitles/2/Stream.ass"
                }
            ]
        })];

        attach_media_sources(&mut value, sources);

        assert_eq!(value["HasSubtitles"], true);
        let streams = value["MediaStreams"].as_array().unwrap();
        assert!(streams.iter().any(|stream| stream["Type"] == "Subtitle"));
        assert_eq!(value["MediaSourceCount"], 1);
        assert_eq!(value["EnableMediaSourceDisplay"], false);
    }

    #[test]
    fn attach_media_sources_marks_items_without_subtitles() {
        let mut value = json!({
            "Id": "episode",
            "Type": "Episode",
            "HasSubtitles": null,
            "MediaStreams": null,
        });

        attach_media_sources(
            &mut value,
            vec![json!({
                "Id": "version-a",
                "MediaStreams": [
                    { "Index": 0, "Type": "Video", "Codec": "h264" },
                    { "Index": 1, "Type": "Audio", "Codec": "aac" }
                ]
            })],
        );

        assert_eq!(value["HasSubtitles"], false);
        assert_eq!(
            value["MediaStreams"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|stream| stream["Type"] == "Subtitle")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn item_detail_counts_hide_private_parent_children() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["tv".into(), "TV".into(), "tvshows".into()],
        ))
        .await
        .unwrap();
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(
            &db,
            "season-public",
            "S1",
            "series",
            "Season",
            1,
            1,
            None,
            None,
        )
        .await;
        insert_item(
            &db,
            "season-private",
            "S2",
            "series",
            "Season",
            1,
            0,
            None,
            None,
        )
        .await;
        insert_item(
            &db,
            "episode-public",
            "E1",
            "season-public",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        insert_item(
            &db,
            "episode-public-duplicate",
            "E1 4K",
            "season-public",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        insert_item(
            &db,
            "episode-under-private-season",
            "E2",
            "season-private",
            "Episode",
            0,
            1,
            Some(2),
            Some(1),
        )
        .await;

        let series = crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "series")
            .await
            .unwrap()
            .unwrap();
        let series_json = item_json_with_provider_ids(&db, "u1", series, true)
            .await
            .unwrap();
        assert_eq!(series_json["SeasonCount"], 1);
        assert_eq!(series_json["ChildCount"], 1);
        assert_eq!(series_json["EpisodeCount"], 1);
        assert_eq!(series_json["RecursiveItemCount"], 1);

        let season =
            crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "season-public")
                .await
                .unwrap()
                .unwrap();
        let season_json = item_json_with_provider_ids(&db, "u1", season, true)
            .await
            .unwrap();
        assert_eq!(season_json["SeriesId"], "series");
        assert_eq!(season_json["ChildCount"], 1);
        assert_eq!(season_json["EpisodeCount"], 1);
        assert_eq!(season_json["RecursiveItemCount"], 1);

        let hidden_episode = crate::jellyfin::item_queries::find_media_item_for_admin(
            &db,
            "u1",
            "episode-under-private-season",
        )
        .await
        .unwrap()
        .unwrap();
        let hidden_json = item_json_with_provider_ids(&db, "u1", hidden_episode.clone(), true)
            .await
            .unwrap();
        assert!(hidden_json.get("SeriesId").is_none());
        assert!(
            get_episode_parent_info(&db, "season-private")
                .await
                .is_none()
        );

        let enriched = enrich_episode_list(&db, "u1", vec![hidden_episode]).await;
        assert!(enriched[0].get("SeriesId").is_none());
    }

    async fn insert_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        season_number: Option<i64>,
        episode_number: Option<i64>,
    ) {
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, season_number, episode_number, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'tv', ?, ?, ?, ?, ?, ?, 1, 1, 1)",
            vec![
                id.into(),
                title.into(),
                id.into(),
                parent_id.into(),
                item_type.into(),
                is_folder.into(),
                is_public.into(),
                season_number.into(),
                episode_number.into(),
            ],
        ))
        .await
        .unwrap();
    }
}
