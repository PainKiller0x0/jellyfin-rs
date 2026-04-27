use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::Row;

use crate::{
    app::state::AppState,
    jellyfin::{common::internal_error, item_queries},
    library::models::MediaItem,
};

use super::media_list_response;

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
    item_queries::decode_media_items(rows)
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
    let sql = item_queries::media_item_select_sql(
        "WHERE media_items.is_folder = 0 ORDER BY media_items.title ASC",
    );
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .fetch_all(db)
        .await
        .context("failed to fetch search hints")?;

    let items = item_queries::decode_media_items(rows)?;
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
        item_queries::media_item_select_sql("")
    );
    let rows = if let Some(series_id) = series_id {
        sql = format!(
            r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND (COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) = 0) AND media_items.parent_id IN (SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Season') ORDER BY media_items.modified_at DESC LIMIT ?"#,
            item_queries::media_item_select_sql("")
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

    item_queries::decode_media_items(rows)
}

pub async fn shows_missing() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}
