use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, sea_query::OnConflict,
};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        genres::{self, Entity as Genres},
        image_assets::{self, Entity as ImageAssets},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        provider_ids::{self, Entity as ProviderIds},
        studios::{self, Entity as Studios},
        tags::{self, Entity as Tags},
        user_data::{self, Entity as UserData},
    },
    jellyfin::common::internal_error,
    library::path_utils,
    playback::streaming::readable_media_path,
    util::{normalize_yyyy_mm_dd, now_unix, stable_text_id, year_from_yyyy_mm_dd},
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
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
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
    MediaStreams::delete_many()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media_streams for item: {item_id}"))?;
    UserData::delete_many()
        .filter(user_data::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete user_data for item: {item_id}"))?;
    MediaPeople::delete_many()
        .filter(media_people::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media_people for item: {item_id}"))?;
    MediaGenres::delete_many()
        .filter(media_genres::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media_genres for item: {item_id}"))?;
    MediaTags::delete_many()
        .filter(media_tags::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media_tags for item: {item_id}"))?;
    MediaStudios::delete_many()
        .filter(media_studios::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media_studios for item: {item_id}"))?;
    ProviderIds::delete_many()
        .filter(provider_ids::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete provider_ids for item: {item_id}"))?;
    ImageAssets::delete_many()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to delete image_assets for item: {item_id}"))?;
    MediaItems::delete_by_id(item_id.to_string())
        .exec(db)
        .await
        .with_context(|| format!("failed to delete media item: {item_id}"))?;
    Ok(())
}

async fn media_item_exists(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    Ok(MediaItems::find_by_id(item_id.to_string())
        .one(db)
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
    let existing = MediaItems::find_by_id(item_id.to_string())
        .one(db)
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
        .unwrap_or_else(|| existing.title.clone());
    let overview = body
        .get("Overview")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| existing.overview.clone());
    let production_year = body
        .get("ProductionYear")
        .and_then(Value::as_i64)
        .or_else(|| {
            body.get("PremiereDate")
                .and_then(Value::as_str)
                .and_then(year_from_yyyy_mm_dd)
        })
        .or(existing.production_year);
    let premiere_date = body
        .get("PremiereDate")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| existing.premiere_date.clone());
    let community_rating = body
        .get("CommunityRating")
        .and_then(Value::as_f64)
        .or(existing.community_rating);
    let runtime_ticks = body
        .get("RuntimeTicks")
        .and_then(Value::as_i64)
        .or(existing.runtime_ticks);

    let mut active: media_items::ActiveModel = existing.into();
    active.title = Set(title);
    active.overview = Set(overview);
    active.production_year = Set(production_year);
    active.premiere_date = Set(premiere_date);
    active.community_rating = Set(community_rating);
    active.runtime_ticks = Set(runtime_ticks);
    active.updated_at = Set(now);
    active
        .update(db)
        .await
        .with_context(|| format!("failed to update item metadata: {item_id}"))?;

    if let Some(provider_ids) = body.get("ProviderIds").and_then(Value::as_object) {
        for (provider, provider_item_id) in provider_ids {
            let Some(provider_item_id) =
                provider_item_id.as_str().filter(|value| !value.is_empty())
            else {
                continue;
            };
            crate::db::provider_ids::upsert(db, item_id, provider, provider_item_id)
                .await
                .with_context(|| format!("failed to update provider id for item: {item_id}"))?;
        }
    }

    update_named_relations(db, item_id, NamedRelationKind::Genre, "Genres", &body).await?;
    update_named_relations(db, item_id, NamedRelationKind::Tag, "Tags", &body).await?;
    update_named_relations(db, item_id, NamedRelationKind::Studio, "Studios", &body).await?;
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
    if let Some(value) = input.get("PremiereDate") {
        if !value.is_null() {
            let date = value
                .as_str()
                .and_then(normalize_yyyy_mm_dd)
                .ok_or((StatusCode::BAD_REQUEST, "Invalid premiere date"))?;
            normalized.insert("PremiereDate".to_string(), Value::String(date));
        }
    }
    if let Some(value) = input.get("CommunityRating") {
        if !value.is_null() {
            let rating = value
                .as_f64()
                .ok_or((StatusCode::BAD_REQUEST, "Invalid community rating"))?;
            if !(0.0..=10.0).contains(&rating) {
                return Err((StatusCode::BAD_REQUEST, "Invalid community rating"));
            }
            normalized.insert("CommunityRating".to_string(), Value::from(rating));
        }
    }
    if let Some(value) = input.get("RuntimeTicks") {
        if !value.is_null() {
            let runtime_ticks = value
                .as_i64()
                .ok_or((StatusCode::BAD_REQUEST, "Invalid runtime ticks"))?;
            if runtime_ticks < 0 {
                return Err((StatusCode::BAD_REQUEST, "Invalid runtime ticks"));
            }
            normalized.insert("RuntimeTicks".to_string(), Value::from(runtime_ticks));
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
    let Some(item) = MediaItems::find_by_id(item_id.to_string())
        .one(db)
        .await
        .with_context(|| format!("failed to find media item content type path: {item_id}"))?
    else {
        return Ok(None);
    };
    let path = path_utils::normalize_path(&item.path);
    if path.trim().is_empty() {
        return Ok(Some(item_id.to_string()));
    }
    if item.is_folder != 0 {
        return Ok(Some(path));
    }
    Ok(Some(path_utils::parent_path(&path).unwrap_or(path)))
}

#[derive(Clone, Copy)]
enum NamedRelationKind {
    Genre,
    Tag,
    Studio,
}

async fn update_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
    body_key: &str,
    body: &Value,
) -> anyhow::Result<()> {
    let Some(values) = body.get(body_key).and_then(Value::as_array) else {
        return Ok(());
    };
    clear_named_relations(db, item_id, kind).await?;
    for value in values {
        let Some(name) = value.as_str().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let relation_id = upsert_named_value(db, kind, name.trim()).await?;
        link_named_relation(db, item_id, kind, &relation_id).await?;
    }
    Ok(())
}

async fn clear_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
) -> anyhow::Result<()> {
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::delete_many()
                .filter(media_genres::Column::ItemId.eq(item_id))
                .exec(db)
                .await
                .with_context(|| format!("failed to clear media_genres for item: {item_id}"))?;
        }
        NamedRelationKind::Tag => {
            MediaTags::delete_many()
                .filter(media_tags::Column::ItemId.eq(item_id))
                .exec(db)
                .await
                .with_context(|| format!("failed to clear media_tags for item: {item_id}"))?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::delete_many()
                .filter(media_studios::Column::ItemId.eq(item_id))
                .exec(db)
                .await
                .with_context(|| format!("failed to clear media_studios for item: {item_id}"))?;
        }
    }
    Ok(())
}

async fn upsert_named_value(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    name: &str,
) -> anyhow::Result<String> {
    let now = now_unix();
    match kind {
        NamedRelationKind::Genre => {
            let id = stable_text_id(&format!("genres:{}", name.to_ascii_lowercase()));
            Genres::insert(genres::ActiveModel {
                id: Set(id.clone()),
                name: Set(name.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(genres::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to upsert genre: {name}"))?;
            Ok(Genres::find()
                .filter(genres::Column::Name.eq(name))
                .one(db)
                .await?
                .map(|genre| genre.id)
                .unwrap_or(id))
        }
        NamedRelationKind::Tag => {
            let id = stable_text_id(&format!("tags:{}", name.to_ascii_lowercase()));
            Tags::insert(tags::ActiveModel {
                id: Set(id.clone()),
                name: Set(name.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(tags::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to upsert tag: {name}"))?;
            Ok(Tags::find()
                .filter(tags::Column::Name.eq(name))
                .one(db)
                .await?
                .map(|tag| tag.id)
                .unwrap_or(id))
        }
        NamedRelationKind::Studio => {
            let id = stable_text_id(&format!("studios:{}", name.to_ascii_lowercase()));
            Studios::insert(studios::ActiveModel {
                id: Set(id.clone()),
                name: Set(name.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(studios::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to upsert studio: {name}"))?;
            Ok(Studios::find()
                .filter(studios::Column::Name.eq(name))
                .one(db)
                .await?
                .map(|studio| studio.id)
                .unwrap_or(id))
        }
    }
}

async fn link_named_relation(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
    relation_id: &str,
) -> anyhow::Result<()> {
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::insert(media_genres::ActiveModel {
                item_id: Set(item_id.to_string()),
                genre_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([media_genres::Column::ItemId, media_genres::Column::GenreId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to link genre to item: {item_id}"))?;
        }
        NamedRelationKind::Tag => {
            MediaTags::insert(media_tags::ActiveModel {
                item_id: Set(item_id.to_string()),
                tag_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([media_tags::Column::ItemId, media_tags::Column::TagId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to link tag to item: {item_id}"))?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::insert(media_studios::ActiveModel {
                item_id: Set(item_id.to_string()),
                studio_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([
                    media_studios::Column::ItemId,
                    media_studios::Column::StudioId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to link studio to item: {item_id}"))?;
        }
    }
    Ok(())
}

async fn update_people(db: &DatabaseConnection, item_id: &str, body: &Value) -> anyhow::Result<()> {
    let Some(values) = body.get("People").and_then(Value::as_array) else {
        return Ok(());
    };
    MediaPeople::delete_many()
        .filter(media_people::Column::ItemId.eq(item_id))
        .exec(db)
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
        People::insert(people::ActiveModel {
            id: Set(id.clone()),
            name: Set(name.trim().to_string()),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(people::Column::Name)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to upsert person: {name}"))?;
        let person_id = People::find()
            .filter(people::Column::Name.eq(name.trim()))
            .one(db)
            .await?
            .map(|person| person.id)
            .unwrap_or(id);
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person_id),
            role: Set(role.map(str::to_string)),
            person_type: Set(person_type.to_string()),
            sort_order: Set(i64::try_from(sort_order).unwrap_or(i64::MAX)),
        })
        .on_conflict(
            OnConflict::columns([
                media_people::Column::ItemId,
                media_people::Column::PersonId,
                media_people::Column::PersonType,
            ])
            .update_columns([media_people::Column::Role, media_people::Column::SortOrder])
            .to_owned(),
        )
        .exec_without_returning(db)
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
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    let tag_id = match Tags::insert(tags::ActiveModel {
        id: Set(id.clone()),
        name: Set(name.trim().to_string()),
        created_at: Set(now_unix()),
    })
    .on_conflict(
        OnConflict::column(tags::Column::Name)
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(&state.db)
    .await
    {
        Ok(_) => match Tags::find()
            .filter(tags::Column::Name.eq(name.trim()))
            .one(&state.db)
            .await
        {
            Ok(Some(tag)) => tag.id,
            Ok(None) => id,
            Err(e) => return internal_error(e.into()),
        },
        Err(e) => return internal_error(e.into()),
    };
    if let Err(e) = MediaTags::insert(media_tags::ActiveModel {
        item_id: Set(item_id),
        tag_id: Set(tag_id),
    })
    .on_conflict(
        OnConflict::columns([media_tags::Column::ItemId, media_tags::Column::TagId])
            .do_nothing()
            .to_owned(),
    )
    .exec_without_returning(&state.db)
    .await
    {
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
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    let tag_id = match Tags::find()
        .filter(tags::Column::Name.eq(name.trim()))
        .one(&state.db)
        .await
    {
        Ok(Some(tag)) => tag.id,
        Ok(None) => id,
        Err(e) => return internal_error(e.into()),
    };
    if let Err(e) = MediaTags::delete_many()
        .filter(media_tags::Column::ItemId.eq(item_id))
        .filter(media_tags::Column::TagId.eq(tag_id))
        .exec(&state.db)
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
    let row = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item_id))
        .filter(media_streams::Column::StreamIndex.eq(index))
        .filter(media_streams::Column::StreamType.eq("Subtitle"))
        .filter(media_streams::Column::IsExternal.eq(1))
        .one(&state.db)
        .await;

    match row {
        Ok(Some(stream)) => {
            if let Some(path) = stream.path {
                if !readable_media_path(&state.db, &path).await {
                    return StatusCode::NOT_FOUND.into_response();
                }
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return internal_error(error.into());
                    }
                }
            }
            if let Err(error) = MediaStreams::delete_many()
                .filter(media_streams::Column::ItemId.eq(item_id))
                .filter(media_streams::Column::StreamIndex.eq(index))
                .filter(media_streams::Column::StreamType.eq("Subtitle"))
                .exec(&state.db)
                .await
            {
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
    let now = now_unix();
    match MediaItems::find_by_id(item_id).one(&state.db).await {
        Ok(Some(item)) => {
            let mut active: media_items::ActiveModel = item.into();
            active.is_public = Set(0);
            active.updated_at = Set(now);
            match active.update(&state.db).await {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(e) => internal_error(e.into()),
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => internal_error(e.into()),
    }
}

/// POST /Items/{id}/MakePublic — make item visible to all users
pub async fn make_item_public(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let now = now_unix();
    match MediaItems::find_by_id(item_id).one(&state.db).await {
        Ok(Some(item)) => {
            let mut active: media_items::ActiveModel = item.into();
            active.is_public = Set(1);
            active.updated_at = Set(now);
            match active.update(&state.db).await {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(e) => internal_error(e.into()),
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
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
    use crate::entities::{media_items, media_items::Entity as MediaItems};
    use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
    use serde_json::json;

    #[tokio::test]
    async fn item_missing_checks_report_false() {
        let Some(db) = test_db().await else {
            return;
        };
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
        let Some(db) = test_db().await else {
            return;
        };
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
        let Some(db) = test_db().await else {
            return;
        };
        insert_media_item(&db, "movie", "", 0).await;
        let item = MediaItems::find_by_id("movie".to_string())
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let mut active: media_items::ActiveModel = item.into();
        active.is_public = Set(0);
        active.update(&db).await.unwrap();
        let item = MediaItems::find_by_id("movie".to_string())
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.is_public, 0);
    }

    #[tokio::test]
    async fn update_item_content_type_stores_folder_override() {
        let Some(db) = test_db().await else {
            return;
        };
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
        let Some(db) = test_db().await else {
            return;
        };
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
        let Some(db) = test_db().await else {
            return;
        };
        assert!(
            !update_item_content_type_inner(&db, "missing", "movies")
                .await
                .unwrap()
        );
    }

    async fn test_db() -> Option<DatabaseConnection> {
        crate::db::test_db().await
    }

    async fn insert_media_item(db: &DatabaseConnection, id: &str, parent_id: &str, is_folder: i64) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(id.to_string()),
            path: Set(format!("/tmp/{id}")),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(is_folder),
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
