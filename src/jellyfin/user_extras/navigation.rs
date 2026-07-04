use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{auth::query_user_id_or_request, common::internal_error, item_queries},
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
    let include_types = query
        .get("IncludeItemTypes")
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    // Build WHERE clause based on ParentId and IncludeItemTypes
    let mut conditions = vec!["media_items.is_public = 1".to_string()];
    let mut values: Vec<sea_orm::Value> = Vec::new();

    if let Some(pid) = parent_id {
        conditions.push("media_items.parent_id = ?".to_string());
        values.push(pid.into());
    }
    if let Some(types) = &include_types {
        if !types.is_empty() {
            let ph = types
                .iter()
                .map(|_| "media_items.item_type = ?")
                .collect::<Vec<_>>()
                .join(" OR ");
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
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &genres_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| {
            r.get_opt_str("name")
                .ok()
                .flatten()
                .map(|n| json!({"Name": n, "Id": n}))
        })
        .collect();

    // Get years
    let years_sql = format!(
        "SELECT DISTINCT media_items.production_year FROM media_items WHERE {} AND media_items.production_year IS NOT NULL ORDER BY media_items.production_year DESC",
        where_clause
    );
    let years: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &years_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| {
            r.get_opt_i64("production_year")
                .ok()
                .flatten()
                .map(|y| json!(y))
        })
        .collect();

    // Get tags
    let tags_sql = format!(
        "SELECT DISTINCT t.name FROM tags t JOIN media_tags mt ON mt.tag_id = t.id JOIN media_items ON media_items.id = mt.item_id WHERE {} ORDER BY t.name ASC",
        where_clause
    );
    let tags: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &tags_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| {
            r.get_opt_str("name")
                .ok()
                .flatten()
                .map(|n| json!({"Name": n, "Id": n}))
        })
        .collect();

    // Get studios
    let studios_sql = format!(
        "SELECT DISTINCT s.name FROM studios s JOIN media_studios ms ON ms.studio_id = s.id JOIN media_items ON media_items.id = ms.item_id WHERE {} ORDER BY s.name ASC",
        where_clause
    );
    let studios: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &studios_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| {
            r.get_opt_str("name")
                .ok()
                .flatten()
                .map(|n| json!({"Name": n, "Id": n}))
        })
        .collect();

    // Get official ratings
    let ratings_sql = format!(
        "SELECT DISTINCT media_items.official_rating FROM media_items WHERE {} AND media_items.official_rating IS NOT NULL AND media_items.official_rating <> '' ORDER BY media_items.official_rating ASC",
        where_clause
    );
    let ratings: Vec<Value> = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &ratings_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| {
            r.get_opt_str("official_rating")
                .ok()
                .flatten()
                .map(|n| json!(n))
        })
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
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
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
                &item_queries::media_item_select_sql(
                    "WHERE media_items.id = ? AND media_items.is_public = 1",
                ),
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
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    crate::jellyfin::items::user_suggestions(State(state), Path(user_id), Query(query)).await
}

/// GET /UserItems/Resume — alternative resume path
pub async fn user_items_resume(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    crate::jellyfin::items::resume_items(State(state), Path(user_id), Query(query)).await
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
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    let start_index = query_usize(&query, &["StartIndex", "startIndex"], 0);
    let limit = query_usize(&query, &["Limit", "limit"], 16).min(200);

    let backend = state.db.get_database_backend();
    let sql = format!(
        "{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND media_items.is_public = 1 ORDER BY media_items.created_at DESC",
        crate::jellyfin::item_queries::media_item_select_sql("")
    );
    let rows = state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![user_id.into()],
        ))
        .await
        .unwrap_or_default();

    let items = crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
    let total = items.len();
    let page = items
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(|i| crate::jellyfin::common::strip_nulls(i.to_jellyfin_json()))
        .collect::<Vec<_>>();
    Json(json!({ "Items": page, "TotalRecordCount": total, "StartIndex": start_index }))
        .into_response()
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

