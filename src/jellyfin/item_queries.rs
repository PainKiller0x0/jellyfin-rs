use std::collections::{HashMap, HashSet};

use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};

use crate::{db::row_ext::QueryResultExt, library::models::MediaItem, util::stable_text_id};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct LibraryItemCounts {
    movie_count: i64,
    series_count: i64,
    episode_count: i64,
}

pub async fn library_views(db: &DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT l.id, l.name, l.collection_type, l.created_at, l.updated_at,
                      COALESCE(MIN(lp.path), '') AS path
               FROM libraries l
               LEFT JOIN library_paths lp ON lp.library_id = l.id
               GROUP BY l.id, l.name, l.collection_type, l.created_at, l.updated_at
               ORDER BY l.name ASC"#,
            vec![],
        ))
        .await
        .context("failed to list libraries")?;
    let library_ids = rows
        .iter()
        .filter_map(|row| row.get_opt_str("id").ok().flatten())
        .collect::<Vec<_>>();
    let counts = library_item_counts(db, &library_ids).await?;
    let image_tags_by_id = batch_item_image_tags(db, &library_ids)
        .await
        .unwrap_or_default();
    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            let id = row.get_str("id")?;
            let name = row.get_str("name")?;
            let collection_type = row.get_str("collection_type")?;
            let path = row.get_str("path").unwrap_or_default();
            let created_at = row.get_i64("created_at").unwrap_or(0);
            let updated_at = row.get_i64("updated_at").unwrap_or(0);
            let item_counts = counts.get(&id).copied().unwrap_or_default();
            let child_count = library_child_count(&collection_type, item_counts);
            let recursive_count = library_recursive_count(&collection_type, item_counts);
            let image_tags = library_image_tags(&id, image_tags_by_id.get(&id).cloned());
            let primary_image_tag = image_tag(&image_tags, "Primary").unwrap_or_default();
            let backdrop_tags = backdrop_image_tags(&image_tags);
            Ok(json!({
                "Name": name,
                "Id": id,
                "ServerId": "jellyfin-rs",
                "Etag": stable_text_id(&format!("library:{id}:{updated_at}")),
                "CollectionType": collection_type,
                "Type": "CollectionFolder",
                "MediaType": "Unknown",
                "IsFolder": true,
                "Path": path,
                "ParentId": "",
                "LibraryId": id,
                "SortName": name,
                "LocationType": if path.is_empty() { "Virtual" } else { "FileSystem" },
                "CanDelete": false,
                "CanDownload": false,
                "PlayAccess": "Full",
                "ChildCount": child_count,
                "RecursiveItemCount": recursive_count,
                "MovieCount": item_counts.movie_count,
                "SeriesCount": item_counts.series_count,
                "EpisodeCount": item_counts.episode_count,
                "MediaSources": [],
                "MediaSourceCount": 0,
                "PartCount": 0,
                "LocalTrailerCount": 0,
                "ProviderIds": {},
                "ImageTags": image_tags,
                "PrimaryImageTag": primary_image_tag,
                "BackdropImageTags": backdrop_tags,
                "Genres": [],
                "GenreItems": [],
                "Tags": [],
                "TagItems": [],
                "Studios": [],
                "People": [],
                "DateCreated": crate::util::unix_to_jellyfin_date(created_at),
                "DateLastMediaAdded": crate::util::unix_to_jellyfin_date(updated_at),
                "LockData": false,
                "LockedFields": [],
                "ExternalUrls": [],
                "UserData": library_user_data(&id),
            }))
        })
        .collect()
}

