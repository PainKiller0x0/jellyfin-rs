use std::collections::HashMap;

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
    let parent_id = query
        .get("ParentId")
        .map(String::as_str)
        .unwrap_or("movies");
    let limit = query_u32(query, "Limit", 50).min(200) as usize;
    let offset = query_u32(query, "StartIndex", 0) as usize;
    let recursive = query_bool(query, "Recursive", false);
    let rows = if recursive {
        sqlx::query(&recursive_media_item_select_sql())
            .bind(parent_id)
            .bind(user_id)
            .bind(parent_id)
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
    decode_media_items(sqlx::query(&media_item_select_sql("WHERE media_items.is_folder = 0 AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC LIMIT 50")).bind(user_id).fetch_all(db).await.context("failed to list resume media items")?)
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
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.container, media_items.overview, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.created_at, media_items.modified_at, COALESCE(user_data.is_favorite, 0) AS is_favorite, COALESCE(user_data.played, 0) AS played, COALESCE(user_data.playback_position_ticks, 0) AS playback_position_ticks, user_data.played_percentage AS played_percentage, COALESCE(user_data.play_count, 0) AS play_count, user_data.last_played_at AS last_played_at FROM media_items LEFT JOIN user_data ON user_data.item_id = media_items.id AND user_data.user_id = ? {where_clause}"#
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
    if let Some(containers) = query_insensitive(query, "Containers") {
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
    if let Some(ids) = query_ids(query, "GenreIds") {
        let item_ids = relation_item_ids(db, "media_genres", "genre_id", &ids).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    if let Some(ids) = query_ids(query, "TagIds") {
        let item_ids = relation_item_ids(db, "media_tags", "tag_id", &ids).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    if let Some(ids) = query_ids(query, "PersonIds") {
        let item_ids = relation_item_ids(db, "media_people", "person_id", &ids).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    if let Some(ids) = query_ids(query, "StudioIds") {
        let item_ids = relation_item_ids(db, "media_studios", "studio_id", &ids).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    if let Some(codecs) = query_insensitive(query, "VideoCodecs") {
        let item_ids = codec_item_ids(db, &codecs).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    if let (Some(min), Some(max)) = (
        query.get("MinWidth").and_then(|v| v.parse::<i64>().ok()),
        query.get("MaxWidth").and_then(|v| v.parse::<i64>().ok()),
    ) {
        let item_ids = width_range_item_ids(db, min, max).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    } else if let Some(min) = query.get("MinWidth").and_then(|v| v.parse::<i64>().ok()) {
        let item_ids = min_width_item_ids(db, min).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    } else if let Some(max) = query.get("MaxWidth").and_then(|v| v.parse::<i64>().ok()) {
        let item_ids = max_width_item_ids(db, max).await?;
        items.retain(|item| item_ids.iter().any(|id| id == &item.id));
    }
    Ok(())
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

async fn width_range_item_ids(db: &AnyPool, min: i64, max: i64) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query("SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ? AND width <= ?")
        .bind(min).bind(max).fetch_all(db).await
        .context("failed to filter by width range")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get("item_id").ok())
        .collect())
}

async fn min_width_item_ids(db: &AnyPool, min: i64) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width >= ?",
    )
    .bind(min)
    .fetch_all(db)
    .await
    .context("failed to filter by min width")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get("item_id").ok())
        .collect())
}

async fn max_width_item_ids(db: &AnyPool, max: i64) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT DISTINCT item_id FROM media_streams WHERE stream_type = 'Video' AND width <= ?",
    )
    .bind(max)
    .fetch_all(db)
    .await
    .context("failed to filter by max width")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get("item_id").ok())
        .collect())
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

fn query_insensitive(query: &HashMap<String, String>, key: &str) -> Option<Vec<String>> {
    query_ids(query, key)
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
