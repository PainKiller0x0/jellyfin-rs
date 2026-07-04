use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::internal_error,
    library::path_utils,
    playback::streaming::readable_media_path,
    util::{now_unix, stable_text_id},
};

const MAX_ITEM_METADATA_NAME_LEN: usize = 512;
const MAX_ITEM_METADATA_OVERVIEW_LEN: usize = 64 * 1024;
const MAX_ITEM_PROVIDER_IDS: usize = 64;
const MAX_ITEM_PROVIDER_KEY_LEN: usize = 64;
const MAX_ITEM_PROVIDER_VALUE_LEN: usize = 512;
const MAX_ITEM_RELATION_NAMES: usize = 256;
const MAX_ITEM_RELATION_NAME_LEN: usize = 256;
const MAX_ITEM_PEOPLE: usize = 512;
const MAX_ITEM_PERSON_ROLE_LEN: usize = 256;
const MAX_ITEM_PERSON_TYPE_LEN: usize = 64;

pub async fn delete_info(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match item_delete_paths(&state.db, &item_id).await {
        Ok(Some(paths)) => Json(json!({ "Paths": paths })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
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

    match delete_items_inner(&state, &ids).await {
        Ok(0) => StatusCode::NOT_FOUND.into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_single_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match delete_items_inner(&state, &[&item_id]).await {
        Ok(0) => StatusCode::NOT_FOUND.into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let body = match normalize_item_update_body(body) {
        Ok(body) => body,
        Err(error) => return validation_error_response(error),
    };
    match update_item_inner(&state.db, &item_id, body).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_item_content_type(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let content_type = query
        .get("contentType")
        .or_else(|| query.get("ContentType"))
        .map(String::as_str)
        .unwrap_or_default();
    match update_item_content_type_inner(&state.db, &item_id, content_type).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_delete_paths(
    db: &DatabaseConnection,
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

async fn delete_items_inner(state: &AppState, ids: &[&str]) -> anyhow::Result<u64> {
    let deleted = delete_item_records_for_ids(&state.db, ids).await?;
    if deleted > 0 {
        crate::jellyfin::system::log_activity(
            state,
            &format!("Deleted {deleted} media items"),
            "MediaDeletion",
            None,
            None,
        )
        .await;
    }
    Ok(deleted)
}

async fn delete_item_records_for_ids(db: &DatabaseConnection, ids: &[&str]) -> anyhow::Result<u64> {
    let mut deleted = 0u64;
    for id in ids {
        let rows = descendant_item_rows(db, id).await?;
        for (item_id, _) in rows.into_iter().rev() {
            delete_item_records(db, &item_id).await?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

async fn descendant_item_rows(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"WITH RECURSIVE tree(id, path) AS (SELECT id, path FROM media_items WHERE id = ? UNION ALL SELECT media_items.id, media_items.path FROM media_items JOIN tree ON media_items.parent_id = tree.id) SELECT id, path FROM tree"#,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list delete paths for item: {item_id}"))?;
    rows.iter()
        .map(|row| Ok((row.get_str("id")?, row.get_str("path")?)))
        .collect()
}

async fn delete_item_records(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    let backend = db.get_database_backend();
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
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("DELETE FROM {table} WHERE item_id = ?"),
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to delete {table} for item: {item_id}"))?;
    }
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_items WHERE id = ?",
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to delete media item: {item_id}"))?;
    Ok(())
}

async fn media_item_exists(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT id FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to find media item: {item_id}"))?
        .is_some())
}

pub(crate) async fn update_item_inner(
    db: &DatabaseConnection,
    item_id: &str,
    body: Value,
) -> anyhow::Result<bool> {
    let body = normalize_item_update_body(body).map_err(|(_, message)| anyhow::anyhow!(message))?;
    let now = now_unix();
    let backend = db.get_database_backend();
    let existing = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT title, overview, production_year FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
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
        .unwrap_or(existing.get_str("title")?);
    let overview = body
        .get("Overview")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or(existing.get_opt_str("overview")?);
    let production_year = body
        .get("ProductionYear")
        .and_then(Value::as_i64)
        .or(existing.get_opt_i64("production_year")?);

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE media_items SET title = ?, overview = ?, production_year = ?, updated_at = ? WHERE id = ?",
        vec![title.into(), overview.into(), production_year.into(), now.into(), item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to update item metadata: {item_id}"))?;

    if let Some(provider_ids) = body.get("ProviderIds").and_then(Value::as_object) {
        for (provider, provider_item_id) in provider_ids {
            let Some(provider_item_id) =
                provider_item_id.as_str().filter(|value| !value.is_empty())
            else {
                continue;
            };
            db.execute(crate::db::helpers::portable_statement(
                backend,
                r#"INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id"#,
                vec![item_id.into(), provider.as_str().into(), provider_item_id.into()],
            ))
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

pub(crate) fn normalize_item_update_body(body: Value) -> Result<Value, (StatusCode, &'static str)> {
    let Some(input) = body.as_object() else {
        return Err((StatusCode::BAD_REQUEST, "Item metadata must be an object"));
    };
    let mut normalized = serde_json::Map::new();

    if input.contains_key("Name") {
        normalized.insert(
            "Name".to_string(),
            Value::String(metadata_string_field(
                input.get("Name"),
                MAX_ITEM_METADATA_NAME_LEN,
                true,
                "Invalid item name",
            )?),
        );
    }
    if input.contains_key("Overview") {
        let overview = metadata_string_field(
            input.get("Overview"),
            MAX_ITEM_METADATA_OVERVIEW_LEN,
            false,
            "Invalid item overview",
        )?;
        normalized.insert("Overview".to_string(), Value::String(overview));
    }
    if let Some(value) = input.get("ProductionYear") {
        if !value.is_null() {
            let year = value
                .as_i64()
                .ok_or((StatusCode::BAD_REQUEST, "Invalid production year"))?;
            if !(0..=9999).contains(&year) {
                return Err((StatusCode::BAD_REQUEST, "Invalid production year"));
            }
            normalized.insert("ProductionYear".to_string(), Value::from(year));
        }
    }
    if let Some(provider_ids) = input.get("ProviderIds") {
        normalized.insert(
            "ProviderIds".to_string(),
            Value::Object(normalize_provider_ids(provider_ids)?),
        );
    }
    for key in ["Genres", "Tags", "Studios"] {
        if let Some(value) = input.get(key) {
            normalized.insert(
                key.to_string(),
                Value::Array(normalize_name_array(value, key)?),
            );
        }
    }
    if let Some(value) = input.get("People") {
        normalized.insert("People".to_string(), Value::Array(normalize_people(value)?));
    }

    Ok(Value::Object(normalized))
}

fn metadata_string_field(
    value: Option<&Value>,
    max_len: usize,
    required: bool,
    error: &'static str,
) -> Result<String, (StatusCode, &'static str)> {
    let value = value
        .and_then(Value::as_str)
        .ok_or((StatusCode::BAD_REQUEST, error))?
        .trim();
    if required && value.is_empty() {
        return Err((StatusCode::BAD_REQUEST, error));
    }
    if value.chars().count() > max_len {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, error));
    }
    if value.contains('\0')
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err((StatusCode::BAD_REQUEST, error));
    }
    Ok(value.to_string())
}

fn normalize_provider_ids(
    value: &Value,
) -> Result<serde_json::Map<String, Value>, (StatusCode, &'static str)> {
    let Some(provider_ids) = value.as_object() else {
        return Err((StatusCode::BAD_REQUEST, "ProviderIds must be an object"));
    };
    if provider_ids.len() > MAX_ITEM_PROVIDER_IDS {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many provider ids"));
    }
    let mut normalized = serde_json::Map::new();
    for (provider, provider_item_id) in provider_ids {
        validate_metadata_name(provider, MAX_ITEM_PROVIDER_KEY_LEN, "Invalid provider")?;
        let Some(provider_item_id) = provider_item_id.as_str() else {
            return Err((StatusCode::BAD_REQUEST, "Invalid provider id"));
        };
        let provider_item_id = provider_item_id.trim();
        if provider_item_id.is_empty() {
            continue;
        }
        validate_metadata_name(
            provider_item_id,
            MAX_ITEM_PROVIDER_VALUE_LEN,
            "Invalid provider id",
        )?;
        normalized.insert(
            provider.trim().to_string(),
            Value::String(provider_item_id.to_string()),
        );
    }
    Ok(normalized)
}

fn normalize_name_array(
    value: &Value,
    field: &'static str,
) -> Result<Vec<Value>, (StatusCode, &'static str)> {
    let Some(values) = value.as_array() else {
        return Err((StatusCode::BAD_REQUEST, field));
    };
    if values.len() > MAX_ITEM_RELATION_NAMES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, field));
    }
    let mut normalized = Vec::new();
    for value in values {
        let Some(name) = value.as_str() else {
            return Err((StatusCode::BAD_REQUEST, field));
        };
        let name = name.trim();
        if name.is_empty()
            || normalized
                .iter()
                .any(|existing: &Value| existing.as_str() == Some(name))
        {
            continue;
        }
        validate_metadata_name(name, MAX_ITEM_RELATION_NAME_LEN, field)?;
        normalized.push(Value::String(name.to_string()));
    }
    Ok(normalized)
}

fn normalize_people(value: &Value) -> Result<Vec<Value>, (StatusCode, &'static str)> {
    let Some(people) = value.as_array() else {
        return Err((StatusCode::BAD_REQUEST, "People must be an array"));
    };
    if people.len() > MAX_ITEM_PEOPLE {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many people"));
    }
    let mut normalized = Vec::new();
    for person in people {
        let Some(person) = person.as_object() else {
            return Err((StatusCode::BAD_REQUEST, "People entries must be objects"));
        };
        let Some(name) = person.get("Name").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        validate_metadata_name(name, MAX_ITEM_RELATION_NAME_LEN, "Invalid person name")?;
        let mut object = serde_json::Map::new();
        object.insert("Name".to_string(), Value::String(name.to_string()));
        if let Some(role) = person.get("Role").and_then(Value::as_str).map(str::trim) {
            if !role.is_empty() {
                validate_metadata_name(role, MAX_ITEM_PERSON_ROLE_LEN, "Invalid person role")?;
                object.insert("Role".to_string(), Value::String(role.to_string()));
            }
        }
        if let Some(person_type) = person.get("Type").and_then(Value::as_str).map(str::trim) {
            if !person_type.is_empty() {
                validate_metadata_name(
                    person_type,
                    MAX_ITEM_PERSON_TYPE_LEN,
                    "Invalid person type",
                )?;
                object.insert("Type".to_string(), Value::String(person_type.to_string()));
            }
        }
        normalized.push(Value::Object(object));
    }
    Ok(normalized)
}

fn validate_metadata_name(
    value: &str,
    max_len: usize,
    error: &'static str,
) -> Result<(), (StatusCode, &'static str)> {
    if value.chars().count() > max_len {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, error));
    }
    if value.contains('\0') || value.chars().any(char::is_control) {
        return Err((StatusCode::BAD_REQUEST, error));
    }
    Ok(())
}

fn validation_error_response(error: (StatusCode, &'static str)) -> Response {
    (error.0, Json(json!({ "Error": error.1 }))).into_response()
}

async fn update_item_content_type_inner(
    db: &DatabaseConnection,
    item_id: &str,
    content_type: &str,
) -> anyhow::Result<bool> {
    let Some(folder_path) = item_content_type_path(db, item_id).await? else {
        return Ok(false);
    };
    let mut config: Value = serde_json::from_str(
        &crate::jellyfin::system::app_setting(db, "server_config", "{}").await,
    )
    .unwrap_or_else(|_| json!({}));
    if !config.is_object() {
        config = json!({});
    }

    let mut content_types = config
        .get("ContentTypes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| {
            entry
                .get("Name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .is_some_and(|name| !name.eq_ignore_ascii_case(&folder_path))
        })
        .collect::<Vec<_>>();
    let content_type = content_type.trim();
    if !content_type.is_empty() {
        content_types.push(json!({ "Name": folder_path, "Value": content_type }));
    }
    config["ContentTypes"] = Value::Array(content_types);
    crate::jellyfin::system::set_app_setting(db, "server_config", &config.to_string()).await?;
    Ok(true)
}

async fn item_content_type_path(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<String>> {
    let Some(row) = db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT path, is_folder FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to find media item content type path: {item_id}"))?
    else {
        return Ok(None);
    };
    let path = path_utils::normalize_path(&row.get_str("path")?);
    if path.trim().is_empty() {
        return Ok(Some(item_id.to_string()));
    }
    if row.get_i64("is_folder").unwrap_or_default() != 0 {
        return Ok(Some(path));
    }
    Ok(Some(path_utils::parent_path(&path).unwrap_or(path)))
}

async fn update_named_relations(
    db: &DatabaseConnection,
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
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        &format!("DELETE FROM {relation_table} WHERE item_id = ?"),
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;
    for value in values {
        let Some(name) = value.as_str().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let id = stable_text_id(&format!("{table}:{}", name.trim().to_ascii_lowercase()));
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"),
            vec![id.clone().into(), name.trim().into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert {table}: {name}"))?;
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("INSERT INTO {relation_table} (item_id, {relation_column}) VALUES (?, ?) ON CONFLICT(item_id, {relation_column}) DO NOTHING"),
            vec![item_id.into(), id.into()],
        ))
        .await
        .with_context(|| format!("failed to link {table} to item: {item_id}"))?;
    }
    Ok(())
}

async fn update_people(db: &DatabaseConnection, item_id: &str, body: &Value) -> anyhow::Result<()> {
    let Some(values) = body.get("People").and_then(Value::as_array) else {
        return Ok(());
    };
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_people WHERE item_id = ?",
        vec![item_id.into()],
    ))
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
        let id = stable_text_id(&format!("people:{}", name.trim().to_ascii_lowercase()));
        let role = value.get("Role").and_then(Value::as_str);
        let person_type = value.get("Type").and_then(Value::as_str).unwrap_or("Actor");
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
            vec![id.clone().into(), name.trim().into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert person: {name}"))?;
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = excluded.role, sort_order = excluded.sort_order",
            vec![item_id.into(), id.into(), role.into(), person_type.into(), i64::try_from(sort_order).unwrap_or(i64::MAX).into()],
        ))
        .await
        .with_context(|| format!("failed to link person to item: {item_id}"))?;
    }
    Ok(())
}

/// POST /Items/{id}/Tags/Add — add a single tag to an item
pub async fn add_item_tag(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
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
    match media_item_exists(&state.db, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return internal_error(e),
    }
    let backend = state.db.get_database_backend();
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    if let Err(e) = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO tags (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
            vec![id.clone().into(), name.trim().into(), now_unix().into()],
        ))
        .await
    {
        return internal_error(e.into());
    }
    if let Err(e) = state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO media_tags (item_id, tag_id) VALUES (?, ?) ON CONFLICT(item_id, tag_id) DO NOTHING",
        vec![item_id.into(), id.into()],
    )).await {
        return internal_error(e.into());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /Items/{id}/Tags/Delete — remove a single tag from an item
pub async fn delete_item_tag(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
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
    match media_item_exists(&state.db, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return internal_error(e),
    }
    let backend = state.db.get_database_backend();
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    if let Err(e) = state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM media_tags WHERE item_id = ? AND tag_id = ?",
            vec![item_id.into(), id.into()],
        ))
        .await
    {
        return internal_error(e.into());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /Items/{id}/Subtitles/{index}/Delete — delete an external subtitle
pub async fn delete_item_subtitle(
    State(state): State<Arc<AppState>>,
    Path((item_id, index)): Path<(String, i64)>,
) -> Response {
    let backend = state.db.get_database_backend();
    // Find the subtitle stream to get its file path
    let row = state.db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT path FROM media_streams WHERE item_id = ? AND stream_index = ? AND stream_type = 'Subtitle' AND is_external = 1",
        vec![item_id.clone().into(), index.into()],
    )).await;

    match row {
        Ok(Some(r)) => {
            if let Ok(path) = r.get_str("path") {
                if !readable_media_path(&state.db, &path).await {
                    return StatusCode::NOT_FOUND.into_response();
                }
                if let Err(error) = tokio::fs::remove_file(&path).await
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return internal_error(error.into());
                }
            }
            // Remove from media_streams
            if let Err(error) = state.db.execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM media_streams WHERE item_id = ? AND stream_index = ? AND stream_type = 'Subtitle'",
                vec![item_id.into(), index.into()],
            )).await {
                return internal_error(error.into());
            }
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// POST /Items/{id}/MakePrivate — restrict item visibility to owner
pub async fn make_item_private(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let now = now_unix();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET is_public = 0, updated_at = ? WHERE id = ?",
            vec![now.into(), item_id.into()],
        ))
        .await
    {
        Ok(result) if result.rows_affected() == 0 => StatusCode::NOT_FOUND.into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(e.into()),
    }
}