/// Look up a library by ID and return it as a BaseItemDto-like JSON object.
/// Used when `/Items/{libraryId}` is called — returns the library as a "CollectionFolder".
pub async fn find_library_as_item(
    db: &DatabaseConnection,
    library_id: &str,
) -> anyhow::Result<Option<Value>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT l.id, l.name, l.collection_type, l.created_at, l.updated_at,
                      COALESCE((SELECT MIN(path) FROM library_paths WHERE library_id = l.id), '') AS path
               FROM libraries l
               WHERE l.id = ?"#,
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
            let path: String = row.get_str("path").unwrap_or_default();
            let counts = library_item_counts(db, std::slice::from_ref(&id)).await?;
            let item_counts = counts.get(&id).copied().unwrap_or_default();
            let child_count = library_child_count(&collection_type, item_counts);
            let recursive_count = library_recursive_count(&collection_type, item_counts);
            let image_tags_by_id = batch_item_image_tags(db, std::slice::from_ref(&id))
                .await
                .unwrap_or_default();
            let image_tags = library_image_tags(&id, image_tags_by_id.get(&id).cloned());
            let primary_image_tag = image_tag(&image_tags, "Primary").unwrap_or_default();
            let backdrop_tags = backdrop_image_tags(&image_tags);
            let mut value = json!({
                "Name": name,
                "Id": id,
                "ServerId": "jellyfin-rs",
                "Etag": stable_text_id(&format!("library:{id}:{updated_at}")),
                "CollectionType": collection_type,
                "Type": "CollectionFolder",
                "MediaType": "Unknown",
                "SortName": name,
                "ProviderIds": {},
                "PlayAccess": "Full",
                "IsFolder": true,
                "Path": path,
                "ParentId": "",
                "LibraryId": id,
                "LocationType": if path.is_empty() { "Virtual" } else { "FileSystem" },
                "ChildCount": child_count,
                "RecursiveItemCount": recursive_count,
                "MovieCount": item_counts.movie_count,
                "SeriesCount": item_counts.series_count,
                "EpisodeCount": item_counts.episode_count,
                "MediaSources": [],
                "MediaSourceCount": 0,
                "PartCount": 0,
                "LocalTrailerCount": 0,
                "ImageTags": image_tags,
                "BackdropImageTags": backdrop_tags,
                "Genres": [],
                "GenreItems": [],
                "Tags": [],
                "TagItems": [],
                "Studios": [],
                "People": [],
                "DateCreated": crate::util::unix_to_jellyfin_date(created_at),
                "DateLastMediaAdded": crate::util::unix_to_jellyfin_date(updated_at),
                "LockData": false,
                "LockedFields": [],
                "ExternalUrls": [],
                "UserData": library_user_data(&id),
                "CanDelete": false,
                "CanDownload": false,
            });
            value["Overview"] = Value::Null;
            value["ProductionYear"] = Value::Null;
            value["PremiereDate"] = Value::Null;
            value["HasSubtitles"] = Value::Null;
            value["PrimaryImageTag"] = json!(primary_image_tag);
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn library_user_data(library_id: &str) -> Value {
    json!({
        "ItemId": library_id,
        "Key": library_id,
        "Played": false,
        "IsFavorite": false,
        "PlayCount": 0,
        "PlaybackPositionTicks": 0,
        "PlayedPercentage": null,
        "Rating": null,
        "LastPlayedDate": null,
        "Likes": null,
        "UnplayedItemCount": null,
    })
}

fn library_image_tag(library_id: &str, image_type: &str) -> String {
    stable_text_id(&format!("library-image:{library_id}:{image_type}"))
}

fn library_image_tags(library_id: &str, tags: Option<Value>) -> Value {
    let mut object = tags
        .and_then(|tags| tags.as_object().cloned())
        .unwrap_or_default();
    object
        .entry("Primary".to_string())
        .or_insert_with(|| json!(library_image_tag(library_id, "Primary")));
    crate::jellyfin::images::add_art_tag_fallback(&mut object);
    Value::Object(object)
}

fn image_tag(tags: &Value, image_type: &str) -> Option<String> {
    tags.get(image_type)
        .and_then(Value::as_str)
        .filter(|tag| !tag.is_empty())
        .map(ToString::to_string)
}

fn backdrop_image_tags(tags: &Value) -> Vec<Value> {
    image_tag(tags, "Backdrop")
        .map(|tag| vec![json!(tag)])
        .unwrap_or_default()
}

async fn library_item_counts(
    db: &DatabaseConnection,
    library_ids: &[String],
) -> anyhow::Result<HashMap<String, LibraryItemCounts>> {
    let mut counts = HashMap::new();
    if library_ids.is_empty() {
        return Ok(counts);
    }

    let placeholders = library_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        r#"SELECT mi.library_id,
                  COUNT(*) FILTER (WHERE mi.item_type = 'Movie') AS movie_count,
                  COUNT(*) FILTER (WHERE mi.item_type = 'Series') AS series_count,
                  COUNT(DISTINCT (mi.parent_id, COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) FILTER (WHERE mi.item_type = 'Episode') AS episode_count
           FROM media_items mi
           WHERE mi.library_id IN ({placeholders}) AND {visible}
           GROUP BY mi.library_id"#
    );
    let values = library_ids
        .iter()
        .map(|id| id.as_str().into())
        .collect::<Vec<sea_orm::Value>>();
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, values))
        .await
        .context("failed to count library items")?;
    for row in &rows {
        let library_id = row.get_str("library_id")?;
        counts.insert(
            library_id,
            LibraryItemCounts {
                movie_count: row.get_i64("movie_count").unwrap_or(0),
                series_count: row.get_i64("series_count").unwrap_or(0),
                episode_count: row.get_i64("episode_count").unwrap_or(0),
            },
        );
    }
    Ok(counts)
}

fn library_child_count(collection_type: &str, counts: LibraryItemCounts) -> i64 {
    match collection_type {
        "movies" => counts.movie_count,
        "tvshows" => counts.series_count,
        _ => counts.movie_count + counts.series_count,
    }
}

fn library_recursive_count(collection_type: &str, counts: LibraryItemCounts) -> i64 {
    match collection_type {
        "movies" => counts.movie_count,
        "tvshows" => counts.episode_count,
        _ => counts.movie_count + counts.series_count + counts.episode_count,
    }
}

