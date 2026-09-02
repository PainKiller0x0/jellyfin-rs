use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Value as SeaValue};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{auth::query_user_id_or_request, common::internal_error, item_queries},
    library::models::MediaItem,
};

pub async fn similar_items(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(12);
    match similar_items_inner(&state.db, &item_id, limit).await {
        Ok(items) => {
            let total = items.len();
            let items = crate::jellyfin::items::enrich_item_list(&state.db, &user_id, items).await;
            Json(json!({ "Items": items, "TotalRecordCount": total, "StartIndex": 0 }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn similar_items_inner(
    db: &DatabaseConnection,
    item_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    if item_queries::find_media_item(db, "", item_id)
        .await
        .context("failed to check similar item seed")?
        .is_none()
    {
        return Ok(Vec::new());
    }

    let similar_ids = db
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT mg_rel.item_id
               FROM media_genres mg_src
               JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id
               JOIN media_items mi_rel ON mi_rel.id = mg_rel.item_id
               WHERE mi_rel.is_public = 1
                    AND (mi_rel.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = mi_rel.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = mi_rel.parent_id AND parent.is_public = 1))
                    AND (mg_src.item_id = ? OR mg_src.item_id = (SELECT parent_id FROM media_items WHERE id = ?))
               GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT ?"#,
            vec![item_id.into(), item_id.into(), i64::try_from(limit).unwrap_or(i64::MAX).into()],
        ))
        .await
        .context("failed to find similar items")?;

    let similar_ids: Vec<String> = similar_ids
        .iter()
        .filter_map(|r| r.get_opt_str("item_id").ok().flatten())
        .collect();

    if similar_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = similar_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.extra_type, media_items.video_type, media_items.iso_type, media_items.video_3d_format, media_items.is_folder, media_items.is_public, media_items.container, media_items.overview, media_items.official_rating, media_items.custom_rating, media_items.extended_video_type, media_items.original_title, media_items.sort_name, media_items.forced_sort_name, media_items.lock_data, media_items.locked_fields, media_items.tagline, media_items.collection_name, media_items.original_language, media_items.preferred_metadata_language, media_items.preferred_metadata_country_code, media_items.series_status, media_items.air_days, media_items.air_time, media_items.home_page_url, media_items.remote_trailers, media_items.production_locations, media_items.production_year, media_items.premiere_date, media_items.end_date, media_items.runtime_ticks, media_items.aspect_ratio, media_items.width, media_items.height, media_items.has_subtitles, media_items.photo_metadata, media_items.display_order, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.episode_number_end, media_items.airs_before_episode_number, media_items.airs_after_season_number, media_items.airs_before_season_number, media_items.series_name, media_items.community_rating, media_items.critic_rating, media_items.created_at, media_items.modified_at, CAST(0 AS bigint) AS is_favorite, CAST(0 AS bigint) AS played, CAST(0 AS bigint) AS playback_position_ticks, NULL AS played_percentage, CAST(0 AS bigint) AS play_count, NULL AS last_played_at FROM media_items LEFT JOIN libraries ON libraries.id = media_items.library_id WHERE media_items.id IN ({placeholders})"#
    );

    let mut values: Vec<SeaValue> = Vec::new();
    for id in &similar_ids {
        values.push(id.as_str().into());
    }
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await
        .context("failed to fetch similar items")?;
    item_queries::decode_media_items(&rows)
}

pub async fn search_hints(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let start_index = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let search_term = match query_value(&query, &["SearchTerm", "searchTerm"]) {
        Some(term) => term,
        None => {
            return Json(json!({
                "SearchHints": [],
                "TotalRecordCount": 0,
                "StartIndex": start_index
            }))
            .into_response();
        }
    };
    let limit = query_usize(&query, &["Limit", "limit"], 25);

    let include_types = query_value(&query, &["IncludeItemTypes", "includeItemTypes"])
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    let parent_id = query_value(&query, &["ParentId", "parentId"]);

    match search_hints_inner(&state.db, &user_id, search_term, include_types, parent_id).await {
        Ok(hints) => {
            let total = hints.len();
            let page = hints
                .into_iter()
                .skip(start_index)
                .take(limit)
                .collect::<Vec<_>>();
            Json(json!({
                "SearchHints": page,
                "TotalRecordCount": total,
                "StartIndex": start_index
            }))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn search_hints_inner(
    db: &DatabaseConnection,
    user_id: &str,
    search_term: &str,
    include_types: Option<Vec<&str>>,
    parent_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let like_pattern = format!("%{}%", search_term);

    let mut where_parts = vec![
        "media_items.is_folder = 0".to_string(),
        "media_items.is_public = 1".to_string(),
    ];
    where_parts.push("LOWER(media_items.title) LIKE LOWER(?)".to_string());

    if parent_id.is_some() {
        where_parts.push("media_items.parent_id = ?".to_string());
        where_parts.push(
            "(EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = ?) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = ? AND parent.is_public = 1))"
                .to_string(),
        );
    }

    if let Some(types) = &include_types {
        if !types.is_empty() {
            let placeholders = types
                .iter()
                .map(|_| "LOWER(media_items.item_type) = LOWER(?)")
                .collect::<Vec<_>>()
                .join(" OR ");
            where_parts.push(format!("({})", placeholders));
        }
    }

    let where_clause = format!(
        "WHERE {} ORDER BY media_items.title ASC",
        where_parts.join(" AND ")
    );
    let sql = item_queries::media_item_select_sql(&where_clause);

    let mut values: Vec<sea_orm::Value> = Vec::new();
    values.push(user_id.into());
    values.push(like_pattern.clone().into());
    if let Some(pid) = parent_id {
        values.push(pid.into());
        values.push(pid.into());
        values.push(pid.into());
    }
    if let Some(types) = &include_types {
        for t in types {
            values.push((*t).into());
        }
    }
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await
        .context("failed to fetch search hints")?;

    let items = item_queries::decode_media_items(&rows)?;

    let image_tags_map = if !items.is_empty() {
        let ids = items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        item_queries::batch_item_image_tags(db, &ids)
            .await
            .unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };

    let mut hints: Vec<Value> = items
        .into_iter()
        .map(|item| {
            let primary_image_tag = image_tags_map
                .get(&item.id)
                .and_then(|tags| tags.get("Primary"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut hint = json!({
                "ItemId": item.id,
                "Id": item.id,
                "Name": item.title,
                "Type": item.item_type,
                "ProductionYear": item.production_year,
                "RunTimeTicks": item.runtime_ticks,
                "MediaType": item.item_type,
                "MatchedTerm": item.title,
            });
            if let Some(tag) = primary_image_tag {
                hint["PrimaryImageTag"] = json!(tag);
            }
            hint
        })
        .collect();

    hints.sort_by(|a, b| {
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

    Ok(hints)
}

pub async fn shows_next_up(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    shows_next_up_response(
        state,
        query_user_id_or_request(&query, &request_user_id),
        query,
    )
    .await
}

pub(super) async fn shows_next_up_response(
    state: Arc<AppState>,
    user_id: String,
    query: HashMap<String, String>,
) -> Response {
    let series_id = query_value(&query, &["SeriesId", "seriesId"]);
    let start_index = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let limit = query_usize(&query, &["Limit", "limit"], 25).min(200);
    match next_up_inner(&state.db, &user_id, series_id).await {
        Ok(items) => {
            let total = items.len();
            let page = items
                .into_iter()
                .skip(start_index)
                .take(limit)
                .collect::<Vec<_>>();
            let json_items =
                crate::jellyfin::items::enrich_episode_list(&state.db, &user_id, page).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": total, "StartIndex": start_index }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn next_up_inner(
    db: &DatabaseConnection,
    user_id: &str,
    series_id: Option<&str>,
) -> anyhow::Result<Vec<MediaItem>> {
    if let Some(series_id) = series_id {
        return next_up_for_series(db, user_id, series_id).await;
    }

    let episode_visible = item_queries::visible_media_item_sql("media_items");
    let candidate_visible = item_queries::visible_media_item_sql("mi3");
    let sql = format!(
        r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0
            AND {episode_visible}
            AND COALESCE(user_data.played, 0) = 0
            AND COALESCE(user_data.playback_position_ticks, 0) = 0
            AND media_items.episode_number = (
                SELECT MIN(mi3.episode_number) FROM media_items mi3
                LEFT JOIN user_data ud3 ON ud3.item_id = mi3.id AND ud3.user_id = ?
                WHERE mi3.parent_id = media_items.parent_id
                    AND mi3.item_type = 'Episode' AND mi3.is_folder = 0 AND {candidate_visible}
                    AND COALESCE(ud3.played, 0) = 0
                    AND COALESCE(ud3.playback_position_ticks, 0) = 0
            )
            ORDER BY media_items.modified_at DESC"#,
        item_queries::media_item_select_sql("")
    );
    let values = vec![user_id.into(), user_id.into()];

    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await
        .context("failed to list next up episodes")?;

    item_queries::decode_media_items(&rows)
}

async fn next_up_for_series(
    db: &DatabaseConnection,
    user_id: &str,
    series_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let episode_visible = item_queries::visible_media_item_sql("media_items");
    let season_visible = item_queries::visible_media_item_sql("s");
    let resume_sql = format!(
        r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0
            AND {episode_visible}
            AND media_items.parent_id IN (SELECT s.id FROM media_items s WHERE s.parent_id = ? AND s.item_type = 'Season' AND {season_visible})
            AND COALESCE(user_data.played, 0) = 0
            AND COALESCE(user_data.playback_position_ticks, 0) > 0
            ORDER BY user_data.updated_at DESC, media_items.parent_id ASC, media_items.episode_number ASC"#,
        item_queries::media_item_select_sql("")
    );
    let resume_rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &resume_sql,
            vec![user_id.into(), series_id.into()],
        ))
        .await
        .context("failed to list in-progress next up episodes")?;
    let resume_items = item_queries::decode_media_items(&resume_rows)?;
    if !resume_items.is_empty() {
        return Ok(resume_items);
    }

    let played_episode_visible = item_queries::visible_media_item_sql("mi");
    let played_season_visible = item_queries::visible_media_item_sql("s");
    let last_played_sql = format!(
        r#"SELECT COALESCE(mi.season_number, 0) AS season_number,
                  COALESCE(mi.episode_number, 0) AS episode_number
           FROM user_data ud
           JOIN media_items mi ON mi.id = ud.item_id
           JOIN media_items s ON s.id = mi.parent_id
           WHERE ud.user_id = ?
             AND ud.played = 1
             AND mi.item_type = 'Episode'
             AND mi.is_folder = 0
             AND {played_episode_visible}
             AND s.parent_id = ?
             AND s.item_type = 'Season'
             AND {played_season_visible}
           ORDER BY COALESCE(mi.season_number, 0) DESC, COALESCE(mi.episode_number, 0) DESC
           LIMIT 1"#
    );
    let last_played = db
        .query_one_raw(crate::db::helpers::pg_statement(
            &last_played_sql,
            vec![user_id.into(), series_id.into()],
        ))
        .await
        .context("failed to find last played episode")?;

    let mut after_clause = String::new();
    let mut values: Vec<SeaValue> = vec![user_id.into(), series_id.into()];
    if let Some(row) = last_played {
        let season_number = row.get_i64("season_number")?;
        let episode_number = row.get_i64("episode_number")?;
        after_clause = "AND (COALESCE(media_items.season_number, 0) > ? OR (COALESCE(media_items.season_number, 0) = ? AND COALESCE(media_items.episode_number, 0) > ?))".to_string();
        values.push(season_number.into());
        values.push(season_number.into());
        values.push(episode_number.into());
    }

    let next_sql = format!(
        r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0
            AND {episode_visible}
            AND media_items.parent_id IN (SELECT s.id FROM media_items s WHERE s.parent_id = ? AND s.item_type = 'Season' AND {season_visible})
            AND COALESCE(user_data.played, 0) = 0
            {after_clause}
            ORDER BY COALESCE(media_items.season_number, 0) ASC, COALESCE(media_items.episode_number, 0) ASC"#,
        item_queries::media_item_select_sql("")
    );
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&next_sql, values))
        .await
        .context("failed to list series next up episodes")?;
    item_queries::decode_media_items(&rows)
}

pub async fn shows_missing() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
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

#[cfg(test)]
mod tests {
    use super::{
        next_up_inner, search_hints, search_hints_inner, shows_next_up, similar_items_inner,
    };
    use crate::entities::{
        genres::{self, Entity as Genres},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
    };
    use axum::{
        body::to_bytes,
        extract::{Extension, Query, State},
        response::IntoResponse,
    };
    use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
    use serde_json::Value;
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[tokio::test]
    async fn similar_items_require_public_seed_and_hide_private_results() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "seed", "Seed", 1).await;
        insert_media_item(&db, "public", "Public", 1).await;
        insert_media_item(&db, "private", "Private", 0).await;
        insert_media_item(&db, "private_seed", "Private Seed", 0).await;
        insert_media_item_typed(
            &db,
            "private-parent",
            "Private Parent",
            "",
            "Movie",
            1,
            None,
        )
        .await;
        update_item_visibility(&db, "private-parent", 0).await;
        insert_media_item_typed(
            &db,
            "public-child-seed",
            "Public Child Seed",
            "private-parent",
            "Movie",
            0,
            None,
        )
        .await;
        insert_genre(&db, "g1", "Drama").await;
        for item_id in [
            "seed",
            "public",
            "private",
            "private_seed",
            "public-child-seed",
        ] {
            link_genre(&db, item_id, "g1").await;
        }

        let items = similar_items_inner(&db, "seed", 10).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "public");

        let items = similar_items_inner(&db, "private_seed", 10).await.unwrap();
        assert!(items.is_empty());

        let items = similar_items_inner(&db, "public-child-seed", 10)
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_hints_inner_returns_all_matches_for_response_paging() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "one", "Alpha One", 1).await;
        insert_media_item(&db, "two", "Alpha Two", 1).await;
        insert_media_item(&db, "private", "Alpha Private", 0).await;

        let hints = search_hints_inner(&db, "u1", "Alpha", None, None)
            .await
            .unwrap();

        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0]["Name"], "Alpha One");
        assert_eq!(hints[1]["Name"], "Alpha Two");
    }

    #[tokio::test]
    async fn search_hints_response_includes_start_index() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "one", "Alpha One", 1).await;
        insert_media_item(&db, "two", "Alpha Two", 1).await;

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("searchTerm".to_string(), "Alpha".to_string());
        query.insert("startIndex".to_string(), "1".to_string());
        query.insert("limit".to_string(), "1".to_string());
        let response = search_hints(State(state), Extension("u1".to_string()), Query(query))
            .await
            .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["TotalRecordCount"], 2);
        assert_eq!(value["StartIndex"], 1);
        let hints = value["SearchHints"].as_array().unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0]["Name"], "Alpha Two");
    }

    #[tokio::test]
    async fn search_hints_require_visible_parent() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item_typed(&db, "visible-parent", "Visible", "", "Folder", 1, None).await;
        insert_media_item_typed(&db, "hidden-parent", "Hidden", "", "Folder", 1, None).await;
        update_item_visibility(&db, "hidden-parent", 0).await;
        insert_media_item_typed(
            &db,
            "visible-child",
            "Alpha Visible",
            "visible-parent",
            "Movie",
            0,
            None,
        )
        .await;
        insert_media_item_typed(
            &db,
            "hidden-child",
            "Alpha Hidden",
            "hidden-parent",
            "Movie",
            0,
            None,
        )
        .await;

        let visible = search_hints_inner(&db, "u1", "Alpha", None, Some("visible-parent"))
            .await
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["ItemId"], "visible-child");

        let hidden = search_hints_inner(&db, "u1", "Alpha", None, Some("hidden-parent"))
            .await
            .unwrap();
        assert!(hidden.is_empty());
    }

    #[tokio::test]
    async fn shows_next_up_paging_keeps_total_record_count() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item_typed(&db, "series", "Series", "", "Series", 1, None).await;
        insert_media_item_typed(&db, "season-1", "S1", "series", "Season", 1, None).await;
        insert_media_item_typed(&db, "season-2", "S2", "series", "Season", 1, None).await;
        insert_media_item_typed(&db, "season-private", "S3", "series", "Season", 1, None).await;
        update_item_visibility(&db, "season-private", 0).await;
        insert_media_item_typed(&db, "episode-1", "E1", "season-1", "Episode", 0, Some(1)).await;
        insert_media_item_typed(&db, "episode-2", "E2", "season-2", "Episode", 0, Some(2)).await;
        insert_media_item_typed(
            &db,
            "episode-hidden",
            "E3",
            "season-private",
            "Episode",
            0,
            Some(3),
        )
        .await;

        let global = next_up_inner(&db, "u1", None).await.unwrap();
        assert_eq!(global.len(), 2);
        assert!(global.iter().all(|item| item.id != "episode-hidden"));

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("UserId".to_string(), "u1".to_string());
        query.insert("SeriesId".to_string(), "series".to_string());
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = shows_next_up(State(state), Extension("u1".to_string()), Query(query))
            .await
            .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["TotalRecordCount"], 2);
        assert_eq!(value["StartIndex"], 1);
        let items = value["Items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["Id"], "episode-2");
    }

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        is_public: i64,
    ) {
        insert_media_item_typed(db, id, title, "", "Movie", 0, None).await;
        update_item_visibility(db, id, is_public).await;
    }

    async fn insert_media_item_typed(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        episode_number: Option<i64>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(1),
            episode_number: Set(episode_number),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn update_item_visibility(db: &sea_orm::DatabaseConnection, id: &str, is_public: i64) {
        let mut active: media_items::ActiveModel = MediaItems::find_by_id(id.to_string())
            .one(db)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.is_public = Set(is_public);
        active.update(db).await.unwrap();
    }

    async fn insert_genre(db: &sea_orm::DatabaseConnection, id: &str, name: &str) {
        Genres::insert(genres::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn link_genre(db: &sea_orm::DatabaseConnection, item_id: &str, genre_id: &str) {
        MediaGenres::insert(media_genres::ActiveModel {
            item_id: Set(item_id.to_string()),
            genre_id: Set(genre_id.to_string()),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    fn test_state(db: DatabaseConnection) -> crate::app::state::AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        crate::app::state::AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"test"),
            access_token: "test-token".to_string(),
            db,
            media_dirs: Vec::new(),
            http_client: reqwest::Client::new(),
            tmdb_api_key: RwLock::new(None),
            tmdb_proxy_url: Arc::new(RwLock::new(None)),
            tmdb_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            douban_cookie: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            chapter_image_task_cancel: tokio::sync::Mutex::new(None),
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            admin_http_log_seq: std::sync::atomic::AtomicU64::new(0),
            admin_http_logs: RwLock::new(std::collections::VecDeque::new()),
            playback_distribution: RwLock::new(crate::app::state::PlaybackDistribution::default()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
