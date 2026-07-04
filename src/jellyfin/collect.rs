use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        auth::request_user_id_and_admin_or_default,
        common::internal_error,
        system::{app_setting, set_app_setting},
    },
    util::{now_unix, stable_text_id},
};

const MAX_COLLECTION_PLAYLIST_NAME_LEN: usize = 256;
const MAX_COLLECTION_PLAYLIST_IDS: usize = 1000;
const MAX_COLLECTION_PLAYLIST_ID_LEN: usize = 256;

/// Filter IDs to only those that exist in media_items.
async fn filter_existing_ids(
    db: &DatabaseConnection,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let backend = db.get_database_backend();
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT id FROM media_items WHERE id IN ({placeholders})");
    let vals: Vec<sea_orm::Value> = ids.iter().map(|id| id.as_str().into()).collect();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(backend, &sql, vals))
        .await
        .context("failed to filter media item ids")?;
    Ok(rows.iter().filter_map(|r| r.get_str("id").ok()).collect())
}

fn ids_query(
    query: &HashMap<String, String>,
    keys: &[&str],
) -> Result<Vec<String>, (StatusCode, &'static str)> {
    normalize_item_ids(
        keys.iter()
            .find_map(|key| query.get(*key))
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    )
}

fn item_ids_from_value(
    value: Option<&JsonValue>,
) -> Result<Vec<String>, (StatusCode, &'static str)> {
    match value {
        Some(JsonValue::Array(items)) => {
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                let Some(id) = item.as_str() else {
                    return Err((StatusCode::BAD_REQUEST, "Ids must contain strings"));
                };
                ids.push(id.to_string());
            }
            normalize_item_ids(ids)
        }
        Some(JsonValue::String(items)) => {
            normalize_item_ids(items.split(',').map(str::to_string).collect())
        }
        Some(JsonValue::Null) | None => Ok(Vec::new()),
        Some(_) => Err((
            StatusCode::BAD_REQUEST,
            "Ids must be an array or CSV string",
        )),
    }
}

fn normalize_item_ids(ids: Vec<String>) -> Result<Vec<String>, (StatusCode, &'static str)> {
    if ids.len() > MAX_COLLECTION_PLAYLIST_IDS {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many item ids"));
    }
    let mut normalized = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() || normalized.iter().any(|existing| existing == id) {
            continue;
        }
        if id.len() > MAX_COLLECTION_PLAYLIST_ID_LEN
            || id.contains('\0')
            || id.chars().any(char::is_control)
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid item id"));
        }
        normalized.push(id.to_string());
    }
    Ok(normalized)
}

fn collection_playlist_name(value: Option<&str>) -> Result<String, (StatusCode, &'static str)> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err((StatusCode::BAD_REQUEST, "Name is required"));
    };
    if value.chars().count() > MAX_COLLECTION_PLAYLIST_NAME_LEN {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Name is too long"));
    }
    if value.contains('\0') || value.chars().any(char::is_control) {
        return Err((StatusCode::BAD_REQUEST, "Invalid name"));
    }
    Ok(value.to_string())
}

fn validation_error_response(error: (StatusCode, &'static str)) -> Response {
    (error.0, Json(json!({ "Error": error.1 }))).into_response()
}

pub async fn create_collection(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = match collection_playlist_name(
        query
            .get("name")
            .or_else(|| query.get("Name"))
            .map(String::as_str),
    ) {
        Ok(name) => name,
        Err(error) => return validation_error_response(error),
    };
    let ids = match ids_query(&query, &["ids", "Ids"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

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

    let valid_ids = filter_existing_ids(db, ids).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        let _ = db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
            vec![
                id.clone().into(),
                item_id.clone().into(),
                i64::try_from(index).unwrap_or(0).into(),
            ],
        ))
        .await;
    }

    Ok(id)
}

