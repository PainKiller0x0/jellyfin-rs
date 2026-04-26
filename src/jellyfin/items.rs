use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::Row;

use crate::{
    app::state::AppState,
    jellyfin::{
        common::internal_error,
        item_queries::{latest_media_items, library_views, list_media_items, resume_media_items},
    },
    library::{models::MediaItem, scanner::scan_media_library},
    util::now_unix,
};

pub use crate::jellyfin::item_queries::find_media_item;

pub async fn views(State(state): State<Arc<AppState>>) -> Response {
    match library_views(&state.db).await {
        Ok(items) => {
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
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match latest_media_items(&state.db, &user_id).await {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|item| item.to_jellyfin_json())
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
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
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

fn media_list_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|item| item.to_jellyfin_json()).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

async fn child_items_by_type(
    db: &sqlx::AnyPool,
    user_id: &str,
    parent_id: &str,
    item_type: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let rows = sqlx::query(&crate::jellyfin::item_queries::media_item_select_sql(
        "WHERE media_items.parent_id = ? AND media_items.item_type = ? ORDER BY media_items.title ASC",
    ))
    .bind(user_id)
    .bind(parent_id)
    .bind(item_type)
    .fetch_all(db)
    .await
    .with_context(|| format!("failed to list {item_type} children for: {parent_id}"))?;
    rows.into_iter()
        .map(MediaItem::from_row)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show child items")
}

async fn descendant_episodes(
    db: &sqlx::AnyPool,
    user_id: &str,
    show_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let rows = sqlx::query(&format!(
        r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.item_type = 'Episode' ORDER BY media_items.title ASC"#,
        crate::jellyfin::item_queries::media_item_select_sql("").trim()
    ))
    .bind(show_id)
    .bind(user_id)
    .bind(show_id)
    .fetch_all(db)
    .await
    .with_context(|| format!("failed to list episodes for show: {show_id}"))?;
    rows.into_iter()
        .map(MediaItem::from_row)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show episodes")
}

pub async fn item_by_id(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, item).await {
            Ok(item) => Json(item).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn item_by_id_public(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = state.user_id.to_string();
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, item).await {
            Ok(item) => Json(item).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response(),
        Err(error) => internal_error(error),
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
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

async fn item_json_with_provider_ids(db: &sqlx::AnyPool, item: MediaItem) -> anyhow::Result<Value> {
    let mut value = item.to_jellyfin_json();
    let rows = sqlx::query("SELECT provider, provider_item_id FROM provider_ids WHERE item_id = ?")
        .bind(&item.id)
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list provider ids for item: {}", item.id))?;
    let provider_ids = rows
        .into_iter()
        .map(|row| -> anyhow::Result<(String, Value)> {
            Ok((
                row.try_get("provider")?,
                Value::String(row.try_get("provider_item_id")?),
            ))
        })
        .collect::<anyhow::Result<serde_json::Map<String, Value>>>()?;
    value["ProviderIds"] = Value::Object(provider_ids);
    value["ImageTags"] = crate::jellyfin::images::item_image_tags(db, &item.id)
        .await
        .context("failed to load image tags")?;
    value["GenreItems"] =
        Value::Array(relation_values(db, "genres", "media_genres", "genre_id", &item.id).await?);
    value["TagItems"] =
        Value::Array(relation_values(db, "tags", "media_tags", "tag_id", &item.id).await?);
    value["Studios"] =
        Value::Array(relation_values(db, "studios", "media_studios", "studio_id", &item.id).await?);
    value["People"] = Value::Array(people_values(db, &item.id).await?);
    Ok(value)
}

async fn relation_values(
    db: &sqlx::AnyPool,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT {table}.id, {table}.name FROM {table} JOIN {relation_table} ON {relation_table}.{relation_column} = {table}.id WHERE {relation_table}.item_id = ? ORDER BY {table}.name ASC"
    );
    let rows = sqlx::query(&sql)
        .bind(item_id)
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list {table} for item: {item_id}"))?;
    rows.into_iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Id": row.try_get::<String, _>("id")?,
                "Name": row.try_get::<String, _>("name")?,
            }))
        })
        .collect()
}

async fn people_values(db: &sqlx::AnyPool, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query("SELECT people.id, people.name, media_people.role, media_people.person_type FROM people JOIN media_people ON media_people.person_id = people.id WHERE media_people.item_id = ? ORDER BY media_people.sort_order ASC, people.name ASC")
        .bind(item_id)
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list people for item: {item_id}"))?;
    rows.into_iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Id": row.try_get::<String, _>("id")?,
                "Name": row.try_get::<String, _>("name")?,
                "Role": row.try_get::<Option<String>, _>("role")?,
                "Type": row.try_get::<Option<String>, _>("person_type")?,
            }))
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

