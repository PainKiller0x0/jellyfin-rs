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
    jellyfin::item_queries::{attach_item_image_tags, batch_item_provider_ids},
    jellyfin::{
        auth::query_user_id_or_request,
        common::{internal_error, strip_nulls},
    },
    library::models::MediaItem,
};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

/// Deduplicate episodes by (parent_id, season_number, episode_number).
/// When multiple video files exist for the same episode, keep the best
/// display representative first, then fall back to the largest source.
fn deduplicate_episodes(
    items: Vec<MediaItem>,
    provider_map: &HashMap<String, Value>,
) -> Vec<MediaItem> {
    let mut map: HashMap<(String, i64, i64), MediaItem> = HashMap::new();
    for item in items {
        let key = (
            item.parent_id.clone(),
            item.season_number.unwrap_or(0),
            item.episode_number.unwrap_or(0),
        );
        let should_replace = match map.get(&key) {
            Some(existing) => {
                episode_representative_score(&item, provider_map)
                    > episode_representative_score(existing, provider_map)
            }
            None => true,
        };
        if should_replace {
            map.insert(key, item);
        }
    }
    let mut result: Vec<_> = map.into_values().collect();
    result.sort_by(|a, b| {
        a.season_number
            .unwrap_or(0)
            .cmp(&b.season_number.unwrap_or(0))
            .then_with(|| {
                a.episode_number
                    .unwrap_or(0)
                    .cmp(&b.episode_number.unwrap_or(0))
            })
            .then_with(|| a.title.cmp(&b.title))
    });
    result
}

fn episode_representative_score<'a>(
    item: &'a MediaItem,
    provider_map: &HashMap<String, Value>,
) -> (u8, i64, &'a str) {
    let has_provider = provider_map
        .get(&item.id)
        .and_then(Value::as_object)
        .is_some_and(|providers| !providers.is_empty());
    let has_primary_image = item
        .image_tags
        .as_ref()
        .and_then(|tags| tags.get("Primary"))
        .and_then(Value::as_str)
        .is_some_and(|tag| !tag.is_empty());
    let has_overview = item
        .overview
        .as_deref()
        .is_some_and(|overview| !overview.trim().is_empty());
    let metadata_score =
        (has_provider as u8) * 4 + (has_primary_image as u8) * 2 + (has_overview as u8);
    (
        metadata_score,
        item.size_bytes.unwrap_or(0),
        item.id.as_str(),
    )
}