pub async fn add_to_collection(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = match ids_query(&query, &["ids", "Ids"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };
    match add_children(&state.db, &collection_id, "BoxSet", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Collection not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn item_collections(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match item_collections_inner(&state.db, &item_id).await {
        Ok(items) => Json(item_collections_result(items)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_collections_inner(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            r#"SELECT mi.id, mi.title, mi.production_year, mi.overview
               FROM linked_children lc
               JOIN media_items mi ON mi.id = lc.parent_id
               WHERE lc.item_id = ? AND mi.item_type = 'BoxSet'
               ORDER BY mi.title ASC"#,
            vec![item_id.into()],
        ))
        .await
        .context("failed to list item collections")?;

    Ok(rows.iter().map(collection_row_json).collect())
}

fn item_collections_result(items: Vec<JsonValue>) -> JsonValue {
    let total = items.len();
    json!({ "Items": items, "TotalRecordCount": total, "StartIndex": 0 })
}

async fn add_children(
    db: &DatabaseConnection,
    parent_id: &str,
    parent_type: &str,
    ids: &[String],
) -> anyhow::Result<bool> {
    if !media_item_exists(db, parent_id, parent_type).await? {
        return Ok(false);
    }
    if ids.is_empty() {
        return Ok(true);
    }
    let valid_ids = filter_existing_ids(db, ids).await?;
    let backend = db.get_database_backend();
    let max_order = max_child_sort_order(db, parent_id).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
            vec![
                parent_id.into(),
                item_id.as_str().into(),
                (max_order + 1 + i64::try_from(index).unwrap_or(0)).into(),
            ],
        ))
        .await
        .context("failed to add linked child")?;
    }
    touch_media_item(db, parent_id).await?;
    Ok(true)
}

async fn remove_children(
    db: &DatabaseConnection,
    parent_id: &str,
    parent_type: &str,
    ids: &[String],
) -> anyhow::Result<bool> {
    if !media_item_exists(db, parent_id, parent_type).await? {
        return Ok(false);
    }
    if ids.is_empty() {
        return Ok(true);
    }
    let backend = db.get_database_backend();
    for item_id in ids {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM linked_children WHERE parent_id = ? AND item_id = ?",
            vec![parent_id.into(), item_id.as_str().into()],
        ))
        .await
        .context("failed to remove linked child")?;
    }
    touch_media_item(db, parent_id).await?;
    Ok(true)
}

async fn media_item_exists(
    db: &DatabaseConnection,
    item_id: &str,
    item_type: &str,
) -> anyhow::Result<bool> {
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT id FROM media_items WHERE id = ? AND item_type = ?",
            vec![item_id.into(), item_type.into()],
        ))
        .await
        .context("failed to find media item")?
        .is_some())
}

async fn max_child_sort_order(db: &DatabaseConnection, parent_id: &str) -> anyhow::Result<i64> {
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT COALESCE(MAX(sort_order), -1) AS max_order FROM linked_children WHERE parent_id = ?",
            vec![parent_id.into()],
        ))
        .await?
        .and_then(|row| row.get_i64("max_order").ok())
        .unwrap_or(-1))
}

async fn touch_media_item(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    let now = now_unix();
    db.execute(crate::db::helpers::portable_statement(
        db.get_database_backend(),
        "UPDATE media_items SET modified_at = ?, updated_at = ? WHERE id = ?",
        vec![now.into(), now.into(), item_id.into()],
    ))
    .await
    .context("failed to update media item timestamp")?;
    Ok(())
}

fn collection_row_json(row: &sea_orm::QueryResult) -> JsonValue {
    let id = row.get_str("id").unwrap_or_default();
    let name = row.get_str("title").unwrap_or_default();
    collection_item_json(
        &id,
        &name,
        row.get_opt_str("overview").ok().flatten(),
        row.get_opt_i64("production_year").ok().flatten(),
    )
}

fn collection_item_json(
    id: &str,
    name: &str,
    overview: Option<String>,
    production_year: Option<i64>,
) -> JsonValue {
    json!({
        "Name": name,
        "Id": id,
        "ServerId": "jellyfin-rs",
        "Type": "BoxSet",
        "IsFolder": true,
        "SortName": name,
        "Overview": overview,
        "ProductionYear": production_year,
        "ImageTags": {},
        "BackdropImageTags": [],
        "ImageBlurHashes": {}
    })
}

