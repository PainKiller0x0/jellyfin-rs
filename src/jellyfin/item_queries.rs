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

/// Look up a library by ID and return it as a BaseItemDto-like JSON object.
/// Used when `/Items/{libraryId}` is called — returns the library as a "CollectionFolder".
pub async fn find_library_as_item(
    db: &DatabaseConnection,
    library_id: &str,
) -> anyhow::Result<Option<Value>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, name, collection_type, created_at, updated_at FROM libraries WHERE id = ?",
            vec![library_id.into()],
        ))
        .await
        .context("failed to find library")?;
    match row {
        Some(row) => {
            let id: String = row.get_str("id")?;
            let name: String = row.get_str("name")?;
            let collection_type: String = row.get_str("collection_type")?;
            let created_at: i64 = row.get_i64("created_at")?;
            let updated_at: i64 = row.get_i64("updated_at")?;
            Ok(Some(json!({
                "Name": name,
                "Id": id,
                "CollectionType": collection_type,
                "Type": "CollectionFolder",
                "ServerId": null,
                "Etag": null,
                "Path": null,
                "ParentId": null,
                "LibraryId": null,
                "Overview": null,
                "ProductionYear": null,
                "PremiereDate": null,
                "SortName": name,
                "ProviderIds": {},
                "CanDelete": false,
                "CanDownload": false,
                "HasSubtitles": null,
                "PlayAccess": "Full",
                "IsFolder": true,
                "LocationType": null,
                "MediaSources": [],
                "ImageTags": {},
                "BackdropImageTags": [],
                "Genres": [],
                "GenreItems": [],
                "Tags": [],
                "TagItems": [],
                "Studios": [],
                "People": [],
                "UserData": {
                    "ItemId": id,
                    "Key": id,
                    "Played": false,
                    "IsFavorite": false,
                    "PlayCount": 0,
                    "PlaybackPositionTicks": 0,
                    "PlayedPercentage": null,
                    "Rating": null,
                    "LastPlayedDate": null,
                    "Likes": null,
                    "UnplayedItemCount": null,
                },
                "DateCreated": crate::util::unix_to_jellyfin_date(created_at),
                "DateLastMediaAdded": crate::util::unix_to_jellyfin_date(updated_at),
                "LockData": false,
                "LockedFields": [],
                "ExternalUrls": [],
            })))
        }
        None => Ok(None),
    }
}