/// GET /Genres/{name} — get genre by name
pub async fn genre_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    named_item_by_name(
        &state,
        &name,
        NamedRelation::new("genres", "media_genres", "genre_id", "Genre"),
    )
    .await
}

/// GET /GameGenres/{name}
pub async fn game_genre_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    named_item_by_name(
        &state,
        &name,
        NamedRelation::new(
            "game_genres",
            "media_game_genres",
            "game_genre_id",
            "GameGenre",
        ),
    )
    .await
}

pub async fn music_genre_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    named_item_by_name(
        &state,
        &name,
        NamedRelation::new("genres", "media_genres", "genre_id", "MusicGenre"),
    )
    .await
}

/// GET /Studios/{name}
pub async fn studio_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    named_item_by_name(
        &state,
        &name,
        NamedRelation::new("studios", "media_studios", "studio_id", "Studio"),
    )
    .await
}

#[derive(Clone, Copy)]
struct NamedRelation {
    table: &'static str,
    relation_table: &'static str,
    relation_column: &'static str,
    item_type: &'static str,
}

impl NamedRelation {
    fn new(
        table: &'static str,
        relation_table: &'static str,
        relation_column: &'static str,
        item_type: &'static str,
    ) -> Self {
        Self {
            table,
            relation_table,
            relation_column,
            item_type,
        }
    }
}

async fn named_item_by_name(state: &AppState, name: &str, relation: NamedRelation) -> Response {
    match named_item_by_name_inner(&state.db, name, relation).await {
        Ok(Some(item)) => Json(item).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn named_item_by_name_inner(
    db: &sea_orm::DatabaseConnection,
    name: &str,
    relation: NamedRelation,
) -> anyhow::Result<Option<Value>> {
    let sql = format!(
        "SELECT DISTINCT named.id, named.name FROM {} named JOIN {} rel ON rel.{} = named.id JOIN media_items ON media_items.id = rel.item_id WHERE media_items.is_public = 1 AND named.name = ? LIMIT 1",
        relation.table, relation.relation_table, relation.relation_column
    );
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &sql,
            vec![name.into()],
        ))
        .await?;

    Ok(row.map(|r| {
        let id = r.get_str("id").unwrap_or_default();
        let name = r.get_str("name").unwrap_or_default();
        named_item_json(&id, &name, relation.item_type)
    }))
}

fn named_item_json(id: &str, name: &str, item_type: &str) -> Value {
    json!({ "Name": name, "Id": id, "Type": item_type })
}

#[cfg(test)]
mod tests {
    use super::{
        NamedRelation, filters2_inner, named_item_by_name_inner, named_item_json, shows_upcoming,
    };
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

    #[test]
    fn named_item_json_uses_requested_type() {
        let item = named_item_json("g1", "Arcade", "GameGenre");
        assert_eq!(item["Id"], "g1");
        assert_eq!(item["Name"], "Arcade");
        assert_eq!(item["Type"], "GameGenre");

        let music = named_item_json("m1", "Rock", "MusicGenre");
        assert_eq!(music["Type"], "MusicGenre");
    }

    #[tokio::test]
    async fn filters2_hides_values_from_private_items() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_media_item(&db, "public", "Public", 1, 2001, "PG").await;
        insert_media_item(&db, "private", "Private", 0, 2002, "R").await;
        insert_named(&db, "genres", "g_public", "PublicGenre").await;
        insert_named(&db, "genres", "g_private", "PrivateGenre").await;
        link_named(&db, "media_genres", "genre_id", "public", "g_public").await;
        link_named(&db, "media_genres", "genre_id", "private", "g_private").await;