#[allow(dead_code)]
pub async fn remove_from_collection(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = match ids_query(&query, &["ids", "Ids"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };
    match remove_children(&state.db, &collection_id, "BoxSet", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Collection not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JsonValue>,
) -> Response {
    if !body.is_object() {
        return validation_error_response((StatusCode::BAD_REQUEST, "Playlist must be an object"));
    }
    let name = match collection_playlist_name(body.get("Name").and_then(JsonValue::as_str)) {
        Ok(name) => name,
        Err(error) => return validation_error_response(error),
    };
    let ids = match item_ids_from_value(body.get("Ids").or_else(|| body.get("ids"))) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

    let media_type = body
        .get("MediaType")
        .and_then(JsonValue::as_str)
        .unwrap_or("Video");

    match create_playlist_inner(&state.db, &name, &ids, media_type).await {
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

    let valid_ids = filter_existing_ids(db, ids).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        let _ = db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?) ON CONFLICT(parent_id, item_id) DO NOTHING",
            vec![
                id.clone().into(),
                item_id.clone().into(),
                i64::try_from(index).unwrap_or(0).into(),
            ],
        ))
        .await;
    }

    Ok(id)
}

pub async fn get_playlist(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match get_playlist_inner(&state.db, &playlist_id, is_admin).await {
        Ok(Some(info)) => Json(info).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn get_playlist_users(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
) -> Response {
    match playlist_users_inner(&state.db, &playlist_id).await {
        Ok(Some(users)) => Json(users).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn get_playlist_user(
    State(state): State<Arc<AppState>>,
    Path((playlist_id, user_id)): Path<(String, String)>,
) -> Response {
    match playlist_user_inner(&state.db, &playlist_id, &user_id).await {
        Ok(Some(user)) => Json(user).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist user not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_playlist_user(
    State(state): State<Arc<AppState>>,
    Path((playlist_id, user_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    if !body.is_object() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match set_playlist_user_permission(&state.db, &playlist_id, &user_id, &body).await {
        Ok(Some(user)) => Json(user).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist user not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn remove_playlist_user(
    State(state): State<Arc<AppState>>,
    Path((playlist_id, user_id)): Path<(String, String)>,
) -> Response {
    match remove_playlist_user_permission(&state.db, &playlist_id, &user_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist user not found" })),
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
    if !body.is_object() {
        return validation_error_response((StatusCode::BAD_REQUEST, "Playlist must be an object"));
    }
    let name = if body.get("Name").is_some() {
        match collection_playlist_name(body.get("Name").and_then(JsonValue::as_str)) {
            Ok(name) => Some(name),
            Err(error) => return validation_error_response(error),
        }
    } else {
        None
    };
    let ids = if body.get("Ids").is_some() || body.get("ids").is_some() {
        match item_ids_from_value(body.get("Ids").or_else(|| body.get("ids"))) {
            Ok(ids) => Some(ids),
            Err(error) => return validation_error_response(error),
        }
    } else {
        None
    };
    match update_playlist_inner(&state.db, &playlist_id, name.as_deref(), ids.as_deref()).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

async fn update_playlist_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    name: Option<&str>,
    ids: Option<&[String]>,
) -> anyhow::Result<bool> {
    if !playlist_exists(db, playlist_id).await? {
        return Ok(false);
    }
    let now = now_unix();
    let backend = db.get_database_backend();
    if let Some(name) = name {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET title = ?, updated_at = ? WHERE id = ? AND item_type = 'Playlist'",
            vec![name.into(), now.into(), playlist_id.into()],
        ))
        .await
        .context("failed to update playlist")?;
    }

    if let Some(ids) = ids {
        let valid_ids = filter_existing_ids(db, &ids).await?;
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM linked_children WHERE parent_id = ?",
            vec![playlist_id.into()],
        ))
        .await
        .context("failed to clear playlist items")?;
        for (index, item_id) in valid_ids.iter().enumerate() {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO linked_children (parent_id, item_id, sort_order) VALUES (?, ?, ?)",
                vec![
                    playlist_id.into(),
                    item_id.as_str().into(),
                    i64::try_from(index).unwrap_or(0).into(),
                ],
            ))
            .await
            .context("failed to insert playlist item")?;
        }
        touch_media_item(db, playlist_id).await?;
    }

    Ok(true)
}

async fn get_playlist_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    include_private: bool,
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
            if include_private {
                "SELECT lc.item_id FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC"
            } else {
                "SELECT lc.item_id FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? AND mi.is_public = 1 ORDER BY lc.sort_order ASC"
            },
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

async fn playlist_users_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
) -> anyhow::Result<Option<Vec<JsonValue>>> {
    if !playlist_exists(db, playlist_id).await? {
        return Ok(None);
    }

    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM users ORDER BY username",
            vec![],
        ))
        .await
        .context("failed to list playlist users")?;

    let permissions = playlist_permissions(db, playlist_id).await;
    Ok(Some(
        rows.iter()
            .filter_map(|row| {
                let id = row.get_str("id").ok()?;
                Some(playlist_user_permissions_json(
                    &id,
                    permissions.get(&id).copied().unwrap_or(true),
                ))
            })
            .collect(),
    ))
}

async fn playlist_user_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<JsonValue>> {
    if !playlist_exists(db, playlist_id).await? {
        return Ok(None);
    }

    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM users WHERE id = ?",
            vec![user_id.into()],
        ))
        .await
        .context("failed to find playlist user")?;

    let permissions = playlist_permissions(db, playlist_id).await;
    Ok(row.map(|_| {
        playlist_user_permissions_json(user_id, permissions.get(user_id).copied().unwrap_or(true))
    }))
}