pub async fn list_media_items(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<(Vec<MediaItem>, usize)> {
    let has_list_item_ids = query.contains_key("ListItemIds") || query.contains_key("ListItemIds");
    let has_person_ids = query.contains_key("PersonIds") || query.contains_key("personIds");
    let parent_id = query
        .get("ParentId")
        .or_else(|| query.get("parentId"))
        .map(String::as_str);
    let limit = query_u32(query, "Limit", 50).min(200) as usize;
    let offset = query_u32(query, "StartIndex", 0) as usize;
    let recursive = query_bool(query, "Recursive", false);

    let backend = db.get_database_backend();

    let search_term = query
        .get("SearchTerm")
        .filter(|v| !v.is_empty())
        .map(|v| format!("%{}%", v));

    // Special path: query by PersonIds directly (for person filmography pages)
    let items = if has_person_ids && parent_id.is_none() {
        let person_ids = query
            .get("PersonIds")
            .or_else(|| query.get("personIds"))
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if person_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = person_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        // SQL template: LEFT JOIN user_data ON ... AND ud.user_id = ? (1st placeholder)
        //              WHERE mp.person_id IN (?, ...) (next placeholders)
        //              LIMIT ? OFFSET ? (last two placeholders)
        let mut values: Vec<sea_orm::Value> = Vec::new();
        values.push(user_id.into());
        for id in &person_ids {
            values.push(id.as_str().into());
        }
        values.push((limit as i64).into());
        values.push((offset as i64).into());

        let sql = format!(
            "{} WHERE mp.person_id IN ({}) ORDER BY mp.sort_order ASC, mi.title ASC LIMIT ? OFFSET ?",
            media_item_select_sql_from_person(""),
            placeholders,
        );
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
            .context("failed to list items by person ids")?;
        let mut items = decode_media_items(&rows)?;
        apply_item_filters(&mut items, query);
        sort_media_items(&mut items, query);
        let mut items: Vec<_> = items.into_iter().collect();
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
        let total = items.len();
        (items, total)
    } else {
        let has_search = search_term.is_some();
        let has_filters = query.get("Filters").is_some();
        let original_parent_id = parent_id;
        let parent_id = parent_id.unwrap_or("movies");
        let parent_is_collection = if original_parent_id.is_some() {
            is_collection_or_playlist(db, parent_id).await?
        } else {
            false
        };

        let like_clause = search_term
            .as_ref()
            .map(|_| "AND LOWER(media_items.title) LIKE LOWER(?)");

        // When no ParentId is specified (global query), use global search for SearchTerm or Filters
        let needs_global_query = original_parent_id.is_none() && (has_search || has_filters);

        let rows = if parent_is_collection {
            let (sql, vals) = if let Some(like) = like_clause {
                let base = linked_children_select_sql();
                let like_for_alias = like.replace("media_items.", "mi.");
                let sql = base.replace("ORDER BY", &format!("{} ORDER BY", like_for_alias));
                let mut vals: Vec<sea_orm::Value> = vec![user_id.into(), parent_id.into()];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                (sql, vals)
            } else {
                (
                    linked_children_select_sql(),
                    vec![user_id.into(), parent_id.into()],
                )
            };
            db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                .await
        } else if needs_global_query {
            // Global search/filter: no parent_id constraint, use SQL LIKE or just global query
            if has_search {
                let base = media_item_select_sql("WHERE 1=1");
                let sql = base.replace(
                    "ORDER BY",
                    "AND LOWER(media_items.title) LIKE LOWER(?) ORDER BY",
                );
                let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                    .await
            } else {
                // Filters without SearchTerm, no ParentId — global query
                let sql = media_item_select_sql("WHERE 1=1");
                let vals: Vec<sea_orm::Value> = vec![user_id.into()];
                db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                    .await
            }
        } else if recursive {
            let (sql, vals) = if let Some(like) = like_clause {
                let base = recursive_media_item_select_sql();
                let sql = base.replace(
                    "WHERE media_items.id IN",
                    &format!(
                        "WHERE {} AND media_items.id IN",
                        like.trim_start_matches("AND ")
                    ),
                );
                let mut vals: Vec<sea_orm::Value> =
                    vec![parent_id.into(), user_id.into(), parent_id.into()];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                (sql, vals)
            } else {
                (
                    recursive_media_item_select_sql(),
                    vec![parent_id.into(), user_id.into(), parent_id.into()],
                )
            };
            db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                .await
        } else if has_list_item_ids {
            let (sql, vals) = if let Some(like) = like_clause {
                let base = media_item_select_sql("WHERE 1=1");
                let sql = base.replace("ORDER BY", &format!("{} ORDER BY", like));
                let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                (sql, vals)
            } else {
                (media_item_select_sql(""), vec![user_id.into()])
            };
            db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                .await
        } else {
            let (sql, vals) = (
                media_item_select_sql(
                    "WHERE media_items.parent_id = ? AND media_items.is_public = 1 AND (EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = ?) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = ? AND parent.is_public = 1))",
                ),
                vec![
                    user_id.into(),
                    parent_id.into(),
                    parent_id.into(),
                    parent_id.into(),
                ],
            );
            db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
                .await
        }
        .context("failed to list media items")?;
        let mut items = decode_media_items(&rows)?;
        apply_item_filters(&mut items, query);
        apply_relation_filters(db, &mut items, query).await?;
        sort_media_items(&mut items, query);
        let total_count = items.len();
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
        (items, total_count)
    };

    Ok(items)
}

pub async fn list_trailers(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<(Vec<MediaItem>, usize)> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(
                "WHERE media_items.item_type = 'Trailer' ORDER BY media_items.title ASC",
            ),
            vec![user_id.into()],
        ))
        .await
        .context("failed to list trailers")?;

    let mut items = decode_media_items(&rows)?;
    apply_item_filters(&mut items, query);
    apply_relation_filters(db, &mut items, query).await?;
    sort_media_items(&mut items, query);
    let total = items.len();
    let offset = query_u32_any(query, &["StartIndex", "startIndex"], 0) as usize;
    let limit = query_u32_any(query, &["Limit", "limit"], 50).min(200) as usize;
    Ok((items.into_iter().skip(offset).take(limit).collect(), total))
}

/// SQL SELECT for querying items joined through media_people.
/// Includes the media_items columns + user_data join.
fn media_item_select_sql_from_person(where_clause: &str) -> String {
    format!(
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, libraries.collection_type, mi.parent_id, mi.item_type, mi.is_folder, mi.is_public, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number, mi.community_rating, mi.critic_rating, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? LEFT JOIN libraries ON libraries.id = mi.library_id {where_clause}"#
    )
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
    r#"SELECT mi.id, mi.title, mi.path, mi.library_id, libraries.collection_type, mi.parent_id, mi.item_type, mi.is_folder, mi.is_public, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number, mi.community_rating, mi.critic_rating, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? LEFT JOIN libraries ON libraries.id = mi.library_id WHERE lc.parent_id = ? AND mi.is_public = 1 ORDER BY lc.sort_order ASC"#.to_string()
}

