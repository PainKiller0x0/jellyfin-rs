use std::collections::{HashMap, HashSet};

use anyhow::Context;
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::library::models::MediaItem;

pub async fn library_views(db: &AnyPool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query("SELECT id, name, collection_type FROM libraries ORDER BY name ASC")
        .fetch_all(db)
        .await
        .context("failed to list libraries")?;
    rows.into_iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({ "Name": row.try_get::<String, _>("name")?, "Id": row.try_get::<String, _>("id")?, "CollectionType": row.try_get::<String, _>("collection_type")?, "Type": "CollectionFolder" }))
        })
        .collect()
}

pub async fn list_media_items(
    db: &AnyPool,
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

    let rows = if parent_is_collection {
        sqlx::query(&linked_children_select_sql())
            .bind(user_id)
            .bind(parent_id)
            .fetch_all(db)
            .await
    } else if recursive {
        sqlx::query(&recursive_media_item_select_sql())
            .bind(parent_id)
            .bind(user_id)
            .bind(parent_id)
            .fetch_all(db)
            .await
    } else if has_list_item_ids {
        sqlx::query(&media_item_select_sql(""))
            .bind(user_id)
            .fetch_all(db)
            .await
    } else {
        sqlx::query(&media_item_select_sql("WHERE media_items.parent_id = ?"))
            .bind(user_id)
            .bind(parent_id)
            .fetch_all(db)
            .await
    }
    .context("failed to list media items")?;
    let mut items = decode_media_items(rows)?;
    apply_item_filters(&mut items, query);
    apply_relation_filters(db, &mut items, query).await?;
    sort_media_items(&mut items, query);
    Ok(items.into_iter().skip(offset).take(limit).collect())
}

async fn is_collection_or_playlist(db: &AnyPool, item_id: &str) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT item_type FROM media_items WHERE id = ? AND item_type IN ('BoxSet', 'Playlist')",
    )
    .bind(item_id)
    .fetch_optional(db)
    .await
    .context("failed to check item type")?;
    Ok(row.is_some())
}

fn linked_children_select_sql() -> String {
    format!(
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, mi.parent_id, mi.item_type, mi.is_folder, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, 0) AS is_favorite, COALESCE(ud.played, 0) AS played, COALESCE(ud.playback_position_ticks, 0) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, 0) AS play_count, ud.last_played_at AS last_played_at FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC"#
    )
}

pub async fn latest_media_items(db: &AnyPool, user_id: &str) -> anyhow::Result<Vec<MediaItem>> {
    decode_media_items(
        sqlx::query(&media_item_select_sql(
            "WHERE media_items.is_folder = 0 ORDER BY media_items.modified_at DESC LIMIT 16",
        ))
        .bind(user_id)
        .fetch_all(db)
        .await
        .context("failed to list latest media items")?,
    )
}

pub async fn resume_media_items(db: &AnyPool, user_id: &str) -> anyhow::Result<Vec<MediaItem>> {
    decode_media_items(
        sqlx::query(&media_item_select_sql(
            "WHERE media_items.is_folder = 0 AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC LIMIT 50",
        ))
        .bind(user_id)
        .fetch_all(db)
        .await
        .context("failed to list resume media items")?,
    )
}

pub async fn find_media_item(
    db: &AnyPool,
    user_id: &str,
    id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let row = sqlx::query(&media_item_select_sql("WHERE media_items.id = ?"))
        .bind(user_id)
        .bind(id)
        .fetch_optional(db)
        .await
        .with_context(|| format!("failed to find media item: {id}"))?;
    row.map(MediaItem::from_row)
        .transpose()
        .context("failed to decode media item")
}

pub fn media_item_select_sql(where_clause: &str) -> String {
    format!(
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.created_at, media_items.modified_at, COALESCE(user_data.is_favorite, 0) AS is_favorite, COALESCE(user_data.played, 0) AS played, COALESCE(user_data.playback_position_ticks, 0) AS playback_position_ticks, user_data.played_percentage AS played_percentage, COALESCE(user_data.play_count, 0) AS play_count, user_data.last_played_at AS last_played_at FROM media_items LEFT JOIN user_data ON user_data.item_id = media_items.id AND user_data.user_id = ? {where_clause}"#
    )
}

fn recursive_media_item_select_sql() -> String {
    format!(
        r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?)"#,
        media_item_select_sql("").trim()
    )
}

pub fn decode_media_items(rows: Vec<sqlx::any::AnyRow>) -> anyhow::Result<Vec<MediaItem>> {
    rows.into_iter()
        .map(MediaItem::from_row)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode media items")
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
    db: &AnyPool,
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

async fn codec_item_ids(db: &AnyPool, codecs: &[String]) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for codec in codecs {
        let rows = sqlx::query(
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND codec = ?",
        )
        .bind(codec)
        .fetch_all(db)
        .await
        .context("failed to filter by video codec")?;
        for row in rows {
            ids.push(row.try_get("item_id")?);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn width_item_ids(
    db: &AnyPool,
    min_width: Option<i64>,
    max_width: Option<i64>,
) -> anyhow::Result<Vec<String>> {
    let mut query = match (min_width, max_width) {
        (Some(_), Some(_)) => sqlx::query(
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ? AND width <= ?",
        ),
        (Some(_), None) => sqlx::query(
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ?",
        ),
        (None, Some(_)) => sqlx::query(
            "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width <= ?",
        ),
        (None, None) => unreachable!("width_item_ids requires at least one bound"),
    };

    if let Some(min_width) = min_width {
        query = query.bind(min_width);
    }
    if let Some(max_width) = max_width {
        query = query.bind(max_width);
    }

    let rows = query
        .fetch_all(db)
        .await
        .context("failed to filter by width")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get("item_id").ok())
        .collect())
}

async fn parent_item_ids(db: &AnyPool, item_ids: &[String]) -> anyhow::Result<Vec<String>> {
    let mut parents = Vec::new();
    for id in item_ids {
        let rows = sqlx::query("SELECT DISTINCT parent_id FROM linked_children WHERE item_id = ?")
            .bind(id)
            .fetch_all(db)
            .await
            .context("failed to find linked parents")?;
        for row in rows {
            parents.push(row.try_get("parent_id")?);
        }
    }
    parents.sort();
    parents.dedup();
    Ok(parents)
}

async fn relation_item_ids(
    db: &AnyPool,
    table: &str,
    id_column: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut item_ids = Vec::new();
    let sql = format!("SELECT DISTINCT item_id FROM {table} WHERE {id_column} = ?");
    for id in ids {
        let rows = sqlx::query(&sql)
            .bind(id)
            .fetch_all(db)
            .await
            .with_context(|| format!("failed to filter items by {table}"))?;
        for row in rows {
            item_ids.push(row.try_get("item_id")?);
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
    let sort_by = query
        .get("SortBy")
        .map(String::as_str)
        .unwrap_or("SortName");
    let primary_sort = sort_by.split(',').next().unwrap_or("SortName");
    match primary_sort {
        "SortName" => items.sort_by(|a, b| {
            a.title
                .to_ascii_lowercase()
                .cmp(&b.title.to_ascii_lowercase())
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
