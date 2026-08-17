use std::{collections::HashMap, path::Path as FsPath, sync::Arc};

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
                .query_one_raw(crate::db::helpers::pg_statement(
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
        .query_all_raw(crate::db::helpers::pg_statement(
            "SELECT 'provider' AS src, provider AS key, provider_item_id AS val, NULL AS etag FROM provider_ids WHERE item_id = ? UNION ALL SELECT 'image' AS src, image_type AS key, NULL AS val, etag FROM image_assets WHERE item_id = ? AND image_type <> 'Chapter'",
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
    if let Some(primary_image_tag) = value
        .get("ImageTags")
        .and_then(|tags| tags.get("Primary"))
        .and_then(Value::as_str)
        .filter(|tag| !tag.is_empty())
    {
        value["PrimaryImageTag"] = json!(primary_image_tag);
    }

    let mut relation_map =
        batch_item_relations(db, user_id, std::slice::from_ref(&item.id)).await?;
    apply_item_relations(
        &mut value,
        relation_map.remove(&item.id).unwrap_or_default(),
    );
    let (local_trailer_count, special_feature_count) = extra_counts_for_item(db, &item).await?;
    value["LocalTrailerCount"] = json!(local_trailer_count);
    value["SpecialFeatureCount"] = json!(special_feature_count);
    if let Some(part_count) = batch_additional_part_counts(db, std::slice::from_ref(&item))
        .await
        .get(&item.id)
    {
        value["PartCount"] = json!(part_count);
    }

    // For Movie/Episode folders, load child video media sources (multi-version support)
    if (item.item_type == "Movie" || item.item_type == "Episode") && item.is_folder {
        if let Ok(sources) =
            crate::jellyfin::playback::child_video_sources(db, &item.id, include_private_sources)
                .await
        {
            if !sources.is_empty() {
                attach_user_media_sources(db, user_id, &item.id, &mut value, sources).await?;
            }
        }
    } else if item.item_type == "Episode" {
        let sources =
            crate::jellyfin::playback::episode_version_sources(db, &item, include_private_sources)
                .await?;
        if !sources.is_empty() {
            attach_user_media_sources(db, user_id, &item.id, &mut value, sources).await?;
        } else {
            let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
                .await
                .unwrap_or_default();
            attach_user_media_sources(
                db,
                user_id,
                &item.id,
                &mut value,
                vec![media_source_json_with_streams(&item, streams)],
            )
            .await?;
        }
    } else if !item.is_folder {
        // For non-folder items (Episode/Video files), load media streams from DB
        let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
            .await
            .unwrap_or_default();
        if !streams.is_empty() {
            // Rebuild MediaSources with streams included
            let source = media_source_json_with_streams(&item, streams);
            attach_user_media_sources(db, user_id, &item.id, &mut value, vec![source]).await?;
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
            .query_one_raw(crate::db::helpers::pg_statement(
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
            .query_one_raw(crate::db::helpers::pg_statement(
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
                .query_one_raw(crate::db::helpers::pg_statement(
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
            .query_one_raw(crate::db::helpers::pg_statement(
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
            .query_one_raw(crate::db::helpers::pg_statement(
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
                .query_one_raw(crate::db::helpers::pg_statement(
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
                        "ImageTag": ch.image_path.as_ref().zip(ch.image_date_modified).and_then(|(path, modified)| (!path.is_empty()).then(|| crate::util::stable_text_id(&format!("chapter-image:{}:{}:{modified}", ch.item_id, ch.start_position_ticks)))),
                    })
                })
                .collect();
            value["Chapters"] = Value::Array(chapter_values);
            value["HasSegments"] = json!(true);
        }
    }

    Ok(value)
}

async fn extra_counts_for_item(
    db: &DatabaseConnection,
    item: &MediaItem,
) -> anyhow::Result<(i64, i64)> {
    let owner_id = if !item.is_folder && !item.parent_id.is_empty() {
        item.parent_id.as_str()
    } else {
        item.id.as_str()
    };
    let visible = visible_media_item_sql("media_items");
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"SELECT
                      COALESCE(SUM(CASE WHEN extra_type = 'Trailer' OR (extra_type IS NULL AND item_type = 'Trailer') THEN 1 ELSE 0 END), 0) AS local_trailer_count,
                      COALESCE(SUM(CASE WHEN extra_type IN ('Unknown','BehindTheScenes','Clip','DeletedScene','Interview','Sample','Scene','Featurette','Short') THEN 1 ELSE 0 END), 0) AS special_feature_count
                   FROM media_items
                   WHERE parent_id = ? AND is_folder = 0 AND {visible}"#
            ),
            vec![owner_id.into()],
        ))
        .await
        .with_context(|| format!("failed to load extra counts for item: {}", item.id))?;
    let Some(row) = row else {
        return Ok((0, 0));
    };
    Ok((
        row.get_i64("local_trailer_count").unwrap_or(0),
        row.get_i64("special_feature_count").unwrap_or(0),
    ))
}

async fn attach_user_media_sources(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    value: &mut Value,
    mut sources: Vec<Value>,
) -> anyhow::Result<()> {
    crate::jellyfin::playback::apply_user_stream_preferences_to_sources(
        db,
        user_id,
        item_id,
        &mut sources,
    )
    .await?;
    attach_media_sources(value, sources);
    Ok(())
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
    let has_lyrics = all_streams.iter().any(|stream| {
        stream
            .get("Type")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("Lyric"))
    });

    value["MediaSources"] = Value::Array(sources);
    value["MediaSourceCount"] = json!(source_count);
    value["EnableMediaSourceDisplay"] = json!(source_count > 1);
    if !all_streams.is_empty() {
        value["MediaStreams"] = Value::Array(all_streams);
        value["HasSubtitles"] = json!(has_subtitles);
        value["HasLyrics"] = json!(has_lyrics);
    }
}

async fn batch_additional_part_counts(
    db: &DatabaseConnection,
    items: &[MediaItem],
) -> HashMap<String, i64> {
    let video_items = items
        .iter()
        .filter(|item| matches!(item.item_type.as_str(), "Movie" | "Episode"))
        .collect::<Vec<_>>();
    if video_items.is_empty() {
        return HashMap::new();
    }
    let collection_types = video_items
        .iter()
        .map(|item| (item.id.as_str(), item.collection_type.as_str()))
        .collect::<HashMap<_, _>>();

    let mut children = HashMap::<String, Vec<(String, i64, i64)>>::new();
    for chunk in video_items.chunks(500) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT parent_id, path FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Video' AND is_public = 1"
        );
        let values = chunk
            .iter()
            .map(|item| item.id.as_str().into())
            .collect::<Vec<sea_orm::Value>>();
        let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
        else {
            continue;
        };
        for row in rows {
            let (Ok(parent_id), Ok(path)) = (row.get_str("parent_id"), row.get_str("path")) else {
                continue;
            };
            let Some(collection_type) = collection_types.get(parent_id.as_str()) else {
                continue;
            };
            if let Some(stack) = stack_part_info(&path, collection_type) {
                children.entry(parent_id).or_default().push(stack);
            }
        }
    }

    let mut counts = HashMap::new();
    for item in video_items {
        let Some(parts) = children.get(&item.id) else {
            continue;
        };
        let primary =
            stack_part_info(&item.path, &item.collection_type).map(|(key, part, _)| (key, part));
        let primary = primary.or_else(|| {
            let mut groups = HashMap::<&str, (usize, i64, i64)>::new();
            for (key, part, resolution) in parts {
                groups
                    .entry(key)
                    .and_modify(|group| {
                        group.0 += 1;
                        group.1 = group.1.min(*part);
                        group.2 = group.2.max(*resolution);
                    })
                    .or_insert((1, *part, *resolution));
            }
            groups
                .into_iter()
                .filter(|(_, (count, _, _))| *count > 1)
                .max_by_key(|(_, (_, _, resolution))| *resolution)
                .map(|(key, (_, first_part, _))| (key.to_string(), first_part))
        });
        let Some((stack_key, first_part)) = primary else {
            continue;
        };
        let additional = parts
            .iter()
            .filter(|(key, part, _)| *key == stack_key && *part > first_part)
            .count();
        if additional > 0 {
            counts.insert(
                item.id.clone(),
                i64::try_from(additional + 1).unwrap_or(i64::MAX),
            );
        }
    }
    counts
}

fn stack_part_info(path: &str, collection_type: &str) -> Option<(String, i64, i64)> {
    let parsed = crate::library::naming::parse_media_name(FsPath::new(path), collection_type);
    let version = parsed.version.unwrap_or_default().to_ascii_lowercase();
    let resolution = [
        ("2160p", 2160),
        ("4k", 2160),
        ("1080p", 1080),
        ("720p", 720),
        ("480p", 480),
    ]
    .into_iter()
    .find_map(|(needle, rank)| version.contains(needle).then_some(rank))
    .unwrap_or_default();
    Some((parsed.stack_key?, parsed.stack_part?, resolution))
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
    let root_visible = "mi.is_public = 1";
    let ancestor_visible = "ancestor_parent.is_public = 1";
    let sql = format!(
        r#"WITH RECURSIVE ancestors(root_id, id, title, item_type, parent_id, depth) AS (
               SELECT mi.id, mi.id, mi.title, mi.item_type, mi.parent_id, CAST(0 AS bigint)
               FROM media_items mi
               WHERE mi.id IN ({placeholders}) AND {root_visible}
               UNION ALL
               SELECT ancestors.root_id, ancestor_parent.id, ancestor_parent.title, ancestor_parent.item_type, ancestor_parent.parent_id, ancestors.depth + CAST(1 AS bigint)
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
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
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

#[derive(Default)]
struct ItemRelations {
    genres: Vec<Value>,
    tags: Vec<Value>,
    studios: Vec<Value>,
    people: Vec<Value>,
}

fn apply_item_relations(value: &mut Value, relations: ItemRelations) {
    value["Genres"] = Value::Array(
        relations
            .genres
            .iter()
            .filter_map(|entry| entry.get("Name").cloned())
            .collect(),
    );
    value["Tags"] = Value::Array(
        relations
            .tags
            .iter()
            .filter_map(|entry| entry.get("Name").cloned())
            .collect(),
    );
    value["GenreItems"] = Value::Array(relations.genres);
    value["TagItems"] = Value::Array(relations.tags);
    value["Studios"] = Value::Array(relations.studios);
    let artist_items = relations
        .people
        .iter()
        .filter(|person| {
            person
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("Artist"))
        })
        .map(|person| {
            json!({
                "Name": person.get("Name").cloned().unwrap_or(Value::Null),
                "Id": person.get("Id").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let album_artist_items = relations
        .people
        .iter()
        .filter(|person| {
            person
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("AlbumArtist"))
        })
        .map(|person| {
            json!({
                "Name": person.get("Name").cloned().unwrap_or(Value::Null),
                "Id": person.get("Id").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    value["Artists"] = Value::Array(
        artist_items
            .iter()
            .filter_map(|artist| artist.get("Name").cloned())
            .collect(),
    );
    value["ArtistItems"] = Value::Array(artist_items);
    value["AlbumArtist"] = album_artist_items
        .first()
        .and_then(|artist| artist.get("Name"))
        .cloned()
        .unwrap_or(Value::Null);
    value["AlbumArtists"] = Value::Array(album_artist_items);
    value["People"] = Value::Array(relations.people);
}

async fn batch_item_relations(
    db: &DatabaseConnection,
    user_id: &str,
    item_ids: &[String],
) -> anyhow::Result<HashMap<String, ItemRelations>> {
    let mut result = HashMap::<String, ItemRelations>::new();
    for chunk in item_ids.chunks(100) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            r#"SELECT mg.item_id, 'genre' AS kind, g.id, g.name
               FROM media_genres mg JOIN genres g ON g.id = mg.genre_id
               WHERE mg.item_id IN ({placeholders})
               UNION ALL
               SELECT mt.item_id, 'tag' AS kind, t.id, t.name
               FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
               WHERE mt.item_id IN ({placeholders})
               UNION ALL
               SELECT ms.item_id, 'studio' AS kind, s.id, s.name
               FROM media_studios ms JOIN studios s ON s.id = ms.studio_id
               WHERE ms.item_id IN ({placeholders})
               ORDER BY item_id, kind, name, id"#
        );
        let mut values = Vec::with_capacity(chunk.len() * 3);
        for _ in 0..3 {
            values.extend(chunk.iter().map(|id| id.as_str().into()));
        }
        let rows = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
            .context("failed to batch-load item relations")?;
        for row in &rows {
            let item_id = row.get_str("item_id")?;
            let entry = json!({
                "Id": row.get_str("id")?,
                "Name": row.get_str("name")?,
            });
            let relations = result.entry(item_id).or_default();
            match row.get_str("kind")?.as_str() {
                "genre" => relations.genres.push(entry),
                "tag" => relations.tags.push(entry),
                "studio" => relations.studios.push(entry),
                _ => {}
            }
        }

        let people_sql = format!(
            r#"SELECT mp.item_id, people.id, people.name, people.tmdb_id,
                      mp.role, mp.person_type,
                      COALESCE(ud.is_favorite, 0) AS is_favorite,
                      (SELECT ia.etag FROM image_assets ia
                       WHERE ia.item_id = people.id AND ia.image_type = 'Primary'
                       ORDER BY ia.image_index ASC, ia.id ASC LIMIT 1) AS primary_image_tag
               FROM media_people mp
               JOIN people ON people.id = mp.person_id
               LEFT JOIN user_data ud ON ud.item_id = people.id AND ud.user_id = ?
               WHERE mp.item_id IN ({placeholders})
               ORDER BY mp.item_id, mp.sort_order ASC, people.name ASC, people.id ASC, mp.person_type ASC"#
        );
        let mut values: Vec<sea_orm::Value> = vec![user_id.into()];
        values.extend(chunk.iter().map(|id| id.as_str().into()));
        let people_rows = db
            .query_all_raw(crate::db::helpers::pg_statement(&people_sql, values))
            .await
            .context("failed to batch-load item people")?;
        for row in &people_rows {
            let item_id = row.get_str("item_id")?;
            let person_id = row.get_str("id")?;
            let mut provider_ids = serde_json::Map::new();
            if let Some(tmdb_id) = row.get_opt_str("tmdb_id")?.filter(|id| !id.is_empty()) {
                provider_ids.insert("Tmdb".to_string(), json!(tmdb_id));
            }
            let mut person = json!({
                "Id": person_id,
                "Name": row.get_str("name")?,
                "Role": row.get_opt_str("role")?,
                "Type": row.get_opt_str("person_type")?,
                "ProviderIds": provider_ids,
                "UserData": {
                    "ItemId": person_id,
                    "Key": person_id,
                    "IsFavorite": row.get_i64("is_favorite").unwrap_or(0) != 0,
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
            if let Some(tag) = row
                .get_opt_str("primary_image_tag")?
                .filter(|tag| !tag.is_empty())
            {
                person["PrimaryImageTag"] = json!(tag);
            }
            result.entry(item_id).or_default().people.push(person);
        }
    }
    Ok(result)
}

/// Batch-enrich BaseItemDto list items with persisted metadata relations,
/// provider IDs, media sources, and TV parent/count fields.
pub async fn enrich_item_list(
    db: &DatabaseConnection,
    user_id: &str,
    mut items: Vec<MediaItem>,
) -> Vec<Value> {
    // Collect unique episode parent IDs and season IDs for parent lookups.
    // Keep these IDs owned so the image-tag query and the ancestor query can
    // run at the same time without holding an immutable borrow of `items`.
    let parent_lookup_ids: Vec<String> = items
        .iter()
        .filter_map(|i| match i.item_type.as_str() {
            "Episode" => Some(i.parent_id.clone()),
            "Season" => Some(i.id.clone()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let parent_lookup_refs = parent_lookup_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let (image_tags_result, parent_info_map) = tokio::join!(
        crate::jellyfin::item_queries::attach_item_image_tags(db, &mut items),
        batch_episode_parent_info(db, &parent_lookup_refs),
    );
    let _ = image_tags_result;

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
    let item_ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
    let (parent_image_tags, provider_map, mut relation_map) = tokio::join!(
        async {
            crate::jellyfin::item_queries::batch_item_image_tags(db, &parent_image_ids)
                .await
                .unwrap_or_default()
        },
        async {
            crate::jellyfin::item_queries::batch_item_provider_ids(db, &item_ids)
                .await
                .unwrap_or_default()
        },
        async {
            batch_item_relations(db, user_id, &item_ids)
                .await
                .unwrap_or_default()
        }
    );

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
        let values: Vec<sea_orm::Value> = parent_lookup_ids
            .iter()
            .map(|id| id.as_str().into())
            .collect();
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
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
        let mut values: Vec<sea_orm::Value> = parent_lookup_ids
            .iter()
            .map(|id| id.as_str().into())
            .collect();
        values.push(user_id.into());
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
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
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
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
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
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
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(series_id), Ok(cnt)) = (row.get_str("series_id"), row.get_i64("cnt")) {
                    series_played_episode_count_map.insert(series_id, cnt);
                }
            }
        }
    }

    let media_source_map = list_media_source_map(db, user_id, &items).await;
    let additional_part_counts = batch_additional_part_counts(db, &items).await;

    let mut enriched_items = Vec::with_capacity(items.len());
    for item in items {
        let mut val = item.to_jellyfin_json();
        if let Some(provider_ids) = provider_map.get(&item.id) {
            val["ProviderIds"] = provider_ids.clone();
        }
        apply_item_relations(&mut val, relation_map.remove(&item.id).unwrap_or_default());
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
        if let Some(part_count) = additional_part_counts.get(&item.id) {
            val["PartCount"] = json!(part_count);
        }
        enriched_items.push(strip_nulls(val));
    }
    enriched_items
}

pub async fn enrich_episode_list(
    db: &DatabaseConnection,
    user_id: &str,
    items: Vec<MediaItem>,
) -> Vec<Value> {
    enrich_item_list(db, user_id, items).await
}

async fn list_media_source_map(
    db: &DatabaseConnection,
    user_id: &str,
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
    if !direct_items.is_empty() {
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
    }

    let _ = crate::jellyfin::playback::apply_user_stream_preferences_to_source_map(
        db,
        user_id,
        &mut source_map,
    )
    .await;

    source_map
}

fn is_direct_playable_list_item(item: &MediaItem) -> bool {
    !item.is_folder
        && matches!(
            item.item_type.as_str(),
            "Movie" | "Episode" | "Video" | "Audio" | "Trailer"
        )
}

pub async fn enrich_resume_items(
    db: &DatabaseConnection,
    user_id: &str,
    items: Vec<MediaItem>,
) -> Vec<Value> {
    let item_state = items
        .iter()
        .map(|item| {
            (
                item.item_type == "Episode",
                item.runtime_ticks,
                item.played_percentage,
                item.playback_position_ticks,
            )
        })
        .collect::<Vec<_>>();
    let mut values = enrich_item_list(db, user_id, items).await;
    for (value, (is_episode, runtime_ticks, played_percentage, playback_position_ticks)) in
        values.iter_mut().zip(item_state)
    {
        if is_episode {
            value["SupportsResume"] = json!(true);
        }
        if let Some(runtime_ticks) = runtime_ticks {
            if value.get("RunTimeTicks").and_then(Value::as_i64).is_none() {
                value["RunTimeTicks"] = json!(runtime_ticks);
            }
        }
        if played_percentage.is_none() && playback_position_ticks > 0 {
            if let Some(runtime_ticks) = value
                .get("RunTimeTicks")
                .and_then(Value::as_i64)
                .filter(|runtime| *runtime > 0)
            {
                value["UserData"]["PlayedPercentage"] = json!(
                    (playback_position_ticks as f64 / runtime_ticks as f64 * 100.0).min(100.0)
                );
            }
        }
        *value = strip_nulls(std::mem::take(value));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::{
        ItemRelations, apply_item_relations, attach_media_sources, batch_additional_part_counts,
        enrich_episode_list, get_episode_parent_info, item_json_with_provider_ids,
    };
    use crate::entities::{
        genres::{self, Entity as Genres},
        image_assets::{self, Entity as ImageAssets},
        libraries::{self, Entity as Libraries},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        studios::{self, Entity as Studios},
        tags::{self, Entity as Tags},
        user_data::{self, Entity as UserData},
        users::{self, Entity as Users},
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
        assert!(value.get("PartCount").is_none());
    }

    #[tokio::test]
    async fn stacked_movie_part_count_is_not_alternate_version_count() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies-parts", "Movies", "movies").await;
        insert_item_with_path(
            &db,
            "movie-parts",
            "Movie",
            "/tmp/movie-parts",
            "movies-parts",
            "movies-parts",
            "Movie",
            1,
            1,
            None,
            None,
            None,
        )
        .await;
        for (id, path) in [
            ("movie-cd1", "/tmp/movie-parts/Movie - cd1.mkv"),
            ("movie-cd2", "/tmp/movie-parts/Movie - cd2.mkv"),
        ] {
            insert_item_with_path(
                &db,
                id,
                id,
                path,
                "movies-parts",
                "movie-parts",
                "Video",
                0,
                1,
                Some("mkv"),
                None,
                None,
            )
            .await;
        }
        let item =
            crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "movie-parts")
                .await
                .unwrap()
                .unwrap();

        let counts = batch_additional_part_counts(&db, std::slice::from_ref(&item)).await;
        assert_eq!(counts.get("movie-parts"), Some(&2));
        let enriched = enrich_episode_list(&db, "u1", vec![item]).await;
        assert_eq!(enriched[0]["PartCount"], 2);
        assert_eq!(enriched[0]["MediaSourceCount"], 1);
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
        insert_image(
            &db,
            "direct-series-primary",
            "direct-series",
            "Primary",
            "series-primary-tag",
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
        assert_eq!(direct_json["SeriesPrimaryImageTag"], "series-primary-tag");
        assert_eq!(direct_json["ParentPrimaryImageItemId"], "direct-series");
        assert_eq!(direct_json["ParentPrimaryImageTag"], "series-primary-tag");

        let direct_enriched = enrich_episode_list(&db, "u1", vec![direct_episode]).await;
        assert_eq!(direct_enriched[0]["SeriesId"], "direct-series");
        assert_eq!(direct_enriched[0]["SeriesName"], "Direct Series");
        assert_eq!(direct_enriched[0]["SeasonId"], "direct-series");
        assert_eq!(direct_enriched[0]["SeasonName"], "Season 1");
        assert_eq!(
            direct_enriched[0]["SeriesPrimaryImageTag"],
            "series-primary-tag"
        );
        assert_eq!(
            direct_enriched[0]["ParentPrimaryImageItemId"],
            "direct-series"
        );
        assert_eq!(
            direct_enriched[0]["ParentPrimaryImageTag"],
            "series-primary-tag"
        );

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
    async fn item_detail_counts_local_extras_by_extra_type() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_item_with_path(
            &db,
            "movie",
            "Movie",
            "/movies/Movie",
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
        insert_extra_item(
            &db,
            "trailer",
            "Trailer",
            "/movies/Movie/trailers/Trailer.mkv",
            "movies",
            "movie",
            "Trailer",
            "Trailer",
            1,
        )
        .await;
        insert_extra_item(
            &db,
            "behind",
            "Behind the Scenes",
            "/movies/Movie/extras/Behind the Scenes.mkv",
            "movies",
            "movie",
            "Video",
            "BehindTheScenes",
            1,
        )
        .await;
        insert_extra_item(
            &db,
            "hidden-behind",
            "Hidden Behind the Scenes",
            "/movies/Movie/extras/Hidden Behind the Scenes.mkv",
            "movies",
            "movie",
            "Video",
            "BehindTheScenes",
            0,
        )
        .await;

        let movie = crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "movie")
            .await
            .unwrap()
            .unwrap();
        let movie_json = item_json_with_provider_ids(&db, "u1", movie, true)
            .await
            .unwrap();

        assert_eq!(movie_json["LocalTrailerCount"], 1);
        assert_eq!(movie_json["SpecialFeatureCount"], 1);
    }

    #[tokio::test]
    async fn list_relations_match_item_detail_without_n_plus_one() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_item(&db, "movie", "Movie", "movies", "Movie", 1, 1, None, None).await;
        Users::insert(users::ActiveModel {
            id: Set("u1".to_string()),
            username: Set("u1".to_string()),
            display_name: Set("User".to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        for (id, name) in [("genre-drama", "Drama"), ("genre-action", "Action")] {
            Genres::insert(genres::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                created_at: Set(1),
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
            MediaGenres::insert(media_genres::ActiveModel {
                item_id: Set("movie".to_string()),
                genre_id: Set(id.to_string()),
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }
        Tags::insert(tags::ActiveModel {
            id: Set("tag-restored".to_string()),
            name: Set("Restored".to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        MediaTags::insert(media_tags::ActiveModel {
            item_id: Set("movie".to_string()),
            tag_id: Set("tag-restored".to_string()),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        Studios::insert(studios::ActiveModel {
            id: Set("studio-one".to_string()),
            name: Set("Studio One".to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        MediaStudios::insert(media_studios::ActiveModel {
            item_id: Set("movie".to_string()),
            studio_id: Set("studio-one".to_string()),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        for (id, name, person_type, role, sort_order) in [
            ("director", "Director", "Director", None, 1),
            ("actor", "Actor", "Actor", Some("Lead"), 2),
        ] {
            People::insert(people::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                created_at: Set(1),
                overview: Set(None),
                tmdb_id: Set((id == "actor").then(|| "123".to_string())),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
            MediaPeople::insert(media_people::ActiveModel {
                item_id: Set("movie".to_string()),
                person_id: Set(id.to_string()),
                person_type: Set(person_type.to_string()),
                role: Set(role.map(ToString::to_string)),
                sort_order: Set(sort_order),
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }
        insert_image(&db, "actor-primary", "actor", "Primary", "actor-tag").await;
        UserData::insert(user_data::ActiveModel {
            user_id: Set("u1".to_string()),
            item_id: Set("director".to_string()),
            is_favorite: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        let detail_item =
            crate::jellyfin::item_queries::find_media_item_for_admin(&db, "u1", "movie")
                .await
                .unwrap()
                .unwrap();
        let list_item = detail_item.clone();
        let detail = crate::jellyfin::common::strip_nulls(
            item_json_with_provider_ids(&db, "u1", detail_item, true)
                .await
                .unwrap(),
        );
        let list = enrich_episode_list(&db, "u1", vec![list_item]).await;
        let list = &list[0];

        for key in [
            "Genres",
            "GenreItems",
            "Tags",
            "TagItems",
            "Studios",
            "People",
        ] {
            assert_eq!(list[key], detail[key], "field {key} differs");
        }
        assert_eq!(list["Genres"], json!(["Action", "Drama"]));
        assert_eq!(list["People"][0]["Id"], "director");
        assert_eq!(list["People"][0]["UserData"]["IsFavorite"], true);
        assert_eq!(list["People"][1]["Id"], "actor");
        assert_eq!(list["People"][1]["Role"], "Lead");
        assert_eq!(list["People"][1]["PrimaryImageTag"], "actor-tag");
        assert_eq!(list["People"][1]["ProviderIds"]["Tmdb"], "123");
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

    #[allow(clippy::too_many_arguments)]
    async fn insert_extra_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        extra_type: &str,
        is_public: i64,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(library_id.to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            extra_type: Set(Some(extra_type.to_string())),
            is_folder: Set(0),
            is_public: Set(is_public),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_image(
        db: &DatabaseConnection,
        id: &str,
        item_id: &str,
        image_type: &str,
        etag: &str,
    ) {
        ImageAssets::insert(image_assets::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            image_type: Set(image_type.to_string()),
            image_index: Set(0),
            path: Set(Some(format!("/tmp/{id}.jpg"))),
            etag: Set(Some(etag.to_string())),
            width: Set(Some(1000)),
            height: Set(Some(1500)),
            size_bytes: Set(Some(1)),
            created_at: Set(1),
            updated_at: Set(1),
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

    #[test]
    fn artist_people_populate_jellyfin_artist_fields() {
        let mut value = json!({});
        apply_item_relations(
            &mut value,
            ItemRelations {
                people: vec![
                    json!({"Id": "artist-1", "Name": "Artist One", "Type": "Artist"}),
                    json!({"Id": "album-artist-1", "Name": "Album Artist", "Type": "AlbumArtist"}),
                    json!({"Id": "actor-1", "Name": "Actor One", "Type": "Actor"}),
                ],
                ..Default::default()
            },
        );

        assert_eq!(value["Artists"], json!(["Artist One"]));
        assert_eq!(
            value["ArtistItems"],
            json!([{"Id": "artist-1", "Name": "Artist One"}])
        );
        assert_eq!(value["AlbumArtist"], "Album Artist");
        assert_eq!(
            value["AlbumArtists"],
            json!([{"Id": "album-artist-1", "Name": "Album Artist"}])
        );
        assert_eq!(value["People"].as_array().unwrap().len(), 3);
    }
}