pub async fn latest_media_items(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: Option<&str>,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();

    if let Some(pid) = parent_id {
        // Determine the collection type for this library
        let collection_type = db
            .query_one(crate::db::helpers::portable_statement(
                backend,
                "SELECT collection_type FROM libraries WHERE id = ?",
                vec![pid.into()],
            ))
            .await
            .context("failed to get library collection type")?
            .and_then(|r| r.get_opt_str("collection_type").ok().flatten())
            .unwrap_or_default();

        let item_type_filter = match collection_type.as_str() {
            "movies" => "'Movie'",
            "tvshows" => "'Series'",
            _ => "'Movie'",
        };

        let where_clause = format!(
            "WHERE media_items.library_id = ? AND media_items.item_type = {} ORDER BY media_items.modified_at DESC LIMIT 16",
            item_type_filter
        );
        let mut items = decode_media_items(
            &db.query_all(crate::db::helpers::portable_statement(
                backend,
                &media_item_select_sql(&where_clause),
                vec![user_id.into(), pid.into()],
            ))
            .await
            .context("failed to list latest media items")?,
        )?;
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
    } else {
        // No parent specified: query each library separately and merge
        let libraries = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                "SELECT id, collection_type FROM libraries ORDER BY name ASC",
                vec![],
            ))
            .await
            .context("failed to list libraries")?;

        let mut all_items = Vec::new();
        for row in &libraries {
            let lib_id: String = row.get_str("id")?;
            let collection_type = row.get_opt_str("collection_type")?.unwrap_or_default();

            let item_type_filter = match collection_type.as_str() {
                "movies" => "'Movie'",
                "tvshows" => "'Series'",
                _ => continue,
            };

            let where_clause = format!(
                "WHERE media_items.library_id = ? AND media_items.item_type = {} ORDER BY media_items.modified_at DESC LIMIT 8",
                item_type_filter
            );
            let items = decode_media_items(
                &db.query_all(crate::db::helpers::portable_statement(
                    backend,
                    &media_item_select_sql(&where_clause),
                    vec![user_id.into(), lib_id.clone().into()],
                ))
                .await?,
            )?;
            all_items.extend(items);
        }

        // Sort all items by modified_at desc and limit
        all_items.sort_by_key(|i| std::cmp::Reverse(i.modified_at));
        all_items.truncate(16);

        // Batch load image tags
        if !all_items.is_empty() {
            let ids: Vec<String> = all_items.iter().map(|i| i.id.clone()).collect();
            if let Ok(tags_map) = batch_item_image_tags(db, &ids).await {
                for item in &mut all_items {
                    if let Some(tags) = tags_map.get(&item.id) {
                        item.image_tags = Some(tags.clone());
                    }
                }
            }
        }
        Ok(all_items)
    }
}

pub async fn resume_media_items(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let mut items = decode_media_items(
        &db.query_all(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(
                "WHERE media_items.is_folder = 0 AND media_items.item_type <> 'Video' AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC LIMIT 50",
            ),
            vec![user_id.into()],
        ))
        .await
        .context("failed to list resume media items")?,
    )?;
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

pub async fn find_media_item(
    db: &DatabaseConnection,
    user_id: &str,
    id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    find_media_item_with_clause(
        db,
        user_id,
        id,
        "WHERE media_items.id = ? AND media_items.is_public = 1",
    )
    .await
}

pub async fn find_media_item_for_admin(
    db: &DatabaseConnection,
    user_id: &str,
    id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    find_media_item_with_clause(db, user_id, id, "WHERE media_items.id = ?").await
}