async fn playlist_exists(db: &DatabaseConnection, playlist_id: &str) -> anyhow::Result<bool> {
    let backend = db.get_database_backend();
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM media_items WHERE id = ? AND item_type = 'Playlist'",
            vec![playlist_id.into()],
        ))
        .await
        .context("failed to find playlist")?
        .is_some())
}

async fn set_playlist_user_permission(
    db: &DatabaseConnection,
    playlist_id: &str,
    user_id: &str,
    body: &JsonValue,
) -> anyhow::Result<Option<JsonValue>> {
    if !playlist_exists(db, playlist_id).await? || !user_exists(db, user_id).await? {
        return Ok(None);
    }
    let mut permissions = playlist_permissions(db, playlist_id).await;
    let can_edit = body
        .get("CanEdit")
        .and_then(JsonValue::as_bool)
        .unwrap_or_else(|| permissions.get(user_id).copied().unwrap_or(true));
    permissions.insert(user_id.to_string(), can_edit);
    save_playlist_permissions(db, playlist_id, &permissions).await?;
    Ok(Some(playlist_user_permissions_json(user_id, can_edit)))
}

async fn remove_playlist_user_permission(
    db: &DatabaseConnection,
    playlist_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    if !playlist_exists(db, playlist_id).await? || !user_exists(db, user_id).await? {
        return Ok(false);
    }
    let mut permissions = playlist_permissions(db, playlist_id).await;
    permissions.remove(user_id);
    save_playlist_permissions(db, playlist_id, &permissions).await?;
    Ok(true)
}

async fn user_exists(db: &DatabaseConnection, user_id: &str) -> anyhow::Result<bool> {
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT id FROM users WHERE id = ?",
            vec![user_id.into()],
        ))
        .await?
        .is_some())
}

async fn playlist_permissions(db: &DatabaseConnection, playlist_id: &str) -> HashMap<String, bool> {
    serde_json::from_str(&app_setting(db, &playlist_permissions_key(playlist_id), "{}").await)
        .unwrap_or_default()
}

async fn save_playlist_permissions(
    db: &DatabaseConnection,
    playlist_id: &str,
    permissions: &HashMap<String, bool>,
) -> anyhow::Result<()> {
    set_app_setting(
        db,
        &playlist_permissions_key(playlist_id),
        &serde_json::to_string(permissions).unwrap_or_else(|_| "{}".to_string()),
    )
    .await
}

fn playlist_permissions_key(playlist_id: &str) -> String {
    format!("playlist_permissions:{playlist_id}")
}

fn playlist_user_permissions_json(user_id: &str, can_edit: bool) -> JsonValue {
    json!({
        "UserId": user_id,
        "CanEdit": can_edit,
    })
}

