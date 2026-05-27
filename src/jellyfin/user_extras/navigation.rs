use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        common::internal_error,
        item_queries,
    },
};

/// GET /Items/Filters2 — return available filter values for a query
pub async fn filters2(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match filters2_inner(&state.db, &query).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn filters2_inner(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let parent_id = query.get("ParentId").map(String::as_str);
    let include_types = query.get("IncludeItemTypes").map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    // Build WHERE clause based on ParentId and IncludeItemTypes
    let mut conditions = vec!["1=1".to_string()];
    let mut values: Vec<sea_orm::Value> = Vec::new();

    if let Some(pid) = parent_id {
        conditions.push("media_items.parent_id = ?".to_string());
        values.push(pid.into());
    }
    if let Some(types) = &include_types {
        if !types.is_empty() {
            let ph = types.iter().map(|_| "media_items.item_type = ?").collect::<Vec<_>>().join(" OR ");
            conditions.push(format!("({})", ph));
            for t in types {
                values.push((*t).into());
            }
        }
    }

    let where_clause = conditions.join(" AND ");

    // Get genres
    let genres_sql = format!(
        "SELECT DISTINCT g.name FROM genres g JOIN media_genres mg ON mg.genre_id = g.id JOIN media_items ON media_items.id = mg.item_id WHERE {} ORDER BY g.name ASC",
        where_clause
    );
    let genres: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &genres_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get years
    let years_sql = format!(
        "SELECT DISTINCT media_items.production_year FROM media_items WHERE {} AND media_items.production_year IS NOT NULL ORDER BY media_items.production_year DESC",
        where_clause
    );
    let years: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &years_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_i64("production_year").ok().flatten().map(|y| json!(y)))
        .collect();

    // Get tags
    let tags_sql = format!(
        "SELECT DISTINCT t.name FROM tags t JOIN media_tags mt ON mt.tag_id = t.id JOIN media_items ON media_items.id = mt.item_id WHERE {} ORDER BY t.name ASC",
        where_clause
    );
    let tags: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &tags_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get studios
    let studios_sql = format!(
        "SELECT DISTINCT s.name FROM studios s JOIN media_studios ms ON ms.studio_id = s.id JOIN media_items ON media_items.id = ms.item_id WHERE {} ORDER BY s.name ASC",
        where_clause
    );
    let studios: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &studios_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten().map(|n| json!({"Name": n, "Id": n})))
        .collect();

    // Get official ratings
    let ratings_sql = format!(
        "SELECT DISTINCT media_items.official_rating FROM media_items WHERE {} AND media_items.official_rating IS NOT NULL AND media_items.official_rating <> '' ORDER BY media_items.official_rating ASC",
        where_clause
    );
    let ratings: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(backend, &ratings_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("official_rating").ok().flatten().map(|n| json!(n)))
        .collect();

    Ok(json!({
        "Genres": genres,
        "Years": years,
        "Tags": tags,
        "Studios": studios,
        "OfficialRatings": ratings,
        "VideoTypes": ["VideoFile", "Iso", "Dvd", "BluRay"],
    }))
}

/// GET /Items/{item_id}/Ancestors — breadcrumb navigation
pub async fn item_ancestors(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match item_ancestors_inner(&state.db, &user_id, &item_id).await {
        Ok(ancestors) => Json(ancestors).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_ancestors_inner(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let mut ancestors = Vec::new();
    let mut current_id = Some(item_id.to_string());

    // Walk up the parent chain
    while let Some(id) = current_id {
        let row = db
            .query_one(crate::db::helpers::portable_statement(
                backend,
                &item_queries::media_item_select_sql("WHERE media_items.id = ?"),
                vec![user_id.into(), id.clone().into()],
            ))
            .await?;

        match row {
            Some(r) => {
                let item = crate::library::models::MediaItem::from_query_result(&r)?;
                current_id = if item.parent_id.is_empty() || item.parent_id == item.id {
                    None
                } else {
                    Some(item.parent_id.clone())
                };
                ancestors.push(json!({
                    "Name": item.title,
                    "Id": item.id,
                    "Type": item.item_type,
                    "IsFolder": item.is_folder,
                    "Path": item.path,
                }));
            }
            None => break,
        }
    }

    ancestors.reverse();
    Ok(ancestors)
}

/// GET /Items/Suggestions — alternative suggestions path
pub async fn items_suggestions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    crate::jellyfin::items::user_suggestions(State(state), Path(user_id), Query(query)).await
}

/// GET /UserItems/Resume — alternative resume path
pub async fn user_items_resume(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    crate::jellyfin::items::resume_items(State(state), Path(user_id)).await
}

/// GET /Items/Filters — older filters endpoint (same as Filters2)
pub async fn items_filters(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    filters2(State(state), Query(query)).await
}

/// GET /Shows/Upcoming — upcoming episodes (recently aired)
pub async fn shows_upcoming(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);

    let backend = state.db.get_database_backend();
    let sql = format!(
        "{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 ORDER BY media_items.created_at DESC LIMIT ?",
        crate::jellyfin::item_queries::media_item_select_sql("")
    );
    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![user_id.into(), (limit as i64).into()],
        ))
        .await
        .unwrap_or_default();

    let items = crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|i| crate::jellyfin::common::strip_nulls(i.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

/// GET /Genres/{name} — get genre by name
pub async fn genre_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let row = state.db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, name FROM genres WHERE name = ?",
            vec![name.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            let id = r.get_str("id").unwrap_or_default();
            let name = r.get_str("name").unwrap_or_default();
            Json(json!({ "Name": name, "Id": id, "Type": "Genre" })).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /Studios/{name} — get studio by name
pub async fn studio_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let row = state.db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, name FROM studios WHERE name = ?",
            vec![name.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            let id = r.get_str("id").unwrap_or_default();
            let name = r.get_str("name").unwrap_or_default();
            Json(json!({ "Name": name, "Id": id, "Type": "Studio" })).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
