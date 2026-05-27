use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::{internal_error, strip_nulls},
};

pub async fn movie_recommendations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let parent_id = query.get("ParentId").map(String::as_str);
    let category_limit = query
        .get("CategoryLimit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let item_limit = query
        .get("ItemLimit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);
    match movie_recommendations_inner(&state.db, &user_id, parent_id, category_limit, item_limit).await {
        Ok(categories) => Json(categories).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn movie_recommendations_inner(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    parent_id: Option<&str>,
    category_limit: usize,
    item_limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let mut category_counter: i64 = 1;

    // Get recently played Movie folders (is_folder=1, item_type='Movie')
    let recent_movies = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type = 'Movie' AND COALESCE(user_data.played, 0) = 1 AND COALESCE(user_data.play_count, 0) > 0 {} ORDER BY user_data.last_played_at DESC LIMIT 12",
                crate::jellyfin::item_queries::media_item_select_sql(""),
                parent_id.map(|_p| "AND media_items.library_id = ?").unwrap_or(""),
            ),
            if parent_id.is_some() {
                vec![user_id.into(), parent_id.unwrap().into()]
            } else {
                vec![user_id.into()]
            },
        ))
        .await
        .context("failed to get recent movies")?;

    let recent_movies = crate::jellyfin::item_queries::decode_media_items(&recent_movies)?;

    let mut categories = Vec::new();

    // Fallback: when no playback history, return top-rated and recently added
    if recent_movies.is_empty() {
        // Category: Top rated movies
        let top_rated_sql = format!(
            "{} WHERE media_items.is_folder = 1 AND media_items.item_type = 'Movie' AND media_items.community_rating IS NOT NULL {} ORDER BY media_items.community_rating DESC LIMIT ?",
            crate::jellyfin::item_queries::media_item_select_sql(""),
            parent_id.map(|_| "AND media_items.library_id = ?").unwrap_or(""),
        );
        let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
        if let Some(pid) = parent_id { vals.push(pid.into()); }
        vals.push((item_limit as i64).into());
        if let Ok(rows) = db.query_all(crate::db::helpers::portable_statement(backend, &top_rated_sql, vals)).await {
            let items = crate::jellyfin::item_queries::decode_media_items(&rows)?;
            if !items.is_empty() {
                categories.push(json!({
                    "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                    "RecommendationType": "SimilarToRecentlyPlayed",
                    "BaselineItemName": "Top Rated",
                    "CategoryId": category_counter,
                }));
                category_counter += 1;
            }
        }
        if categories.len() >= category_limit { return Ok(categories); }

        // Category: Recently added movies
        let recent_sql = format!(
            "{} WHERE media_items.is_folder = 1 AND media_items.item_type = 'Movie' {} ORDER BY media_items.created_at DESC LIMIT ?",
            crate::jellyfin::item_queries::media_item_select_sql(""),
            parent_id.map(|_| "AND media_items.library_id = ?").unwrap_or(""),
        );
        let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
        if let Some(pid) = parent_id { vals.push(pid.into()); }
        vals.push((item_limit as i64).into());
        if let Ok(rows) = db.query_all(crate::db::helpers::portable_statement(backend, &recent_sql, vals)).await {
            let items = crate::jellyfin::item_queries::decode_media_items(&rows)?;
            if !items.is_empty() {
                categories.push(json!({
                    "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                    "RecommendationType": "HasActorFromRecentlyPlayed",
                    "BaselineItemName": "Recently Added",
                    "CategoryId": category_counter,
                }));
            }
        }
        return Ok(categories);
    }

    // Category 1: Similar by genre to recently played
    let mut similar_items = Vec::new();
    for movie in &recent_movies[..recent_movies.len().min(4)] {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                r#"SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id WHERE mg_src.item_id = ? GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT 8"#,
                vec![movie.id.clone().into()],
            ))
            .await?;
        for row in &rows {
            if let Ok(id) = row.get_str("item_id") {
                if !similar_items.contains(&id) {
                    similar_items.push(id);
                }
            }
        }
    }

    if !similar_items.is_empty() {
        let ph = similar_items.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let items = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &format!("{} WHERE media_items.id IN ({}) ORDER BY media_items.production_year DESC LIMIT {item_limit}", crate::jellyfin::item_queries::media_item_select_sql(""), ph),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &similar_items { vals.push(id.as_str().into()); }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            categories.push(json!({
                "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                "RecommendationType": "SimilarToRecentlyPlayed",
                "BaselineItemName": recent_movies[0].title.clone(),
                "CategoryId": category_counter,
            }));
            category_counter += 1;
        }
    }
    if categories.len() >= category_limit { return Ok(categories); }

    // Category 2: Movies with same actors
    let mut actor_items = Vec::new();
    for movie in &recent_movies[..recent_movies.len().min(3)] {
        let rows = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                r#"SELECT mp2.item_id FROM media_people mp1 JOIN media_people mp2 ON mp1.person_id = mp2.person_id AND mp1.item_id <> mp2.item_id WHERE mp1.item_id = ? AND mp2.item_id NOT IN (SELECT id FROM media_items WHERE item_type IN ('Video', 'Episode')) GROUP BY mp2.item_id LIMIT 4"#,
                vec![movie.id.clone().into()],
            ))
            .await?;
        for row in &rows {
            if let Ok(id) = row.get_str("item_id") {
                if !actor_items.contains(&id) {
                    actor_items.push(id);
                }
            }
        }
    }

    if !actor_items.is_empty() {
        let ph = actor_items.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let items = db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &format!("{} WHERE media_items.id IN ({}) LIMIT {item_limit}", crate::jellyfin::item_queries::media_item_select_sql(""), ph),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &actor_items { vals.push(id.as_str().into()); }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            categories.push(json!({
                "Items": items.into_iter().map(|i| i.to_jellyfin_json()).collect::<Vec<_>>(),
                "RecommendationType": "HasActorFromRecentlyPlayed",
                "BaselineItemName": recent_movies[0].title.clone(),
                "CategoryId": category_counter,
            }));
        }
    }

    Ok(categories)
}

