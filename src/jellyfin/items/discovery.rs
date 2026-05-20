use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Value as SeaValue};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
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
    db: &DatabaseConnection,
    item_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let similar_ids = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id WHERE (mg_src.item_id = ? OR mg_src.item_id = (SELECT parent_id FROM media_items WHERE id = ?)) GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT ?"#,
            vec![item_id.into(), item_id.into(), i64::try_from(limit).unwrap_or(i64::MAX).into()],
        ))
        .await
        .context("failed to find similar items")?;

    let similar_ids: Vec<String> = similar_ids
        .iter()
        .filter_map(|r| r.get_opt_str("item_id").ok().flatten())
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
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.created_at, media_items.modified_at, CAST(0 AS bigint) AS is_favorite, CAST(0 AS bigint) AS played, CAST(0 AS bigint) AS playback_position_ticks, NULL AS played_percentage, CAST(0 AS bigint) AS play_count, NULL AS last_played_at FROM media_items LEFT JOIN libraries ON libraries.id = media_items.library_id WHERE media_items.id IN ({placeholders})"#
    );

    let mut values: Vec<SeaValue> = Vec::new();
    for id in &similar_ids {
        values.push(id.as_str().into());
    }
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await
        .context("failed to fetch similar items")?;
    item_queries::decode_media_items(&rows)
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
    db: &DatabaseConnection,
    user_id: &str,
    search_term: &str,
    include_types: Option<Vec<&str>>,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let sql = item_queries::media_item_select_sql(
        "WHERE media_items.is_folder = 0 ORDER BY media_items.title ASC",
    );
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![user_id.into()],
        ))
        .await
        .context("failed to fetch search hints")?;

    let items = item_queries::decode_media_items(&rows)?;
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
    db: &DatabaseConnection,
    user_id: &str,
    series_id: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let (sql, values) = if let Some(series_id) = series_id {
        (
            format!(
                r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND (COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) = 0) AND media_items.parent_id IN (SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Season') ORDER BY media_items.modified_at DESC LIMIT ?"#,
                item_queries::media_item_select_sql("")
            ),
            vec![
                user_id.into(),
                series_id.into(),
                i64::try_from(limit).unwrap_or(25).into(),
            ],
        )
    } else {
        (
            format!(
                r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND (COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) = 0) ORDER BY media_items.modified_at DESC LIMIT ?"#,
                item_queries::media_item_select_sql("")
            ),
            vec![
                user_id.into(),
                i64::try_from(limit).unwrap_or(i64::MAX).into(),
            ],
        )
    };

    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await
        .context("failed to list next up episodes")?;

    item_queries::decode_media_items(&rows)
}

pub async fn shows_missing() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}
