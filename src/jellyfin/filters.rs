use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, QueryOrder};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        game_genres::Entity as GameGenres, genres::Entity as Genres, people::Entity as People,
        studios::Entity as Studios, tags::Entity as Tags,
    },
    jellyfin::common::internal_error,
};

pub async fn genres(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::Genre, &query).await
}

pub async fn tags(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::Tag, &query).await
}

pub async fn persons(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::Person, &query).await
}

pub async fn studios(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::Studio, &query).await
}

pub async fn game_genres(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::GameGenre, &query).await
}

pub async fn years(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_years(&state.db, &query).await
}

pub async fn official_ratings(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_distinct_values(&state.db, "media_items", "official_rating", &query).await
}

pub async fn containers(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_distinct_values(&state.db, "media_items", "container", &query).await
}

pub async fn video_codecs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_distinct_values(&state.db, "media_streams", "codec", &query).await
}

pub async fn extended_video_types(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_extended_video_types_inner(&state.db, &query).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn list_extended_video_types_inner(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT extended_video_type FROM media_items WHERE extended_video_type IS NOT NULL AND extended_video_type <> ''",
            vec![],
        ))
        .await
        .context("failed to list extended video types")?;

    let mut names = Vec::<String>::new();
    for row in &rows {
        let value: String = row.get_str("extended_video_type")?;
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if !names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(name))
            {
                names.push(name.to_string());
            }
        }
    }

    let mut items: Vec<Value> = names
        .into_iter()
        .map(|name| json!({ "Name": name, "Id": name, "Type": "ExtendedVideoType" }))
        .collect();
    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

/// Return unique first characters of item titles (for alphabetical index navigation)
pub async fn items_prefixes(
    State(state): State<Arc<AppState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match list_prefixes(&state.db, "media_items", "title").await {
        Ok(prefixes) => Json(prefixes).into_response(),
        Err(error) => internal_error(error),
    }
}

/// Return unique first characters of artist/person names
pub async fn artists_prefixes(
    State(state): State<Arc<AppState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match list_prefixes(&state.db, "people", "name").await {
        Ok(prefixes) => Json(prefixes).into_response(),
        Err(error) => internal_error(error),
    }
}

/// Return unique first characters of usernames
pub async fn users_prefixes(
    State(state): State<Arc<AppState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match list_prefixes(&state.db, "users", "username").await {
        Ok(prefixes) => Json(prefixes).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn list_prefixes(db: &DatabaseConnection, table: &str, column: &str) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let sql = format!(
        "SELECT DISTINCT UPPER(SUBSTR({}, 1, 1)) AS prefix FROM {} WHERE {} IS NOT NULL AND {} <> '' ORDER BY prefix ASC",
        column, table, column, column
    );
    let rows = db
        .query_all(crate::db::helpers::portable_statement(backend, &sql, vec![]))
        .await
        .context("failed to list prefixes")?;
    Ok(rows
        .iter()
        .filter_map(|r| r.get_str("prefix").ok())
        .map(|p| json!(p))
        .collect())
}

async fn list_years(db: &DatabaseConnection, query: &HashMap<String, String>) -> Response {
    match list_years_inner(db, query).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn list_years_inner(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT DISTINCT production_year FROM media_items WHERE production_year IS NOT NULL AND production_year > 0 ORDER BY production_year DESC",
            vec![],
        ))
        .await
        .context("failed to list years")?;

    let mut items: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let year: i64 = row.get_i64("production_year").ok()?;
            let year_str = year.to_string();
            Some(json!({
                "Name": year_str,
                "Id": year_str,
                "Type": "Year",
            }))
        })
        .collect();

    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

async fn list_distinct_values(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
    query: &HashMap<String, String>,
) -> Response {
    match list_distinct_values_inner(db, table, column, query).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn list_distinct_values_inner(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT {column} FROM {table} WHERE {column} IS NOT NULL AND {column} <> '' ORDER BY {column} ASC"
    );
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![],
        ))
        .await
        .with_context(|| format!("failed to list distinct {column} from {table}"))?;

    let mut items: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let name: String = row.get_str(column).ok()?;
            Some(json!({
                "Name": name,
                "Id": name,
                "Type": column,
            }))
        })
        .collect();

    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

