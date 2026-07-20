use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Value as DbValue};
use serde_json::{Value, json};

use crate::{app::state::AppState, db::row_ext::QueryResultExt, jellyfin::common::internal_error};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

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

pub async fn music_genres(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_filter_items(&state.db, FilterKind::MusicGenre, &query).await
}

pub async fn years(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    list_years(&state.db, &query).await
}

pub async fn year_by_year(State(state): State<Arc<AppState>>, Path(year): Path<i64>) -> Response {
    match year_exists(&state.db, year).await {
        Ok(true) => Json(year_item(year)).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Year not found" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
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
    list_media_stream_values(&state.db, "codec", &query).await
}

pub async fn extended_video_types(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_extended_video_types_inner(&state.db, &query).await {
        Ok(items) => Json(paged_query_result(items, &query)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn list_extended_video_types_inner(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT mi.extended_video_type FROM media_items mi WHERE {visible} AND mi.extended_video_type IS NOT NULL AND mi.extended_video_type <> ''"
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![]))
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
    filter_by_search_and_sort(&mut items, query);
    Ok(items)
}

/// Return unique first characters of item titles (for alphabetical index navigation)
pub async fn items_prefixes(
    State(state): State<Arc<AppState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match list_media_item_prefixes(&state.db).await {
        Ok(prefixes) => Json(prefixes).into_response(),
        Err(error) => internal_error(error),
    }
}

/// Return unique first characters of artist/person names
pub async fn artists_prefixes(
    State(state): State<Arc<AppState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match list_artist_prefixes(&state.db).await {
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

async fn list_prefixes(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT UPPER(SUBSTR({}, 1, 1)) AS prefix FROM {} WHERE {} IS NOT NULL AND {} <> '' ORDER BY prefix ASC",
        column, table, column, column
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![]))
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
        Ok(items) => Json(paged_query_result(items, query)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn list_years_inner(
    db: &DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT mi.production_year FROM media_items mi WHERE {visible} AND mi.production_year IS NOT NULL AND mi.production_year > 0 ORDER BY mi.production_year DESC"
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![]))
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

    filter_by_search_and_sort(&mut items, query);
    Ok(items)
}

async fn year_exists(db: &DatabaseConnection, year: i64) -> anyhow::Result<bool> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT COUNT(*) AS cnt FROM media_items mi WHERE {visible} AND mi.production_year = ?"
    );
    let row = db
        .query_one(crate::db::helpers::pg_statement(&sql, vec![year.into()]))
        .await
        .context("failed to check year")?;
    Ok(row
        .map(|row| row.get_i64("cnt").unwrap_or_default() > 0)
        .unwrap_or(false))
}

fn year_item(year: i64) -> Value {
    let year = year.to_string();
    json!({
        "Name": year,
        "Id": year,
        "ServerId": "jellyfin-rs",
        "Type": "Year",
        "IsFolder": true,
        "SortName": year,
        "ImageTags": {},
        "BackdropImageTags": [],
        "ImageBlurHashes": {},
        "UserData": {
            "ItemId": year,
            "Key": year,
            "Played": false,
            "IsFavorite": false,
            "PlayCount": 0,
            "PlaybackPositionTicks": 0
        }
    })
}

