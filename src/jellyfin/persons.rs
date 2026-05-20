use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Value,
};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{image_assets::Entity as ImageAssets, people::Entity as People},
    jellyfin::common::internal_error,
};

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
) -> anyhow::Result<Option<JsonValue>> {
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
) -> anyhow::Result<JsonValue> {
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

async fn find_person_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> anyhow::Result<Option<PersonRow>> {
    let Some(model) = People::find()
        .filter(crate::entities::people::Column::Name.eq(name))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(PersonRow {
        id: model.id,
        name: model.name,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_tagged_items(
    db: &DatabaseConnection,
    person_id: &str,
    user_id: Option<&str>,
    include_item_types: &[&str],
    person_types: &[&str],
    sort_by: &str,
    sort_order: &str,
    limit: i64,
    start_index: i64,
) -> anyhow::Result<Vec<JsonValue>> {
    let mut sql = String::from(
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, mi.parent_id, mi.item_type, mi.is_folder, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.runtime_ticks, mi.size_bytes, mi.created_at, mi.modified_at"#,
    );

    let mut values: Vec<Value> = vec![person_id.into()];

    if user_id.is_some() {
        sql.push_str(
            r#", COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at"#,
        );
    }

    if !include_item_types.is_empty() {
        let placeholders = include_item_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mi.item_type IN ({placeholders})"));
        for item_type in include_item_types {
            values.push((*item_type).into());
        }
    }

    if !person_types.is_empty() {
        let placeholders = person_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mp.person_type IN ({placeholders})"));
        for person_type in person_types {
            values.push((*person_type).into());
        }
    }

    sql.push_str(
        r#" FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id WHERE mp.person_id = ?"#,
    );

    if let Some(uid) = user_id {
        sql.push_str(" LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ?");
        values.push(uid.into());
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
    values.push(limit.into());
    values.push(start_index.into());

    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await?;
    let items = rows
        .iter()
        .map(|row| {
            let item_type: String = row.get_str("item_type").unwrap_or_default();
            let is_folder: i64 = row.get_i64("is_folder").unwrap_or_default();
            let user_data = if user_id.is_some() {
                let last_played: Option<i64> = row.get_opt_i64("last_played_at").unwrap_or_default();
                json!({
                    "IsFavorite": row.get_bool_from_i64("is_favorite").unwrap_or_default(),
                    "Played": row.get_bool_from_i64("played").unwrap_or_default(),
                    "PlaybackPositionTicks": row.get_i64("playback_position_ticks").unwrap_or_default(),
                    "PlayCount": row.get_i64("play_count").unwrap_or_default(),
                    "PlayedPercentage": row.get_f64("played_percentage").unwrap_or_default(),
                    "LastPlayedDate": last_played.map(crate::util::unix_to_jellyfin_date),
                })
            } else {
                json!(null)
            };
            json!({
                "Name": row.get_str("title").unwrap_or_default(),
                "Id": row.get_str("id").unwrap_or_default(),
                "Type": item_type,
                "IsFolder": is_folder != 0,
                "ProductionYear": row.get_opt_i64("production_year").unwrap_or_default(),
                "RunTimeTicks": row.get_opt_i64("runtime_ticks").unwrap_or_default(),
                "Overview": row.get_opt_str("overview").unwrap_or_default(),
                "Path": row.get_str("path").unwrap_or_default(),
                "LibraryId": row.get_str("library_id").unwrap_or_default(),
                "ParentId": row.get_str("parent_id").unwrap_or_default(),
                "Container": row.get_opt_str("container").unwrap_or_default(),
                "Size": row.get_opt_i64("size_bytes").unwrap_or_default(),
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
    db: &DatabaseConnection,
    person_id: &str,
    include_item_types: &[&str],
    person_types: &[&str],
) -> anyhow::Result<i64> {
    let mut sql = String::from(
        "SELECT COUNT(*) as cnt FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id WHERE mp.person_id = ?",
    );
    let mut values: Vec<Value> = vec![person_id.into()];

    if !include_item_types.is_empty() {
        let placeholders = include_item_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mi.item_type IN ({placeholders})"));
        for item_type in include_item_types {
            values.push((*item_type).into());
        }
    }
    if !person_types.is_empty() {
        let placeholders = person_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mp.person_type IN ({placeholders})"));
        for person_type in person_types {
            values.push((*person_type).into());
        }
    }

    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await?;
    Ok(row.map(|row| row.get_i64("cnt").unwrap_or(0)).unwrap_or(0))
}

async fn person_images(db: &DatabaseConnection, person_id: &str) -> anyhow::Result<JsonValue> {
    let models = ImageAssets::find()
        .filter(crate::entities::image_assets::Column::ItemId.eq(person_id))
        .order_by_asc(crate::entities::image_assets::Column::ImageIndex)
        .all(db)
        .await?;

    let mut tags = serde_json::Map::new();
    for m in &models {
        let etag = m.etag.as_deref().unwrap_or_default();
        tags.entry(m.image_type.clone())
            .or_insert_with(|| json!(etag));
    }
    Ok(JsonValue::Object(tags))
}

struct PersonRow {
    id: String,
    name: String,
}
