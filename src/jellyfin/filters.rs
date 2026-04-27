use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{app::state::AppState, jellyfin::common::internal_error};

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
    db: &AnyPool,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT extended_video_type FROM media_items WHERE extended_video_type IS NOT NULL AND extended_video_type <> ''",
    )
    .fetch_all(db)
    .await
    .context("failed to list extended video types")?;

    let mut names = Vec::<String>::new();
    for row in rows {
        let value: String = row.try_get("extended_video_type")?;
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

    let mut items = names
        .into_iter()
        .map(|name| json!({ "Name": name, "Id": name, "Type": "ExtendedVideoType" }))
        .collect();
    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

async fn list_years(db: &AnyPool, query: &HashMap<String, String>) -> Response {
    match list_years_inner(db, query).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn list_years_inner(
    db: &AnyPool,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT DISTINCT production_year FROM media_items WHERE production_year IS NOT NULL AND production_year > 0 ORDER BY production_year DESC",
    )
    .fetch_all(db)
    .await
    .context("failed to list years")?;

    let mut items: Vec<Value> = rows
        .into_iter()
        .filter_map(|row| {
            let year: i64 = row.try_get("production_year").ok()?;
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
    db: &AnyPool,
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
    db: &AnyPool,
    table: &str,
    column: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT {column} FROM {table} WHERE {column} IS NOT NULL AND {column} <> '' ORDER BY {column} ASC"
    );
    let rows = sqlx::query(&sql)
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list distinct {column} from {table}"))?;

    let mut items: Vec<Value> = rows
        .into_iter()
        .filter_map(|row| {
            let name: String = row.try_get(column).ok()?;
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
    db: &AnyPool,
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
    db: &AnyPool,
    kind: FilterKind,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(kind.select_sql())
        .fetch_all(db)
        .await
        .with_context(|| format!("failed to list {}", kind.name()))?;
    let mut items = rows
        .into_iter()
        .map(|row| -> anyhow::Result<Value> {
            let id: String = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            Ok(json!({
                "Name": name,
                "Id": id,
                "Type": kind.item_type(),
                "ImageTags": {},
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    filter_by_search_and_paginate(&mut items, query);
    Ok(items)
}

#[derive(Copy, Clone)]
enum FilterKind {
    Genre,
    Tag,
    Person,
    Studio,
}

impl FilterKind {
    fn select_sql(self) -> &'static str {
        match self {
            Self::Genre => "SELECT id, name FROM genres",
            Self::Tag => "SELECT id, name FROM tags",
            Self::Person => "SELECT id, name FROM people",
            Self::Studio => "SELECT id, name FROM studios",
        }
    }

    fn item_type(self) -> &'static str {
        match self {
            Self::Genre => "Genre",
            Self::Tag => "Tag",
            Self::Person => "Person",
            Self::Studio => "Studio",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Genre => "genres",
            Self::Tag => "tags",
            Self::Person => "persons",
            Self::Studio => "studios",
        }
    }
}
