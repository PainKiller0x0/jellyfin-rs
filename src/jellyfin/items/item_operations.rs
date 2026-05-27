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

pub async fn delete_single_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match delete_items_inner(&state, &[&item_id]).await {
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

pub(crate) async fn update_item_inner(
    db: &DatabaseConnection,
    item_id: &str,
    body: Value,
) -> anyhow::Result<bool> {
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
    let Some(name) = body.get("Name").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "Error": "Name is required" }))).into_response();
    };
    let backend = state.db.get_database_backend();
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    if let Err(e) = state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "INSERT INTO tags (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
        vec![id.clone().into(), name.trim().into(), now_unix().into()],
    )).await {
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
    let Some(name) = body.get("Name").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "Error": "Name is required" }))).into_response();
    };
    let backend = state.db.get_database_backend();
    let id = stable_text_id(&format!("tags:{}", name.trim().to_ascii_lowercase()));
    if let Err(e) = state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_tags WHERE item_id = ? AND tag_id = ?",
        vec![item_id.into(), id.into()],
    )).await {
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
                let _ = tokio::fs::remove_file(&path).await;
            }
            // Remove from media_streams
            let _ = state.db.execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM media_streams WHERE item_id = ? AND stream_index = ? AND stream_type = 'Subtitle'",
                vec![item_id.into(), index.into()],
            )).await;
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
    match state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE media_items SET is_public = 0, updated_at = ? WHERE id = ?",
        vec![now.into(), item_id.into()],
    )).await {
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
    match state.db.execute(crate::db::helpers::portable_statement(
        backend,
        "UPDATE media_items SET is_public = 1, updated_at = ? WHERE id = ?",
        vec![now.into(), item_id.into()],
    )).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => internal_error(e.into()),
    }
}