fn filter_by_search_and_paginate(items: &mut Vec<Value>, query: &HashMap<String, String>) {
    if let Some(search_term) = query.get("SearchTerm").filter(|value| !value.is_empty()) {
        let search_term = search_term.to_ascii_lowercase();
        items.retain(|item| {
            item.get("Name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.to_ascii_lowercase().contains(&search_term))
        });
    }

    items.sort_by(|a, b| {
        a.get("Name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &b.get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
    });
    if query
        .get("SortOrder")
        .is_some_and(|value| value.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }

    let offset = query
        .get("StartIndex")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    let limit = query
        .get("Limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    *items = items.drain(..).skip(offset).take(limit).collect();
}

async fn list_filter_items(
    db: &DatabaseConnection,
    kind: FilterKind,
    query: &HashMap<String, String>,
) -> Response {
    match list_filter_items_inner(db, kind, query).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn list_filter_items_inner(
    db: &DatabaseConnection,
    kind: FilterKind,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let mut items = match kind {
        FilterKind::Genre => {
            let models = Genres::find()
                .order_by_asc(crate::entities::genres::Column::Name)
                .all(db)
                .await
                .context("failed to list genres")?;
            models
                .into_iter()
                .map(|m| json!({ "Name": m.name, "Id": m.id, "Type": kind.item_type(), "ImageTags": {} }))
                .collect()
        }
        FilterKind::Tag => {
            let models = Tags::find()
                .order_by_asc(crate::entities::tags::Column::Name)
                .all(db)
                .await
                .context("failed to list tags")?;
            models
                .into_iter()
                .map(|m| json!({ "Name": m.name, "Id": m.id, "Type": kind.item_type(), "ImageTags": {} }))
                .collect()
        }
        FilterKind::Person => {
            let filters = query.get("Filters").unwrap_or(&String::new()).clone();
            let has_fav_filter = filters.contains("IsFavorite");
            let user_id = query.get("UserId").or_else(|| query.get("userId"));

            if has_fav_filter && user_id.is_some() {
                // Filter persons by user_data.is_favorite
                let uid = user_id.unwrap();
                let models = db
                    .query_all(crate::db::helpers::portable_statement(
                        db.get_database_backend(),
                        "SELECT p.id, p.name, ia.etag AS primary_image_tag FROM people p JOIN user_data ud ON ud.item_id = p.id LEFT JOIN image_assets ia ON ia.item_id = p.id AND ia.image_type = 'Primary' WHERE ud.user_id = ? AND ud.is_favorite = 1 ORDER BY p.name ASC",
                        vec![uid.as_str().into()],
                    ))
                    .await
                    .context("failed to list favorite persons")?;
                models
                    .iter()
                    .filter_map(|r| {
                        let id = r.get_str("id").ok()?;
                        let name = r.get_str("name").ok()?;
                        let image_tag = r.get_opt_str("primary_image_tag").ok().flatten().unwrap_or_default();
                        let mut item = json!({ "Name": name, "Id": id, "Type": kind.item_type(), "ImageTags": {} });
                        if !image_tag.is_empty() {
                            item["PrimaryImageTag"] = json!(image_tag);
                            item["ImageTags"] = json!({"Primary": image_tag});
                        }
                        Some(item)
                    })
                    .collect()
            } else {
                let models = People::find()
                    .order_by_asc(crate::entities::people::Column::Name)
                    .all(db)
                    .await
                    .context("failed to list persons")?;
                models
                    .into_iter()
                    .map(|m| json!({ "Name": m.name, "Id": m.id, "Type": kind.item_type(), "ImageTags": {} }))
                    .collect()
            }
        }
        FilterKind::Studio => {
            let models = Studios::find()
                .order_by_asc(crate::entities::studios::Column::Name)
                .all(db)
                .await
                .context("failed to list studios")?;
            models
                .into_iter()
                .map(|m| json!({ "Name": m.name, "Id": m.id, "Type": kind.item_type(), "ImageTags": {} }))
                .collect()
        }
        FilterKind::GameGenre => {
            let models = GameGenres::find()
                .order_by_asc(crate::entities::game_genres::Column::Name)
                .all(db)
                .await
                .context("failed to list game genres")?;
            models
                .into_iter()
                .map(|m| json!({ "Name": m.name, "Id": m.id, "Type": kind.item_type(), "ImageTags": {} }))
                .collect()
        }
    };

    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

#[derive(Copy, Clone)]
enum FilterKind {
    Genre,
    Tag,
    Person,
    Studio,
    GameGenre,
}

impl FilterKind {
    fn item_type(self) -> &'static str {
        match self {
            Self::Genre => "Genre",
            Self::Tag => "Tag",
            Self::Person => "Person",
            Self::Studio => "Studio",
            Self::GameGenre => "GameGenre",
        }
    }
}