pub async fn show_seasons(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let start_index = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let limit = query_limit(&query);
    match child_items_by_type(&state.db, &user_id, &show_id, "Season").await {
        Ok(items) => {
            let total = items.len();
            let page = items
                .into_iter()
                .skip(start_index)
                .take(limit)
                .collect::<Vec<_>>();
            let json_items = enrich_season_list(&state.db, &user_id, page).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": total, "StartIndex": start_index }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn show_episodes(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let start_index = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let limit = query_limit(&query);
    let result = if let Some(season_id) = query_value(&query, &["SeasonId", "seasonId"]) {
        child_items_by_type(&state.db, &user_id, season_id, "Episode").await
    } else {
        descendant_episodes(&state.db, &user_id, &show_id).await
    };
    match result {
        Ok(items) => {
            let total = items.len();
            let page = items
                .into_iter()
                .skip(start_index)
                .take(limit)
                .collect::<Vec<_>>();
            let json_items = super::enrich_episode_list(&state.db, &user_id, page).await;
            Json(json!({ "Items": json_items, "TotalRecordCount": total, "StartIndex": start_index }))
                .into_response()
        }
        Err(error) => internal_error(error),
    }
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

fn query_limit(query: &HashMap<String, String>) -> usize {
    query_value(query, &["Limit", "limit"])
        .and_then(|value| value.parse::<usize>().ok())
        .map(|limit| limit.min(200))
        .unwrap_or(usize::MAX)
}

/// Batch-enrich season items with RecursiveItemCount and UnplayedItemCount.
async fn enrich_season_list(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    items: Vec<MediaItem>,
) -> Vec<Value> {
    let season_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    // Collect unique series IDs (parent_id of each season)
    let series_ids: Vec<String> = items
        .iter()
        .map(|i| i.parent_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // Batch query: series id -> title
    let mut series_map: HashMap<String, String> = HashMap::new();
    if !series_ids.is_empty() {
        let placeholders = series_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let visible = visible_media_item_sql("media_items");
        let sql =
            format!("SELECT id, title FROM media_items WHERE id IN ({placeholders}) AND {visible}");
        let values: Vec<sea_orm::Value> = series_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(id), Ok(title)) = (row.get_str("id"), row.get_str("title")) {
                    series_map.insert(id, title);
                }
            }
        }
    }

    // Batch query: count episodes per season
    let mut count_map: HashMap<String, i64> = HashMap::new();
    if !season_ids.is_empty() {
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let visible = visible_media_item_sql("media_items");
        let sql = format!(
            "SELECT parent_id, COUNT(DISTINCT (COALESCE(season_number, 0), COALESCE(episode_number, 0))) AS cnt FROM media_items WHERE parent_id IN ({placeholders}) AND item_type = 'Episode' AND {visible} GROUP BY parent_id"
        );
        let values: Vec<sea_orm::Value> = season_ids.iter().map(|id| id.as_str().into()).collect();
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(pid), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    count_map.insert(pid, cnt);
                }
            }
        }
    }

    // Batch query: count played episodes per season for user
    let mut played_map: HashMap<String, i64> = HashMap::new();
    if !season_ids.is_empty() {
        let placeholders = season_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let visible = visible_media_item_sql("mi");
        let sql = format!(
            "SELECT mi.parent_id, COUNT(DISTINCT (COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) AS cnt FROM user_data ud JOIN media_items mi ON mi.id = ud.item_id WHERE mi.parent_id IN ({placeholders}) AND mi.item_type = 'Episode' AND {visible} AND ud.user_id = ? AND ud.played = 1 GROUP BY mi.parent_id"
        );
        let mut values: Vec<sea_orm::Value> =
            season_ids.iter().map(|id| id.as_str().into()).collect();
        values.push(user_id.into());
        if let Ok(rows) = db
            .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
            .await
        {
            for row in &rows {
                if let (Ok(pid), Ok(cnt)) = (row.get_str("parent_id"), row.get_i64("cnt")) {
                    played_map.insert(pid, cnt);
                }
            }
        }
    }

    let provider_map = batch_item_provider_ids(db, &season_ids)
        .await
        .unwrap_or_default();

    items
        .into_iter()
        .map(|item| {
            let mut val = item.to_jellyfin_json();
            let total = count_map.get(&item.id).copied().unwrap_or(0);
            let played = played_map.get(&item.id).copied().unwrap_or(0);
            if let Some(provider_ids) = provider_map.get(&item.id) {
                val["ProviderIds"] = provider_ids.clone();
            }
            val["ChildCount"] = json!(total);
            val["RecursiveItemCount"] = json!(total);
            val["EpisodeCount"] = json!(total);
            val["UserData"]["UnplayedItemCount"] = json!(total - played);
            // Add SeriesId and SeriesName
            val["SeriesId"] = json!(item.parent_id);
            if let Some(series_name) = series_map.get(&item.parent_id) {
                val["SeriesName"] = json!(series_name);
            }
            strip_nulls(val)
        })
        .collect()
}