async fn list_distinct_values(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
    query: &HashMap<String, String>,
) -> Response {
    match list_distinct_values_inner(db, table, column, query).await {
        Ok(items) => Json(paged_query_result(items, query)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn list_distinct_values_inner(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    debug_assert_eq!(table, "media_items");
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT mi.{column} FROM {table} mi WHERE {visible} AND mi.{column} IS NOT NULL AND mi.{column} <> '' ORDER BY mi.{column} ASC"
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![]))
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

    filter_by_search_and_sort(&mut items, query);
    Ok(items)
}

async fn list_media_stream_values(
    db: &DatabaseConnection,
    column: &str,
    query: &HashMap<String, String>,
) -> Response {
    match list_media_stream_values_inner(db, column, query).await {
        Ok(items) => Json(paged_query_result(items, query)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn list_media_stream_values_inner(
    db: &DatabaseConnection,
    column: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT media_streams.{column} FROM media_streams JOIN media_items mi ON mi.id = media_streams.item_id WHERE {visible} AND media_streams.{column} IS NOT NULL AND media_streams.{column} <> '' ORDER BY media_streams.{column} ASC"
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(&sql, vec![]))
        .await
        .with_context(|| format!("failed to list distinct {column} from media_streams"))?;

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

    filter_by_search_and_sort(&mut items, query);
    Ok(items)
}

fn filter_by_search_and_sort(items: &mut Vec<Value>, query: &HashMap<String, String>) {
    if let Some(search_term) = query_value(query, "SearchTerm").filter(|value| !value.is_empty()) {
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
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("SortOrder"))
        .map(|(_, value)| value)
        .is_some_and(|value| value.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }
}

fn paged_query_result(items: Vec<Value>, query: &HashMap<String, String>) -> Value {
    let total = items.len();
    let start_index = query_usize(query, "StartIndex", 0);
    let limit = query_usize(query, "Limit", usize::MAX);
    let page = items
        .into_iter()
        .skip(start_index)
        .take(limit)
        .collect::<Vec<_>>();
    json!({
        "Items": page,
        "TotalRecordCount": total,
        "StartIndex": start_index,
    })
}

fn query_value<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a String> {
    query
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn query_usize(query: &HashMap<String, String>, key: &str, default: usize) -> usize {
    query_value(query, key)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

async fn list_filter_items(
    db: &DatabaseConnection,
    kind: FilterKind,
    query: &HashMap<String, String>,
) -> Response {
    match list_filter_items_inner(db, kind, query).await {
        Ok(items) => Json(paged_query_result(items, query)).into_response(),
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
            list_public_related_items(
                db,
                "genres",
                "media_genres",
                "genre_id",
                kind.item_type(),
                None,
            )
            .await?
        }
        FilterKind::Tag => {
            list_public_related_items(db, "tags", "media_tags", "tag_id", kind.item_type(), None)
                .await?
        }
        FilterKind::Person => {
            let filters = query.get("Filters").unwrap_or(&String::new()).clone();
            let has_fav_filter = filters.contains("IsFavorite");
            let user_id = query.get("UserId").or_else(|| query.get("userId"));

            if has_fav_filter && user_id.is_some() {
                // Filter persons by user_data.is_favorite
                if let Some(uid) = user_id {
                    let visible = visible_media_item_sql("mi");
                    let sql = format!(
                        "SELECT DISTINCT p.id, p.name, ia.etag AS primary_image_tag FROM people p JOIN user_data ud ON ud.item_id = p.id JOIN media_people mp ON mp.person_id = p.id JOIN media_items mi ON mi.id = mp.item_id LEFT JOIN image_assets ia ON ia.item_id = p.id AND ia.image_type = 'Primary' WHERE {visible} AND ud.user_id = ? AND ud.is_favorite = 1 ORDER BY p.name ASC"
                    );
                    let models = db
                        .query_all(crate::db::helpers::pg_statement(
                            &sql,
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
                    list_public_related_items(
                        db,
                        "people",
                        "media_people",
                        "person_id",
                        kind.item_type(),
                        Some("failed to list persons"),
                    )
                    .await?
                }
            } else {
                list_public_related_items(
                    db,
                    "people",
                    "media_people",
                    "person_id",
                    kind.item_type(),
                    Some("failed to list persons"),
                )
                .await?
            }
        }
        FilterKind::Studio => {
            list_public_related_items(
                db,
                "studios",
                "media_studios",
                "studio_id",
                kind.item_type(),
                None,
            )
            .await?
        }
        FilterKind::GameGenre => {
            list_public_related_items(
                db,
                "game_genres",
                "media_game_genres",
                "game_genre_id",
                kind.item_type(),
                None,
            )
            .await?
        }
        FilterKind::MusicGenre => {
            list_public_related_items(
                db,
                "genres",
                "media_genres",
                "genre_id",
                kind.item_type(),
                None,
            )
            .await?
        }
    };

    filter_by_search_and_sort(&mut items, query);
    Ok(items)
}

async fn list_public_related_items(
    db: &DatabaseConnection,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    item_type: &str,
    context: Option<&'static str>,
) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT named.id, named.name FROM {table} named JOIN {relation_table} rel ON rel.{relation_column} = named.id JOIN media_items mi ON mi.id = rel.item_id WHERE {visible} ORDER BY named.name ASC"
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &sql,
            Vec::<DbValue>::new(),
        ))
        .await
        .with_context(|| context.unwrap_or("failed to list filter items"))?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            let id = row.get_str("id").ok()?;
            let name = row.get_str("name").ok()?;
            Some(json!({ "Name": name, "Id": id, "Type": item_type, "ImageTags": {} }))
        })
        .collect())
}