        let result = filters2_inner(&db, &Default::default()).await.unwrap();
        assert_eq!(result["Years"], serde_json::json!([2001]));
        assert_eq!(result["OfficialRatings"], serde_json::json!(["PG"]));
        assert_eq!(result["Genres"][0]["Name"], "PublicGenre");
        assert_eq!(result["Genres"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn named_filter_items_require_public_media_relation() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_media_item(&db, "public", "Public", 1, 2001, "PG").await;
        insert_media_item(&db, "private", "Private", 0, 2002, "R").await;
        insert_named(&db, "genres", "g_public", "PublicGenre").await;
        insert_named(&db, "genres", "g_private", "PrivateGenre").await;
        insert_named(&db, "studios", "s_public", "PublicStudio").await;
        insert_named(&db, "studios", "s_private", "PrivateStudio").await;
        insert_named(&db, "game_genres", "gg_public", "PublicGameGenre").await;
        insert_named(&db, "game_genres", "gg_private", "PrivateGameGenre").await;
        link_named(&db, "media_genres", "genre_id", "public", "g_public").await;
        link_named(&db, "media_genres", "genre_id", "private", "g_private").await;
        link_named(&db, "media_studios", "studio_id", "public", "s_public").await;
        link_named(&db, "media_studios", "studio_id", "private", "s_private").await;
        link_named(
            &db,
            "media_game_genres",
            "game_genre_id",
            "public",
            "gg_public",
        )
        .await;
        link_named(
            &db,
            "media_game_genres",
            "game_genre_id",
            "private",
            "gg_private",
        )
        .await;

        let genre = NamedRelation::new("genres", "media_genres", "genre_id", "Genre");
        assert_eq!(
            named_item_by_name_inner(&db, "PublicGenre", genre)
                .await
                .unwrap()
                .unwrap()["Type"],
            "Genre"
        );
        assert!(
            named_item_by_name_inner(&db, "PrivateGenre", genre)
                .await
                .unwrap()
                .is_none()
        );

        let studio = NamedRelation::new("studios", "media_studios", "studio_id", "Studio");
        assert_eq!(
            named_item_by_name_inner(&db, "PublicStudio", studio)
                .await
                .unwrap()
                .unwrap()["Name"],
            "PublicStudio"
        );
        assert!(
            named_item_by_name_inner(&db, "PrivateStudio", studio)
                .await
                .unwrap()
                .is_none()
        );

        let game = NamedRelation::new(
            "game_genres",
            "media_game_genres",
            "game_genre_id",
            "GameGenre",
        );
        assert_eq!(
            named_item_by_name_inner(&db, "PublicGameGenre", game)
                .await
                .unwrap()
                .unwrap()["Type"],
            "GameGenre"
        );
        assert!(
            named_item_by_name_inner(&db, "PrivateGameGenre", game)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn shows_upcoming_returns_paged_query_result() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_episode(&db, "episode-1", "E1", 1, 10).await;
        insert_episode(&db, "episode-2", "E2", 1, 20).await;
        insert_episode(&db, "private", "Private", 0, 30).await;

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("UserId".to_string(), "u1".to_string());
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = shows_upcoming(State(state), Extension("u1".to_string()), Query(query))
            .await
            .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["TotalRecordCount"], 2);
        assert_eq!(value["StartIndex"], 1);
        let items = value["Items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["Id"], "episode-1");
    }

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        is_public: i64,
        year: i64,
        rating: &str,
    ) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, production_year, official_rating, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, ?, ?, ?, 1, 1, 1)",
            vec![
                id.into(),
                title.into(),
                id.into(),
                is_public.into(),
                year.into(),
                rating.into(),
            ],
        ))
        .await
        .unwrap();
    }

    async fn insert_episode(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        is_public: i64,
        created_at: i64,
    ) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Episode', 0, ?, 1, ?, 1)",
            vec![
                id.into(),
                title.into(),
                id.into(),
                is_public.into(),
                created_at.into(),
            ],
        ))
        .await
        .unwrap();
    }

    async fn insert_named(db: &sea_orm::DatabaseConnection, table: &str, id: &str, name: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, 1)"),
            vec![id.into(), name.into()],
        ))
        .await
        .unwrap();
    }

    async fn link_named(
        db: &sea_orm::DatabaseConnection,
        table: &str,
        column: &str,
        item_id: &str,
        value_id: &str,
    ) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &format!("INSERT INTO {table} (item_id, {column}) VALUES (?, ?)"),
            vec![item_id.into(), value_id.into()],
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
