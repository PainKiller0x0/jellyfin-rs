use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{
    app::state::AppState,
    jellyfin::common::internal_error,
    util::{now_unix, stable_text_id},
};

pub async fn create_collection(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query
        .get("name")
        .or_else(|| query.get("Name"))
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string());
    let Some(name) = name else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name is required" })),
        )
            .into_response();
    };
    let ids: Vec<String> = query
        .get("ids")
        .or_else(|| query.get("Ids"))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();

    match create_collection_inner(&state.db, &name, &ids).await {
        Ok(id) => Json(json!({ "Id": id })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn create_collection_inner(
    db: &AnyPool,
    name: &str,
    ids: &[String],
) -> anyhow::Result<String> {
    let now = now_unix();
    let id = stable_text_id(&format!("boxset:{}:{}", name.to_ascii_lowercase(), now));
    sqlx::query(
        "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, created_at, modified_at, updated_at) VALUES (?, ?, ?, '', '', 'BoxSet', 1, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&id)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .context("failed to create collection")?;

    for (index, item_id) in ids.iter().enumerate() {
        sqlx::query(
            "INSERT OR IGNORE INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(item_id)
        .bind(i64::try_from(index).unwrap_or(0))
        .execute(db)
        .await
        .context("failed to link item to collection")?;
    }

    Ok(id)
}

pub async fn add_to_collection(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = query
        .get("ids")
        .or_else(|| query.get("Ids"))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();

    if ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let max_order: i64 = sqlx::query(
        "SELECT COALESCE(MAX(sort_order), -1) FROM linked_children WHERE parent_id = ?",
    )
    .bind(&collection_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get(0).ok())
    .unwrap_or(-1);

    for (index, item_id) in ids.iter().enumerate() {
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
        )
        .bind(&collection_id)
        .bind(item_id)
        .bind(max_order + 1 + i64::try_from(index).unwrap_or(0))
        .execute(&state.db)
        .await;
    }

    let now = now_unix();
    let _ = sqlx::query("UPDATE media_items SET modified_at = ?, updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(now)
        .bind(&collection_id)
        .execute(&state.db)
        .await;

    StatusCode::NO_CONTENT.into_response()
}

pub async fn remove_from_collection(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = query
        .get("ids")
        .or_else(|| query.get("Ids"))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();

    for item_id in &ids {
        let _ = sqlx::query("DELETE FROM linked_children WHERE parent_id = ? AND item_id = ?")
            .bind(&collection_id)
            .bind(item_id)
            .execute(&state.db)
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let Some(name) = body
        .get("Name")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Name is required" })),
        )
            .into_response();
    };

    let ids: Vec<String> = body
        .get("Ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();

    let media_type = body
        .get("MediaType")
        .and_then(Value::as_str)
        .unwrap_or("Video");

    match create_playlist_inner(&state.db, name, &ids, media_type).await {
        Ok(id) => Json(json!({ "Id": id })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn create_playlist_inner(
    db: &AnyPool,
    name: &str,
    ids: &[String],
    _media_type: &str,
) -> anyhow::Result<String> {
    let now = now_unix();
    let id = stable_text_id(&format!("playlist:{}:{}", name.to_ascii_lowercase(), now));
    sqlx::query(
        "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, created_at, modified_at, updated_at) VALUES (?, ?, ?, '', '', 'Playlist', 1, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(format!("playlist:{id}"))
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .context("failed to create playlist")?;

    for (index, item_id) in ids.iter().enumerate() {
        sqlx::query(
            "INSERT OR IGNORE INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(item_id)
        .bind(i64::try_from(index).unwrap_or(0))
        .execute(db)
        .await
        .context("failed to link item to playlist")?;
    }

    Ok(id)
}

pub async fn get_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
) -> Response {
    match get_playlist_inner(&state.db, &playlist_id).await {
        Ok(Some(info)) => Json(info).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let now = now_unix();
    if let Some(name) = body
        .get("Name")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        let _ = sqlx::query("UPDATE media_items SET title = ?, updated_at = ? WHERE id = ? AND item_type = 'Playlist'")
            .bind(name.trim())
            .bind(now)
            .bind(&playlist_id)
            .execute(&state.db)
            .await;
    }

    if let Some(ids) = body.get("Ids").and_then(Value::as_array) {
        let _ = sqlx::query("DELETE FROM linked_children WHERE parent_id = ?")
            .bind(&playlist_id)
            .execute(&state.db)
            .await;
        for (index, id_val) in ids.iter().enumerate() {
            if let Some(item_id) = id_val.as_str() {
                let _ = sqlx::query(
                    "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
                )
                .bind(&playlist_id)
                .bind(item_id)
                .bind(i64::try_from(index).unwrap_or(0))
                .execute(&state.db)
                .await;
            }
        }
        let _ = sqlx::query("UPDATE media_items SET modified_at = ?, updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(now)
            .bind(&playlist_id)
            .execute(&state.db)
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn get_playlist_inner(db: &AnyPool, playlist_id: &str) -> anyhow::Result<Option<Value>> {
    let row =
        sqlx::query("SELECT id, title FROM media_items WHERE id = ? AND item_type = 'Playlist'")
            .bind(playlist_id)
            .fetch_optional(db)
            .await
            .context("failed to find playlist")?;

    let Some(row) = row else {
        return Ok(None);
    };
    let title: String = row.try_get("title")?;

    let child_rows = sqlx::query(
        "SELECT item_id FROM linked_children WHERE parent_id = ? ORDER BY sort_order ASC",
    )
    .bind(playlist_id)
    .fetch_all(db)
    .await
    .context("failed to list playlist items")?;

    let item_ids: Vec<String> = child_rows
        .into_iter()
        .filter_map(|row| row.try_get("item_id").ok())
        .collect();

    Ok(Some(json!({
        "Name": title,
        "Id": playlist_id,
        "OpenAccess": false,
        "Shares": [],
        "ItemIds": item_ids,
    })))
}

pub async fn get_playlist_items(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let offset = query
        .get("StartIndex")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    match playlist_items_inner(&state.db, &playlist_id, offset, limit).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn add_to_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = query
        .get("ids")
        .or_else(|| query.get("Ids"))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();

    if ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let max_order: i64 = sqlx::query(
        "SELECT COALESCE(MAX(sort_order), -1) FROM linked_children WHERE parent_id = ?",
    )
    .bind(&playlist_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get(0).ok())
    .unwrap_or(-1);

    for (index, item_id) in ids.iter().enumerate() {
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
        )
        .bind(&playlist_id)
        .bind(item_id)
        .bind(max_order + 1 + i64::try_from(index).unwrap_or(0))
        .execute(&state.db)
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn remove_from_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids: Vec<String> = query
        .get("ids")
        .or_else(|| query.get("Ids"))
        .or_else(|| query.get("entryIds"))
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();

    for item_id in &ids {
        let _ = sqlx::query("DELETE FROM linked_children WHERE parent_id = ? AND item_id = ?")
            .bind(&playlist_id)
            .bind(item_id)
            .execute(&state.db)
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn playlist_items_inner(
    db: &AnyPool,
    playlist_id: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"SELECT mi.id, mi.title, mi.item_type, mi.production_year, mi.runtime_ticks, lc.sort_order FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC LIMIT ? OFFSET ?"#,
    )
    .bind(playlist_id)
    .bind(i64::try_from(limit).unwrap_or(50))
    .bind(i64::try_from(offset).unwrap_or(0))
    .fetch_all(db)
    .await
    .context("failed to list playlist items")?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let id: String = row.try_get("id").unwrap_or_default();
            let title: String = row.try_get("title").unwrap_or_default();
            let sort_order: i64 = row.try_get("sort_order").unwrap_or_default();
            json!({
                "Id": id,
                "Name": title,
                "PlaylistItemId": id,
                "Type": row.try_get::<String, _>("item_type").unwrap_or_default(),
                "ProductionYear": row.try_get::<Option<i64>, _>("production_year").ok().flatten(),
                "RunTimeTicks": row.try_get::<Option<i64>, _>("runtime_ticks").ok().flatten(),
                "IndexNumber": sort_order,
            })
        })
        .collect())
}