pub async fn list_media_items(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<(Vec<MediaItem>, usize)> {
    let has_list_item_ids = query_contains_any(query, &["ListItemIds", "listItemIds"]);
    let has_person_ids = query_contains_any(query, &["PersonIds", "personIds"]);
    let parent_id = query_value_any(query, &["ParentId", "parentId"]);
    let limit = query_u32(query, "Limit", 50).min(200) as usize;
    let offset = query_u32(query, "StartIndex", 0) as usize;
    let recursive = query_bool(query, "Recursive", false);

    let search_term = query
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("SearchTerm"))
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value));

    // Special path: query by PersonIds directly (for person filmography pages)
    let items = if has_person_ids && parent_id.is_none() {
        let person_ids = query
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PersonIds"))
            .map(|(_, value)| {
                value
                    .split(',')
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
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await
            .context("failed to list items by person ids")?;
        let mut items = decode_media_items(&rows)?;
        apply_item_filters(&mut items, query);
        deduplicate_episode_versions(db, &mut items).await;
        sort_media_items(&mut items, query);
        let mut items: Vec<_> = items.into_iter().collect();
        let _ = attach_item_image_tags(db, &mut items).await;
        let total = items.len();
        (items, total)
    } else {
        let has_search = search_term.is_some();
        let has_filters = query_value_any(query, &["Filters", "filters"]).is_some();
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
        let needs_global_query =
            original_parent_id.is_none() && (recursive || has_search || has_filters);

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
            db.query_all(crate::db::helpers::pg_statement( &sql, vals))
                .await
        } else if needs_global_query {
            // Global search/filter: no parent_id constraint, use SQL LIKE or just global query
            if has_search {
                let sql = media_item_select_sql(
                    "WHERE media_items.is_public = 1 AND LOWER(media_items.title) LIKE LOWER(?) ORDER BY media_items.title ASC",
                );
                let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                db.query_all(crate::db::helpers::pg_statement( &sql, vals))
                    .await
            } else {
                // Filters without SearchTerm, no ParentId — global query
                let sql = media_item_select_sql("WHERE 1=1");
                let vals: Vec<sea_orm::Value> = vec![user_id.into()];
                db.query_all(crate::db::helpers::pg_statement( &sql, vals))
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
                    vec![
                        parent_id.into(),
                        parent_id.into(),
                        parent_id.into(),
                        user_id.into(),
                    ];
                vals.push(search_term.as_ref().unwrap().as_str().into());
                vals.push(parent_id.into());
                (sql, vals)
            } else {
                (
                    recursive_media_item_select_sql(),
                    vec![
                        parent_id.into(),
                        parent_id.into(),
                        parent_id.into(),
                        user_id.into(),
                        parent_id.into(),
                    ],
                )
            };
            db.query_all(crate::db::helpers::pg_statement( &sql, vals))
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
            db.query_all(crate::db::helpers::pg_statement( &sql, vals))
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
            db.query_all(crate::db::helpers::pg_statement( &sql, vals))
                .await
        }
        .context("failed to list media items")?;
        let mut items = decode_media_items(&rows)?;
        apply_item_filters(&mut items, query);
        apply_relation_filters(db, &mut items, query).await?;
        deduplicate_episode_versions(db, &mut items).await;
        sort_media_items(&mut items, query);
        let total_count = items.len();
        let mut items: Vec<_> = items.into_iter().skip(offset).take(limit).collect();
        let _ = attach_item_image_tags(db, &mut items).await;
        (items, total_count)
    };

    Ok(items)
}

pub async fn list_trailers(
    db: &DatabaseConnection,
    user_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<(Vec<MediaItem>, usize)> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
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
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, libraries.collection_type, mi.parent_id, mi.item_type, mi.is_folder, mi.is_public, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.premiere_date, mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number, mi.community_rating, mi.critic_rating, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? LEFT JOIN libraries ON libraries.id = mi.library_id {where_clause}"#
    )
}

async fn is_collection_or_playlist(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT item_type FROM media_items WHERE id = ? AND item_type IN ('BoxSet', 'Playlist')",
            vec![item_id.into()],
        ))
        .await
        .context("failed to check item type")?;
    Ok(row.is_some())
}

fn linked_children_select_sql() -> String {
    r#"SELECT mi.id, mi.title, mi.path, mi.library_id, libraries.collection_type, mi.parent_id, mi.item_type, mi.is_folder, mi.is_public, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.premiere_date, mi.runtime_ticks, mi.size_bytes, mi.season_number, mi.episode_number, mi.community_rating, mi.critic_rating, mi.created_at, mi.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ? LEFT JOIN libraries ON libraries.id = mi.library_id WHERE lc.parent_id = ? AND mi.is_public = 1 ORDER BY lc.sort_order ASC"#.to_string()
}

