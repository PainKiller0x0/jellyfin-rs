use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
    sea_query::OnConflict,
};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    entities::{
        libraries::{self, Entity as Libraries},
        linked_children::{self, Entity as LinkedChildren},
        media_items::{self, Entity as MediaItems},
        users::{self, Entity as Users},
    },
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
    let rows = MediaItems::find()
        .filter(media_items::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await
        .context("failed to filter media item ids")?;
    let existing = rows.into_iter().map(|item| item.id).collect::<HashSet<_>>();
    Ok(ids
        .iter()
        .filter(|id| existing.contains(*id))
        .cloned()
        .collect())
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

fn query_value(query: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            query
                .iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        })
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn playlist_users_from_value(
    value: Option<&JsonValue>,
) -> Result<HashMap<String, bool>, (StatusCode, &'static str)> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let Some(items) = value.as_array() else {
        return Err((StatusCode::BAD_REQUEST, "Users must be an array"));
    };
    let mut users = HashMap::new();
    for item in items {
        let Some(user_id) = item.get("UserId").and_then(JsonValue::as_str) else {
            return Err((StatusCode::BAD_REQUEST, "UserId is required"));
        };
        let user_id = user_id.trim();
        if user_id.is_empty()
            || user_id.len() > MAX_COLLECTION_PLAYLIST_ID_LEN
            || user_id.contains('\0')
            || user_id.chars().any(char::is_control)
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid UserId"));
        }
        let can_edit = item
            .get("CanEdit")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        users.insert(user_id.to_string(), can_edit);
        if users.len() > MAX_COLLECTION_PLAYLIST_IDS {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many users"));
        }
    }
    Ok(users)
}

