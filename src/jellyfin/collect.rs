use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
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
    db: &DatabaseConnection,
    name: &str,
    ids: &[String],
) -> anyhow::Result<String> {
    let now = now_unix();
    let id = stable_text_id(&format!("boxset:{}:{}", name.to_ascii_lowercase(), now));
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, created_at, modified_at, updated_at) VALUES (?, ?, ?, '', '', 'BoxSet', 1, ?, ?, ?)",
        vec![
            id.clone().into(),
            name.into(),
            id.clone().into(),
            now.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to create collection")?;

    for (index, item_id) in ids.iter().enumerate() {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
            vec![
                id.clone().into(),
                item_id.clone().into(),
                i64::try_from(index).unwrap_or(0).into(),
            ],
        ))
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

    let backend = state.db.get_database_backend();
    let max_order: i64 = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT COALESCE(MAX(sort_order), -1) FROM linked_children WHERE parent_id = ?",
            vec![collection_id.clone().into()],
        ))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get_i64("COALESCE(MAX(sort_order), -1)").ok())
        .unwrap_or(-1);

    for (index, item_id) in ids.iter().enumerate() {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
                vec![
                    collection_id.clone().into(),
                    item_id.clone().into(),
                    (max_order + 1 + i64::try_from(index).unwrap_or(0)).into(),
                ],
            ))
            .await;
    }

    let now = now_unix();
    let _ = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET modified_at = ?, updated_at = ? WHERE id = ?",
            vec![now.into(), now.into(), collection_id.into()],
        ))
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

    let backend = state.db.get_database_backend();
    for item_id in &ids {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM linked_children WHERE parent_id = ? AND item_id = ?",
                vec![collection_id.clone().into(), item_id.clone().into()],
            ))
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JsonValue>,
) -> Response {
    let Some(name) = body
        .get("Name")
        .and_then(JsonValue::as_str)
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
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();

    let media_type = body
        .get("MediaType")
        .and_then(JsonValue::as_str)
        .unwrap_or("Video");

    match create_playlist_inner(&state.db, name, &ids, media_type).await {
        Ok(id) => Json(json!({ "Id": id })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn create_playlist_inner(
    db: &DatabaseConnection,
    name: &str,
    ids: &[String],
    _media_type: &str,
) -> anyhow::Result<String> {
    let now = now_unix();
    let id = stable_text_id(&format!("playlist:{}:{}", name.to_ascii_lowercase(), now));
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, created_at, modified_at, updated_at) VALUES (?, ?, ?, '', '', 'Playlist', 1, ?, ?, ?)",
        vec![
            id.clone().into(),
            name.into(),
            format!("playlist:{id}").into(),
            now.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to create playlist")?;

    for (index, item_id) in ids.iter().enumerate() {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
            vec![
                id.clone().into(),
                item_id.clone().into(),
                i64::try_from(index).unwrap_or(0).into(),
            ],
        ))
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
    Json(body): Json<JsonValue>,
) -> Response {
    let now = now_unix();
    let backend = state.db.get_database_backend();
    if let Some(name) = body
        .get("Name")
        .and_then(JsonValue::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET title = ?, updated_at = ? WHERE id = ? AND item_type = 'Playlist'",
                vec![name.trim().into(), now.into(), playlist_id.clone().into()],
            ))
            .await;
    }

    if let Some(ids) = body.get("Ids").and_then(JsonValue::as_array) {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM linked_children WHERE parent_id = ?",
                vec![playlist_id.clone().into()],
            ))
            .await;
        for (index, id_val) in ids.iter().enumerate() {
            if let Some(item_id) = id_val.as_str() {
                let _ = state
                    .db
                    .execute(crate::db::helpers::portable_statement(
                        backend,
                        "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
                        vec![
                            playlist_id.clone().into(),
                            item_id.into(),
                            i64::try_from(index).unwrap_or(0).into(),
                        ],
                    ))
                    .await;
            }
        }
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET modified_at = ?, updated_at = ? WHERE id = ?",
                vec![now.into(), now.into(), playlist_id.into()],
            ))
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn get_playlist_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
) -> anyhow::Result<Option<JsonValue>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, title FROM media_items WHERE id = ? AND item_type = 'Playlist'",
            vec![playlist_id.into()],
        ))
        .await
        .context("failed to find playlist")?;

    let Some(row) = row else {
        return Ok(None);
    };
    let title: String = row.get_str("title")?;

    let child_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT item_id FROM linked_children WHERE parent_id = ? ORDER BY sort_order ASC",
            vec![playlist_id.into()],
        ))
        .await
        .context("failed to list playlist items")?;

    let item_ids: Vec<String> = child_rows
        .iter()
        .filter_map(|row| row.get_str("item_id").ok())
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

    let backend = state.db.get_database_backend();
    let max_order: i64 = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT COALESCE(MAX(sort_order), -1) FROM linked_children WHERE parent_id = ?",
            vec![playlist_id.clone().into()],
        ))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get_i64("COALESCE(MAX(sort_order), -1)").ok())
        .unwrap_or(-1);

    for (index, item_id) in ids.iter().enumerate() {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
                vec![
                    playlist_id.clone().into(),
                    item_id.clone().into(),
                    (max_order + 1 + i64::try_from(index).unwrap_or(0)).into(),
                ],
            ))
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

    let backend = state.db.get_database_backend();
    for item_id in &ids {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM linked_children WHERE parent_id = ? AND item_id = ?",
                vec![playlist_id.clone().into(), item_id.clone().into()],
            ))
            .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

async fn playlist_items_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Vec<JsonValue>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT mi.id, mi.title, mi.item_type, mi.production_year, mi.runtime_ticks, lc.sort_order FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC LIMIT ? OFFSET ?"#,
            vec![
                playlist_id.into(),
                i64::try_from(limit).unwrap_or(50).into(),
                i64::try_from(offset).unwrap_or(0).into(),
            ],
        ))
        .await
        .context("failed to list playlist items")?;

    Ok(rows
        .iter()
        .map(|row| {
            let id: String = row.get_str("id").unwrap_or_default();
            let title: String = row.get_str("title").unwrap_or_default();
            let sort_order: i64 = row.get_i64("sort_order").unwrap_or_default();
            json!({
                "Id": id,
                "Name": title,
                "PlaylistItemId": id,
                "Type": row.get_str("item_type").unwrap_or_default(),
                "ProductionYear": row.get_opt_i64("production_year").ok().flatten(),
                "RunTimeTicks": row.get_opt_i64("runtime_ticks").ok().flatten(),
                "IndexNumber": sort_order,
            })
        })
        .collect())
}
