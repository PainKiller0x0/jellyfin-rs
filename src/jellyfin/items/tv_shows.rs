use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::{internal_error, strip_nulls},
    library::models::MediaItem,
};

/// Deduplicate episodes by (parent_id, season_number, episode_number).
/// When multiple video files exist for the same episode (multi-version),
/// keep the one with the largest size_bytes (highest quality).
fn deduplicate_episodes(items: Vec<MediaItem>) -> Vec<MediaItem> {
    let mut map: HashMap<(String, i64, i64), MediaItem> = HashMap::new();
    for item in items {
        let key = (
            item.parent_id.clone(),
            item.season_number.unwrap_or(0),
            item.episode_number.unwrap_or(0),
        );
        let should_replace = match map.get(&key) {
            Some(existing) => item.size_bytes > existing.size_bytes,
            None => true,
        };
        if should_replace {
            map.insert(key, item);
        }
    }
    let mut result: Vec<_> = map.into_values().collect();
    result.sort_by(|a, b| {
        a.season_number
            .unwrap_or(0)
            .cmp(&b.season_number.unwrap_or(0))
            .then_with(|| {
                a.episode_number
                    .unwrap_or(0)
                    .cmp(&b.episode_number.unwrap_or(0))
            })
            .then_with(|| a.title.cmp(&b.title))
    });
    result
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
        Ok(items) => {
            let json_items = enrich_season_list(&state.db, &user_id, items).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": json_items.len() }))
                .into_response()
        }
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
        Ok(items) => {
            let json_items = super::enrich_episode_list(&state.db, items).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": json_items.len() }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// Batch-enrich season items with RecursiveItemCount and UnplayedItemCount.
async fn enrich_season_list(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    items: Vec<MediaItem>,
) -> Vec<Value> {
    let season_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    // Collect unique series IDs (parent_id of each season)
    let series_ids: Vec<String> = items
        .iter()
        .map(|i| i.parent_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query: series id -> title
    let mut series_map: HashMap<String, String> = HashMap::new();
    if !series_ids.is_empty() {
        let backend = db.get_database_backend();
        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, title FROM media_items WHERE id IN ({placeholders})");
        let values: Vec<sea_orm::Value> = series_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
        {
            for row in &rows {
                if let (Ok(id), Ok(title)) = (row.get_str("id"), row.get_str("title")) {
                    series_map.insert(id, title);
                }
            }
        }
    }

    // Batch query: count episodes per season
    let mut count_map: HashMap<String, i64> = HashMap::new();
    if !season_ids.is_empty() {
        let backend = db.get_database_backend();
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT parent_id, COUNT(*) AS cnt FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Episode' GROUP BY parent_id"
        );
        let values: Vec<sea_orm::Value> = season_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
        {
            for row in &rows {
                if let (Ok(pid), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    count_map.insert(pid, cnt);
                }
            }
        }
    }

    // Batch query: count played episodes per season for user
    let mut played_map: HashMap<String, i64> = HashMap::new();
    if !season_ids.is_empty() {
        let backend = db.get_database_backend();
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT mi.parent_id, COUNT(*) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id IN ({placeholders}) AND mi.item_type = 'Episode' AND ud.user_id = ? AND ud.played = 1 GROUP BY mi.parent_id"
        );
        let mut values: Vec<sea_orm::Value> =
            season_ids.iter().map(|id| id.as_str().into()).collect();
        values.push(user_id.into());
        if let Ok(rows) = db
            .query_all(crate::db::helpers::portable_statement(
                backend, &sql, values,
            ))
            .await
        {
            for row in &rows {
                if let (Ok(pid), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    played_map.insert(pid, cnt);
                }
            }
        }
    }

    items
        .into_iter()
        .map(|item| {
            let mut val = item.to_jellyfin_json();
            let total = count_map.get(&item.id).copied().unwrap_or(0);
            let played = played_map.get(&item.id).copied().unwrap_or(0);
            val["RecursiveItemCount"] = json!(total);
            val["UserData"]["UnplayedItemCount"] = json!(total - played);
            // Add SeriesId and SeriesName
            val["SeriesId"] = json!(item.parent_id);
            if let Some(series_name) = series_map.get(&item.parent_id) {
                val["SeriesName"] = json!(series_name);
            }
            strip_nulls(val)
        })
        .collect()
}

async fn child_items_by_type(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    parent_id: &str,
    item_type: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let order = if item_type == "Episode" {
        "ORDER BY media_items.season_number ASC, media_items.episode_number ASC"
    } else {
        "ORDER BY media_items.title ASC"
    };
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &crate::jellyfin::item_queries::media_item_select_sql(&format!(
                "WHERE media_items.parent_id = ? AND media_items.item_type = ? {order}"
            )),
            vec![user_id.into(), parent_id.into(), item_type.into()],
        ))
        .await
        .with_context(|| format!("failed to list {item_type} children for: {parent_id}"))?;
    let mut items = rows
        .iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show child items")?;
    // Deduplicate episodes by (parent_id, season_number, episode_number), keep largest file
    if item_type == "Episode" && items.len() > 1 {
        items = deduplicate_episodes(items);
    }
    // Batch load image tags
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        if let Ok(tags_map) = crate::jellyfin::item_queries::batch_item_image_tags(db, &ids).await {
            for item in &mut items {
                if let Some(tags) = tags_map.get(&item.id) {
                    item.image_tags = Some(tags.clone());
                }
            }
        }
    }
    Ok(items)
}

async fn descendant_episodes(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    show_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.item_type = 'Episode' ORDER BY media_items.title ASC"#,
                crate::jellyfin::item_queries::media_item_select_sql("").trim()
            ),
            vec![show_id.into(), user_id.into(), show_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list episodes for show: {show_id}"))?;
    let mut items = rows
        .iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show episodes")?;
    // Deduplicate episodes by (parent_id, season_number, episode_number), keep largest file
    if items.len() > 1 {
        items = deduplicate_episodes(items);
    }
    // Batch load image tags
    if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        if let Ok(tags_map) = crate::jellyfin::item_queries::batch_item_image_tags(db, &ids).await {
            for item in &mut items {
                if let Some(tags) = tags_map.get(&item.id) {
                    item.image_tags = Some(tags.clone());
                }
            }
        }
    }
    Ok(items)
}