fn validation_error_response(error: (StatusCode, &'static str)) -> Response {
    (error.0, Json(json!({ "Error": error.1 }))).into_response()
}

fn playlist_forbidden_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "Error": "Playlist access is denied" })),
    )
        .into_response()
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

    MediaItems::insert(media_items::ActiveModel {
        id: Set(id.clone()),
        title: Set(name.to_string()),
        path: Set(id.clone()),
        library_id: Set(String::new()),
        parent_id: Set(String::new()),
        item_type: Set("BoxSet".to_string()),
        is_folder: Set(1),
        created_at: Set(now),
        modified_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec_without_returning(db)
    .await
    .context("failed to create collection")?;

    let valid_ids = filter_existing_ids(db, ids).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        let _ = LinkedChildren::insert(linked_children::ActiveModel {
            parent_id: Set(id.clone()),
            item_id: Set(item_id.clone()),
            sort_order: Set(i64::try_from(index).unwrap_or(0)),
        })
        .on_conflict(
            OnConflict::columns([
                linked_children::Column::ParentId,
                linked_children::Column::ItemId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
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
    let parent_ids = LinkedChildren::find()
        .filter(linked_children::Column::ItemId.eq(item_id))
        .all(db)
        .await
        .context("failed to list item collections")?
        .into_iter()
        .map(|child| child.parent_id)
        .collect::<Vec<_>>();

    if parent_ids.is_empty() {
        return Ok(Vec::new());
    }

    let collections = MediaItems::find()
        .filter(media_items::Column::Id.is_in(parent_ids))
        .filter(media_items::Column::ItemType.eq("BoxSet"))
        .order_by_asc(media_items::Column::Title)
        .all(db)
        .await
        .context("failed to list item collections")?;

    Ok(collections.iter().map(collection_model_json).collect())
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
    let max_order = max_child_sort_order(db, parent_id).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        LinkedChildren::insert(linked_children::ActiveModel {
            parent_id: Set(parent_id.to_string()),
            item_id: Set(item_id.clone()),
            sort_order: Set(max_order + 1 + i64::try_from(index).unwrap_or(0)),
        })
        .on_conflict(
            OnConflict::columns([
                linked_children::Column::ParentId,
                linked_children::Column::ItemId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
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
    LinkedChildren::delete_many()
        .filter(linked_children::Column::ParentId.eq(parent_id))
        .filter(linked_children::Column::ItemId.is_in(ids.to_vec()))
        .exec(db)
        .await
        .context("failed to remove linked child")?;
    touch_media_item(db, parent_id).await?;
    Ok(true)
}

async fn media_item_exists(
    db: &DatabaseConnection,
    item_id: &str,
    item_type: &str,
) -> anyhow::Result<bool> {
    Ok(MediaItems::find()
        .filter(media_items::Column::Id.eq(item_id))
        .filter(media_items::Column::ItemType.eq(item_type))
        .one(db)
        .await
        .context("failed to find media item")?
        .is_some())
}

async fn max_child_sort_order(db: &DatabaseConnection, parent_id: &str) -> anyhow::Result<i64> {
    Ok(LinkedChildren::find()
        .filter(linked_children::Column::ParentId.eq(parent_id))
        .order_by_desc(linked_children::Column::SortOrder)
        .one(db)
        .await?
        .map(|child| child.sort_order)
        .unwrap_or(-1))
}

async fn touch_media_item(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    let now = now_unix();
    if let Some(item) = MediaItems::find_by_id(item_id.to_string())
        .one(db)
        .await
        .context("failed to find media item for timestamp update")?
    {
        let mut active: media_items::ActiveModel = item.into();
        active.modified_at = Set(now);
        active.updated_at = Set(now);
        active
            .update(db)
            .await
            .context("failed to update media item timestamp")?;
    }
    Ok(())
}

fn collection_model_json(item: &media_items::Model) -> JsonValue {
    collection_item_json(
        &item.id,
        &item.title,
        item.overview.clone(),
        item.production_year,
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
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    if !body.is_object() {
        return validation_error_response((StatusCode::BAD_REQUEST, "Playlist must be an object"));
    }
    let (request_user_id, is_admin) =
        request_user_id_and_admin_or_default(&state, &headers, &query).await;
    let owner_user_id = query_value(&query, &["userId", "UserId"])
        .or_else(|| {
            body.get("UserId")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| request_user_id.clone());
    if owner_user_id != request_user_id && !is_admin {
        return playlist_forbidden_response();
    }

    let name = match collection_playlist_name(
        query_value(&query, &["name", "Name"])
            .as_deref()
            .or_else(|| body.get("Name").and_then(JsonValue::as_str)),
    ) {
        Ok(name) => name,
        Err(error) => return validation_error_response(error),
    };
    let ids = match ids_query(&query, &["ids", "Ids"]).and_then(|query_ids| {
        if query_ids.is_empty() {
            item_ids_from_value(body.get("Ids").or_else(|| body.get("ids")))
        } else {
            Ok(query_ids)
        }
    }) {
        Ok(ids) => ids,
        Err(error) => return validation_error_response(error),
    };

    let media_type = query_value(&query, &["mediaType", "MediaType"])
        .or_else(|| {
            body.get("MediaType")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Video".to_string());
    let playlist_users = match playlist_users_from_value(body.get("Users")) {
        Ok(users) => users,
        Err(error) => return validation_error_response(error),
    };

    match create_playlist_inner(
        &state.db,
        &name,
        &ids,
        &media_type,
        &owner_user_id,
        playlist_users,
    )
    .await
    {
        Ok(id) => Json(json!({ "Id": id })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn create_playlist_inner(
    db: &DatabaseConnection,
    name: &str,
    ids: &[String],
    _media_type: &str,
    owner_user_id: &str,
    mut users: HashMap<String, bool>,
) -> anyhow::Result<String> {
    let now = now_unix();
    let id = stable_text_id(&format!("playlist:{}:{}", name.to_ascii_lowercase(), now));

    MediaItems::insert(media_items::ActiveModel {
        id: Set(id.clone()),
        title: Set(name.to_string()),
        path: Set(format!("playlist:{id}")),
        library_id: Set(String::new()),
        parent_id: Set(String::new()),
        item_type: Set("Playlist".to_string()),
        is_folder: Set(1),
        created_at: Set(now),
        modified_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec_without_returning(db)
    .await
    .context("failed to create playlist")?;

    let valid_ids = filter_existing_ids(db, ids).await?;
    for (index, item_id) in valid_ids.iter().enumerate() {
        let _ = LinkedChildren::insert(linked_children::ActiveModel {
            parent_id: Set(id.clone()),
            item_id: Set(item_id.clone()),
            sort_order: Set(i64::try_from(index).unwrap_or(0)),
        })
        .on_conflict(
            OnConflict::columns([
                linked_children::Column::ParentId,
                linked_children::Column::ItemId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(db)
        .await;
    }

    if !owner_user_id.trim().is_empty() {
        users.insert(owner_user_id.to_string(), true);
    }
    if !users.is_empty() {
        save_playlist_permissions(db, &id, &users).await?;
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
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }
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
    headers: HeaderMap,
    Path((playlist_id, user_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }
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
    headers: HeaderMap,
    Path((playlist_id, user_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    if !body.is_object() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
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
    headers: HeaderMap,
    Path((playlist_id, user_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }
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
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    if !body.is_object() {
        return validation_error_response((StatusCode::BAD_REQUEST, "Playlist must be an object"));
    }
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
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
    let users = if body.get("Users").is_some() {
        match playlist_users_from_value(body.get("Users")) {
            Ok(users) => Some(users),
            Err(error) => return validation_error_response(error),
        }
    } else {
        None
    };
    match update_playlist_inner(
        &state.db,
        &playlist_id,
        name.as_deref(),
        ids.as_deref(),
        users,
    )
    .await
    {
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
    users: Option<HashMap<String, bool>>,
) -> anyhow::Result<bool> {
    if !playlist_exists(db, playlist_id).await? {
        return Ok(false);
    }
    let now = now_unix();
    if let Some(name) = name {
        if let Some(item) = MediaItems::find()
            .filter(media_items::Column::Id.eq(playlist_id))
            .filter(media_items::Column::ItemType.eq("Playlist"))
            .one(db)
            .await
            .context("failed to find playlist for update")?
        {
            let mut active: media_items::ActiveModel = item.into();
            active.title = Set(name.to_string());
            active.updated_at = Set(now);
            active
                .update(db)
                .await
                .context("failed to update playlist")?;
        }
    }

    if let Some(ids) = ids {
        let valid_ids = filter_existing_ids(db, ids).await?;
        LinkedChildren::delete_many()
            .filter(linked_children::Column::ParentId.eq(playlist_id))
            .exec(db)
            .await
            .context("failed to clear playlist items")?;
        for (index, item_id) in valid_ids.iter().enumerate() {
            LinkedChildren::insert(linked_children::ActiveModel {
                parent_id: Set(playlist_id.to_string()),
                item_id: Set(item_id.clone()),
                sort_order: Set(i64::try_from(index).unwrap_or(0)),
            })
            .exec_without_returning(db)
            .await
            .context("failed to insert playlist item")?;
        }
        touch_media_item(db, playlist_id).await?;
    }

    if let Some(users) = users {
        save_playlist_permissions(db, playlist_id, &users).await?;
        touch_media_item(db, playlist_id).await?;
    }

    Ok(true)
}

async fn get_playlist_inner(
    db: &DatabaseConnection,
    playlist_id: &str,
    include_private: bool,
) -> anyhow::Result<Option<JsonValue>> {
    let playlist = MediaItems::find()
        .filter(media_items::Column::Id.eq(playlist_id))
        .filter(media_items::Column::ItemType.eq("Playlist"))
        .one(db)
        .await
        .context("failed to find playlist")?;

    let Some(playlist) = playlist else {
        return Ok(None);
    };

    let item_ids: Vec<String> = playlist_item_models(db, playlist_id, include_private)
        .await
        .context("failed to list playlist items")?
        .iter()
        .map(|(item, _)| item.id.clone())
        .collect();

    let permissions = playlist_permissions(db, playlist_id).await;
    let shares = permissions
        .iter()
        .map(|(user_id, can_edit)| playlist_user_permissions_json(user_id, *can_edit))
        .collect::<Vec<_>>();

    Ok(Some(json!({
        "Name": playlist.title,
        "Id": playlist_id,
        "OpenAccess": false,
        "Shares": shares,
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
    let users = Users::find()
        .order_by_asc(users::Column::Username)
        .all(db)
        .await
        .context("failed to list playlist users")?;

    let permissions = playlist_permissions(db, playlist_id).await;
    Ok(Some(
        users
            .iter()
            .map(|user| {
                playlist_user_permissions_json(
                    &user.id,
                    permissions.get(&user.id).copied().unwrap_or(false),
                )
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
    let user = Users::find_by_id(user_id.to_string())
        .one(db)
        .await
        .context("failed to find playlist user")?;

    let permissions = playlist_permissions(db, playlist_id).await;
    Ok(user.map(|_| {
        playlist_user_permissions_json(user_id, permissions.get(user_id).copied().unwrap_or(false))
    }))
}

async fn playlist_exists(db: &DatabaseConnection, playlist_id: &str) -> anyhow::Result<bool> {
    media_item_exists(db, playlist_id, "Playlist").await
}

pub(crate) async fn playlist_write_access(
    db: &DatabaseConnection,
    playlist_id: &str,
    user_id: &str,
    is_admin: bool,
) -> anyhow::Result<Option<bool>> {
    if !playlist_exists(db, playlist_id).await? {
        return Ok(None);
    }
    if is_admin {
        return Ok(Some(true));
    }
    Ok(Some(
        playlist_permissions(db, playlist_id)
            .await
            .get(user_id)
            .copied()
            .unwrap_or(false),
    ))
}

async fn playlist_write_access_from_request(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    playlist_id: &str,
) -> anyhow::Result<Option<bool>> {
    let (user_id, is_admin) = request_user_id_and_admin_or_default(state, headers, query).await;
    playlist_write_access(&state.db, playlist_id, &user_id, is_admin).await
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
        .unwrap_or_else(|| permissions.get(user_id).copied().unwrap_or(false));
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
    Ok(Users::find_by_id(user_id.to_string())
        .one(db)
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
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }
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
    headers: HeaderMap,
    Path(playlist_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match playlist_write_access_from_request(&state, &headers, &query, &playlist_id).await {
        Ok(Some(true)) => {}
        Ok(Some(false)) => return playlist_forbidden_response(),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": "Playlist not found" })),
            )
                .into_response();
        }
        Err(error) => return internal_error(error),
    }
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
    let playlist_items = playlist_item_models(db, playlist_id, include_private)
        .await
        .context("failed to list playlist items")?;
    let total = playlist_items.len();
    let items = playlist_items
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(item, sort_order)| playlist_item_json(&item, sort_order))
        .collect();
    Ok((items, total))
}

async fn playlist_item_models(
    db: &DatabaseConnection,
    playlist_id: &str,
    include_private: bool,
) -> anyhow::Result<Vec<(media_items::Model, i64)>> {
    let children = LinkedChildren::find()
        .filter(linked_children::Column::ParentId.eq(playlist_id))
        .order_by_asc(linked_children::Column::SortOrder)
        .all(db)
        .await?;
    if children.is_empty() {
        return Ok(Vec::new());
    }

    let item_ids = children
        .iter()
        .map(|child| child.item_id.clone())
        .collect::<Vec<_>>();
    let mut items_by_id = MediaItems::find()
        .filter(media_items::Column::Id.is_in(item_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<HashMap<_, _>>();

    let mut items = children
        .into_iter()
        .filter_map(|child| {
            items_by_id
                .remove(&child.item_id)
                .map(|item| (item, child.sort_order))
        })
        .collect::<Vec<_>>();

    if !include_private {
        let visible_items =
            visible_media_items(db, items.iter().map(|(item, _)| item.clone()).collect()).await?;
        let visible_ids = visible_items
            .into_iter()
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        items.retain(|(item, _)| visible_ids.contains(&item.id));
    }

    Ok(items)
}

async fn visible_media_items(
    db: &DatabaseConnection,
    items: Vec<media_items::Model>,
) -> anyhow::Result<Vec<media_items::Model>> {
    let parent_ids = items
        .iter()
        .filter(|item| item.is_public == 1 && !item.parent_id.is_empty())
        .map(|item| item.parent_id.clone())
        .collect::<HashSet<_>>();
    if parent_ids.is_empty() {
        return Ok(items
            .into_iter()
            .filter(|item| item.is_public == 1 && item.parent_id.is_empty())
            .collect());
    }

    let parent_ids_vec = parent_ids.iter().cloned().collect::<Vec<_>>();
    let library_parent_ids = Libraries::find()
        .filter(libraries::Column::Id.is_in(parent_ids_vec.clone()))
        .all(db)
        .await?
        .into_iter()
        .map(|library| library.id)
        .collect::<HashSet<_>>();
    let media_parent_ids = MediaItems::find()
        .filter(media_items::Column::Id.is_in(parent_ids_vec))
        .filter(media_items::Column::IsPublic.eq(1_i64))
        .all(db)
        .await?
        .into_iter()
        .map(|item| item.id)
        .collect::<HashSet<_>>();

    Ok(items
        .into_iter()
        .filter(|item| {
            item.is_public == 1
                && (item.parent_id.is_empty()
                    || library_parent_ids.contains(&item.parent_id)
                    || media_parent_ids.contains(&item.parent_id))
        })
        .collect())
}

fn playlist_item_json(item: &media_items::Model, sort_order: i64) -> JsonValue {
    json!({
        "Id": item.id.clone(),
        "Name": item.title.clone(),
        "PlaylistItemId": item.id.clone(),
        "Type": item.item_type.clone(),
        "ProductionYear": item.production_year,
        "PremiereDate": item.premiere_date.as_deref().and_then(crate::util::yyyy_mm_dd_to_jellyfin_date),
        "RunTimeTicks": item.runtime_ticks,
        "IndexNumber": sort_order,
    })
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
        collection_item_json, collection_playlist_name, create_playlist_inner, get_playlist_inner,
        item_collections_result, item_ids_from_value, normalize_item_ids, playlist_items_inner,
        playlist_user_inner, playlist_user_permissions_json, playlist_users_from_value,
        playlist_write_access, remove_children, set_playlist_user_permission,
        update_playlist_inner,
    };
    use crate::entities::{
        media_items::{self, Entity as MediaItems},
        users::{self, Entity as Users},
    };
    use sea_orm::{EntityTrait, Set};
    use serde_json::json;
    use std::collections::HashMap;

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
    fn playlist_user_inputs_are_normalized_and_limited() {
        let users = playlist_users_from_value(Some(&json!([
            { "UserId": " u1 ", "CanEdit": true },
            { "UserId": "u2" }
        ])))
        .unwrap();
        assert_eq!(users.get("u1"), Some(&true));
        assert_eq!(users.get("u2"), Some(&false));
        assert!(playlist_users_from_value(Some(&json!({"UserId": "u1"}))).is_err());
        assert!(playlist_users_from_value(Some(&json!([{ "CanEdit": true }]))).is_err());
        assert!(playlist_users_from_value(Some(&json!([{ "UserId": "bad\nid" }]))).is_err());
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_user(&db, "u1").await;
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;

        let updated = set_playlist_user_permission(&db, "p1", "u1", &json!({ "CanEdit": false }))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated["CanEdit"], false);

        let loaded = playlist_user_inner(&db, "p1", "u1").await.unwrap().unwrap();
        assert_eq!(loaded["CanEdit"], false);

        insert_user(&db, "u2").await;
        let inherited = playlist_user_inner(&db, "p1", "u2").await.unwrap().unwrap();
        assert_eq!(inherited["CanEdit"], false);
    }

    #[tokio::test]
    async fn playlist_creation_grants_owner_edit_access() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_user(&db, "owner").await;
        insert_user(&db, "viewer").await;
        insert_media_item(&db, "m1", "Movie", "Movie").await;

        let mut users = HashMap::new();
        users.insert("viewer".to_string(), false);
        let playlist_id = create_playlist_inner(
            &db,
            "Road Trip",
            &["m1".to_string()],
            "Video",
            "owner",
            users,
        )
        .await
        .unwrap();

        assert_eq!(
            playlist_write_access(&db, &playlist_id, "owner", false)
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            playlist_write_access(&db, &playlist_id, "viewer", false)
                .await
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            playlist_write_access(&db, &playlist_id, "admin", true)
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            playlist_write_access(&db, "missing", "owner", false)
                .await
                .unwrap(),
            None
        );

        let playlist = get_playlist_inner(&db, &playlist_id, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(playlist["ItemIds"], json!(["m1"]));
        assert!(
            playlist["Shares"]
                .as_array()
                .unwrap()
                .iter()
                .any(|user| user["UserId"] == "owner" && user["CanEdit"] == true)
        );
    }

    #[tokio::test]
    async fn playlist_update_replaces_share_permissions() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_user(&db, "u1").await;
        insert_user(&db, "u2").await;
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;

        let users = playlist_users_from_value(Some(&json!([
            { "UserId": "u1", "CanEdit": true },
            { "UserId": "u2", "CanEdit": false }
        ])))
        .unwrap();
        assert!(
            update_playlist_inner(&db, "p1", None, None, Some(users))
                .await
                .unwrap()
        );
        assert_eq!(
            playlist_write_access(&db, "p1", "u1", false).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            playlist_write_access(&db, "p1", "u2", false).await.unwrap(),
            Some(false)
        );
    }

    #[tokio::test]
    async fn playlist_add_remove_children_reports_missing_parent() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "p1", "Playlist", "Playlist").await;
        insert_media_item_with_visibility(&db, "public", "Public", "Movie", 1).await;
        insert_media_item_with_visibility(&db, "private", "Private", "Movie", 0).await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "Movie", "", 0).await;
        insert_media_item_with_parent(
            &db,
            "hidden-child",
            "Hidden Child",
            "Movie",
            "hidden-parent",
            1,
        )
        .await;

        assert!(
            add_children(
                &db,
                "p1",
                "Playlist",
                &[
                    "public".to_string(),
                    "private".to_string(),
                    "hidden-child".to_string(),
                ]
            )
            .await
            .unwrap()
        );

        let (visible, visible_total) = playlist_items_inner(&db, "p1", 0, 10, false).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible_total, 1);
        assert_eq!(visible[0]["Id"], "public");

        let (all, all_total) = playlist_items_inner(&db, "p1", 0, 10, true).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all_total, 3);

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
        assert_eq!(item_ids, vec!["hidden-child", "private", "public"]);
    }

    #[tokio::test]
    async fn playlist_items_total_count_ignores_page_limit() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        Users::insert(users::ActiveModel {
            id: Set(id.to_string()),
            username: Set(id.to_string()),
            display_name: Set(id.to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
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
        insert_media_item_with_parent(db, id, title, item_type, "", is_public).await;
    }

    async fn insert_media_item_with_parent(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        item_type: &str,
        parent_id: &str,
        is_public: i64,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(1),
            is_public: Set(is_public),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}
