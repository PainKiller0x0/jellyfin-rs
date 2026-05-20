use std::collections::{HashMap, HashSet};

use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};

use crate::{db::row_ext::QueryResultExt, library::models::MediaItem};

pub async fn library_views(db: &DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, name, collection_type FROM libraries ORDER BY name ASC",
            vec![],
        ))
        .await
        .context("failed to list libraries")?;
    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({ "Name": row.get_str("name")?, "Id": row.get_str("id")?, "CollectionType": row.get_str("collection_type")?, "Type": "CollectionFolder" }))
        })
        .collect()
}

pub async fn list_media_items(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<MediaItem>> {
    let has_list_item_ids = query.contains_key("ListItemIds");
    let parent_id = query
        .get("ParentId")
        .map(String::as_str)
        .unwrap_or("movies");
    let limit = query_u32(query, "Limit", 50).min(200) as usize;
    let offset = query_u32(query, "StartIndex", 0) as usize;
    let recursive = query_bool(query, "Recursive", false);

    let parent_is_collection = is_collection_or_playlist(db, parent_id).await?;
    let backend = db.get_database_backend();

    let rows = if parent_is_collection {
        db.query_all(crate::db::helpers::portable_statement(
            backend,
            &linked_children_select_sql(),
            vec![user_id.into(), parent_id.into()],
        ))
        .await
    } else if recursive {
        db.query_all(crate::db::helpers::portable_statement(
            backend,
            &recursive_media_item_select_sql(),
            vec![parent_id.into(), user_id.into(), parent_id.into()],
        ))
        .await
    } else if has_list_item_ids {
        db.query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(""),
            vec![user_id.into()],
        ))
        .await
    } else {
        db.query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql("WHERE media_items.parent_id = ?"),
            vec![user_id.into(), parent_id.into()],
        ))
        .await
    }
    .context("failed to list media items")?;
    let mut items = decode_media_items(&rows)?;
    apply_item_filters(&mut items, query);
    apply_relation_filters(db, &mut items, query).await?;
    sort_media_items(&mut items, query);
    let mut items: Vec<_> = items.into_iter().skip(offset).take(limit).collect();
    // Batch load image tags
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        if let Ok(tags_map) = batch_item_image_tags(db, &ids).await {
            for item in &mut items {
                if let Some(tags) = tags_map.get(&item.id) {
                    item.image_tags = Some(tags.clone());
                }
            }
        }
    }
    Ok(items)
}

async fn is_collection_or_playlist(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT item_type FROM media_items WHERE id = ? AND item_type IN ('BoxSet', 'Playlist')",
            vec![item_id.into()],
        ))
        .await
        .context("failed to check item type")?;
    Ok(row.is_some())
}

fn linked_children_select_sql() -> String {
    r#"SELECT mi.id, mi.title, mi.path, mi.library_id, libraries.collection_type, mi.parent_id, mi.item_type, mi.is_folder, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? LEFT JOIN libraries ON libraries.id = mi.library_id WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC"#.to_string()
}

pub async fn latest_media_items(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    decode_media_items(
        &db.query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(
                "WHERE media_items.is_folder = 0 ORDER BY media_items.modified_at DESC LIMIT 16",
            ),
            vec![user_id.into()],
        ))
        .await
        .context("failed to list latest media items")?,
    )
}

pub async fn resume_media_items(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    decode_media_items(
        &db.query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(
                "WHERE media_items.is_folder = 0 AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC LIMIT 50",
            ),
            vec![user_id.into()],
        ))
        .await
        .context("failed to list resume media items")?,
    )
}

pub async fn find_media_item(
    db: &DatabaseConnection,
    user_id: &str,
    id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql("WHERE media_items.id = ?"),
            vec![user_id.into(), id.into()],
        ))
        .await
        .with_context(|| format!("failed to find media item: {id}"))?;
    row.map(|r| MediaItem::from_query_result(&r))
        .transpose()
        .context("failed to decode media item")
}

