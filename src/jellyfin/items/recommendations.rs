use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{auth::query_user_id_or_request, common::internal_error, item_queries},
};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

fn query_value<'a>(query: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    query
        .iter()
        .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn query_usize(query: &HashMap<String, String>, keys: &[&str], default: usize) -> usize {
    query_value(query, keys)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

pub async fn movie_recommendations(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let parent_id = query_value(&query, &["ParentId", "parentId"]);
    let category_limit = query_usize(&query, &["CategoryLimit", "categoryLimit"], 5);
    let item_limit = query_usize(&query, &["ItemLimit", "itemLimit"], 8);
    match movie_recommendations_inner(&state.db, &user_id, parent_id, category_limit, item_limit)
        .await
    {
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
    let mut category_counter: i64 = 1;
    let media_visible = visible_media_item_sql("media_items");
    let related_visible = visible_media_item_sql("mi_rel");

    // Jellyfin Movie items may be file-backed, ISO, DVD, or Blu-ray paths.
    let recent_movies = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                "{} WHERE media_items.item_type = 'Movie' AND {media_visible} AND user_data.played = 1 AND user_data.play_count > 0 {} ORDER BY user_data.last_played_at DESC LIMIT 12",
                crate::jellyfin::item_queries::media_item_select_sql(""),
                parent_id.map(|_p| "AND media_items.library_id = ?").unwrap_or(""),
            ),
            if let Some(pid) = parent_id {
                vec![user_id.into(), pid.into()]
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
            "{} WHERE media_items.item_type = 'Movie' AND {media_visible} AND media_items.community_rating IS NOT NULL {} ORDER BY media_items.community_rating DESC LIMIT ?",
            crate::jellyfin::item_queries::media_item_select_sql(""),
            parent_id
                .map(|_| "AND media_items.library_id = ?")
                .unwrap_or(""),
        );
        let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
        if let Some(pid) = parent_id {
            vals.push(pid.into());
        }
        vals.push((item_limit as i64).into());
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&top_rated_sql, vals))
            .await
        {
            let items = crate::jellyfin::item_queries::decode_media_items(&rows)?;
            if !items.is_empty() {
                let items = crate::jellyfin::items::enrich_item_list(db, user_id, items).await;
                categories.push(json!({
                    "Items": items,
                    "RecommendationType": "SimilarToRecentlyPlayed",
                    "BaselineItemName": "Top Rated",
                    "CategoryId": category_counter,
                }));
                category_counter += 1;
            }
        }
        if categories.len() >= category_limit {
            return Ok(categories);
        }

        // Category: Recently added movies
        let recent_sql = format!(
            "{} WHERE media_items.item_type = 'Movie' AND {media_visible} {} ORDER BY media_items.created_at DESC LIMIT ?",
            crate::jellyfin::item_queries::media_item_select_sql(""),
            parent_id
                .map(|_| "AND media_items.library_id = ?")
                .unwrap_or(""),
        );
        let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
        if let Some(pid) = parent_id {
            vals.push(pid.into());
        }
        vals.push((item_limit as i64).into());
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&recent_sql, vals))
            .await
        {
            let items = crate::jellyfin::item_queries::decode_media_items(&rows)?;
            if !items.is_empty() {
                let items = crate::jellyfin::items::enrich_item_list(db, user_id, items).await;
                categories.push(json!({
                    "Items": items,
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
            .query_all_raw(crate::db::helpers::pg_statement(
                &format!(
                    "SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id JOIN media_items mi_rel ON mi_rel.id = mg_rel.item_id WHERE mg_src.item_id = ? AND {related_visible} GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT 8"
                ),
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
        let ph = similar_items
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let items = db
            .query_all_raw(crate::db::helpers::pg_statement(
                &format!("{} WHERE media_items.id IN ({}) AND {media_visible} ORDER BY media_items.production_year DESC LIMIT {item_limit}", crate::jellyfin::item_queries::media_item_select_sql(""), ph),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &similar_items { vals.push(id.as_str().into()); }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            let items = crate::jellyfin::items::enrich_item_list(db, user_id, items).await;
            categories.push(json!({
                "Items": items,
                "RecommendationType": "SimilarToRecentlyPlayed",
                "BaselineItemName": recent_movies[0].title.clone(),
                "CategoryId": category_counter,
            }));
            category_counter += 1;
        }
    }
    if categories.len() >= category_limit {
        return Ok(categories);
    }

    // Category 2: Movies with same actors
    let mut actor_items = Vec::new();
    for movie in &recent_movies[..recent_movies.len().min(3)] {
        let rows = db
            .query_all_raw(crate::db::helpers::pg_statement(
                &format!(
                    "SELECT mp2.item_id FROM media_people mp1 JOIN media_people mp2 ON mp1.person_id = mp2.person_id AND mp1.item_id <> mp2.item_id JOIN media_items mi_rel ON mi_rel.id = mp2.item_id WHERE mp1.item_id = ? AND {related_visible} AND mi_rel.item_type NOT IN ('Video', 'Episode') GROUP BY mp2.item_id LIMIT 4"
                ),
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
        let ph = actor_items
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let items = db
            .query_all_raw(crate::db::helpers::pg_statement(
                &format!(
                    "{} WHERE media_items.id IN ({}) AND {media_visible} LIMIT {item_limit}",
                    crate::jellyfin::item_queries::media_item_select_sql(""),
                    ph
                ),
                {
                    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
                    for id in &actor_items {
                        vals.push(id.as_str().into());
                    }
                    vals
                },
            ))
            .await?;
        let items = crate::jellyfin::item_queries::decode_media_items(&items)?;
        if !items.is_empty() {
            let items = crate::jellyfin::items::enrich_item_list(db, user_id, items).await;
            categories.push(json!({
                "Items": items,
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
    let limit = query_usize(&query, &["Limit", "limit"], 16);
    let start = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let parent_id = query_value(&query, &["ParentId", "parentId"]);

    // Return recently added unplayed items as suggestions
    let visible = visible_media_item_sql("media_items");
    let (sql, vals) = if let Some(pid) = parent_id {
        (
            format!(
                "{} WHERE (media_items.item_type = 'Movie' OR (media_items.item_type = 'Series' AND media_items.is_folder = 1)) AND {visible} AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC",
                crate::jellyfin::item_queries::media_item_select_sql(
                    "AND media_items.library_id = ?"
                )
            ),
            vec![user_id.clone().into(), pid.into()],
        )
    } else {
        (
            format!(
                "{} WHERE (media_items.item_type = 'Movie' OR (media_items.item_type = 'Series' AND media_items.is_folder = 1)) AND {visible} AND COALESCE(user_data.played, 0) = 0 ORDER BY media_items.created_at DESC",
                crate::jellyfin::item_queries::media_item_select_sql("")
            ),
            vec![user_id.clone().into()],
        )
    };

    match state
        .db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, vals))
        .await
    {
        Ok(rows) => {
            let mut items =
                crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
            let total = items.len();
            items = items.into_iter().skip(start).take(limit).collect();

            let json_items =
                crate::jellyfin::items::enrich_item_list(&state.db, &user_id, items).await;

            Json(json!({ "Items": json_items, "TotalRecordCount": total, "StartIndex": start }))
                .into_response()
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
                    let start = query
                        .get("StartIndex")
                        .or_else(|| query.get("startIndex"))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let limit = query
                        .get("Limit")
                        .or_else(|| query.get("limit"))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(total);
                    let page = items.into_iter().skip(start).take(limit).collect();
                    let enriched = super::enrich_resume_items(&state.db, &user_id, page).await;
                    Json(json!({ "Items": enriched, "TotalRecordCount": total, "StartIndex": start }))
                        .into_response()
                }
                Err(error) => internal_error(error),
            }
        }
        "nextup" => super::discovery::shows_next_up_response(state, user_id, query).await,
        "latest-movies" | "latest-tvshows" => {
            let collection_type = if section_id == "latest-movies" {
                "movies"
            } else {
                "tvshows"
            };
            // Find libraries matching the collection type
            let lib_rows = state
                .db
                .query_all_raw(crate::db::helpers::pg_statement(
                    "SELECT id FROM libraries WHERE collection_type = ?",
                    vec![collection_type.into()],
                ))
                .await
                .unwrap_or_default();

            let mut all_items = Vec::new();
            for row in &lib_rows {
                if let Ok(lib_id) = row.get_str("id") {
                    if let Ok(items) = crate::jellyfin::item_queries::latest_media_items(
                        &state.db,
                        &user_id,
                        Some(&lib_id),
                    )
                    .await
                    {
                        all_items.extend(items);
                    }
                }
            }
            all_items.sort_by_key(|i| std::cmp::Reverse(i.modified_at));
            all_items.truncate(limit);

            let _ = item_queries::attach_item_image_tags(&state.db, &mut all_items).await;

            let json_items =
                crate::jellyfin::items::enrich_episode_list(&state.db, &user_id, all_items).await;
            Json(json_items).into_response()
        }
        "suggestions" => user_suggestions(State(state), Path(user_id), Query(query)).await,
        _ => Json(json!([])).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::movie_recommendations_inner;
    use crate::entities::{
        libraries::{self, Entity as Libraries},
        media_items::{self, Entity as MediaItems},
    };
    use sea_orm::{DatabaseConnection, EntityTrait, Set};

    #[tokio::test]
    async fn movie_recommendations_ignore_private_candidates_before_limit() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        for (id, title, parent_id, is_public, rating) in [
            ("private", "Private", "", 0, 10.0),
            ("hidden-parent", "Hidden Parent", "", 0, 9.5),
            ("hidden-child", "Hidden Child", "hidden-parent", 1, 9.0),
            ("public", "Public", "", 1, 8.0),
        ] {
            insert_movie(&db, id, title, parent_id, is_public, rating).await;
        }

        let categories = movie_recommendations_inner(&db, "u1", None, 1, 1)
            .await
            .unwrap();
        assert_eq!(categories.len(), 1);
        assert_eq!(categories[0]["Items"][0]["Id"], "public");
    }

    async fn insert_library(db: &DatabaseConnection) {
        Libraries::insert(libraries::ActiveModel {
            id: Set("movies".to_string()),
            name: Set("Movies".to_string()),
            collection_type: Set("movies".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_movie(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        is_public: i64,
        rating: f64,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set("movies".to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(1),
            is_public: Set(is_public),
            community_rating: Set(Some(rating)),
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
