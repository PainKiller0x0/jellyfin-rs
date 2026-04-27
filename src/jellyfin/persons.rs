use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{app::state::AppState, jellyfin::common::internal_error};

pub async fn person_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match person_detail(&state, &name, &query).await {
        Ok(Some(person)) => Json(person).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"Error": "Person not found"})),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn person_items(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match person_items_inner(&state, &name, &query).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn person_detail(
    state: &AppState,
    name: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Option<Value>> {
    let name = name.trim();
    let Some(person) = find_person_by_name(&state.db, name).await? else {
        return Ok(None);
    };

    let user_id = query
        .get("UserId")
        .or_else(|| query.get("userId"))
        .map(String::as_str);

    let include_item_types = query
        .get("IncludeItemTypes")
        .or_else(|| query.get("includeItemTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();

    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100);

    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let sort_by = query
        .get("SortBy")
        .or_else(|| query.get("sortBy"))
        .map(String::as_str)
        .unwrap_or("SortName");

    let sort_order = query
        .get("SortOrder")
        .or_else(|| query.get("sortOrder"))
        .map(String::as_str)
        .unwrap_or("Ascending");

    let person_item_types = query
        .get("PersonTypes")
        .or_else(|| query.get("personTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();

    let tagged_items = fetch_tagged_items(
        &state.db,
        &person.id,
        user_id,
        &include_item_types,
        &person_item_types,
        sort_by,
        sort_order,
        limit,
        start_index,
    )
    .await?;

    let total = count_tagged_items(
        &state.db,
        &person.id,
        &include_item_types,
        &person_item_types,
    )
    .await?;

    let image_tags = person_images(&state.db, &person.id).await?;

    Ok(Some(json!({
        "Name": person.name,
        "Id": person.id,
        "ServerId": "jellyfin-rs",
        "Type": "Person",
        "ImageTags": image_tags,
        "ImageBlurHashes": {},
        "TaggedItems": tagged_items,
        "TotalRecordCount": total,
        "StartIndex": start_index,
    })))
}

async fn person_items_inner(
    state: &AppState,
    name: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let name = name.trim();
    let Some(person) = find_person_by_name(&state.db, name).await? else {
        return Ok(json!({"Items": [], "TotalRecordCount": 0}));
    };

    let user_id = query
        .get("UserId")
        .or_else(|| query.get("userId"))
        .map(String::as_str);
    let include_item_types = query
        .get("IncludeItemTypes")
        .or_else(|| query.get("includeItemTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    let person_item_types = query
        .get("PersonTypes")
        .or_else(|| query.get("personTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100);
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let sort_by = query
        .get("SortBy")
        .or_else(|| query.get("sortBy"))
        .map(String::as_str)
        .unwrap_or("SortName");
    let sort_order = query
        .get("SortOrder")
        .or_else(|| query.get("sortOrder"))
        .map(String::as_str)
        .unwrap_or("Ascending");

    let items = fetch_tagged_items(
        &state.db,
        &person.id,
        user_id,
        &include_item_types,
        &person_item_types,
        sort_by,
        sort_order,
        limit,
        start_index,
    )
    .await?;
    let total = count_tagged_items(
        &state.db,
        &person.id,
        &include_item_types,
        &person_item_types,
    )
    .await?;

    Ok(json!({"Items": items, "TotalRecordCount": total, "StartIndex": start_index}))
}

async fn find_person_by_name(db: &AnyPool, name: &str) -> anyhow::Result<Option<PersonRow>> {
    let row = sqlx::query("SELECT id, name FROM people WHERE name = ?")
        .bind(name)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|row| PersonRow {
        id: row.try_get("id").unwrap_or_default(),
        name: row.try_get("name").unwrap_or_default(),
    }))
}

async fn fetch_tagged_items(
    db: &AnyPool,
    person_id: &str,
    user_id: Option<&str>,
    include_item_types: &[&str],
    person_types: &[&str],
    sort_by: &str,
    sort_order: &str,
    limit: i64,
    start_index: i64,
) -> anyhow::Result<Vec<Value>> {
    let mut sql = String::from(
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, mi.parent_id, mi.item_type, mi.is_folder, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.created_at, mi.modified_at"#,
    );

    if let Some(_uid) = user_id {
        sql.push_str(
            r#", COALESCE(ud.is_favorite, 0) AS is_favorite, COALESCE(ud.played, 0) AS played, COALESCE(ud.playback_position_ticks, 0) AS playback_position_ticks, ud.played_percentage, COALESCE(ud.play_count, 0) AS play_count, ud.last_played_at"#,
        );
    }

    sql.push_str(
        r#" FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id WHERE mp.person_id = ?"#,
    );

    if !include_item_types.is_empty() {
        let placeholders = include_item_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mi.item_type IN ({placeholders})"));
    }

    if !person_types.is_empty() {
        let placeholders = person_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mp.person_type IN ({placeholders})"));
    }

    if let Some(_uid) = user_id {
        sql.push_str(" LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ?");
    }

    let order = match sort_order {
        "Descending" => "DESC",
        _ => "ASC",
    };
    match sort_by {
        "ProductionYear" => sql.push_str(&format!(" ORDER BY mi.production_year {order}")),
        "Runtime" => sql.push_str(&format!(" ORDER BY mi.runtime_ticks {order}")),
        "DateCreated" => sql.push_str(&format!(" ORDER BY mi.created_at {order}")),
        "Random" => sql.push_str(" ORDER BY RANDOM()"),
        _ => sql.push_str(&format!(" ORDER BY mp.sort_order ASC, mi.title {order}")),
    }

    sql.push_str(" LIMIT ? OFFSET ?");

    let mut query = sqlx::query(&sql).bind(person_id);
    for item_type in include_item_types {
        query = query.bind(*item_type);
    }
    for person_type in person_types {
        query = query.bind(*person_type);
    }
    if let Some(uid) = user_id {
        query = query.bind(uid);
    }
    query = query.bind(limit).bind(start_index);

    let rows = query.fetch_all(db).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let item_type: String = row.try_get("item_type").unwrap_or_default();
            let is_folder: i64 = row.try_get("is_folder").unwrap_or_default();
            let user_data = if user_id.is_some() {
                let last_played: Option<i64> = row.try_get("last_played_at").unwrap_or(None);
                json!({
                    "IsFavorite": row.try_get::<i64, _>("is_favorite").unwrap_or_default() != 0,
                    "Played": row.try_get::<i64, _>("played").unwrap_or_default() != 0,
                    "PlaybackPositionTicks": row.try_get::<i64, _>("playback_position_ticks").unwrap_or_default(),
                    "PlayCount": row.try_get::<i64, _>("play_count").unwrap_or_default(),
                    "PlayedPercentage": row.try_get::<Option<f64>, _>("played_percentage").unwrap_or_default(),
                    "LastPlayedDate": last_played.map(crate::util::unix_to_jellyfin_date),
                })
            } else {
                json!(null)
            };
            json!({
                "Name": row.try_get::<String, _>("title").unwrap_or_default(),
                "Id": row.try_get::<String, _>("id").unwrap_or_default(),
                "Type": item_type,
                "IsFolder": is_folder != 0,
                "ProductionYear": row.try_get::<Option<i64>, _>("production_year").unwrap_or_default(),
                "RunTimeTicks": row.try_get::<Option<i64>, _>("runtime_ticks").unwrap_or_default(),
                "Overview": row.try_get::<Option<String>, _>("overview").unwrap_or_default(),
                "Path": row.try_get::<String, _>("path").unwrap_or_default(),
                "LibraryId": row.try_get::<String, _>("library_id").unwrap_or_default(),
                "ParentId": row.try_get::<String, _>("parent_id").unwrap_or_default(),
                "Container": row.try_get::<Option<String>, _>("container").unwrap_or_default(),
                "Size": row.try_get::<Option<i64>, _>("size_bytes").unwrap_or_default(),
                "IndexNumber": null,
                "ParentIndexNumber": null,
                "ImageTags": {},
                "UserData": user_data,
            })
        })
        .collect();
    Ok(items)
}

async fn count_tagged_items(
    db: &AnyPool,
    person_id: &str,
    include_item_types: &[&str],
    person_types: &[&str],
) -> anyhow::Result<i64> {
    let mut sql = String::from(
        "SELECT COUNT(*) as cnt FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id WHERE mp.person_id = ?",
    );

    if !include_item_types.is_empty() {
        let placeholders = include_item_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mi.item_type IN ({placeholders})"));
    }
    if !person_types.is_empty() {
        let placeholders = person_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mp.person_type IN ({placeholders})"));
    }

    let mut query = sqlx::query(&sql).bind(person_id);
    for item_type in include_item_types {
        query = query.bind(*item_type);
    }
    for person_type in person_types {
        query = query.bind(*person_type);
    }

    let row = query.fetch_one(db).await?;
    Ok(row.try_get::<i64, _>("cnt").unwrap_or(0))
}

async fn person_images(db: &AnyPool, person_id: &str) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT image_type, etag, width, height FROM image_assets WHERE item_id = ? ORDER BY image_index ASC",
    )
    .bind(person_id)
    .fetch_all(db)
    .await?;

    let mut tags = json!({});
    if let Some(obj) = tags.as_object_mut() {
        for row in &rows {
            let image_type: String = row.try_get("image_type").unwrap_or_default();
            let etag: String = row.try_get("etag").unwrap_or_default();
            obj.insert(image_type, json!(etag));
        }
    }
    Ok(tags)
}

struct PersonRow {
    id: String,
    name: String,
}