async fn list_media_item_prefixes(db: &DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT UPPER(SUBSTR(mi.title, 1, 1)) AS prefix FROM media_items mi WHERE {visible} AND mi.title IS NOT NULL AND mi.title <> '' ORDER BY prefix ASC"
    );
    list_prefixes_sql(db, &sql, Vec::<DbValue>::new()).await
}

async fn list_artist_prefixes(db: &DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let visible = visible_media_item_sql("mi");
    let sql = format!(
        "SELECT DISTINCT UPPER(SUBSTR(p.name, 1, 1)) AS prefix FROM people p JOIN media_people mp ON mp.person_id = p.id JOIN media_items mi ON mi.id = mp.item_id WHERE {visible} AND p.name IS NOT NULL AND p.name <> '' ORDER BY prefix ASC"
    );
    list_prefixes_sql(db, &sql, Vec::<DbValue>::new()).await
}

async fn list_prefixes_sql(
    db: &DatabaseConnection,
    sql: &str,
    values: Vec<DbValue>,
) -> anyhow::Result<Vec<Value>> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(sql, values))
        .await
        .context("failed to list prefixes")?;
    Ok(rows
        .iter()
        .filter_map(|r| r.get_str("prefix").ok())
        .map(|p| json!(p))
        .collect())
}

#[derive(Copy, Clone)]
enum FilterKind {
    Genre,
    Tag,
    Person,
    Studio,
    GameGenre,
    MusicGenre,
}

impl FilterKind {
    fn item_type(self) -> &'static str {
        match self {
            Self::Genre => "Genre",
            Self::Tag => "Tag",
            Self::Person => "Person",
            Self::Studio => "Studio",
            Self::GameGenre => "GameGenre",
            Self::MusicGenre => "MusicGenre",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FilterKind, list_artist_prefixes, list_distinct_values_inner,
        list_extended_video_types_inner, list_filter_items_inner, list_media_item_prefixes,
        list_media_stream_values_inner, list_years_inner, paged_query_result, year_exists,
        year_item,
    };
    use crate::entities::{
        game_genres::{self, Entity as GameGenres},
        genres::{self, Entity as Genres},
        media_game_genres::{self, Entity as MediaGameGenres},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        studios::{self, Entity as Studios},
        tags::{self, Entity as Tags},
        user_data::{self, Entity as UserData},
        users::{self, Entity as Users},
    };
    use sea_orm::{EntityTrait, Set};
    use serde_json::{Value, json};
    use std::collections::HashMap;

    #[test]
    fn year_item_has_jellyfin_shape() {
        let item = year_item(1999);
        assert_eq!(item["Name"], "1999");
        assert_eq!(item["Id"], "1999");
        assert_eq!(item["Type"], "Year");
        assert_eq!(item["IsFolder"], true);
    }

    #[test]
    fn paged_query_result_keeps_total_count_and_start_index() {
        let mut query = HashMap::new();
        query.insert("startIndex".to_string(), "1".to_string());
        query.insert("limit".to_string(), "1".to_string());

        let result = paged_query_result(
            vec![
                json!({ "Name": "A" }),
                json!({ "Name": "B" }),
                json!({ "Name": "C" }),
            ],
            &query,
        );

        assert_eq!(result["TotalRecordCount"], 3);
        assert_eq!(result["StartIndex"], 1);
        assert_eq!(result["Items"].as_array().unwrap().len(), 1);
        assert_eq!(result["Items"][0]["Name"], "B");
    }

