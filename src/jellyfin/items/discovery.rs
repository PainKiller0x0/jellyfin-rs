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

use super::media_list_response;

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

pub async fn similar_items(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(12);
    match similar_items_inner(&state.db, &item_id, limit).await {
        Ok(mut items) => {
            if !items.is_empty() {
                let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
                if let Ok(tags_map) =
                    crate::jellyfin::item_queries::batch_item_image_tags(&state.db, &ids).await
                {
                    for item in &mut items {
                        if let Some(tags) = tags_map.get(&item.id) {
                            item.image_tags = Some(tags.clone());
                        }
                    }
                }
            }
            media_list_response(items)
        }
        Err(error) => internal_error(error),
    }
}

async fn similar_items_inner(
    db: &DatabaseConnection,
    item_id: &str,
    limit: usize,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    if item_queries::find_media_item(db, "", item_id)
        .await
        .context("failed to check similar item seed")?
        .is_none()
    {
        return Ok(Vec::new());
    }

    let similar_ids = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
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
        r#"SELECT media_items.id, media_items.title, media_items.path, media_items.library_id, libraries.collection_type, media_items.parent_id, media_items.item_type, media_items.is_folder, media_items.is_public, media_items.container, media_items.overview, media_items.official_rating, media_items.extended_video_type, media_items.production_year, media_items.runtime_ticks, media_items.size_bytes, media_items.season_number, media_items.episode_number, media_items.community_rating, media_items.critic_rating, media_items.created_at, media_items.modified_at, CAST(0 AS bigint) AS is_favorite, CAST(0 AS bigint) AS played, CAST(0 AS bigint) AS playback_position_ticks, NULL AS played_percentage, CAST(0 AS bigint) AS play_count, NULL AS last_played_at FROM media_items LEFT JOIN libraries ON libraries.id = media_items.library_id WHERE media_items.id IN ({placeholders})"#
    );

    let mut values: Vec<SeaValue> = Vec::new();
    for id in &similar_ids {
        values.push(id.as_str().into());
    }
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
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
    let search_term = match query.get("SearchTerm").filter(|value| !value.is_empty()) {
        Some(term) => term.clone(),
        None => return Json(json!({ "SearchHints": [], "TotalRecordCount": 0 })).into_response(),
    };
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(25);

    let include_types = query
        .get("IncludeItemTypes")
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    let parent_id = query.get("ParentId").map(String::as_str);

    match search_hints_inner(&state.db, &user_id, &search_term, include_types, parent_id).await {
        Ok(hints) => {
            let total = hints.len();
            let page = hints
                .into_iter()
                .skip(start_index)
                .take(limit)
                .collect::<Vec<_>>();
            Json(json!({ "SearchHints": page, "TotalRecordCount": total })).into_response()
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
    let backend = db.get_database_backend();
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
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await
        .context("failed to fetch search hints")?;

    let items = item_queries::decode_media_items(&rows)?;

    // Batch load image tags for PrimaryImageTag
    let image_tags_map = if !items.is_empty() {
        let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
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
            let json_items = crate::jellyfin::items::enrich_episode_list(&state.db, page).await;
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
    let backend = db.get_database_backend();
    let (sql, values) = if let Some(series_id) = series_id {
        let episode_visible = visible_media_item_sql("media_items");
        let season_visible = visible_media_item_sql("s");
        // For a specific series: return all unwatched episodes, sorted by season and episode number
        (
            format!(
                r#"{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0
                    AND {episode_visible}
                    AND media_items.parent_id IN (SELECT s.id FROM media_items s WHERE s.parent_id = ? AND s.item_type = 'Season' AND {season_visible})
                    AND COALESCE(user_data.played, 0) = 0
                    AND COALESCE(user_data.playback_position_ticks, 0) = 0
                    ORDER BY media_items.parent_id ASC, media_items.episode_number ASC"#,
                item_queries::media_item_select_sql("")
            ),
            vec![user_id.into(), series_id.into()],
        )
    } else {
        let episode_visible = visible_media_item_sql("media_items");
        let candidate_visible = visible_media_item_sql("mi3");
        // Global next up: for each series, show the next unwatched episode
        (
            format!(
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
            ),
            vec![user_id.into(), user_id.into()],
        )
    };

    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend, &sql, values,
        ))
        .await
        .context("failed to list next up episodes")?;

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
    use super::{next_up_inner, search_hints_inner, shows_next_up, similar_items_inner};
    use axum::{
        body::to_bytes,
        extract::{Extension, Query, State},
        response::IntoResponse,
    };
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use serde_json::Value;
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[tokio::test]
    async fn similar_items_require_public_seed_and_hide_private_results() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
    async fn search_hints_require_visible_parent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
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
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, ?, 1, 1, 1)",
            vec![id.into(), title.into(), id.into(), is_public.into()],
        ))
        .await
        .unwrap();
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
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, episode_number, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, ?, ?, 1, ?, 1, 1, 1)",
            vec![
                id.into(),
                title.into(),
                id.into(),
                parent_id.into(),
                item_type.into(),
                is_folder.into(),
                episode_number.into(),
            ],
        ))
        .await
        .unwrap();
    }

    async fn update_item_visibility(db: &sea_orm::DatabaseConnection, id: &str, is_public: i64) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "UPDATE media_items SET is_public = ? WHERE id = ?",
            vec![is_public.into(), id.into()],
        ))
        .await
        .unwrap();
    }

    async fn insert_genre(db: &sea_orm::DatabaseConnection, id: &str, name: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO genres (id, name, created_at) VALUES (?, ?, 1)",
            vec![id.into(), name.into()],
        ))
        .await
        .unwrap();
    }

    async fn link_genre(db: &sea_orm::DatabaseConnection, item_id: &str, genre_id: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_genres (item_id, genre_id) VALUES (?, ?)",
            vec![item_id.into(), genre_id.into()],
        ))
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
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