pub fn media_item_select_sql(where_clause: &str) -> String {
    format!(
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.created_at, media_items.modified_at, COALESCE(user_data.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(user_data.played, CAST(0 AS bigint)) AS played, COALESCE(user_data.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, user_data.played_percentage AS played_percentage, COALESCE(user_data.play_count, CAST(0 AS bigint)) AS play_count, user_data.last_played_at AS last_played_at FROM media_items LEFT JOIN user_data ON user_data.item_id = media_items.id AND user_data.user_id = ? LEFT JOIN libraries ON libraries.id = media_items.library_id {where_clause}"#
    )
}

fn recursive_media_item_select_sql() -> String {
    format!(
        r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?)"#,
        media_item_select_sql("").trim()
    )
}

pub fn decode_media_items(rows: &[sea_orm::QueryResult]) -> anyhow::Result<Vec<MediaItem>> {
    rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode media items")
}

async fn batch_item_image_tags(
    db: &DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    use crate::entities::image_assets::{Entity as ImageAssets, Column};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let mut map = HashMap::new();
    for chunk in item_ids.chunks(100) {
        let models = ImageAssets::find()
            .filter(Column::ItemId.is_in(chunk.iter().map(|s| s.as_str())))
            .order_by_asc(Column::ImageType)
            .order_by_asc(Column::ImageIndex)
            .all(db).await?;
        for m in &models {
            let etag = m.etag.as_deref().unwrap_or_default();
            let entry: &mut serde_json::Value = map.entry(m.item_id.clone()).or_insert_with(|| serde_json::Map::new().into());
            if let Some(obj) = entry.as_object_mut() {
                obj.entry(m.image_type.clone()).or_insert_with(|| json!(etag));
            }
        }
    }
    Ok(map)
}

fn query_u32(query: &HashMap<String, String>, key: &str, default: u32) -> u32 {
    query
        .get(key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn query_bool(query: &HashMap<String, String>, key: &str, default: bool) -> bool {
    query
        .get(key)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(default)
}

fn apply_item_filters(items: &mut Vec<MediaItem>, query: &HashMap<String, String>) {
    if let Some(search_term) = query.get("SearchTerm").filter(|value| !value.is_empty()) {
        let search_term = search_term.to_ascii_lowercase();
        items.retain(|item| item.title.to_ascii_lowercase().contains(&search_term));
    }
    if let Some(include_types) = query
        .get("IncludeItemTypes")
        .or_else(|| query.get("IncludeSearchTypes"))
        .filter(|value| !value.is_empty())
    {
        let include_types = include_types.split(',').map(str::trim).collect::<Vec<_>>();
        items.retain(|item| {
            include_types
                .iter()
                .any(|item_type| *item_type == item.item_type)
        });
    }
    if let Some(exclude_types) = query
        .get("ExcludeItemTypes")
        .filter(|value| !value.is_empty())
    {
        let exclude_types = exclude_types.split(',').map(str::trim).collect::<Vec<_>>();
        items.retain(|item| {
            !exclude_types
                .iter()
                .any(|item_type| *item_type == item.item_type)
        });
    }
    if let Some(media_types) = query.get("MediaTypes").filter(|value| !value.is_empty()) {
        let media_types = media_types.split(',').map(str::trim).collect::<Vec<_>>();
        for media_type in &media_types {
            match *media_type {
                "Video" => items.retain(|item| {
                    matches!(
                        item.item_type.as_str(),
                        "Movie" | "Series" | "Season" | "Episode" | "Video"
                    )
                }),
                "Audio" => {
                    items.retain(|item| matches!(item.item_type.as_str(), "Audio" | "MusicAlbum"))
                }
                "Photo" => items.retain(|item| item.item_type == "Photo"),
                _ => {}
            }
        }
    }
    if let Some(filters) = query.get("Filters") {
        if filters.contains("IsFavorite") {
            items.retain(|item| item.is_favorite);
        }
        if filters.contains("IsPlayed") {
            items.retain(|item| item.played);
        }
        if filters.contains("IsUnplayed") {
            items.retain(|item| !item.played);
        }
        if filters.contains("IsResumable") {
            items.retain(|item| item.playback_position_ticks > 0);
        }
    }
    if let Some(years) = query_ids(query, "Years").or_else(|| query_ids(query, "years")) {
        items.retain(|item| {
            item.production_year
                .is_some_and(|year| years.iter().any(|y| y == &year.to_string()))
        });
    }
    if let Some(containers) = query_ids(query, "Containers") {
        items.retain(|item| {
            item.container.as_ref().is_some_and(|container| {
                containers.iter().any(|c| c.eq_ignore_ascii_case(container))
            })
        });
    }
}

async fn apply_relation_filters(
    db: &DatabaseConnection,
    items: &mut Vec<MediaItem>,
    query: &HashMap<String, String>,
) -> anyhow::Result<()> {
    for (query_key, table, id_column) in [
        ("GenreIds", "media_genres", "genre_id"),
        ("TagIds", "media_tags", "tag_id"),
        ("PersonIds", "media_people", "person_id"),
        ("StudioIds", "media_studios", "studio_id"),
    ] {
        if let Some(ids) = query_ids(query, query_key) {
            let item_ids = relation_item_ids(db, table, id_column, &ids).await?;
            retain_item_ids(items, &item_ids);
        }
    }
    if let Some(ids) = query_ids(query, "ListItemIds") {
        let item_ids = parent_item_ids(db, &ids).await?;
        retain_item_ids(items, &item_ids);
    }
    if let Some(codecs) = query_ids(query, "VideoCodecs") {
        let item_ids = codec_item_ids(db, &codecs).await?;
        retain_item_ids(items, &item_ids);
    }
    let min_width = query.get("MinWidth").and_then(|v| v.parse::<i64>().ok());
    let max_width = query.get("MaxWidth").and_then(|v| v.parse::<i64>().ok());
    if min_width.is_some() || max_width.is_some() {
        let item_ids = width_item_ids(db, min_width, max_width).await?;
        retain_item_ids(items, &item_ids);
    }
    Ok(())
}

fn retain_item_ids(items: &mut Vec<MediaItem>, item_ids: &[String]) {
    let item_ids = item_ids.iter().map(String::as_str).collect::<HashSet<_>>();
    items.retain(|item| item_ids.contains(item.id.as_str()));
}

async fn codec_item_ids(db: &DatabaseConnection, codecs: &[String]) -> anyhow::Result<Vec<String>> {
    let backend = db.get_database_backend();
    let mut ids = Vec::new();
    for codec in codecs {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND codec = ?",
                vec![codec.as_str().into()],
            ))
            .await
            .context("failed to filter by video codec")?;
        for row in &rows {
            ids.push(row.get_str("item_id")?);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn width_item_ids(
    db: &DatabaseConnection,
    min_width: Option<i64>,
    max_width: Option<i64>,
) -> anyhow::Result<Vec<String>> {
    let backend = db.get_database_backend();
    let (sql, values) = match (min_width, max_width) {
        (Some(_), Some(_)) => (
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ? AND width <= ?",
            vec![min_width.into(), max_width.into()],
        ),
        (Some(_), None) => (
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ?",
            vec![min_width.into()],
        ),
        (None, Some(_)) => (
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width <= ?",
            vec![max_width.into()],
        ),
        (None, None) => unreachable!("width_item_ids requires at least one bound"),
    };

    let rows = db
        .query_all(crate::db::helpers::portable_statement(backend, sql, values))
        .await
        .context("failed to filter by width")?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get_opt_str("item_id").ok().flatten())
        .collect())
}

async fn parent_item_ids(
    db: &DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let backend = db.get_database_backend();
    let mut parents = Vec::new();
    for id in item_ids {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                "SELECT DISTINCT parent_id FROM linked_children WHERE item_id = ?",
                vec![id.as_str().into()],
            ))
            .await
            .context("failed to find linked parents")?;
        for row in &rows {
            parents.push(row.get_str("parent_id")?);
        }
    }
    parents.sort();
    parents.dedup();
    Ok(parents)
}

