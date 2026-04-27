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
    jellyfin::common::internal_error,
    util::{now_unix, stable_text_id},
};

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

async fn delete_items_inner(state: &AppState, ids: &[&str]) -> anyhow::Result<()> {
    let mut deleted = 0u64;
    for id in ids {
        let rows = descendant_item_rows(&state.db, id).await?;
        for (item_id, _) in rows.into_iter().rev() {
            delete_item_records(&state.db, &item_id).await?;
            deleted += 1;
        }
    }
    crate::jellyfin::system::log_activity(
        state,
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

pub(super) async fn update_item_inner(
    db: &sqlx::AnyPool,
    item_id: &str,
    body: Value,
) -> anyhow::Result<bool> {
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
        let id = stable_text_id(&format!("{table}:{}", name.trim().to_ascii_lowercase()));
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
        let id = stable_text_id(&format!("people:{}", name.trim().to_ascii_lowercase()));
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