async fn subtitle_list_inner(db: &sqlx::AnyPool, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT stream_index, codec, language, title, is_external FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' ORDER BY stream_index ASC",
    )
    .bind(item_id)
    .fetch_all(db)
    .await
    .context("failed to list subtitles")?;

    rows.into_iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Index": row.try_get::<i64, _>("stream_index")?,
                "Codec": row.try_get::<Option<String>, _>("codec")?,
                "Language": row.try_get::<Option<String>, _>("language")?,
                "DisplayTitle": row.try_get::<Option<String>, _>("title")?,
                "IsExternal": row.try_get::<i64, _>("is_external").unwrap_or_default() != 0,
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
    for item_id in &item_ids {
        let _ = sqlx::query("UPDATE media_items SET overview = NULL, production_year = NULL, updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(item_id)
            .execute(&state.db)
            .await;

        for table in [
            "media_people",
            "media_genres",
            "media_tags",
            "media_studios",
            "provider_ids",
        ] {
            let _ = sqlx::query(&format!("DELETE FROM {table} WHERE item_id = ?"))
                .bind(item_id)
                .execute(&state.db)
                .await;
        }

        crate::jellyfin::system::log_activity(
            &state.db,
            "Metadata reset",
            "MetadataReset",
            None,
            Some(item_id),
        )
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn similar_items(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(12);
    match similar_items_inner(&state.db, &item_id, limit).await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

async fn similar_items_inner(
    db: &sqlx::AnyPool,
    item_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    let similar_ids = sqlx::query(
        r#"SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id WHERE (mg_src.item_id = ? OR mg_src.item_id = (SELECT parent_id FROM media_items WHERE id = ?)) GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT ?"#,
    )
    .bind(item_id)
    .bind(item_id)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await
    .context("failed to find similar items")?;

    let similar_ids: Vec<String> = similar_ids
        .into_iter()
        .filter_map(|r| r.try_get("item_id").ok())
        .collect();

    if similar_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = similar_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.container, media_items.overview, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.created_at, media_items.modified_at, 0 AS is_favorite, 0 AS played, 0 AS playback_position_ticks, NULL AS played_percentage, 0 AS play_count, NULL AS last_played_at FROM media_items WHERE media_items.id IN ({placeholders})"#
    );

    let mut query = sqlx::query(&sql);
    for id in &similar_ids {
        query = query.bind(id);
    }
    let rows = query
        .fetch_all(db)
        .await
        .context("failed to fetch similar items")?;
    crate::jellyfin::item_queries::decode_media_items(rows)
}

pub async fn search_hints(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let search_term = match query.get("SearchTerm").filter(|value| !value.is_empty()) {
        Some(term) => term.to_ascii_lowercase(),
        None => return Json(json!({ "SearchHints": [], "TotalRecordCount": 0 })).into_response(),
    };
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(25);

    let include_types = query
        .get("IncludeItemTypes")
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    match search_hints_inner(&state.db, &user_id, &search_term, include_types, limit).await {
        Ok(hints) => {
            let total = hints.len();
            Json(json!({ "SearchHints": hints, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn search_hints_inner(
    db: &sqlx::AnyPool,
    user_id: &str,
    search_term: &str,
    include_types: Option<Vec<&str>>,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let sql = crate::jellyfin::item_queries::media_item_select_sql(
        "WHERE media_items.is_folder = 0 ORDER BY media_items.title ASC",
    );
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .fetch_all(db)
        .await
        .context("failed to fetch search hints")?;

    let items = crate::jellyfin::item_queries::decode_media_items(rows)?;
    let mut hints: Vec<Value> = items
        .into_iter()
        .filter(|item| {
            item.title.to_ascii_lowercase().contains(search_term)
                && include_types.as_ref().is_none_or(|types| {
                    types
                        .iter()
                        .any(|t| t.eq_ignore_ascii_case(&item.item_type))
                })
        })
        .take(limit)
        .map(|item| {
            json!({
                "Id": item.id,
                "Name": item.title,
                "Type": item.item_type,
                "ProductionYear": item.production_year,
                "RunTimeTicks": item.runtime_ticks,
                "MediaType": item.item_type,
            })
        })
        .collect();

    hints.sort_by(|a, b| {
        a.get("Name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &b.get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
    });

    Ok(hints)
}

pub async fn shows_next_up(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let series_id = query.get("SeriesId").cloned().filter(|v| !v.is_empty());
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(25);
    match next_up_inner(&state.db, &user_id, series_id.as_deref(), limit).await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

async fn next_up_inner(
    db: &sqlx::AnyPool,
    user_id: &str,
    series_id: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    let mut sql = format!(
        r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND (COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) = 0) ORDER BY media_items.modified_at DESC LIMIT ?"#,
        crate::jellyfin::item_queries::media_item_select_sql("")
    );
    let rows = if let Some(series_id) = series_id {
        sql = format!(
            r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND (COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) = 0) AND media_items.parent_id IN (SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Season') ORDER BY media_items.modified_at DESC LIMIT ?"#,
            crate::jellyfin::item_queries::media_item_select_sql("")
        );
        sqlx::query(&sql)
            .bind(user_id)
            .bind(series_id)
            .bind(i64::try_from(limit).unwrap_or(25))
            .fetch_all(db)
            .await
    } else {
        sqlx::query(&sql)
            .bind(user_id)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(db)
            .await
    }
    .context("failed to list next up episodes")?;

    crate::jellyfin::item_queries::decode_media_items(rows)
}

pub async fn shows_missing() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
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
    match sqlx::query(
        r#"INSERT INTO display_preferences (id, user_id, preferences_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET preferences_json = excluded.preferences_json, user_id = excluded.user_id, updated_at = excluded.updated_at"#,
    )
    .bind(&id)
    .bind(user_id)
    .bind(&prefs_json)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

async fn display_preferences_inner(
    db: &sqlx::AnyPool,
    prefs_id: &str,
) -> anyhow::Result<Option<Value>> {
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let row = sqlx::query("SELECT preferences_json FROM display_preferences WHERE id = ?")
        .bind(&id)
        .fetch_optional(db)
        .await
        .context("failed to load display preferences")?;
    match row {
        Some(row) => {
            let json_str: String = row.try_get("preferences_json")?;
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
    db: &sqlx::AnyPool,
    _query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        r#"SELECT library_id, item_type, COUNT(*) AS count FROM media_items WHERE is_folder = 0 GROUP BY library_id, item_type"#,
    )
    .fetch_all(db)
    .await
    .context("failed to count items")?;

    let mut counts = serde_json::Map::new();
    for row in rows {
        let library_id: String = row.try_get("library_id")?;
        let item_type: String = row.try_get("item_type")?;
        let count: i64 = row.try_get("count")?;
        counts
            .entry(library_id)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|obj| {
                let existing = obj.get(&item_type).and_then(Value::as_i64).unwrap_or(0);
                obj.insert(item_type, json!(existing + count));
            });
    }

    Ok(Value::Object(counts))
}

pub async fn scan_handler(State(state): State<Arc<AppState>>) -> Response {
    let start = now_unix();
    let result = scan_media_library(&state).await;
    let end = now_unix();
    let (status, message) = match &result {
        Ok(count) => ("Completed", Some(format!("Scanned {count} items"))),
        Err(error) => ("Failed", Some(format!("{error:#}"))),
    };
    crate::jellyfin::system::upsert_task_result(
        &state.db,
        "scan-library",
        status,
        start,
        end,
        message.as_deref(),
    )
    .await;
    crate::jellyfin::system::log_activity(&state.db, "Library scan", "LibraryScan", None, None)
        .await;
    match result {
        Ok(scanned) => Json(json!({ "Scanned": scanned })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_info(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match item_delete_paths(&state.db, &item_id).await {
        Ok(Some(paths)) => Json(json!({ "Paths": paths })).into_response(),
        Ok(None) => Json(json!({ "Paths": [] })).into_response(),
        Err(error) => internal_error(error),
    }
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

pub async fn remote_search(
    State(state): State<Arc<AppState>>,
    Path(item_type): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let search_info = body.get("SearchInfo").unwrap_or(&body);
    let name = search_info
        .get("Name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Untitled")
        .trim();
    let production_year = search_info.get("Year").and_then(Value::as_i64);
    let provider_ids = search_info
        .get("ProviderIds")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let provider_name = provider_ids
        .as_object()
        .and_then(|providers| providers.keys().next())
        .cloned()
        .unwrap_or_else(|| "jellyfin-rs".to_string());

    if item_type.eq_ignore_ascii_case("Movie") {
        if let Some(api_key) = state.tmdb_api_key.as_deref().filter(|key| !key.is_empty()) {
            match crate::jellyfin::providers::tmdb_movie_search(
                &state.http_client,
                api_key,
                name,
                production_year,
            )
            .await
            {
                Ok(results) if !results.is_empty() => return Json(results).into_response(),
                Ok(_) => {}
                Err(error) => tracing::warn!("TMDb movie search failed: {error:#}"),
            }
        }
    }

    Json(json!([{
        "Name": name,
        "ProductionYear": production_year,
        "SearchProviderName": provider_name,
        "ProviderIds": provider_ids,
        "ImageUrl": null,
        "Overview": format!("Local {item_type} match generated by jellyfin-rs")
    }]))
    .into_response()
}

pub async fn apply_remote_search(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let body = enrich_remote_search_result(&state, body).await;
    let mut update = json!({});
    if let Some(name) = body.get("Name").cloned() {
        update["Name"] = name;
    }
    if let Some(year) = body.get("ProductionYear").cloned() {
        update["ProductionYear"] = year;
    }
    if let Some(overview) = body.get("Overview").cloned() {
        update["Overview"] = overview;
    }
    if let Some(provider_ids) = body.get("ProviderIds").cloned() {
        update["ProviderIds"] = provider_ids;
    }
    for key in ["Genres", "Tags", "Studios", "People"] {
        if let Some(value) = body.get(key).cloned() {
            update[key] = value;
        }
    }

    match update_item_inner(&state.db, &item_id, update).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Item not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

async fn enrich_remote_search_result(state: &AppState, body: Value) -> Value {
    let Some(api_key) = state.tmdb_api_key.as_deref().filter(|key| !key.is_empty()) else {
        return body;
    };
    let Some(tmdb_id) = body
        .get("ProviderIds")
        .and_then(Value::as_object)
        .and_then(|providers| providers.get("Tmdb"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return body;
    };

    match crate::jellyfin::providers::tmdb_movie_details(&state.http_client, api_key, tmdb_id).await
    {
        Ok(details) => merge_remote_search_values(body, details),
        Err(error) => {
            tracing::warn!("TMDb movie details failed: {error:#}");
            body
        }
    }
}

fn merge_remote_search_values(mut base: Value, details: Value) -> Value {
    for key in [
        "Name",
        "Overview",
        "ProductionYear",
        "Genres",
        "Studios",
        "People",
    ] {
        if let Some(value) = details.get(key).filter(|value| !value.is_null()) {
            base[key] = value.clone();
        }
    }

    if let Some(details_providers) = details.get("ProviderIds").and_then(Value::as_object) {
        if !base.get("ProviderIds").is_some_and(Value::is_object) {
            base["ProviderIds"] = json!({});
        }
        for (provider, value) in details_providers {
            if value.as_str().is_some_and(|value| value.is_empty()) {
                continue;
            }
            base["ProviderIds"][provider] = value.clone();
        }
    }

    base
}

pub async fn delete_items(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = query
        .get("Ids")
        .map(String::as_str)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    match delete_items_inner(&state.db, &ids).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    match update_item_inner(&state.db, &item_id, body).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => Json(json!({ "Error": "Item not found" })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_delete_paths(
    db: &sqlx::AnyPool,
    item_id: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let rows = descendant_item_rows(db, item_id).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .map(|(_, path)| path)
            .filter(|path| !path.is_empty())
            .collect(),
    ))
}

async fn delete_items_inner(db: &sqlx::AnyPool, ids: &[&str]) -> anyhow::Result<()> {
    let mut deleted = 0u64;
    for id in ids {
        let rows = descendant_item_rows(db, id).await?;
        for (item_id, _) in rows.into_iter().rev() {
            delete_item_records(db, &item_id).await?;
            deleted += 1;
        }
    }
    crate::jellyfin::system::log_activity(
        db,
        &format!("Deleted {deleted} media items"),
        "MediaDeletion",
        None,
        None,
    )
    .await;
    Ok(())
}

async fn descendant_item_rows(
    db: &sqlx::AnyPool,
    item_id: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    let rows = sqlx::query(r#"WITH RECURSIVE tree(id, path) AS (SELECT id, path FROM media_items WHERE id = ? UNION ALL SELECT media_items.id, media_items.path FROM media_items JOIN tree ON media_items.parent_id = tree.id) SELECT id, path FROM tree"#)
        .bind(item_id)
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list delete paths for item: {item_id}"))?;
    rows.into_iter()
        .map(|row| Ok((row.try_get("id")?, row.try_get("path")?)))
        .collect()
}

async fn delete_item_records(db: &sqlx::AnyPool, item_id: &str) -> anyhow::Result<()> {
    for table in [
        "media_streams",
        "user_data",
        "media_people",
        "media_genres",
        "media_tags",
        "media_studios",
        "provider_ids",
        "image_assets",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE item_id = ?"))
            .bind(item_id)
            .execute(db)
            .await
            .with_context(|| format!("failed to delete {table} for item: {item_id}"))?;
    }
    sqlx::query("DELETE FROM media_items WHERE id = ?")
        .bind(item_id)
        .execute(db)
        .await
        .with_context(|| format!("failed to delete media item: {item_id}"))?;
    Ok(())
}

async fn update_item_inner(db: &sqlx::AnyPool, item_id: &str, body: Value) -> anyhow::Result<bool> {
    let now = now_unix();
    let existing =
        sqlx::query("SELECT title, overview, production_year FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(db)
            .await
            .with_context(|| format!("failed to fetch item for update: {item_id}"))?;
    let Some(existing) = existing else {
        return Ok(false);
    };

    let title = body
        .get("Name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .map(ToString::to_string)
        .unwrap_or(existing.try_get("title")?);
    let overview = body
        .get("Overview")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or(existing.try_get("overview")?);
    let production_year = body
        .get("ProductionYear")
        .and_then(Value::as_i64)
        .or(existing.try_get("production_year")?);

    sqlx::query(
        "UPDATE media_items SET title = ?, overview = ?, production_year = ?, updated_at = ? WHERE id = ?",
    )
    .bind(title)
    .bind(overview)
    .bind(production_year)
    .bind(now)
    .bind(item_id)
    .execute(db)
    .await
    .with_context(|| format!("failed to update item metadata: {item_id}"))?;

    if let Some(provider_ids) = body.get("ProviderIds").and_then(Value::as_object) {
        for (provider, provider_item_id) in provider_ids {
            let Some(provider_item_id) =
                provider_item_id.as_str().filter(|value| !value.is_empty())
            else {
                continue;
            };
            sqlx::query(r#"INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id"#)
                .bind(item_id)
                .bind(provider)
                .bind(provider_item_id)
                .execute(db)
                .await
                .with_context(|| format!("failed to update provider id for item: {item_id}"))?;
        }
    }

    update_named_relations(
        db,
        item_id,
        "genres",
        "media_genres",
        "genre_id",
        "Genres",
        &body,
    )
    .await?;
    update_named_relations(db, item_id, "tags", "media_tags", "tag_id", "Tags", &body).await?;
    update_named_relations(
        db,
        item_id,
        "studios",
        "media_studios",
        "studio_id",
        "Studios",
        &body,
    )
    .await?;
    update_people(db, item_id, &body).await?;

    Ok(true)
}

async fn update_named_relations(
    db: &sqlx::AnyPool,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    body_key: &str,
    body: &Value,
) -> anyhow::Result<()> {
    let Some(values) = body.get(body_key).and_then(Value::as_array) else {
        return Ok(());
    };
    sqlx::query(&format!("DELETE FROM {relation_table} WHERE item_id = ?"))
        .bind(item_id)
        .execute(db)
        .await
        .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;
    for value in values {
        let Some(name) = value.as_str().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let id =
            crate::util::stable_text_id(&format!("{table}:{}", name.trim().to_ascii_lowercase()));
        sqlx::query(&format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"))
            .bind(&id)
            .bind(name.trim())
            .bind(now_unix())
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert {table}: {name}"))?;
        sqlx::query(&format!("INSERT INTO {relation_table} (item_id, {relation_column}) VALUES (?, ?) ON CONFLICT(item_id, {relation_column}) DO NOTHING"))
            .bind(item_id)
            .bind(id)
            .execute(db)
            .await
            .with_context(|| format!("failed to link {table} to item: {item_id}"))?;
    }
    Ok(())
}

async fn update_people(db: &sqlx::AnyPool, item_id: &str, body: &Value) -> anyhow::Result<()> {
    let Some(values) = body.get("People").and_then(Value::as_array) else {
        return Ok(());
    };
    sqlx::query("DELETE FROM media_people WHERE item_id = ?")
        .bind(item_id)
        .execute(db)
        .await
        .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    for (sort_order, value) in values.iter().enumerate() {
        let Some(name) = value
            .get("Name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let id =
            crate::util::stable_text_id(&format!("people:{}", name.trim().to_ascii_lowercase()));
        let role = value.get("Role").and_then(Value::as_str);
        let person_type = value.get("Type").and_then(Value::as_str).unwrap_or("Actor");
        sqlx::query("INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING")
            .bind(&id)
            .bind(name.trim())
            .bind(now_unix())
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert person: {name}"))?;
        sqlx::query("INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = excluded.role, sort_order = excluded.sort_order")
            .bind(item_id)
            .bind(id)
            .bind(role)
            .bind(person_type)
            .bind(i64::try_from(sort_order).unwrap_or(i64::MAX))
            .execute(db)
            .await
            .with_context(|| format!("failed to link person to item: {item_id}"))?;
    }
    Ok(())
}