async fn relation_item_ids(
    db: &DatabaseConnection,
    table: &str,
    id_column: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let backend = db.get_database_backend();
    let mut item_ids = Vec::new();
    let sql = format!("SELECT DISTINCT item_id FROM {table} WHERE {id_column} = ?");
    for id in ids {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &sql,
                vec![id.as_str().into()],
            ))
            .await
            .with_context(|| format!("failed to filter items by {table}"))?;
        for row in &rows {
            item_ids.push(row.get_str("item_id")?);
        }
    }
    item_ids.sort();
    item_ids.dedup();
    Ok(item_ids)
}

fn query_ids(query: &HashMap<String, String>, key: &str) -> Option<Vec<String>> {
    query
        .get(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|ids| !ids.is_empty())
}

fn sort_media_items(items: &mut [MediaItem], query: &HashMap<String, String>) {
    let default_sort = if items.first().is_some_and(|i| i.item_type == "Episode") {
        "IndexNumber"
    } else {
        "SortName"
    };
    let sort_by = query
        .get("SortBy")
        .map(String::as_str)
        .unwrap_or(default_sort);
    let primary_sort = sort_by.split(',').next().unwrap_or(default_sort);
    match primary_sort {
        "SortName" => items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
        }),
        "IndexNumber" | "AiredEpisodeOrder" => items.sort_by(|a, b| {
            a.season_number
                .unwrap_or(0)
                .cmp(&b.season_number.unwrap_or(0))
                .then_with(|| {
                    a.episode_number
                        .unwrap_or(0)
                        .cmp(&b.episode_number.unwrap_or(0))
                })
                .then_with(|| a.title.cmp(&b.title))
        }),
        "DateCreated" => items.sort_by_key(|item| item.created_at),
        "DateLastMediaAdded" | "DateLastContentAdded" => items.sort_by_key(|item| item.modified_at),
        "IsFolder" => items.sort_by(|a, b| {
            b.is_folder
                .cmp(&a.is_folder)
                .then_with(|| a.title.cmp(&b.title))
        }),
        "ProductionYear" | "PremiereDate" => items.sort_by(|a, b| {
            a.production_year
                .unwrap_or(i64::MAX)
                .cmp(&b.production_year.unwrap_or(i64::MAX))
                .then_with(|| {
                    a.title
                        .to_ascii_lowercase()
                        .cmp(&b.title.to_ascii_lowercase())
                })
        }),
        "Runtime" => items.sort_by(|a, b| {
            a.runtime_ticks
                .unwrap_or(i64::MAX)
                .cmp(&b.runtime_ticks.unwrap_or(i64::MAX))
                .then_with(|| {
                    a.title
                        .to_ascii_lowercase()
                        .cmp(&b.title.to_ascii_lowercase())
                })
        }),
        "DatePlayed" => items.sort_by(|a, b| {
            a.last_played_at
                .unwrap_or(0)
                .cmp(&b.last_played_at.unwrap_or(0))
                .then_with(|| {
                    a.title
                        .to_ascii_lowercase()
                        .cmp(&b.title.to_ascii_lowercase())
                })
        }),
        "CommunityRating" | "CriticRating" | "OfficialRating" => items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
        }),
        "Random" => {
            fastrand::shuffle(items);
            return;
        }
        "IsFavoriteOrLiked" => items.sort_by(|a, b| {
            b.is_favorite.cmp(&a.is_favorite).then_with(|| {
                a.title
                    .to_ascii_lowercase()
                    .cmp(&b.title.to_ascii_lowercase())
            })
        }),
        "PlayCount" => items.sort_by(|a, b| {
            b.play_count.cmp(&a.play_count).then_with(|| {
                a.title
                    .to_ascii_lowercase()
                    .cmp(&b.title.to_ascii_lowercase())
            })
        }),
        _ => items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
        }),
    }
    if query
        .get("SortOrder")
        .is_some_and(|value| value.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }
}
