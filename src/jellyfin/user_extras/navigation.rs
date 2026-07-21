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

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

/// GET /Items/Filters2 — return available filter values for a query
pub async fn filters2(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match filters2_emby_inner(&state.db, &query).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn filters2_inner(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    filter_values(db, query).await.map(filters2_jellyfin_json)
}

async fn filters2_emby_inner(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    filter_values(db, query).await.map(filters2_emby_json)
}

async fn filters_legacy_inner(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    filter_values(db, query).await.map(filters_legacy_json)
}

struct FilterValues {
    genres: Vec<String>,
    years: Vec<i64>,
    tags: Vec<String>,
    studios: Vec<String>,
    ratings: Vec<String>,
}

async fn filter_values(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<FilterValues> {
    let parent_id = query.get("ParentId").map(String::as_str);
    let include_types = query
        .get("IncludeItemTypes")
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>());

    // Build WHERE clause based on ParentId and IncludeItemTypes
    let mut conditions = vec![visible_media_item_sql("media_items")];
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
    let genres: Vec<String> = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &genres_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten())
        .collect();

    // Get years
    let years_sql = format!(
        "SELECT DISTINCT media_items.production_year FROM media_items WHERE {} AND media_items.production_year IS NOT NULL ORDER BY media_items.production_year DESC",
        where_clause
    );
    let years: Vec<i64> = db
        .query_all_raw(crate::db::helpers::pg_statement(&years_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_i64("production_year").ok().flatten())
        .collect();

    // Get tags
    let tags_sql = format!(
        "SELECT DISTINCT t.name FROM tags t JOIN media_tags mt ON mt.tag_id = t.id JOIN media_items ON media_items.id = mt.item_id WHERE {} ORDER BY t.name ASC",
        where_clause
    );
    let tags: Vec<String> = db
        .query_all_raw(crate::db::helpers::pg_statement(&tags_sql, values.clone()))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten())
        .collect();

    // Get studios
    let studios_sql = format!(
        "SELECT DISTINCT s.name FROM studios s JOIN media_studios ms ON ms.studio_id = s.id JOIN media_items ON media_items.id = ms.item_id WHERE {} ORDER BY s.name ASC",
        where_clause
    );
    let studios: Vec<String> = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &studios_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("name").ok().flatten())
        .collect();

    // Get official ratings
    let ratings_sql = format!(
        "SELECT DISTINCT media_items.official_rating FROM media_items WHERE {} AND media_items.official_rating IS NOT NULL AND media_items.official_rating <> '' ORDER BY media_items.official_rating ASC",
        where_clause
    );
    let ratings: Vec<String> = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &ratings_sql,
            values.clone(),
        ))
        .await?
        .iter()
        .filter_map(|r| r.get_opt_str("official_rating").ok().flatten())
        .collect();

    Ok(FilterValues {
        genres,
        years,
        tags,
        studios,
        ratings,
    })
}

fn filters2_jellyfin_json(values: FilterValues) -> Value {
    json!({
        "Genres": name_id_pairs(&values.genres),
        "Years": values.years,
        "Tags": name_id_pairs(&values.tags),
        "Studios": name_id_pairs(&values.studios),
        "OfficialRatings": values.ratings,
        "VideoTypes": ["VideoFile", "Iso", "Dvd", "BluRay"],
    })
}

fn filters2_emby_json(values: FilterValues) -> Value {
    json!({
        "Genres": name_long_id_pairs(&values.genres),
        "Studios": name_long_id_pairs(&values.studios),
        "Tags": values.tags,
    })
}

fn filters_legacy_json(values: FilterValues) -> Value {
    json!({
        "Genres": values.genres,
        "Tags": values.tags,
        "OfficialRatings": values.ratings,
        "Years": values.years,
    })
}

fn name_id_pairs(values: &[String]) -> Vec<Value> {
    values
        .iter()
        .map(|name| json!({"Name": name, "Id": name}))
        .collect()
}

fn name_long_id_pairs(values: &[String]) -> Vec<Value> {
    values
        .iter()
        .map(|name| json!({"Name": name, "Id": stable_long_id(name)}))
        .collect()
}