/// POST /Items/{id}/MakePublic — make item visible to all users
pub async fn make_item_public(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let now = now_unix();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET is_public = 1, updated_at = ? WHERE id = ?",
            vec![now.into(), item_id.into()],
        ))
        .await
    {
        Ok(result) if result.rows_affected() == 0 => StatusCode::NOT_FOUND.into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_ITEM_METADATA_NAME_LEN, MAX_ITEM_PEOPLE, MAX_ITEM_PROVIDER_IDS,
        MAX_ITEM_RELATION_NAMES, delete_item_records_for_ids, media_item_exists,
        normalize_item_update_body, update_item_content_type_inner, update_item_inner,
    };
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use serde_json::json;

    #[tokio::test]
    async fn item_missing_checks_report_false() {
        let db = test_db().await;
        assert!(!media_item_exists(&db, "missing").await.unwrap());
        assert!(
            !update_item_inner(&db, "missing", json!({ "Name": "Nope" }))
                .await
                .unwrap()
        );
        assert_eq!(
            delete_item_records_for_ids(&db, &["missing"])
                .await
                .unwrap(),
            0
        );
    }

    #[test]
    fn item_update_body_is_normalized_and_limited() {
        let body = normalize_item_update_body(json!({
            "Name": " Movie ",
            "Overview": "Line one\nLine two",
            "ProductionYear": 2024,
            "ProviderIds": { "Tmdb": " 123 ", "Empty": "" },
            "Genres": ["Drama", "Drama", " "],
            "Tags": ["Favorite"],
            "Studios": ["Studio"],
            "People": [
                { "Name": " Actor ", "Role": " Lead ", "Type": "Actor", "Ignored": true },
                { "Name": "" }
            ],
            "Ignored": "field"
        }))
        .unwrap();
        assert_eq!(body["Name"], "Movie");
        assert_eq!(body["ProviderIds"]["Tmdb"], "123");
        assert!(body["ProviderIds"]["Empty"].is_null());
        assert_eq!(body["Genres"], json!(["Drama"]));
        assert_eq!(body["People"][0]["Name"], "Actor");
        assert!(body["Ignored"].is_null());

        assert!(normalize_item_update_body(json!([])).is_err());
        assert!(
            normalize_item_update_body(
                json!({ "Name": "x".repeat(MAX_ITEM_METADATA_NAME_LEN + 1) })
            )
            .is_err()
        );
        assert!(normalize_item_update_body(json!({ "ProductionYear": 10000 })).is_err());
        assert!(
            normalize_item_update_body(json!({
                "ProviderIds": (0..=MAX_ITEM_PROVIDER_IDS)
                    .map(|index| (format!("P{index}"), json!("id")))
                    .collect::<serde_json::Map<_, _>>()
            }))
            .is_err()
        );
        assert!(
            normalize_item_update_body(json!({
                "Genres": vec!["x"; MAX_ITEM_RELATION_NAMES + 1]
            }))
            .is_err()
        );
        assert!(
            normalize_item_update_body(json!({
                "People": vec![json!({"Name": "Actor"}); MAX_ITEM_PEOPLE + 1]
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn delete_item_records_removes_descendants() {
        let db = test_db().await;
        insert_media_item(&db, "parent", "", 1).await;
        insert_media_item(&db, "child", "parent", 0).await;

        assert_eq!(
            delete_item_records_for_ids(&db, &["parent"]).await.unwrap(),
            2
        );
        assert!(!media_item_exists(&db, "parent").await.unwrap());
        assert!(!media_item_exists(&db, "child").await.unwrap());
    }

    #[tokio::test]
    async fn is_public_column_is_migrated() {
        let db = test_db().await;
        insert_media_item(&db, "movie", "", 0).await;
        let result = db
            .execute(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "UPDATE media_items SET is_public = 0 WHERE id = ?",
                vec!["movie".into()],
            ))
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);
    }

    #[tokio::test]
    async fn update_item_content_type_stores_folder_override() {
        let db = test_db().await;
        insert_media_item(&db, "file", "", 0).await;

        assert!(
            update_item_content_type_inner(&db, "file", "tvshows")
                .await
                .unwrap()
        );

        let value = crate::jellyfin::system::app_setting(&db, "server_config", "{}").await;
        let config: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert_eq!(config["ContentTypes"][0]["Name"], "/tmp");
        assert_eq!(config["ContentTypes"][0]["Value"], "tvshows");
    }

    #[tokio::test]
    async fn update_item_content_type_empty_value_clears_override() {
        let db = test_db().await;
        insert_media_item(&db, "folder", "", 1).await;

        update_item_content_type_inner(&db, "folder", "movies")
            .await
            .unwrap();
        update_item_content_type_inner(&db, "folder", "")
            .await
            .unwrap();

        let value = crate::jellyfin::system::app_setting(&db, "server_config", "{}").await;
        let config: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert!(config["ContentTypes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn update_item_content_type_reports_missing_item() {
        let db = test_db().await;
        assert!(
            !update_item_content_type_inner(&db, "missing", "movies")
                .await
                .unwrap()
        );
    }

    async fn test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db
    }

    async fn insert_media_item(db: &DatabaseConnection, id: &str, parent_id: &str, is_folder: i64) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, 'Movie', ?, 1, 1, 1)",
            vec![
                id.into(),
                id.into(),
                format!("/tmp/{id}").into(),
                parent_id.into(),
                is_folder.into(),
            ],
        ))
        .await
        .unwrap();
    }
}