async fn find_media_item_with_clause(
    db: &DatabaseConnection,
    user_id: &str,
    id: &str,
    where_clause: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            &media_item_select_sql(where_clause),
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
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.is_public, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.community_rating, media_items.critic_rating, media_items.created_at, media_items.modified_at, COALESCE(user_data.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(user_data.played, CAST(0 AS bigint)) AS played, COALESCE(user_data.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, user_data.played_percentage AS played_percentage, COALESCE(user_data.play_count, CAST(0 AS bigint)) AS play_count, user_data.last_played_at AS last_played_at FROM media_items LEFT JOIN user_data ON user_data.item_id = media_items.id AND user_data.user_id = ? LEFT JOIN libraries ON libraries.id = media_items.library_id {where_clause}"#
    )
}

fn recursive_media_item_select_sql() -> String {
    format!(
        r#"WITH RECURSIVE tree(id) AS (SELECT id FROM media_items WHERE id = ? AND is_public = 1 UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id WHERE media_items.is_public = 1) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.is_public = 1"#,
        media_item_select_sql("").trim()
    )
}

pub fn decode_media_items(rows: &[sea_orm::QueryResult]) -> anyhow::Result<Vec<MediaItem>> {
    Ok(rows
        .iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode media items")?
        .into_iter()
        .filter(|item| item.is_public)
        .collect())
}

pub fn decode_media_items_for_admin(
    rows: &[sea_orm::QueryResult],
) -> anyhow::Result<Vec<MediaItem>> {
    rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode media items")
}

pub(super) async fn batch_item_image_tags(
    db: &DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    use crate::entities::image_assets::{Column, Entity as ImageAssets};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let mut map = HashMap::new();
    for chunk in item_ids.chunks(100) {
        let models = ImageAssets::find()
            .filter(Column::ItemId.is_in(chunk.iter().map(|s| s.as_str())))
            .order_by_asc(Column::ImageType)
            .order_by_asc(Column::ImageIndex)
            .all(db)
            .await?;
        for m in &models {
            let etag = m.etag.as_deref().unwrap_or_default();
            let entry: &mut serde_json::Value = map
                .entry(m.item_id.clone())
                .or_insert_with(|| serde_json::Map::new().into());
            if let Some(obj) = entry.as_object_mut() {
                obj.entry(m.image_type.clone())
                    .or_insert_with(|| json!(etag));
            }
        }
    }
    Ok(map)
}

fn query_u32(query: &HashMap<String, String>, key: &str, default: u32) -> u32 {
    query_u32_any(query, &[key], default)
}

fn query_u32_any(query: &HashMap<String, String>, keys: &[&str], default: u32) -> u32 {
    query
        .iter()
        .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
        .map(|(_, value)| value)
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
    if let Some(search_term) = query
        .get("SearchTerm")
        .or_else(|| query.get("searchTerm"))
        .filter(|value| !value.is_empty())
    {
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
            // Video files are playback-only items in movie libraries, not shown in listings
            if item.item_type == "Video" {
                return false;
            }
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
                        "Movie" | "Series" | "Season" | "Episode"
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

#[cfg(test)]
mod tests {
    use super::{find_media_item, find_media_item_for_admin, query_u32_any};
    use sea_orm::{ConnectionTrait, Database};
    use std::collections::HashMap;

    #[test]
    fn query_u32_any_reads_jellyfin_casing() {
        let mut query = HashMap::new();
        query.insert("startIndex".to_string(), "7".to_string());
        assert_eq!(query_u32_any(&query, &["StartIndex", "startIndex"], 0), 7);
    }

    #[tokio::test]
    async fn find_media_item_hides_private_items_without_admin_bypass() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, ?, 1, 1, 1)",
            vec![
                "private".into(),
                "Private".into(),
                "/tmp/private.mkv".into(),
                0_i64.into(),
            ],
        ))
        .await
        .unwrap();

        assert!(
            find_media_item(&db, "u1", "private")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            find_media_item_for_admin(&db, "u1", "private")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn list_media_items_hides_private_items() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["movies".into(), "Movies".into(), "movies".into()],
        ))
        .await
        .unwrap();
        for (id, is_public) in [("public", 1_i64), ("private", 0_i64)] {
            db.execute(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', 'movies', 'Movie', 0, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}.mkv").into(),
                    is_public.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let mut query = HashMap::new();
        query.insert("ParentId".to_string(), "movies".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].id, "public");
    }

    #[tokio::test]
    async fn list_media_items_requires_visible_media_parent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["movies".into(), "Movies".into(), "movies".into()],
        ))
        .await
        .unwrap();
        for (id, parent_id, is_folder, is_public) in [
            ("private-parent", "movies", 1_i64, 0_i64),
            ("public-child", "private-parent", 0_i64, 1_i64),
        ] {
            db.execute(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', ?, 'Movie', ?, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}").into(),
                    parent_id.into(),
                    is_folder.into(),
                    is_public.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let mut query = HashMap::new();
        query.insert("ParentId".to_string(), "private-parent".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());

        query.insert("Recursive".to_string(), "true".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());
    }
}