fn stable_long_id(value: &str) -> i64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    (hash & 0x7fff_ffff_ffff_ffff) as i64
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
    let mut ancestors = Vec::new();
    let mut current_id = Some(item_id.to_string());

    // Walk up the parent chain
    while let Some(id) = current_id {
        let row = db
            .query_one_raw(crate::db::helpers::pg_statement(
                &item_queries::media_item_select_sql(
                    "WHERE media_items.id = ? AND media_items.is_public = 1 AND (media_items.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = media_items.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = media_items.parent_id AND parent.is_public = 1))",
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
    match filters_legacy_inner(&state.db, &query).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
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
    let visible = visible_media_item_sql("media_items");
    let sql = format!(
        "{} WHERE media_items.item_type = 'Episode' AND media_items.is_folder = 0 AND {visible} ORDER BY media_items.created_at DESC",
        crate::jellyfin::item_queries::media_item_select_sql("")
    );
    let rows = state
        .db
        .query_all_raw(crate::db::helpers::pg_statement(
            &sql,
            vec![user_id.as_str().into()],
        ))
        .await
        .unwrap_or_default();

    let items = crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
    let total = items.len();
    let page = items
        .into_iter()
        .skip(start_index)
        .take(limit)
        .collect::<Vec<_>>();
    let page = crate::jellyfin::items::enrich_item_list(&state.db, &user_id, page).await;
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
    let visible = visible_media_item_sql("media_items");
    let sql = format!(
        "SELECT DISTINCT named.id, named.name FROM {} named JOIN {} rel ON rel.{} = named.id JOIN media_items ON media_items.id = rel.item_id WHERE {visible} AND named.name = ? LIMIT 1",
        relation.table, relation.relation_table, relation.relation_column
    );
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(&sql, vec![name.into()]))
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
    use crate::entities::{
        game_genres::{self, Entity as GameGenres},
        genres::{self, Entity as Genres},
        media_game_genres::{self, Entity as MediaGameGenres},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_studios::{self, Entity as MediaStudios},
        studios::{self, Entity as Studios},
    };
    use axum::{
        body::to_bytes,
        extract::{Extension, Query, State},
        response::IntoResponse,
    };
    use sea_orm::{DatabaseConnection, EntityTrait, Set};
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "public", "Public", 1, 2001, "PG").await;
        insert_media_item(&db, "private", "Private", 0, 2002, "R").await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0, 2003, "NC-17")
            .await;
        insert_media_item_with_parent(
            &db,
            "hidden-child",
            "Hidden Child",
            "hidden-parent",
            1,
            2004,
            "G",
        )
        .await;
        insert_named(&db, "genres", "g_public", "PublicGenre").await;
        insert_named(&db, "genres", "g_private", "PrivateGenre").await;
        insert_named(&db, "genres", "g_hidden", "HiddenGenre").await;
        link_named(&db, "media_genres", "genre_id", "public", "g_public").await;
        link_named(&db, "media_genres", "genre_id", "private", "g_private").await;
        link_named(&db, "media_genres", "genre_id", "hidden-child", "g_hidden").await;

        let result = filters2_inner(&db, &Default::default()).await.unwrap();
        assert_eq!(result["Years"], serde_json::json!([2001]));
        assert_eq!(result["OfficialRatings"], serde_json::json!(["PG"]));
        assert_eq!(result["Genres"][0]["Name"], "PublicGenre");
        assert_eq!(result["Genres"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn named_filter_items_require_public_media_relation() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db, "public", "Public", 1, 2001, "PG").await;
        insert_media_item(&db, "private", "Private", 0, 2002, "R").await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0, 2003, "NC-17")
            .await;
        insert_media_item_with_parent(
            &db,
            "hidden-child",
            "Hidden Child",
            "hidden-parent",
            1,
            2004,
            "G",
        )
        .await;
        insert_named(&db, "genres", "g_public", "PublicGenre").await;
        insert_named(&db, "genres", "g_private", "PrivateGenre").await;
        insert_named(&db, "genres", "g_hidden", "HiddenGenre").await;
        insert_named(&db, "studios", "s_public", "PublicStudio").await;
        insert_named(&db, "studios", "s_private", "PrivateStudio").await;
        insert_named(&db, "game_genres", "gg_public", "PublicGameGenre").await;
        insert_named(&db, "game_genres", "gg_private", "PrivateGameGenre").await;
        link_named(&db, "media_genres", "genre_id", "public", "g_public").await;
        link_named(&db, "media_genres", "genre_id", "private", "g_private").await;
        link_named(&db, "media_genres", "genre_id", "hidden-child", "g_hidden").await;
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
        assert!(
            named_item_by_name_inner(&db, "HiddenGenre", genre)
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_episode(&db, "episode-1", "E1", 1, 10).await;
        insert_episode(&db, "episode-2", "E2", 1, 20).await;
        insert_episode(&db, "private", "Private", 0, 30).await;
        insert_episode_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0, 40).await;
        insert_episode_with_parent(&db, "hidden-child", "Hidden Child", "hidden-parent", 1, 50)
            .await;

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
        insert_media_item_with_parent(db, id, title, "", is_public, year, rating).await;
    }

    async fn insert_media_item_with_parent(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        is_public: i64,
        year: i64,
        rating: &str,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(0),
            is_public: Set(is_public),
            production_year: Set(Some(year)),
            official_rating: Set(Some(rating.to_string())),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
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
        insert_episode_with_parent(db, id, title, "", is_public, created_at).await;
    }

    async fn insert_episode_with_parent(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        is_public: i64,
        created_at: i64,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(id.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Episode".to_string()),
            is_folder: Set(0),
            is_public: Set(is_public),
            modified_at: Set(1),
            created_at: Set(created_at),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_named(db: &sea_orm::DatabaseConnection, table: &str, id: &str, name: &str) {
        match table {
            "genres" => Genres::insert(genres::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                created_at: Set(1),
            })
            .exec_without_returning(db)
            .await
            .unwrap(),
            "studios" => Studios::insert(studios::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                created_at: Set(1),
            })
            .exec_without_returning(db)
            .await
            .unwrap(),
            "game_genres" => GameGenres::insert(game_genres::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                created_at: Set(1),
            })
            .exec_without_returning(db)
            .await
            .unwrap(),
            _ => panic!("unsupported named test table: {table}"),
        };
    }

    async fn link_named(
        db: &sea_orm::DatabaseConnection,
        table: &str,
        column: &str,
        item_id: &str,
        value_id: &str,
    ) {
        match (table, column) {
            ("media_genres", "genre_id") => {
                MediaGenres::insert(media_genres::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    genre_id: Set(value_id.to_string()),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            ("media_studios", "studio_id") => {
                MediaStudios::insert(media_studios::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    studio_id: Set(value_id.to_string()),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            ("media_game_genres", "game_genre_id") => {
                MediaGameGenres::insert(media_game_genres::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    game_genre_id: Set(value_id.to_string()),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            _ => panic!("unsupported named test relation: {table}.{column}"),
        }
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