async fn child_items_by_type(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    parent_id: &str,
    item_type: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let order = if item_type == "Episode" {
        "ORDER BY media_items.season_number ASC, media_items.episode_number ASC"
    } else if item_type == "Season" {
        "ORDER BY media_items.season_number ASC NULLS LAST, media_items.title ASC"
    } else {
        "ORDER BY media_items.title ASC"
    };
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &crate::jellyfin::item_queries::media_item_select_sql(&format!(
                "WHERE media_items.parent_id = ? AND media_items.item_type = ? AND {} {order}",
                visible_media_item_sql("media_items")
            )),
            vec![user_id.into(), parent_id.into(), item_type.into()],
        ))
        .await
        .with_context(|| format!("failed to list {item_type} children for: {parent_id}"))?;
    let mut items = rows
        .iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show child items")?;
    let mut provider_map = HashMap::new();
    if !items.is_empty() {
        let _ = attach_item_image_tags(db, &mut items).await;
        if item_type == "Episode" {
            let ids = items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>();
            provider_map = batch_item_provider_ids(db, &ids).await.unwrap_or_default();
        }
    }
    if item_type == "Episode" && items.len() > 1 {
        items = deduplicate_episodes(items, &provider_map);
    }
    Ok(items)
}

async fn descendant_episodes(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    show_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &format!(
                r#"WITH RECURSIVE tree(id) AS (SELECT media_items.id FROM media_items WHERE media_items.id = ? AND {} UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id WHERE {}) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.item_type = 'Episode' AND {} ORDER BY media_items.title ASC"#,
                visible_media_item_sql("media_items"),
                visible_media_item_sql("media_items"),
                crate::jellyfin::item_queries::media_item_select_sql("").trim()
                ,
                visible_media_item_sql("media_items")
            ),
            vec![show_id.into(), user_id.into(), show_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list episodes for show: {show_id}"))?;
    let mut items = rows
        .iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show episodes")?;
    let mut provider_map = HashMap::new();
    if !items.is_empty() {
        let _ = attach_item_image_tags(db, &mut items).await;
        let ids = items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        provider_map = batch_item_provider_ids(db, &ids).await.unwrap_or_default();
    }
    if items.len() > 1 {
        items = deduplicate_episodes(items, &provider_map);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::{
        child_items_by_type, descendant_episodes, enrich_season_list, show_episodes, show_seasons,
    };
    use crate::entities::{
        image_assets::{self, Entity as ImageAssets},
        libraries::{self, Entity as Libraries},
        media_items::{self, Entity as MediaItems},
    };
    use axum::{
        body::to_bytes,
        extract::{Extension, Path, Query, State},
        response::IntoResponse,
    };
    use sea_orm::{DatabaseConnection, EntityTrait, Set};
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[tokio::test]
    async fn show_child_queries_hide_private_tree_members() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(
            &db,
            "season-public",
            "S1",
            "series",
            "Season",
            1,
            1,
            None,
            None,
        )
        .await;
        insert_item(
            &db,
            "season-private",
            "S2",
            "series",
            "Season",
            1,
            0,
            None,
            None,
        )
        .await;
        insert_item(
            &db,
            "episode-public",
            "E1",
            "season-public",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        insert_item(
            &db,
            "episode-private",
            "E2",
            "season-public",
            "Episode",
            0,
            0,
            Some(1),
            Some(2),
        )
        .await;
        insert_item(
            &db,
            "episode-under-private-season",
            "E3",
            "season-private",
            "Episode",
            0,
            1,
            Some(2),
            Some(1),
        )
        .await;

        let seasons = child_items_by_type(&db, "u1", "series", "Season")
            .await
            .unwrap();
        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].id, "season-public");

        let enriched = enrich_season_list(&db, "u1", seasons).await;
        assert_eq!(enriched[0]["RecursiveItemCount"], 1);

        let episodes = descendant_episodes(&db, "u1", "series").await.unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, "episode-public");

        let private_parent = descendant_episodes(&db, "u1", "season-private")
            .await
            .unwrap();
        assert!(private_parent.is_empty());

        let private_season_children = child_items_by_type(&db, "u1", "season-private", "Episode")
            .await
            .unwrap();
        assert!(private_season_children.is_empty());
    }

    #[tokio::test]
    async fn show_episodes_returns_paged_query_result() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(&db, "season", "S1", "series", "Season", 1, 1, None, None).await;
        insert_item(
            &db,
            "episode-1",
            "E1",
            "season",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        insert_item(
            &db,
            "episode-2",
            "E2",
            "season",
            "Episode",
            0,
            1,
            Some(1),
            Some(2),
        )
        .await;

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("UserId".to_string(), "u1".to_string());
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = show_episodes(
            State(state),
            Path("series".to_string()),
            Extension("u1".to_string()),
            Query(query),
        )
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

    #[tokio::test]
    async fn show_episodes_prefers_metadata_rich_duplicate_over_larger_file() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(&db, "season", "S1", "series", "Season", 1, 1, None, None).await;
        insert_episode_with_size(
            &db,
            "large-generic",
            "Series 2023",
            "season",
            Some(1),
            Some(2),
            10_000,
            None,
        )
        .await;
        insert_episode_with_size(
            &db,
            "smaller-scraped",
            "Real Episode",
            "season",
            Some(1),
            Some(2),
            8_000,
            Some("scraped overview"),
        )
        .await;
        crate::db::provider_ids::upsert(&db, "smaller-scraped", "Tmdb", "episode-tmdb")
            .await
            .unwrap();
        ImageAssets::insert(image_assets::ActiveModel {
            id: Set("image-1".to_string()),
            item_id: Set("smaller-scraped".to_string()),
            image_type: Set("Primary".to_string()),
            image_index: Set(0),
            etag: Set(Some("etag-1".to_string())),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        let episodes = child_items_by_type(&db, "u1", "season", "Episode")
            .await
            .unwrap();

        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, "smaller-scraped");
        assert_eq!(episodes[0].image_tags, Some(json!({"Primary": "etag-1"})));
    }

    #[tokio::test]
    async fn enrich_season_list_counts_duplicate_episode_versions_once() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(&db, "season", "S1", "series", "Season", 1, 1, None, None).await;
        insert_item(
            &db,
            "episode-1080p",
            "Episode",
            "season",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;
        insert_item(
            &db,
            "episode-2160p",
            "Episode",
            "season",
            "Episode",
            0,
            1,
            Some(1),
            Some(1),
        )
        .await;

        let seasons = child_items_by_type(&db, "u1", "series", "Season")
            .await
            .unwrap();
        let enriched = enrich_season_list(&db, "u1", seasons).await;

        assert_eq!(enriched[0]["ChildCount"], 1);
        assert_eq!(enriched[0]["RecursiveItemCount"], 1);
    }

    #[tokio::test]
    async fn show_seasons_returns_start_index() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db).await;
        insert_item(&db, "series", "Series", "tv", "Series", 1, 1, None, None).await;
        insert_item(&db, "season-1", "S1", "series", "Season", 1, 1, None, None).await;

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("userId".to_string(), "u1".to_string());
        let response = show_seasons(
            State(state),
            Path("series".to_string()),
            Extension("u1".to_string()),
            Query(query),
        )
        .await
        .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["TotalRecordCount"], 1);
        assert_eq!(value["StartIndex"], 0);
    }

    async fn insert_library(db: &sea_orm::DatabaseConnection) {
        Libraries::insert(libraries::ActiveModel {
            id: Set("tv".to_string()),
            name: Set("TV".to_string()),
            collection_type: Set("tvshows".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        season_number: Option<i64>,
        episode_number: Option<i64>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set("tv".to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(is_public),
            season_number: Set(season_number),
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

    async fn insert_episode_with_size(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        season_number: Option<i64>,
        episode_number: Option<i64>,
        size_bytes: i64,
        overview: Option<&str>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set("tv".to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Episode".to_string()),
            is_folder: Set(0),
            is_public: Set(1),
            overview: Set(overview.map(ToString::to_string)),
            season_number: Set(season_number),
            episode_number: Set(episode_number),
            size_bytes: Set(Some(size_bytes)),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
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