    #[tokio::test]
    async fn media_filter_lists_hide_private_only_values() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        insert_user(&db, "u1").await;
        insert_media_item(&db, "public", "Alpha Public", 1, 2001, "PG", "mkv", "Dvd").await;
        insert_media_item(
            &db,
            "private",
            "Zulu Private",
            0,
            2002,
            "R",
            "avi",
            "BluRay",
        )
        .await;
        insert_media_item(
            &db,
            "private-parent",
            "Hidden Parent",
            0,
            1999,
            "NC-17",
            "mp4",
            "Hdr10",
        )
        .await;
        insert_media_item_with_parent(
            &db,
            "hidden-child",
            "Beta Hidden Child",
            "private-parent",
            1,
            2003,
            "G",
            "mov",
            "DolbyVision",
        )
        .await;
        insert_stream(&db, "public", "h264").await;
        insert_stream(&db, "private", "hevc").await;
        insert_stream(&db, "hidden-child", "vp9").await;

        insert_named(&db, "genres", "g_public", "PublicGenre").await;
        insert_named(&db, "genres", "g_private", "PrivateGenre").await;
        insert_named(&db, "genres", "g_hidden", "HiddenGenre").await;
        insert_named(&db, "tags", "t_public", "PublicTag").await;
        insert_named(&db, "tags", "t_private", "PrivateTag").await;
        insert_named(&db, "tags", "t_hidden", "HiddenTag").await;
        insert_named(&db, "studios", "s_public", "PublicStudio").await;
        insert_named(&db, "studios", "s_private", "PrivateStudio").await;
        insert_named(&db, "studios", "s_hidden", "HiddenStudio").await;
        insert_named(&db, "people", "p_public", "PublicPerson").await;
        insert_named(&db, "people", "p_private", "PrivatePerson").await;
        insert_named(&db, "people", "p_hidden", "HiddenPerson").await;
        insert_named(&db, "game_genres", "gg_public", "PublicGameGenre").await;
        insert_named(&db, "game_genres", "gg_private", "PrivateGameGenre").await;
        insert_named(&db, "game_genres", "gg_hidden", "HiddenGameGenre").await;
        link_named(&db, "media_genres", "genre_id", "public", "g_public").await;
        link_named(&db, "media_genres", "genre_id", "private", "g_private").await;
        link_named(&db, "media_genres", "genre_id", "hidden-child", "g_hidden").await;
        link_named(&db, "media_tags", "tag_id", "public", "t_public").await;
        link_named(&db, "media_tags", "tag_id", "private", "t_private").await;
        link_named(&db, "media_tags", "tag_id", "hidden-child", "t_hidden").await;
        link_named(&db, "media_studios", "studio_id", "public", "s_public").await;
        link_named(&db, "media_studios", "studio_id", "private", "s_private").await;
        link_named(
            &db,
            "media_studios",
            "studio_id",
            "hidden-child",
            "s_hidden",
        )
        .await;
        link_named(&db, "media_people", "person_id", "public", "p_public").await;
        link_named(&db, "media_people", "person_id", "private", "p_private").await;
        link_named(&db, "media_people", "person_id", "hidden-child", "p_hidden").await;
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
        link_named(
            &db,
            "media_game_genres",
            "game_genre_id",
            "hidden-child",
            "gg_hidden",
        )
        .await;
        favorite_item(&db, "u1", "p_public").await;
        favorite_item(&db, "u1", "p_hidden").await;

        let query = Default::default();
        assert_names(
            list_filter_items_inner(&db, FilterKind::Genre, &query)
                .await
                .unwrap(),
            &["PublicGenre"],
        );
        assert_names(
            list_filter_items_inner(&db, FilterKind::Tag, &query)
                .await
                .unwrap(),
            &["PublicTag"],
        );
        assert_names(
            list_filter_items_inner(&db, FilterKind::Studio, &query)
                .await
                .unwrap(),
            &["PublicStudio"],
        );
        assert_names(
            list_filter_items_inner(&db, FilterKind::Person, &query)
                .await
                .unwrap(),
            &["PublicPerson"],
        );
        assert_names(
            list_filter_items_inner(&db, FilterKind::GameGenre, &query)
                .await
                .unwrap(),
            &["PublicGameGenre"],
        );