/// GET /Users/{user_id}/Suggestions — personalized suggestions
pub async fn user_suggestions(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);
    let parent_id = query.get("ParentId").map(String::as_str);

    // Return recently added unplayed items as suggestions
    let backend = state.db.get_database_backend();
    let (sql, vals) = if let Some(pid) = parent_id {
        (
            format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type IN ('Movie', 'Series') AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC LIMIT ?",
                crate::jellyfin::item_queries::media_item_select_sql("AND media_items.library_id = ?")
            ),
            vec![user_id.clone().into(), pid.into(), (limit as i64).into()],
        )
    } else {
        (
            format!(
                "{} WHERE media_items.is_folder = 1 AND media_items.item_type IN ('Movie', 'Series') AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC LIMIT ?",
                crate::jellyfin::item_queries::media_item_select_sql("")
            ),
            vec![user_id.clone().into(), (limit as i64).into()],
        )
    };

    match state.db.query_all(crate::db::helpers::portable_statement(backend, &sql, vals)).await {
        Ok(rows) => {
            let items = crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
            let total = items.len();

            // Batch load image tags
            let image_tags_map = if !items.is_empty() {
                let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
                crate::jellyfin::item_queries::batch_item_image_tags(&state.db, &ids).await.unwrap_or_default()
            } else {
                std::collections::HashMap::new()
            };

            let json_items: Vec<Value> = items.into_iter().map(|i| {
                let mut json = strip_nulls(i.to_jellyfin_json());
                if let Some(tags) = image_tags_map.get(&i.id) {
                    if let Some(primary) = tags.get("Primary") {
                        json["PrimaryImageTag"] = primary.clone();
                    }
                }
                json
            }).collect();

            Json(json!({ "Items": json_items, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

/// GET /Users/{user_id}/HomeSections — home screen layout
pub async fn home_sections(
    State(_state): State<Arc<AppState>>,
    Path(_user_id): Path<String>,
) -> Response {
    // Return a standard home screen layout matching ContentSection SDK model
    let sections = json!([
        { "Name": "Continue Watching", "SectionType": "Resume", "ViewType": "Resume", "Id": "resume", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Next Up", "SectionType": "NextUp", "ViewType": "NextUp", "Id": "nextup", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Latest Movies", "SectionType": "Latest", "ViewType": "Latest", "CollectionType": "movies", "Id": "latest-movies", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Latest TV Shows", "SectionType": "Latest", "ViewType": "Latest", "CollectionType": "tvshows", "Id": "latest-tvshows", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
        { "Name": "Suggestions", "SectionType": "Suggestions", "ViewType": "Suggestions", "Id": "suggestions", "ScrollDirection": "Horizontal", "CardSizeOffset": 0 },
    ]);
    Json(sections).into_response()
}

/// GET /Users/{user_id}/Sections/{section_id}/Items — items for a home section
pub async fn home_section_items(
    State(state): State<Arc<AppState>>,
    Path((user_id, section_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);

    match section_id.as_str() {
        "resume" => {
            match crate::jellyfin::item_queries::resume_media_items(&state.db, &user_id).await {
                Ok(items) => {
                    let total = items.len();
                    let enriched = super::enrich_resume_items(&state.db, items).await;
                    Json(json!({ "Items": enriched, "TotalRecordCount": total })).into_response()
                }
        Err(error) => internal_error(error.into()),
            }
        }
        "nextup" => {
            match super::discovery::shows_next_up(State(state), Query(query)).await {
                resp => resp,
            }
        }
        "latest-movies" | "latest-tvshows" => {
            let collection_type = if section_id == "latest-movies" { "movies" } else { "tvshows" };
            let backend = state.db.get_database_backend();
            // Find libraries matching the collection type
            let lib_rows = state.db
                .query_all(crate::db::helpers::portable_statement(
                    backend,
                    "SELECT id FROM libraries WHERE collection_type = ?",
                    vec![collection_type.into()],
                ))
                .await
                .unwrap_or_default();

            let mut all_items = Vec::new();
            for row in &lib_rows {
                if let Ok(lib_id) = row.get_str("id") {
                    if let Ok(items) = crate::jellyfin::item_queries::latest_media_items(&state.db, &user_id, Some(&lib_id)).await {
                        all_items.extend(items);
                    }
                }
            }
            all_items.sort_by_key(|i| std::cmp::Reverse(i.modified_at));
            all_items.truncate(limit);

            if !all_items.is_empty() {
                let ids: Vec<String> = all_items.iter().map(|i| i.id.clone()).collect();
                if let Ok(tags_map) = crate::jellyfin::item_queries::batch_item_image_tags(&state.db, &ids).await {
                    for item in &mut all_items {
                        if let Some(tags) = tags_map.get(&item.id) {
                            item.image_tags = Some(tags.clone());
                        }
                    }
                }
            }

            Json(json!(all_items.into_iter().map(|i| strip_nulls(i.to_jellyfin_json())).collect::<Vec<_>>())).into_response()
        }
        "suggestions" => {
            user_suggestions(State(state), Path(user_id), Query(query)).await
        }
        _ => Json(json!([])).into_response(),
    }
}