pub async fn get_playlist_items(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50);
    let offset = query
        .get("StartIndex")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    match playlist_items_inner(&state.db, &playlist_id, offset, limit, is_admin).await {
        Ok((items, total)) => {
            Json(json!({ "Items": items, "TotalRecordCount": total, "StartIndex": offset }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn add_to_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = match ids_query(&query, &["ids", "Ids"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };
    match add_children(&state.db, &playlist_id, "Playlist", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn remove_from_playlist(
    State(state): State<Arc<AppState>>,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = match ids_query(&query, &["ids", "Ids", "entryIds", "EntryIds"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };
    match remove_children(&state.db, &playlist_id, "Playlist", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Playlist not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

async fn playlist_items_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    offset: usize,
    limit: usize,
    include_private: bool,
) -> anyhow::Result<(Vec<JsonValue>, usize)> {
    let backend = db.get_database_backend();
    let count_row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            if include_private {
                "SELECT COUNT(*) AS cnt FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ?"
            } else {
                "SELECT COUNT(*) AS cnt FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? AND mi.is_public = 1"
            },
            vec![playlist_id.into()],
        ))
        .await
        .context("failed to count playlist items")?;
    let total = count_row
        .and_then(|row| row.get_i64("cnt").ok())
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0);

    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            if include_private {
                r#"SELECT mi.id, mi.title, mi.item_type, mi.production_year, mi.runtime_ticks, lc.sort_order FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? ORDER BY lc.sort_order ASC LIMIT ? OFFSET ?"#
            } else {
                r#"SELECT mi.id, mi.title, mi.item_type, mi.production_year, mi.runtime_ticks, lc.sort_order FROM linked_children lc JOIN media_items mi ON mi.id = lc.item_id WHERE lc.parent_id = ? AND mi.is_public = 1 ORDER BY lc.sort_order ASC LIMIT ? OFFSET ?"#
            },
            vec![
                playlist_id.into(),
                i64::try_from(limit).unwrap_or(50).into(),
                i64::try_from(offset).unwrap_or(0).into(),
            ],
        ))
        .await
        .context("failed to list playlist items")?;

    let items = rows
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
        .collect();
    Ok((items, total))
}

/// POST /Collections/{id}/Items/Delete — batch remove items from collection
pub async fn remove_from_collection_batch(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let ids = match item_ids_from_value(body.get("Ids").or_else(|| body.get("ids"))) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

    if ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    match remove_children(&state.db, &collection_id, "BoxSet", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Collection not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// DELETE /Collections/{id}/Items — remove items from collection (query param version)
pub async fn remove_from_collection_delete(
    State(state): State<Arc<AppState>>,
    Path(collection_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let ids = match ids_query(&query, &["ids", "Ids"]) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

    if ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    match remove_children(&state.db, &collection_id, "BoxSet", &ids).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Collection not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_COLLECTION_PLAYLIST_IDS, MAX_COLLECTION_PLAYLIST_NAME_LEN, add_children,
        collection_item_json, collection_playlist_name, get_playlist_inner,
        item_collections_result, item_ids_from_value, normalize_item_ids, playlist_items_inner,
        playlist_user_inner, playlist_user_permissions_json, remove_children,
        set_playlist_user_permission,
    };
    use sea_orm::{ConnectionTrait, Database};
    use serde_json::json;

    #[test]
    fn collection_item_shape_is_boxset() {
        let item =
            collection_item_json("c1", "Collection", Some("overview".to_string()), Some(1999));
        assert_eq!(item["Id"], "c1");
        assert_eq!(item["Name"], "Collection");
        assert_eq!(item["Type"], "BoxSet");
        assert_eq!(item["IsFolder"], true);
        assert_eq!(item["ProductionYear"], 1999);
    }

    #[test]
    fn item_collections_result_has_query_result_shape() {
        let result =
            item_collections_result(vec![collection_item_json("c1", "Collection", None, None)]);
        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["StartIndex"], 0);
        assert_eq!(result["Items"][0]["Id"], "c1");
    }

    #[test]
    fn playlist_user_permissions_shape_matches_jellyfin() {
        let user = playlist_user_permissions_json("u1", false);
        assert_eq!(user["UserId"], "u1");
        assert_eq!(user["CanEdit"], false);
    }

    #[test]
    fn collection_playlist_inputs_are_normalized_and_limited() {
        assert_eq!(
            collection_playlist_name(Some("  Road Trip  ")).unwrap(),
            "Road Trip"
        );
        assert!(collection_playlist_name(Some("bad\nname")).is_err());
        assert!(
            collection_playlist_name(Some(&"x".repeat(MAX_COLLECTION_PLAYLIST_NAME_LEN + 1)))
                .is_err()
        );

        assert_eq!(
            normalize_item_ids(vec![
                " m1 ".to_string(),
                "m1".to_string(),
                "".to_string(),
                "m2".to_string()
            ])
            .unwrap(),
            vec!["m1".to_string(), "m2".to_string()]
        );
        assert_eq!(
            item_ids_from_value(Some(&json!("m1, m2,,m1"))).unwrap(),
            vec!["m1".to_string(), "m2".to_string()]
        );
        assert!(item_ids_from_value(Some(&json!(["m1", 42]))).is_err());
        assert!(normalize_item_ids(vec!["bad\nid".to_string()]).is_err());
        assert!(
            normalize_item_ids(vec!["x".to_string(); MAX_COLLECTION_PLAYLIST_IDS + 1]).is_err()
        );
    }

    #[tokio::test]
    async fn playlist_user_permission_is_persisted() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_user(&db, "u1").await;
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;

        let updated = set_playlist_user_permission(&db, "p1", "u1", &json!({ "CanEdit": false }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated["CanEdit"], false);

        let loaded = playlist_user_inner(&db, "p1", "u1").await.unwrap().unwrap();
        assert_eq!(loaded["CanEdit"], false);
    }

    #[tokio::test]
    async fn playlist_add_remove_children_reports_missing_parent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_media_item(&db, "m1", "Movie", "Movie").await;

        assert!(
            !add_children(&db, "missing", "Playlist", &["m1".to_string()])
                .await
                .unwrap()
        );

        insert_media_item(&db, "p1", "Playlist", "Playlist").await;
        assert!(
            add_children(&db, "p1", "Playlist", &["m1".to_string()])
                .await
                .unwrap()
        );
        let (items, total) = playlist_items_inner(&db, "p1", 0, 10, false).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(total, 1);
        assert!(
            remove_children(&db, "p1", "Playlist", &["m1".to_string()])
                .await
                .unwrap()
        );
        let (items, total) = playlist_items_inner(&db, "p1", 0, 10, false).await.unwrap();
        assert!(items.is_empty());
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn playlist_items_hide_private_children_unless_requested() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;
        insert_media_item_with_visibility(&db, "public", "Public", "Movie", 1).await;
        insert_media_item_with_visibility(&db, "private", "Private", "Movie", 0).await;

        assert!(
            add_children(
                &db,
                "p1",
                "Playlist",
                &["public".to_string(), "private".to_string()]
            )
            .await
            .unwrap()
        );

        let (visible, visible_total) = playlist_items_inner(&db, "p1", 0, 10, false).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible_total, 1);
        assert_eq!(visible[0]["Id"], "public");

        let (all, all_total) = playlist_items_inner(&db, "p1", 0, 10, true).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all_total, 2);

        let playlist = get_playlist_inner(&db, "p1", false).await.unwrap().unwrap();
        assert_eq!(playlist["ItemIds"], json!(["public"]));
        let playlist = get_playlist_inner(&db, "p1", true).await.unwrap().unwrap();
        let mut item_ids = playlist["ItemIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        item_ids.sort_unstable();
        assert_eq!(item_ids, vec!["private", "public"]);
    }

    #[tokio::test]
    async fn playlist_items_total_count_ignores_page_limit() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;
        for id in ["m1", "m2", "m3"] {
            insert_media_item(&db, id, id, "Movie").await;
        }

        assert!(
            add_children(
                &db,
                "p1",
                "Playlist",
                &["m1".to_string(), "m2".to_string(), "m3".to_string()]
            )
            .await
            .unwrap()
        );

        let (items, total) = playlist_items_inner(&db, "p1", 1, 1, false).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["Id"], "m2");
        assert_eq!(total, 3);
    }

    async fn insert_user(db: &sea_orm::DatabaseConnection, id: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec![id.into(), id.into(), id.into()],
        ))
        .await
        .unwrap();
    }

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        item_type: &str,
    ) {
        insert_media_item_with_visibility(db, id, title, item_type, 1).await;
    }

    async fn insert_media_item_with_visibility(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        item_type: &str,
        is_public: i64,
    ) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', ?, 1, ?, 1, 1, 1)",
            vec![
                id.into(),
                title.into(),
                id.into(),
                item_type.into(),
                is_public.into(),
            ],
        ))
        .await
        .unwrap();
    }
}