        assert_names(list_years_inner(&db, &query).await.unwrap(), &["2001"]);
        assert!(year_exists(&db, 2001).await.unwrap());
        assert!(!year_exists(&db, 2002).await.unwrap());
        assert_names(
            list_distinct_values_inner(&db, "media_items", "official_rating", &query)
                .await
                .unwrap(),
            &["PG"],
        );
        assert_names(
            list_distinct_values_inner(&db, "media_items", "container", &query)
                .await
                .unwrap(),
            &["mkv"],
        );
        assert_names(
            list_extended_video_types_inner(&db, &query).await.unwrap(),
            &["Dvd"],
        );
        assert_names(
            list_media_stream_values_inner(&db, "codec", &query)
                .await
                .unwrap(),
            &["h264"],
        );
        assert_eq!(
            list_media_item_prefixes(&db).await.unwrap(),
            vec![Value::from("A")]
        );
        assert_eq!(
            list_artist_prefixes(&db).await.unwrap(),
            vec![Value::from("P")]
        );

        let favorite_query = HashMap::from([
            ("Filters".to_string(), "IsFavorite".to_string()),
            ("UserId".to_string(), "u1".to_string()),
        ]);
        assert_names(
            list_filter_items_inner(&db, FilterKind::Person, &favorite_query)
                .await
                .unwrap(),
            &["PublicPerson"],
        );
    }

    fn assert_names(items: Vec<Value>, expected: &[&str]) {
        let names = items
            .iter()
            .filter_map(|item| item.get("Name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
    }

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        is_public: i64,
        year: i64,
        rating: &str,
        container: &str,
        extended_video_type: &str,
    ) {
        insert_media_item_with_parent(
            db,
            id,
            title,
            "",
            is_public,
            year,
            rating,
            container,
            extended_video_type,
        )
        .await;
    }

    async fn insert_media_item_with_parent(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        is_public: i64,
        year: i64,
        rating: &str,
        container: &str,
        extended_video_type: &str,
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
            container: Set(Some(container.to_string())),
            extended_video_type: Set(Some(extended_video_type.to_string())),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_user(db: &sea_orm::DatabaseConnection, user_id: &str) {
        Users::insert(users::ActiveModel {
            id: Set(user_id.to_string()),
            username: Set(user_id.to_string()),
            display_name: Set(user_id.to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn favorite_item(db: &sea_orm::DatabaseConnection, user_id: &str, item_id: &str) {
        UserData::insert(user_data::ActiveModel {
            user_id: Set(user_id.to_string()),
            item_id: Set(item_id.to_string()),
            is_favorite: Set(1),
            played: Set(0),
            playback_position_ticks: Set(0),
            play_count: Set(0),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_stream(db: &sea_orm::DatabaseConnection, item_id: &str, codec: &str) {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(format!("stream-{item_id}")),
            item_id: Set(item_id.to_string()),
            stream_index: Set(0),
            stream_type: Set("Video".to_string()),
            codec: Set(Some(codec.to_string())),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_named(db: &sea_orm::DatabaseConnection, table: &str, id: &str, name: &str) {
        match table {
            "genres" => {
                Genres::insert(genres::ActiveModel {
                    id: Set(id.to_string()),
                    name: Set(name.to_string()),
                    created_at: Set(1),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            "tags" => {
                Tags::insert(tags::ActiveModel {
                    id: Set(id.to_string()),
                    name: Set(name.to_string()),
                    created_at: Set(1),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            "studios" => {
                Studios::insert(studios::ActiveModel {
                    id: Set(id.to_string()),
                    name: Set(name.to_string()),
                    created_at: Set(1),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            "people" => {
                People::insert(people::ActiveModel {
                    id: Set(id.to_string()),
                    name: Set(name.to_string()),
                    created_at: Set(1),
                    ..Default::default()
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            "game_genres" => {
                GameGenres::insert(game_genres::ActiveModel {
                    id: Set(id.to_string()),
                    name: Set(name.to_string()),
                    created_at: Set(1),
                })
                .exec_without_returning(db)
                .await
                .unwrap();
            }
            _ => panic!("unsupported named table: {table}"),
        }
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
            ("media_tags", "tag_id") => {
                MediaTags::insert(media_tags::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    tag_id: Set(value_id.to_string()),
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
            ("media_people", "person_id") => {
                MediaPeople::insert(media_people::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    person_id: Set(value_id.to_string()),
                    person_type: Set("Actor".to_string()),
                    sort_order: Set(0),
                    ..Default::default()
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
            _ => panic!("unsupported relation: {table}.{column}"),
        }
    }
}