pub async fn latest_media_items(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: Option<&str>,
) -> anyhow::Result<Vec<MediaItem>> {
    if let Some(pid) = parent_id {
        // Determine the collection type for this library
        let collection_type = db
            .query_one(crate::db::helpers::pg_statement(
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
            &db.query_all(crate::db::helpers::pg_statement(
                &media_item_select_sql(&where_clause),
                vec![user_id.into(), pid.into()],
            ))
            .await
            .context("failed to list latest media items")?,
        )?;
        let _ = attach_item_image_tags(db, &mut items).await;
        Ok(items)
    } else {
        // No parent specified: query each library separately and merge
        let libraries = db
            .query_all(crate::db::helpers::pg_statement(
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
                &db.query_all(crate::db::helpers::pg_statement(
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

        let _ = attach_item_image_tags(db, &mut all_items).await;
        Ok(all_items)
    }
}

pub async fn resume_media_items(
    db: &DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let mut items = decode_media_items(
        &db.query_all(crate::db::helpers::pg_statement(
            r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.is_public, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.premiere_date, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.community_rating, media_items.critic_rating, media_items.created_at, media_items.modified_at, COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage AS played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at AS last_played_at
               FROM user_data ud
               JOIN media_items ON media_items.id = ud.item_id
               LEFT JOIN libraries ON libraries.id = media_items.library_id
               WHERE ud.user_id = ?
                 AND ud.playback_position_ticks > 0
                 AND media_items.is_folder = 0
                 AND media_items.item_type <> 'Video'
               ORDER BY ud.updated_at DESC
               LIMIT 500"#,
            vec![user_id.into()],
        ))
        .await
        .context("failed to list resume media items")?,
    )?;
    deduplicate_episode_versions(db, &mut items).await;
    items.truncate(50);
    let _ = attach_item_image_tags(db, &mut items).await;
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
        "WHERE media_items.id = ? AND media_items.is_public = 1 AND (media_items.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = media_items.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = media_items.parent_id AND parent.is_public = 1))",
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

pub async fn find_first_playable_child(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    find_media_item_with_clause(
        db,
        user_id,
        parent_id,
        "WHERE media_items.parent_id = ? AND media_items.is_folder = 0 AND media_items.item_type IN ('Video', 'Episode', 'Audio') AND media_items.is_public = 1 AND (media_items.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = media_items.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = media_items.parent_id AND parent.is_public = 1)) ORDER BY CASE media_items.item_type WHEN 'Video' THEN 0 WHEN 'Episode' THEN 1 WHEN 'Audio' THEN 2 ELSE 3 END, media_items.title ASC LIMIT 1",
    )
    .await
}

async fn find_media_item_with_clause(
    db: &DatabaseConnection,
    user_id: &str,
    id: &str,
    where_clause: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
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
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.is_public, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.premiere_date, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.community_rating, media_items.critic_rating, media_items.created_at, media_items.modified_at, COALESCE(user_data.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(user_data.played, CAST(0 AS bigint)) AS played, COALESCE(user_data.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, user_data.played_percentage AS played_percentage, COALESCE(user_data.play_count, CAST(0 AS bigint)) AS play_count, user_data.last_played_at AS last_played_at FROM media_items LEFT JOIN user_data ON user_data.item_id = media_items.id AND user_data.user_id = ? LEFT JOIN libraries ON libraries.id = media_items.library_id {where_clause}"#
    )
}

fn recursive_media_item_select_sql() -> String {
    format!(
        r#"WITH RECURSIVE tree(id) AS (SELECT id FROM media_items WHERE (id = ? OR (parent_id = ? AND EXISTS (SELECT 1 FROM libraries WHERE libraries.id = ?))) AND is_public = 1 UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id WHERE media_items.is_public = 1) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.is_public = 1"#,
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

pub(crate) async fn batch_item_image_tags<S: AsRef<str>>(
    db: &DatabaseConnection,
    item_ids: &[S],
) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    use crate::entities::image_assets::{Column, Entity as ImageAssets};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let mut map = HashMap::new();
    for chunk in item_ids.chunks(100) {
        let models = ImageAssets::find()
            .filter(Column::ItemId.is_in(chunk.iter().map(|id| id.as_ref())))
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
    for tags in map.values_mut() {
        if let Some(obj) = tags.as_object_mut() {
            crate::jellyfin::images::add_art_tag_fallback(obj);
        }
    }
    Ok(map)
}

pub(crate) async fn attach_item_image_tags(
    db: &DatabaseConnection,
    items: &mut [MediaItem],
) -> anyhow::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let item_ids = items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    let mut tags_map = batch_item_image_tags(db, &item_ids).await?;
    drop(item_ids);
    for item in items {
        if let Some(tags) = tags_map.remove(item.id.as_str()) {
            item.image_tags = Some(tags);
        }
    }
    Ok(())
}

pub(super) async fn batch_item_provider_ids<S: AsRef<str>>(
    db: &DatabaseConnection,
    item_ids: &[S],
) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    let mut map = HashMap::new();
    for chunk in item_ids.chunks(100) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT item_id, provider, provider_item_id FROM provider_ids WHERE item_id IN ({placeholders})"
        );
        let values: Vec<sea_orm::Value> = chunk.iter().map(|id| id.as_ref().into()).collect();
        let rows = db
            .query_all(crate::db::helpers::pg_statement(&sql, values))
            .await?;
        for row in &rows {
            let item_id = row.get_str("item_id").unwrap_or_default();
            let provider = row.get_str("provider").unwrap_or_default();
            let provider_item_id = row.get_str("provider_item_id").unwrap_or_default();
            if item_id.is_empty() || provider.is_empty() || provider_item_id.is_empty() {
                continue;
            }
            let entry: &mut serde_json::Value = map.entry(item_id).or_insert_with(|| json!({}));
            if let Some(obj) = entry.as_object_mut() {
                obj.insert(provider, json!(provider_item_id));
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

fn query_contains_any(query: &HashMap<String, String>, keys: &[&str]) -> bool {
    query
        .keys()
        .any(|candidate| keys.iter().any(|key| candidate.eq_ignore_ascii_case(key)))
}

fn query_value_any<'a>(query: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    query
        .iter()
        .find(|(candidate, _)| keys.iter().any(|key| candidate.eq_ignore_ascii_case(key)))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn query_bool(query: &HashMap<String, String>, key: &str, default: bool) -> bool {
    query
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(default)
}

fn apply_item_filters(items: &mut Vec<MediaItem>, query: &HashMap<String, String>) {
    if let Some(search_term) = query_value_any(query, &["SearchTerm", "searchTerm"]) {
        let search_term = search_term.to_ascii_lowercase();
        items.retain(|item| item.title.to_ascii_lowercase().contains(&search_term));
    }
    if let Some(include_types) = query_value_any(
        query,
        &[
            "IncludeItemTypes",
            "includeItemTypes",
            "IncludeSearchTypes",
            "includeSearchTypes",
        ],
    ) {
        let include_types = include_types.split(',').map(str::trim).collect::<Vec<_>>();
        items.retain(|item| {
            // Video files are playback-only items in movie libraries, not shown in listings
            if item.item_type == "Video" {
                return false;
            }
            include_types
                .iter()
                .any(|item_type| item_type.eq_ignore_ascii_case(&item.item_type))
        });
    }
    if let Some(exclude_types) = query_value_any(query, &["ExcludeItemTypes", "excludeItemTypes"]) {
        let exclude_types = exclude_types.split(',').map(str::trim).collect::<Vec<_>>();
        items.retain(|item| {
            !exclude_types
                .iter()
                .any(|item_type| item_type.eq_ignore_ascii_case(&item.item_type))
        });
    }
    if let Some(media_types) = query_value_any(query, &["MediaTypes", "mediaTypes"]) {
        let media_types = media_types.split(',').map(str::trim).collect::<Vec<_>>();
        for media_type in &media_types {
            if media_type.eq_ignore_ascii_case("Video") {
                items.retain(|item| {
                    matches!(
                        item.item_type.as_str(),
                        "Movie" | "Series" | "Season" | "Episode"
                    )
                });
            } else if media_type.eq_ignore_ascii_case("Audio") {
                items.retain(|item| matches!(item.item_type.as_str(), "Audio" | "MusicAlbum"));
            } else if media_type.eq_ignore_ascii_case("Photo") {
                items.retain(|item| item.item_type == "Photo");
            }
        }
    }
    if let Some(filters) = query_value_any(query, &["Filters", "filters"]) {
        let filters = filters.to_ascii_lowercase();
        if filters.contains("isfavorite") {
            items.retain(|item| item.is_favorite);
        }
        if filters.contains("isplayed") {
            items.retain(|item| item.played);
        }
        if filters.contains("isunplayed") {
            items.retain(|item| !item.played);
        }
        if filters.contains("isresumable") {
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

async fn deduplicate_episode_versions(db: &DatabaseConnection, items: &mut Vec<MediaItem>) {
    if items.len() < 2 || !items.iter().any(|item| item.item_type == "Episode") {
        return;
    }

    let episode_ids = items
        .iter()
        .filter(|item| item.item_type == "Episode")
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if episode_ids.len() < 2 {
        return;
    }

    if let Ok(mut tags_map) = batch_item_image_tags(db, &episode_ids).await {
        for item in items.iter_mut().filter(|item| item.item_type == "Episode") {
            if let Some(tags) = tags_map.remove(item.id.as_str()) {
                item.image_tags = Some(tags);
            }
        }
    }
    let provider_map = batch_item_provider_ids(db, &episode_ids)
        .await
        .unwrap_or_default();

    let mut representative_by_episode: HashMap<(String, i64, i64), usize> = HashMap::new();
    let mut deduped = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        let Some(episode_number) = (item.item_type == "Episode")
            .then_some(item.episode_number)
            .flatten()
        else {
            deduped.push(item);
            continue;
        };

        let key = (
            item.parent_id.clone(),
            item.season_number.unwrap_or(0),
            episode_number,
        );
        if let Some(existing_index) = representative_by_episode.get(&key).copied() {
            if episode_representative_score(&item, &provider_map)
                > episode_representative_score(&deduped[existing_index], &provider_map)
            {
                deduped[existing_index] = item;
            }
        } else {
            representative_by_episode.insert(key, deduped.len());
            deduped.push(item);
        }
    }

    *items = deduped;
}

fn episode_representative_score<'a>(
    item: &'a MediaItem,
    provider_map: &HashMap<String, Value>,
) -> (u8, i64, &'a str) {
    let has_provider = provider_map
        .get(&item.id)
        .and_then(Value::as_object)
        .is_some_and(|providers| !providers.is_empty());
    let has_primary_image = item
        .image_tags
        .as_ref()
        .and_then(|tags| tags.get("Primary"))
        .and_then(Value::as_str)
        .is_some_and(|tag| !tag.is_empty());
    let has_overview = item
        .overview
        .as_deref()
        .is_some_and(|overview| !overview.trim().is_empty());
    let metadata_score =
        (has_provider as u8) * 4 + (has_primary_image as u8) * 2 + (has_overview as u8);
    (
        metadata_score,
        item.size_bytes.unwrap_or_default(),
        item.id.as_str(),
    )
}

async fn codec_item_ids(db: &DatabaseConnection, codecs: &[String]) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    let visible = visible_media_item_sql("media_items");
    let sql = format!(
        "SELECT DISTINCT media_streams.item_id FROM media_streams JOIN media_items ON media_items.id = media_streams.item_id WHERE {visible} AND media_streams.stream_type = 'Video' AND media_streams.codec = ?"
    );
    for codec in codecs {
        let rows = db
            .query_all(crate::db::helpers::pg_statement(
                &sql,
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
    let visible = visible_media_item_sql("media_items");
    let (sql, values) = match (min_width, max_width) {
        (Some(_), Some(_)) => (
            format!(
                "SELECT DISTINCT media_streams.item_id FROM media_streams JOIN media_items ON media_items.id = media_streams.item_id WHERE {visible} AND media_streams.stream_type = 'Video' AND media_streams.width >= ? AND media_streams.width <= ?"
            ),
            vec![min_width.into(), max_width.into()],
        ),
        (Some(_), None) => (
            format!(
                "SELECT DISTINCT media_streams.item_id FROM media_streams JOIN media_items ON media_items.id = media_streams.item_id WHERE {visible} AND media_streams.stream_type = 'Video' AND media_streams.width >= ?"
            ),
            vec![min_width.into()],
        ),
        (None, Some(_)) => (
            format!(
                "SELECT DISTINCT media_streams.item_id FROM media_streams JOIN media_items ON media_items.id = media_streams.item_id WHERE {visible} AND media_streams.stream_type = 'Video' AND media_streams.width <= ?"
            ),
            vec![max_width.into()],
        ),
        (None, None) => unreachable!("width_item_ids requires at least one bound"),
    };

    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, values))
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
    let mut parents = Vec::new();
    let linked_parent_visible = visible_media_item_sql("linked_parent");
    let linked_child_visible = visible_media_item_sql("linked_child");
    let sql = format!(
        "SELECT DISTINCT linked_children.parent_id FROM linked_children JOIN media_items linked_parent ON linked_parent.id = linked_children.parent_id JOIN media_items linked_child ON linked_child.id = linked_children.item_id WHERE {linked_parent_visible} AND {linked_child_visible} AND linked_children.item_id = ?"
    );
    for id in item_ids {
        let rows = db
            .query_all(crate::db::helpers::pg_statement(
                &sql,
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
    let mut item_ids = Vec::new();
    let visible = visible_media_item_sql("media_items");
    let sql = format!(
        "SELECT DISTINCT rel.item_id FROM {table} rel JOIN media_items ON media_items.id = rel.item_id WHERE {visible} AND rel.{id_column} = ?"
    );
    for id in ids {
        let rows = db
            .query_all(crate::db::helpers::pg_statement(
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
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| {
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
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("SortBy"))
        .map(|(_, value)| value.as_str())
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
        "ProductionYear" => items.sort_by(|a, b| {
            a.production_year
                .unwrap_or(i64::MAX)
                .cmp(&b.production_year.unwrap_or(i64::MAX))
                .then_with(|| {
                    a.title
                        .to_ascii_lowercase()
                        .cmp(&b.title.to_ascii_lowercase())
                })
        }),
        "PremiereDate" => items.sort_by(|a, b| {
            a.premiere_date
                .as_deref()
                .unwrap_or("9999-12-31")
                .cmp(b.premiere_date.as_deref().unwrap_or("9999-12-31"))
                .then_with(|| {
                    a.production_year
                        .unwrap_or(i64::MAX)
                        .cmp(&b.production_year.unwrap_or(i64::MAX))
                })
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
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("SortOrder"))
        .is_some_and(|(_, value)| value.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        find_first_playable_child, find_library_as_item, find_media_item,
        find_media_item_for_admin, library_views, query_u32_any, resume_media_items,
    };
    use sea_orm::ConnectionTrait;
    use std::collections::HashMap;

    #[test]
    fn query_u32_any_reads_jellyfin_casing() {
        let mut query = HashMap::new();
        query.insert("startIndex".to_string(), "7".to_string());
        assert_eq!(query_u32_any(&query, &["StartIndex", "startIndex"], 0), 7);
    }

    #[tokio::test]
    async fn find_media_item_hides_private_items_without_admin_bypass() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
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

        for (id, parent_id, is_folder, is_public) in [
            ("private-parent", "", 1_i64, 0_i64),
            ("public-child", "private-parent", 0_i64, 1_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, 'Movie', ?, ?, 1, 1, 1)",
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
        assert!(
            find_media_item(&db, "u1", "public-child")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            find_media_item_for_admin(&db, "u1", "public-child")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn find_first_playable_child_resolves_movie_folder() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES ('movie', 'Movie', '/tmp/movie', '', '', 'Movie', 1, 1, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        for (id, title, item_type, is_public) in [
            ("private-video", "Private", "Video", 0_i64),
            ("public-video", "Public", "Video", 1_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', 'movie', ?, 0, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("/tmp/{id}.mkv").into(),
                    item_type.into(),
                    is_public.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let child = find_first_playable_child(&db, "u1", "movie")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.id, "public-video");
    }

    #[tokio::test]
    async fn library_views_use_uploaded_library_image_tags() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', 1, 1)",
            vec!["movies".into(), "Movies".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, 1)",
            vec!["path-1".into(), "movies".into(), "/media/movies".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, created_at, updated_at) VALUES (?, ?, 'Primary', 0, 'data/images/movies_primary.png', 'uploaded-tag', 1, 1)",
            vec!["image-1".into(), "movies".into()],
        ))
        .await
        .unwrap();

        let views = library_views(&db).await.unwrap();
        assert_eq!(views[0]["ServerId"], "jellyfin-rs");
        assert_eq!(views[0]["Type"], "CollectionFolder");
        assert_eq!(views[0]["Path"], "/media/movies");
        assert_eq!(views[0]["LocationType"], "FileSystem");
        assert_eq!(views[0]["MediaSourceCount"], 0);
        assert_eq!(views[0]["UserData"]["Key"], "movies");
        assert_eq!(views[0]["ImageTags"]["Primary"], "uploaded-tag");
        assert_eq!(views[0]["PrimaryImageTag"], "uploaded-tag");

        let item = find_library_as_item(&db, "movies").await.unwrap().unwrap();
        assert_eq!(item["ServerId"], "jellyfin-rs");
        assert_eq!(item["Path"], "/media/movies");
        assert_eq!(item["LocationType"], "FileSystem");
        assert_eq!(item["MediaSourceCount"], 0);
        assert_eq!(item["ImageTags"]["Primary"], "uploaded-tag");
        assert_eq!(item["PrimaryImageTag"], "uploaded-tag");
    }

    #[tokio::test]
    async fn list_media_items_hides_private_items() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["movies".into(), "Movies".into(), "movies".into()],
        ))
        .await
        .unwrap();
        for (id, is_public) in [("public", 1_i64), ("private", 0_i64)] {
            db.execute(crate::db::helpers::pg_statement(
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
    async fn list_media_items_deduplicates_episode_versions_before_paging() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["tv".into(), "TV".into(), "tvshows".into()],
        ))
        .await
        .unwrap();
        for (id, title, parent_id, item_type, is_folder) in [
            ("series", "Series", "tv", "Series", 1_i64),
            ("season", "Season 1", "series", "Season", 1_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, season_number, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'tv', ?, ?, ?, 1, 1, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("/tmp/{id}").into(),
                    parent_id.into(),
                    item_type.into(),
                    is_folder.into(),
                ],
            ))
            .await
            .unwrap();
        }
        for (id, title, episode_number, size_bytes) in [
            ("episode-1", "Episode 1", 1_i64, 100_i64),
            ("episode-2-metadata", "Episode 2 1080p", 2_i64, 100_i64),
            ("episode-2-large", "Episode 2 2160p", 2_i64, 200_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, season_number, episode_number, size_bytes, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'tv', 'season', 'Episode', 0, 1, 1, ?, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("/tmp/{id}.mkv").into(),
                    episode_number.into(),
                    size_bytes.into(),
                ],
            ))
            .await
            .unwrap();
        }
        crate::db::provider_ids::upsert(&db, "episode-2-metadata", "Tmdb", "episode-2")
            .await
            .unwrap();

        let mut query = HashMap::new();
        query.insert("ParentId".to_string(), "season".to_string());
        query.insert("IncludeItemTypes".to_string(), "Episode".to_string());
        query.insert("Limit".to_string(), "10".to_string());

        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        let ids = items
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<String>>();

        assert_eq!(total, 2);
        assert_eq!(ids, vec!["episode-1", "episode-2-metadata"]);
    }

    #[tokio::test]
    async fn resume_media_items_deduplicates_episode_versions() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'u1', 'u1', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["tv".into(), "TV".into(), "tvshows".into()],
        ))
        .await
        .unwrap();
        for (id, title, parent_id, item_type, is_folder) in [
            ("series", "Series", "tv", "Series", 1_i64),
            ("season", "Season 1", "series", "Season", 1_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, season_number, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'tv', ?, ?, ?, 1, 1, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("/tmp/{id}").into(),
                    parent_id.into(),
                    item_type.into(),
                    is_folder.into(),
                ],
            ))
            .await
            .unwrap();
        }
        for (id, title, size_bytes) in [
            ("episode-1080", "Episode 1 1080p", 100_i64),
            ("episode-2160", "Episode 1 2160p", 200_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, season_number, episode_number, size_bytes, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'tv', 'season', 'Episode', 0, 1, 1, 1, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("/tmp/{id}.mkv").into(),
                    size_bytes.into(),
                ],
            ))
            .await
            .unwrap();
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, updated_at) VALUES ('u1', ?, 0, 1000, 0, 10)",
                vec![id.into()],
            ))
            .await
            .unwrap();
        }

        let items = resume_media_items(&db, "u1").await.unwrap();
        let episode_ids = items
            .iter()
            .filter(|item| item.item_type == "Episode")
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(episode_ids, vec!["episode-2160"]);
    }

    #[tokio::test]
    async fn recursive_items_without_parent_queries_all_libraries() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (library_id, name) in [("lib-a", "A"), ("lib-b", "B")] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', 1, 1)",
                vec![library_id.into(), name.into()],
            ))
            .await
            .unwrap();
        }
        for (id, library_id) in [("movie-a", "lib-a"), ("movie-b", "lib-b")] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'Movie', 0, 1, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}.mkv").into(),
                    library_id.into(),
                    library_id.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let mut query = HashMap::new();
        query.insert("Recursive".to_string(), "true".to_string());
        query.insert("IncludeItemTypes".to_string(), "Movie".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        let ids = items.into_iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(total, 2);
        assert_eq!(ids, vec!["movie-a", "movie-b"]);
    }

    #[tokio::test]
    async fn recursive_items_with_library_parent_starts_from_library_children() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', 1, 1)",
            vec!["movies".into(), "Movies".into()],
        ))
        .await
        .unwrap();
        for (id, parent_id, item_type, is_folder) in [
            ("movie", "movies", "Movie", 1_i64),
            ("video", "movie", "Video", 0_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', ?, ?, ?, 1, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}").into(),
                    parent_id.into(),
                    item_type.into(),
                    is_folder.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let mut query = HashMap::new();
        query.insert("ParentId".to_string(), "movies".to_string());
        query.insert("Recursive".to_string(), "true".to_string());
        query.insert("IncludeItemTypes".to_string(), "Movie".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].id, "movie");

        query.clear();
        query.insert("parentId".to_string(), "movies".to_string());
        query.insert("recursive".to_string(), "true".to_string());
        query.insert("includeItemTypes".to_string(), "Movie".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].id, "movie");
    }

    #[tokio::test]
    async fn stream_filters_ignore_private_items() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, parent_id, is_public, codec, width) in [
            ("public", "", 1_i64, "h264", 1920_i64),
            ("private", "", 0_i64, "hevc", 3840_i64),
            ("hidden-parent", "", 0_i64, "mpeg2video", 1280_i64),
            ("hidden-child", "hidden-parent", 1_i64, "vp9", 4096_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, 'Movie', 0, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}.mkv").into(),
                    parent_id.into(),
                    is_public.into(),
                ],
            ))
            .await
            .unwrap();
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, width, is_external, created_at) VALUES (?, ?, 0, 'Video', ?, ?, 0, 1)",
                vec![
                    format!("{id}-video").into(),
                    id.into(),
                    codec.into(),
                    width.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let mut query = HashMap::new();
        query.insert("VideoCodecs".to_string(), "hevc".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());

        query.clear();
        query.insert("VideoCodecs".to_string(), "vp9".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());

        query.clear();
        query.insert("MinWidth".to_string(), "3000".to_string());
        let (items, total) = super::list_media_items(&db, "u1", &query).await.unwrap();
        assert_eq!(total, 0);
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn list_media_items_requires_visible_media_parent() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["movies".into(), "Movies".into(), "movies".into()],
        ))
        .await
        .unwrap();
        for (id, parent_id, is_folder, is_public) in [
            ("private-parent", "movies", 1_i64, 0_i64),
            ("public-child", "private-parent", 0_i64, 1_i64),
        ] {
            db.execute(crate::db::helpers::pg_statement(
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
