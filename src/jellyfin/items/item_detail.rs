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
            let source = media_source_json_with_streams(&item, streams);
            attach_media_sources(&mut value, vec![source]);
        }
    }

    // Enrich Episode items with series/season info
    if item.item_type == "Episode" {
        if let Some(parent_info) = get_episode_parent_info(db, &item.parent_id).await {
            let mut parent_ids = vec![parent_info.series_id.clone()];
            if let Some(season_id) = &parent_info.season_id {
                parent_ids.push(season_id.clone());
            }
            let parent_image_tags =
                crate::jellyfin::item_queries::batch_item_image_tags(db, &parent_ids)
                    .await
                    .unwrap_or_default();
            apply_episode_parent_info(
                &mut value,
                &item.parent_id,
                item.season_number,
                &parent_info,
                &parent_image_tags,
            );
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

#[derive(Clone, Debug)]
struct EpisodeParentInfo {
    series_id: String,
    series_name: String,
    season_id: Option<String>,
    season_name: Option<String>,
}

/// Get series and season info for an episode parent. The parent may be a real
/// Season, a Series with episodes directly inside it, or an ordinary grouping
/// folder between the Series and the episode files.
async fn get_episode_parent_info(
    db: &DatabaseConnection,
    parent_id: &str,
) -> Option<EpisodeParentInfo> {
    batch_episode_parent_info(db, &[parent_id])
        .await
        .remove(parent_id)
}

async fn batch_episode_parent_info(
    db: &DatabaseConnection,
    parent_ids: &[&str],
) -> HashMap<String, EpisodeParentInfo> {
    let mut unique_parent_ids = parent_ids
        .iter()
        .copied()
        .filter(|id| !id.is_empty())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    unique_parent_ids.sort_unstable();
    if unique_parent_ids.is_empty() {
        return HashMap::new();
    }

    let placeholders = unique_parent_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let root_visible = visible_media_item_sql("mi");
    let ancestor_visible = visible_media_item_sql("ancestor_parent");
    let sql = format!(
        r#"WITH RECURSIVE ancestors(root_id, id, title, item_type, parent_id, depth) AS (
               SELECT mi.id, mi.id, mi.title, mi.item_type, mi.parent_id, 0
               FROM media_items mi
               WHERE mi.id IN ({placeholders}) AND {root_visible}
               UNION ALL
               SELECT ancestors.root_id, ancestor_parent.id, ancestor_parent.title, ancestor_parent.item_type, ancestor_parent.parent_id, ancestors.depth + 1
               FROM media_items ancestor_parent
               JOIN ancestors ON ancestor_parent.id = ancestors.parent_id
               WHERE ancestors.depth < 8 AND {ancestor_visible}
           )
           SELECT root_id, id, title, item_type, depth
           FROM ancestors
           ORDER BY root_id ASC, depth ASC"#
    );
    let values = unique_parent_ids
        .iter()
        .map(|id| (*id).into())
        .collect::<Vec<sea_orm::Value>>();
    let Ok(rows) = db
        .query_all(crate::db::helpers::pg_statement(&sql, values))
        .await
    else {
        return HashMap::new();
    };

    #[derive(Clone)]
    struct Ancestor {
        id: String,
        title: String,
        item_type: String,
        depth: i64,
    }

    let mut grouped: HashMap<String, Vec<Ancestor>> = HashMap::new();
    for row in &rows {
        let (Ok(root_id), Ok(id), Ok(title), Ok(item_type), Ok(depth)) = (
            row.get_str("root_id"),
            row.get_str("id"),
            row.get_str("title"),
            row.get_str("item_type"),
            row.get_i64("depth"),
        ) else {
            continue;
        };
        grouped.entry(root_id).or_default().push(Ancestor {
            id,
            title,
            item_type,
            depth,
        });
    }

    let mut result = HashMap::new();
    for (root_id, ancestors) in grouped {
        let Some(series) = ancestors
            .iter()
            .filter(|ancestor| ancestor.item_type == "Series")
            .min_by_key(|ancestor| ancestor.depth)
        else {
            continue;
        };
        let season = ancestors
            .iter()
            .filter(|ancestor| ancestor.item_type == "Season" && ancestor.depth < series.depth)
            .min_by_key(|ancestor| ancestor.depth);
        result.insert(
            root_id,
            EpisodeParentInfo {
                series_id: series.id.clone(),
                series_name: series.title.clone(),
                season_id: season.map(|ancestor| ancestor.id.clone()),
                season_name: season.map(|ancestor| ancestor.title.clone()),
            },
        );
    }
    result
}

fn apply_episode_parent_info(
    value: &mut Value,
    item_parent_id: &str,
    season_number: Option<i64>,
    info: &EpisodeParentInfo,
    parent_image_tags: &HashMap<String, Value>,
) {
    value["SeriesName"] = json!(info.series_name.clone());
    value["SeriesId"] = json!(info.series_id.clone());

    let season_name = info
        .season_name
        .clone()
        .or_else(|| season_number.map(display_season_name));
    if let Some(season_name) = season_name {
        value["SeasonName"] = json!(season_name);
        value["SeasonId"] = json!(info.season_id.as_deref().unwrap_or(item_parent_id));
    }

    let season_image_id = info.season_id.as_deref().unwrap_or(&info.series_id);
    apply_episode_parent_images(value, &info.series_id, season_image_id, parent_image_tags);
}

fn display_season_name(season_number: i64) -> String {
    if season_number == 0 {
        "Specials".to_string()
    } else {
        format!("Season {season_number}")
    }
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
    // Collect unique episode parent IDs and season IDs for parent lookups.
    let parent_lookup_ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i.item_type.as_str() {
            "Episode" => Some(i.parent_id.as_str()),
            "Season" => Some(i.id.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let parent_info_map = batch_episode_parent_info(db, &parent_lookup_ids).await;

    // Collect unique series IDs and batch-query inherited image tags.
    let mut series_id_set = parent_info_map
        .values()
        .map(|info| info.series_id.clone())
        .collect::<std::collections::HashSet<_>>();
    series_id_set.extend(
        items
            .iter()
            .filter(|item| item.item_type == "Series")
            .map(|item| item.id.clone()),
    );
    let series_ids: Vec<String> = series_id_set.into_iter().collect();
    let mut parent_image_ids = series_ids.clone();
    parent_image_ids.extend(
        parent_info_map
            .values()
            .filter_map(|info| info.season_id.clone()),
    );
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
    if !parent_lookup_ids.is_empty() {
        let placeholders = parent_lookup_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let visible = visible_media_item_sql("media_items");
        let sql = format!(
            "SELECT parent_id, COUNT(DISTINCT (COALESCE(season_number, 0), COALESCE(episode_number, 0))) AS cnt FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Episode' AND {visible} GROUP BY parent_id"
        );
        let values: Vec<sea_orm::Value> = parent_lookup_ids.iter().map(|id| (*id).into()).collect();
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

        let placeholders = parent_lookup_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let visible = visible_media_item_sql("mi");
        let sql = format!(
            "SELECT mi.parent_id, COUNT(DISTINCT (COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id IN ({placeholders}) AND mi.item_type = 'Episode' AND {visible} AND ud.user_id = ? AND ud.played = 1 GROUP BY mi.parent_id"
        );
        let mut values: Vec<sea_orm::Value> =
            parent_lookup_ids.iter().map(|id| (*id).into()).collect();
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

    let media_source_map = list_media_source_map(db, &items).await;

    let mut enriched_items = Vec::with_capacity(items.len());
    for item in items {
        let mut val = item.to_jellyfin_json();
        if let Some(provider_ids) = provider_map.get(&item.id) {
            val["ProviderIds"] = provider_ids.clone();
        }
        if item.item_type == "Episode" {
            if let Some(parent_info) = parent_info_map.get(&item.parent_id) {
                apply_episode_parent_info(
                    &mut val,
                    &item.parent_id,
                    item.season_number,
                    parent_info,
                    &parent_image_tags,
                );
            }
        } else if item.item_type == "Season" {
            if let Some(parent_info) = parent_info_map.get(&item.id) {
                val["SeriesId"] = json!(parent_info.series_id.clone());
                val["SeriesName"] = json!(parent_info.series_name.clone());
                apply_season_parent_images(&mut val, &parent_info.series_id, &parent_image_tags);
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
        if let Some(sources) = media_source_map.get(&item.id) {
            attach_media_sources(&mut val, sources.clone());
        }
        enriched_items.push(strip_nulls(val));
    }
    enriched_items
}

async fn list_media_source_map(
    db: &DatabaseConnection,
    items: &[MediaItem],
) -> HashMap<String, Vec<Value>> {
    let mut source_map = HashMap::new();

    let folder_media_ids = items
        .iter()
        .filter(|item| item.is_folder && matches!(item.item_type.as_str(), "Movie" | "Episode"))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if !folder_media_ids.is_empty() {
        source_map.extend(
            crate::jellyfin::playback::batch_child_video_sources(
                db,
                &folder_media_ids,
                false,
                true,
            )
            .await
            .unwrap_or_default(),
        );
    }

    source_map.extend(
        crate::jellyfin::playback::batch_episode_version_sources(db, items, false, true)
            .await
            .unwrap_or_default(),
    );

    let direct_items = items
        .iter()
        .filter(|item| is_direct_playable_list_item(item))
        .filter(|item| !source_map.contains_key(&item.id))
        .collect::<Vec<_>>();
    if direct_items.is_empty() {
        return source_map;
    }

    let direct_ids = direct_items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let stream_map = crate::jellyfin::playback::media_streams_for_items(db, &direct_ids)
        .await
        .unwrap_or_default();
    for item in direct_items {
        let streams = stream_map.get(&item.id).cloned().unwrap_or_default();
        source_map.insert(
            item.id.clone(),
            vec![media_source_json_with_streams(item, streams)],
        );
    }

    source_map
}

fn is_direct_playable_list_item(item: &MediaItem) -> bool {
    !item.is_folder
        && matches!(
            item.item_type.as_str(),
            "Movie" | "Episode" | "Video" | "Audio" | "Trailer"
        )
}

pub async fn enrich_resume_items(db: &DatabaseConnection, items: Vec<MediaItem>) -> Vec<Value> {
    use std::collections::HashMap;

    // Collect parent_ids for Episode items to look up series info
    let parent_ids: Vec<&str> = items
        .iter()
        .filter(|i| i.item_type == "Episode")
        .map(|i| i.parent_id.as_str())
        .collect();

    let parent_info_map = batch_episode_parent_info(db, &parent_ids).await;

    let mut source_map: HashMap<String, Vec<Value>> = HashMap::new();
    if !items.is_empty() {
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
                let source = media_source_json_with_streams(item, stream_jsons);
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
                if let Some(parent_info) = parent_info_map.get(&item.parent_id) {
                    value["SeriesName"] = json!(parent_info.series_name.clone());
                    value["SeriesId"] = json!(parent_info.series_id.clone());
                    let season_name = parent_info
                        .season_name
                        .clone()
                        .or_else(|| item.season_number.map(display_season_name));
                    if let Some(season_name) = season_name {
                        value["SeasonName"] = json!(season_name);
                        value["SeasonId"] =
                            json!(parent_info.season_id.as_deref().unwrap_or(&item.parent_id));
                    }
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
    use crate::entities::{
        libraries::{self, Entity as Libraries},
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use sea_orm::{DatabaseConnection, EntityTrait, Set};
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
        insert_library(&db, "tv", "TV", "tvshows").await;
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

        insert_item(
            &db,
            "direct-series",
            "Direct Series",
            "tv",
            "Series",
            1,
            1,
            None,
            None,
        )
        .await;
        insert_item(
            &db,
            "direct-episode",
            "Direct Episode",
            "direct-series",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        let direct_episode =
            crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "direct-episode")
                .await
                .unwrap()
                .unwrap();
        let direct_json = item_json_with_provider_ids(&db, "u1", direct_episode.clone(), true)
            .await
            .unwrap();
        assert_eq!(direct_json["SeriesId"], "direct-series");
        assert_eq!(direct_json["SeriesName"], "Direct Series");
        assert_eq!(direct_json["SeasonId"], "direct-series");
        assert_eq!(direct_json["SeasonName"], "Season 1");

        let direct_enriched = enrich_episode_list(&db, "u1", vec![direct_episode]).await;
        assert_eq!(direct_enriched[0]["SeriesId"], "direct-series");
        assert_eq!(direct_enriched[0]["SeriesName"], "Direct Series");
        assert_eq!(direct_enriched[0]["SeasonId"], "direct-series");
        assert_eq!(direct_enriched[0]["SeasonName"], "Season 1");

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

    #[tokio::test]
    async fn media_library_list_exposes_external_subtitle_streams() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, name, collection_type) in [("movies", "Movies", "movies"), ("tv", "TV", "tvshows")]
        {
            insert_library(&db, id, name, collection_type).await;
        }

        insert_item_with_path(
            &db,
            "movie",
            "Movie",
            "/tmp/movie",
            "movies",
            "movies",
            "Movie",
            1,
            1,
            None,
            None,
            None,
        )
        .await;
        insert_item_with_path(
            &db,
            "movie-video",
            "Movie.mkv",
            "/tmp/movie/Movie.mkv",
            "movies",
            "movie",
            "Video",
            0,
            1,
            Some("mkv"),
            None,
            None,
        )
        .await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(
            &db,
            "season",
            "Season 1",
            "series",
            "Season",
            1,
            1,
            Some(1),
            None,
        )
        .await;
        insert_item(
            &db,
            "episode",
            "Episode 1",
            "season",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;

        for (id, item_id, index, codec, path) in [
            (
                "movie-sub",
                "movie-video",
                2_i64,
                "srt",
                "/tmp/movie/Movie.zh.srt",
            ),
            (
                "episode-sub",
                "episode",
                3_i64,
                "ass",
                "/tmp/show/Season 1/Episode.zh.ass",
            ),
        ] {
            insert_external_subtitle(&db, id, item_id, index, codec, path).await;
        }

        let movie = crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "movie")
            .await
            .unwrap()
            .unwrap();
        let episode =
            crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "episode")
                .await
                .unwrap()
                .unwrap();
        let enriched = enrich_episode_list(&db, "u1", vec![movie, episode]).await;

        for item in enriched {
            assert_eq!(item["HasSubtitles"], true);
            let streams = item["MediaStreams"].as_array().unwrap();
            let subtitle = streams
                .iter()
                .find(|stream| stream["Type"] == "Subtitle" && stream["IsExternal"] == true)
                .unwrap();
            assert_eq!(subtitle["DeliveryMethod"], "External");
            assert!(
                subtitle["DeliveryUrl"]
                    .as_str()
                    .unwrap()
                    .contains("/Subtitles/")
            );
            assert!(
                item["MediaSources"][0]["MediaStreams"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|stream| stream["Type"] == "Subtitle" && stream["IsExternal"] == true)
            );
        }
    }

    async fn insert_library(db: &DatabaseConnection, id: &str, name: &str, collection_type: &str) {
        Libraries::insert(libraries::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            collection_type: Set(collection_type.to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
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
        insert_item_with_path(
            db,
            id,
            title,
            id,
            "tv",
            parent_id,
            item_type,
            is_folder,
            is_public,
            None,
            season_number,
            episode_number,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_item_with_path(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        container: Option<&str>,
        season_number: Option<i64>,
        episode_number: Option<i64>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(library_id.to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(is_public),
            container: Set(container.map(ToString::to_string)),
            season_number: Set(season_number),
            episode_number: Set(episode_number),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_external_subtitle(
        db: &DatabaseConnection,
        id: &str,
        item_id: &str,
        stream_index: i64,
        codec: &str,
        path: &str,
    ) {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            stream_index: Set(stream_index),
            stream_type: Set("Subtitle".to_string()),
            codec: Set(Some(codec.to_string())),
            language: Set(Some("zh-CN".to_string())),
            title: Set(Some(format!("{item_id}.zh.{codec}"))),
            path: Set(Some(path.to_string())),
            is_interlaced: Set(0),
            is_default: Set(0),
            is_forced: Set(0),
            is_hearing_impaired: Set(0),
            is_external: Set(1),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}
