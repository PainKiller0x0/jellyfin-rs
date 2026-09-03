use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Value,
};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        image_assets::{self, Entity as ImageAssets},
        media_people::{self, Entity as MediaPeople},
        people::Entity as People,
        user_data::Entity as UserData,
    },
    jellyfin::{
        auth::request_user_id_and_admin_or_default,
        common::{image as placeholder_image, internal_error, ok_response, wants_json_response},
        item_queries,
    },
};

pub async fn person_by_name(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match person_detail(&state, &name, &query, "Person", is_admin).await {
        Ok(Some(person)) => Json(person).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"Error": "Person not found"})),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn artists(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match artist_list(&state, &query, false, is_admin).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn album_artists(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match artist_list(&state, &query, true, is_admin).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn artist_by_name(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match artist_detail(&state, &name, &query, is_admin).await {
        Ok(Some(person)) => Json(person).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"Error": "Artist not found"})),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn person_items(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match person_items_inner(&state, &name, &query, is_admin).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn person_detail(
    state: &AppState,
    name: &str,
    query: &HashMap<String, String>,
    item_type: &str,
    include_private: bool,
) -> anyhow::Result<Option<JsonValue>> {
    let name = name.trim();
    let Some(person) = find_person_by_name(&state.db, name).await? else {
        return Ok(None);
    };
    if !include_private && !has_person_relation(&state.db, &person.id, false).await? {
        return Ok(None);
    }

    let image_tags = person_images(&state.db, &person.id).await?;
    let metadata = person_dto_metadata(&person);

    let user_id = query
        .get("UserId")
        .or_else(|| query.get("userId"))
        .map(String::as_str);
    let is_favorite = if let Some(uid) = user_id {
        UserData::find_by_id((uid.to_string(), person.id.clone()))
            .one(&state.db)
            .await?
            .map(|data| data.is_favorite != 0)
            .unwrap_or(false)
    } else {
        false
    };

    Ok(Some(json!({
        "Name": person.name,
        "Id": person.id,
        "ServerId": "jellyfin-rs",
        "Type": item_type,
        "Etag": person.id,
        "Path": "",
        "Overview": person.overview.unwrap_or_default(),
        "ProductionYear": metadata.production_year,
        "PremiereDate": metadata.premiere_date,
        "EndDate": metadata.end_date,
        "HomePageUrl": person.home_page_url,
        "ProductionLocations": metadata.production_locations,
        "SortName": person.name,
        "ProviderIds": metadata.provider_ids,
        "CanDelete": false,
        "CanDownload": false,
        "PlayAccess": "Full",
        "IsFolder": item_type == "MusicArtist",
        "LocationType": "Virtual",
        "MediaSources": [],
        "ImageTags": image_tags,
        "BackdropImageTags": [],
        "Genres": [],
        "GenreItems": [],
        "Tags": [],
        "Studios": [],
        "UserData": {
            "ItemId": person.id,
            "Key": person.id,
            "Played": false,
            "IsFavorite": is_favorite,
            "PlayCount": 0,
            "PlaybackPositionTicks": 0,
            "PlayedPercentage": null,
            "Rating": null,
            "LastPlayedDate": null,
            "Likes": null,
            "UnplayedItemCount": null,
        },
        "LockData": false,
        "LockedFields": [],
        "ExternalUrls": [],
    })))
}

async fn artist_detail(
    state: &AppState,
    name: &str,
    query: &HashMap<String, String>,
    include_private: bool,
) -> anyhow::Result<Option<JsonValue>> {
    let name = name.trim();
    let Some(person) = find_person_by_name(&state.db, name).await? else {
        return Ok(None);
    };
    if !has_artist_relation(&state.db, &person.id, query, include_private).await? {
        return Ok(None);
    }
    person_detail(state, name, query, "MusicArtist", include_private).await
}

async fn artist_list(
    state: &AppState,
    query: &HashMap<String, String>,
    album_only: bool,
    include_private: bool,
) -> anyhow::Result<JsonValue> {
    let artist_type = query_param(query, &["ArtistType", "artistType"]);
    let person_types = artist_person_types(album_only, artist_type);
    let placeholders = person_types
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let mut sql = format!(
        r#"SELECT p.id, p.name, p.overview, COUNT(DISTINCT mp.item_id) AS child_count
           FROM people p
           JOIN media_people mp ON mp.person_id = p.id
           JOIN media_items mi ON mi.id = mp.item_id
           WHERE LOWER(mp.person_type) IN ({placeholders})"#
    );
    let mut values: Vec<Value> = person_types.iter().map(|value| (*value).into()).collect();
    if !include_private {
        sql.push_str(&format!(
            " AND {}",
            item_queries::visible_media_item_sql("mi")
        ));
    }

    if let Some(item_id) = query_param(query, &["ItemId", "itemId", "ParentId", "parentId"])
        .filter(|value| !value.is_empty())
    {
        sql.push_str(" AND (mi.id = ? OR mi.parent_id = ? OR mi.library_id = ?)");
        values.push(item_id.into());
        values.push(item_id.into());
        values.push(item_id.into());
    }

    sql.push_str(" GROUP BY p.id, p.name, p.overview ORDER BY p.name ASC");

    let rows = state
        .db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await?;

    let person_ids = rows
        .iter()
        .filter_map(|row| row.get_opt_str("id").ok().flatten())
        .collect::<Vec<_>>();
    let image_tags = crate::jellyfin::item_queries::batch_item_image_tags(&state.db, &person_ids)
        .await
        .unwrap_or_default();

    let search_term = query_param(query, &["SearchTerm", "searchTerm"])
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let mut items = rows
        .iter()
        .filter_map(|row| {
            let id = row.get_str("id").ok()?;
            let name = row.get_str("name").ok()?;
            if search_term
                .as_deref()
                .is_some_and(|term| !name.to_ascii_lowercase().contains(term))
            {
                return None;
            }
            let tags = image_tags.get(&id).cloned().unwrap_or_else(|| json!({}));
            let primary_tag = tags
                .get("Primary")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            let mut item = json!({
                "Name": name,
                "Id": id,
                "ServerId": "jellyfin-rs",
                "Type": "MusicArtist",
                "SortName": name,
                "Overview": row.get_opt_str("overview").ok().flatten(),
                "IsFolder": true,
                "ChildCount": row.get_i64("child_count").unwrap_or_default(),
                "ImageTags": tags,
                "BackdropImageTags": [],
                "ImageBlurHashes": {},
                "UserData": {
                    "ItemId": id,
                    "Key": id,
                    "Played": false,
                    "IsFavorite": false,
                    "PlayCount": 0,
                    "PlaybackPositionTicks": 0
                }
            });
            if let Some(tag) = primary_tag {
                item["PrimaryImageTag"] = json!(tag);
            }
            Some(item)
        })
        .collect::<Vec<_>>();

    if query_param(query, &["SortOrder", "sortOrder"])
        .is_some_and(|value| value.eq_ignore_ascii_case("Descending"))
    {
        items.reverse();
    }

    let total = items.len();
    let start_index = query_usize(query, &["StartIndex", "startIndex"], 0);
    let limit = query_usize(query, &["Limit", "limit"], usize::MAX);
    let items = items
        .into_iter()
        .skip(start_index)
        .take(limit)
        .collect::<Vec<_>>();

    Ok(json!({
        "Items": items,
        "TotalRecordCount": total,
        "StartIndex": start_index
    }))
}

async fn has_artist_relation(
    db: &DatabaseConnection,
    person_id: &str,
    query: &HashMap<String, String>,
    include_private: bool,
) -> anyhow::Result<bool> {
    let artist_type = query_param(query, &["ArtistType", "artistType"]);
    let person_types = artist_person_types(false, artist_type);
    let links = MediaPeople::find()
        .filter(media_people::Column::PersonId.eq(person_id))
        .all(db)
        .await?;
    for link in links {
        if !person_types
            .iter()
            .any(|person_type| link.person_type.eq_ignore_ascii_case(person_type))
        {
            continue;
        }
        if include_private
            || crate::jellyfin::item_queries::find_media_item(db, "", &link.item_id)
                .await?
                .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) async fn has_person_relation(
    db: &DatabaseConnection,
    person_id: &str,
    include_private: bool,
) -> anyhow::Result<bool> {
    let links = MediaPeople::find()
        .filter(media_people::Column::PersonId.eq(person_id))
        .all(db)
        .await?;
    for link in links {
        if include_private
            || crate::jellyfin::item_queries::find_media_item(db, "", &link.item_id)
                .await?
                .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn artist_person_types(album_only: bool, artist_type: Option<&str>) -> Vec<&'static str> {
    if album_only || artist_type.is_some_and(|value| value.eq_ignore_ascii_case("AlbumArtist")) {
        return vec!["albumartist", "audioalbumartist"];
    }
    if artist_type.is_some_and(|value| value.eq_ignore_ascii_case("Artist")) {
        return vec!["artist", "musicartist"];
    }
    vec!["artist", "musicartist", "albumartist", "audioalbumartist"]
}

fn query_param<'a>(query: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| query.get(*key).map(String::as_str))
}

fn query_usize(query: &HashMap<String, String>, keys: &[&str], default: usize) -> usize {
    query_param(query, keys)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

async fn person_items_inner(
    state: &AppState,
    name: &str,
    query: &HashMap<String, String>,
    include_private: bool,
) -> anyhow::Result<JsonValue> {
    let name = name.trim();
    let Some(person) = find_person_by_name(&state.db, name).await? else {
        return Ok(json!({"Items": [], "TotalRecordCount": 0}));
    };

    let user_id = query
        .get("UserId")
        .or_else(|| query.get("userId"))
        .map(String::as_str);
    let include_item_types = query
        .get("IncludeItemTypes")
        .or_else(|| query.get("includeItemTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    let person_item_types = query
        .get("PersonTypes")
        .or_else(|| query.get("personTypes"))
        .map(|v| v.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(100);
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let sort_by = query
        .get("SortBy")
        .or_else(|| query.get("sortBy"))
        .map(String::as_str)
        .unwrap_or("SortName");
    let sort_order = query
        .get("SortOrder")
        .or_else(|| query.get("sortOrder"))
        .map(String::as_str)
        .unwrap_or("Ascending");

    let items = fetch_tagged_items(
        &state.db,
        &person.id,
        user_id,
        &include_item_types,
        &person_item_types,
        sort_by,
        sort_order,
        limit,
        start_index,
        include_private,
    )
    .await?;
    let total = count_tagged_items(
        &state.db,
        &person.id,
        &include_item_types,
        &person_item_types,
        include_private,
    )
    .await?;

    Ok(json!({"Items": items, "TotalRecordCount": total, "StartIndex": start_index}))
}

async fn find_person_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> anyhow::Result<Option<PersonRow>> {
    let Some(model) = People::find()
        .filter(crate::entities::people::Column::Name.eq(name))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(PersonRow {
        id: model.id,
        name: model.name,
        overview: model.overview,
        tmdb_id: model.tmdb_id,
        imdb_id: model.imdb_id,
        home_page_url: model.home_page_url,
        premiere_date: model.premiere_date,
        end_date: model.end_date,
        production_locations: model.production_locations,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_tagged_items(
    db: &DatabaseConnection,
    person_id: &str,
    user_id: Option<&str>,
    include_item_types: &[&str],
    person_types: &[&str],
    sort_by: &str,
    sort_order: &str,
    limit: i64,
    start_index: i64,
    include_private: bool,
) -> anyhow::Result<Vec<JsonValue>> {
    let mut sql = String::from(
        r#"SELECT mi.id, mi.title, mi.path, mi.library_id, mi.parent_id, mi.item_type, mi.is_folder, mi.container, mi.overview, mi.official_rating, mi.extended_video_type, mi.production_year, mi.premiere_date, mi.runtime_ticks, mi.size_bytes, mi.created_at, mi.modified_at"#,
    );

    let has_user_data = user_id.is_some();
    if has_user_data {
        sql.push_str(
            r#", COALESCE(ud.is_favorite, CAST(0 AS bigint)) AS is_favorite, COALESCE(ud.played, CAST(0 AS bigint)) AS played, COALESCE(ud.playback_position_ticks, CAST(0 AS bigint)) AS playback_position_ticks, ud.played_percentage, COALESCE(ud.play_count, CAST(0 AS bigint)) AS play_count, ud.last_played_at"#,
        );
    }

    sql.push_str(" FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id");

    if has_user_data {
        sql.push_str(" LEFT JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ?");
    }

    // Build WHERE conditions
    let mut where_parts = vec!["mp.person_id = ?".to_string()];
    if !include_private {
        where_parts.push(item_queries::visible_media_item_sql("mi"));
    }
    for _ in include_item_types {
        where_parts.push("mi.item_type = ?".to_string());
    }
    for _ in person_types {
        where_parts.push("mp.person_type = ?".to_string());
    }
    sql.push_str(&format!(" WHERE {}", where_parts.join(" AND ")));

    let order = match sort_order {
        "Descending" => "DESC",
        _ => "ASC",
    };
    match sort_by {
        "ProductionYear" => sql.push_str(&format!(" ORDER BY mi.production_year {order}")),
        "Runtime" => sql.push_str(&format!(" ORDER BY mi.runtime_ticks {order}")),
        "DateCreated" => sql.push_str(&format!(" ORDER BY mi.created_at {order}")),
        "Random" => sql.push_str(" ORDER BY RANDOM()"),
        _ => sql.push_str(&format!(" ORDER BY mp.sort_order ASC, mi.title {order}")),
    }

    sql.push_str(" LIMIT ? OFFSET ?");

    // Collect values in SQL placeholder order
    let mut values: Vec<Value> = Vec::new();
    if has_user_data {
        values.push(user_id.unwrap().into());
    }
    values.push(person_id.into());
    for item_type in include_item_types {
        values.push((*item_type).into());
    }
    for person_type in person_types {
        values.push((*person_type).into());
    }
    values.push(limit.into());
    values.push(start_index.into());
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await?;

    let item_ids: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get_opt_str("id").ok().flatten())
        .collect();

    // Batch load image tags for items
    let tags_map = if item_ids.is_empty() {
        Default::default()
    } else {
        crate::jellyfin::item_queries::batch_item_image_tags(db, &item_ids)
            .await
            .unwrap_or_default()
    };

    let items = rows
        .iter()
        .map(|row| {
            let item_type: String = row.get_str("item_type").unwrap_or_default();
            let is_folder: i64 = row.get_i64("is_folder").unwrap_or_default();
            let user_data = if user_id.is_some() {
                let last_played: Option<i64> = row.get_opt_i64("last_played_at").unwrap_or_default();
                json!({
                    "IsFavorite": row.get_bool_from_i64("is_favorite").unwrap_or_default(),
                    "Played": row.get_bool_from_i64("played").unwrap_or_default(),
                    "PlaybackPositionTicks": row.get_i64("playback_position_ticks").unwrap_or_default(),
                    "PlayCount": row.get_i64("play_count").unwrap_or_default(),
                    "PlayedPercentage": row.get_f64("played_percentage").unwrap_or_default(),
                    "LastPlayedDate": last_played.map(crate::util::unix_to_jellyfin_date),
                })
            } else {
                json!(null)
            };
            json!({
                "Name": row.get_str("title").unwrap_or_default(),
                "Id": row.get_str("id").unwrap_or_default(),
                "Type": item_type,
                "IsFolder": is_folder != 0,
                "ProductionYear": row.get_opt_i64("production_year").unwrap_or_default(),
                "PremiereDate": row.get_opt_str("premiere_date").ok().flatten().and_then(|date| crate::util::yyyy_mm_dd_to_jellyfin_date(&date)),
                "RunTimeTicks": row.get_opt_i64("runtime_ticks").unwrap_or_default(),
                "Overview": row.get_opt_str("overview").unwrap_or_default(),
                "Path": row.get_str("path").unwrap_or_default(),
                "LibraryId": row.get_str("library_id").unwrap_or_default(),
                "ParentId": row.get_str("parent_id").unwrap_or_default(),
                "Container": row.get_opt_str("container").unwrap_or_default(),
                "Size": row.get_opt_i64("size_bytes").unwrap_or_default(),
                "IndexNumber": null,
                "ParentIndexNumber": null,
                "ImageTags": tags_map.get(&row.get_str("id").unwrap_or_default()).cloned().unwrap_or_else(|| json!({})),
                "UserData": user_data,
            })
        })
        .collect();

    Ok(items)
}

async fn count_tagged_items(
    db: &DatabaseConnection,
    person_id: &str,
    include_item_types: &[&str],
    person_types: &[&str],
    include_private: bool,
) -> anyhow::Result<i64> {
    let mut sql = String::from(
        "SELECT COUNT(*) as cnt FROM media_people mp JOIN media_items mi ON mi.id = mp.item_id WHERE mp.person_id = ?",
    );
    let mut values: Vec<Value> = vec![person_id.into()];
    if !include_private {
        sql.push_str(&format!(
            " AND {}",
            item_queries::visible_media_item_sql("mi")
        ));
    }

    if !include_item_types.is_empty() {
        let placeholders = include_item_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mi.item_type IN ({placeholders})"));
        for item_type in include_item_types {
            values.push((*item_type).into());
        }
    }
    if !person_types.is_empty() {
        let placeholders = person_types
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND mp.person_type IN ({placeholders})"));
        for person_type in person_types {
            values.push((*person_type).into());
        }
    }
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(&sql, values))
        .await?;
    Ok(row.map(|row| row.get_i64("cnt").unwrap_or(0)).unwrap_or(0))
}

pub async fn person_images(db: &DatabaseConnection, person_id: &str) -> anyhow::Result<JsonValue> {
    let models = ImageAssets::find()
        .filter(crate::entities::image_assets::Column::ItemId.eq(person_id))
        .order_by_asc(crate::entities::image_assets::Column::ImageIndex)
        .all(db)
        .await?;

    let mut tags = serde_json::Map::new();
    for m in &models {
        let etag = m.etag.as_deref().unwrap_or_default();
        tags.entry(m.image_type.clone())
            .or_insert_with(|| json!(etag));
    }
    Ok(JsonValue::Object(tags))
}

struct PersonRow {
    id: String,
    name: String,
    overview: Option<String>,
    tmdb_id: Option<String>,
    imdb_id: Option<String>,
    home_page_url: Option<String>,
    premiere_date: Option<String>,
    end_date: Option<String>,
    production_locations: Option<String>,
}

struct PersonDtoMetadata {
    production_year: Option<i64>,
    premiere_date: Option<String>,
    end_date: Option<String>,
    production_locations: Vec<String>,
    provider_ids: serde_json::Map<String, JsonValue>,
}

fn person_dto_metadata(person: &PersonRow) -> PersonDtoMetadata {
    let mut provider_ids = serde_json::Map::new();
    if let Some(tmdb_id) = person.tmdb_id.as_ref().filter(|value| !value.is_empty()) {
        provider_ids.insert("Tmdb".to_string(), json!(tmdb_id));
    }
    if let Some(imdb_id) = person.imdb_id.as_ref().filter(|value| !value.is_empty()) {
        provider_ids.insert("IMDB".to_string(), json!(imdb_id));
    }
    PersonDtoMetadata {
        production_year: person
            .premiere_date
            .as_deref()
            .and_then(crate::util::year_from_yyyy_mm_dd),
        premiere_date: person
            .premiere_date
            .as_deref()
            .and_then(crate::util::yyyy_mm_dd_to_jellyfin_date),
        end_date: person
            .end_date
            .as_deref()
            .and_then(crate::util::yyyy_mm_dd_to_jellyfin_date),
        production_locations: person
            .production_locations
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .unwrap_or_default(),
        provider_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        artist_person_types, count_tagged_items, fetch_tagged_items, find_person_by_name,
        has_person_relation, person_dto_metadata, serve_person_image,
    };
    use crate::entities::{
        image_assets::{self, Entity as ImageAssets},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        people::{self, Entity as People},
    };
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use sea_orm::{EntityTrait, Set};
    use std::path::PathBuf;

    #[test]
    fn artist_type_filter_matches_artist_kind() {
        assert_eq!(
            artist_person_types(false, None),
            vec!["artist", "musicartist", "albumartist", "audioalbumartist"]
        );
        assert_eq!(
            artist_person_types(false, Some("Artist")),
            vec!["artist", "musicartist"]
        );
        assert_eq!(
            artist_person_types(false, Some("AlbumArtist")),
            vec!["albumartist", "audioalbumartist"]
        );
        assert_eq!(
            artist_person_types(true, Some("Artist")),
            vec!["albumartist", "audioalbumartist"]
        );
    }

    #[tokio::test]
    async fn person_dto_metadata_round_trips_tmdb_fields() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let id = format!("person-dto-{}", uuid::Uuid::new_v4().simple());
        let name = format!("Person DTO {id}");
        People::insert(people::ActiveModel {
            id: Set(id),
            name: Set(name.clone()),
            created_at: Set(1),
            tmdb_id: Set(Some("98765".to_string())),
            imdb_id: Set(Some("nm1234567".to_string())),
            home_page_url: Set(Some("https://person.example".to_string())),
            premiere_date: Set(Some("1980-02-03".to_string())),
            end_date: Set(Some("2024-04-05".to_string())),
            production_locations: Set(Some(r#"["Shanghai, China"]"#.to_string())),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        let person = find_person_by_name(&db, &name).await.unwrap().unwrap();
        let metadata = person_dto_metadata(&person);
        assert_eq!(metadata.production_year, Some(1980));
        assert_eq!(
            metadata.premiere_date.as_deref(),
            Some("1980-02-03T00:00:00.0000000Z")
        );
        assert_eq!(
            metadata.end_date.as_deref(),
            Some("2024-04-05T00:00:00.0000000Z")
        );
        assert_eq!(metadata.production_locations, ["Shanghai, China"]);
        assert_eq!(metadata.provider_ids["Tmdb"], "98765");
        assert_eq!(metadata.provider_ids["IMDB"], "nm1234567");
    }

    #[tokio::test]
    async fn person_item_queries_hide_private_media_unless_requested() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_person(&db, "p1", "Actor").await;
        insert_media_item(&db, "public", "Public", 1).await;
        insert_media_item(&db, "private", "Private", 0).await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0).await;
        insert_media_item_with_parent(&db, "hidden-child", "Hidden Child", "hidden-parent", 1)
            .await;
        insert_media_person(&db, "public", "p1", "Actor").await;
        insert_media_person(&db, "private", "p1", "Actor").await;
        insert_media_person(&db, "hidden-child", "p1", "Actor").await;

        let visible = fetch_tagged_items(
            &db,
            "p1",
            None,
            &[],
            &[],
            "SortName",
            "Ascending",
            10,
            0,
            false,
        )
        .await
        .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["Id"], "public");
        assert_eq!(
            count_tagged_items(&db, "p1", &[], &[], false)
                .await
                .unwrap(),
            1
        );

        let all = fetch_tagged_items(
            &db,
            "p1",
            None,
            &[],
            &[],
            "SortName",
            "Ascending",
            10,
            0,
            true,
        )
        .await
        .unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            count_tagged_items(&db, "p1", &[], &[], true).await.unwrap(),
            3
        );
        assert!(has_person_relation(&db, "p1", false).await.unwrap());
    }

    #[tokio::test]
    async fn private_only_person_is_hidden_without_admin_bypass() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_person(&db, "p1", "Actor").await;
        insert_media_item(&db, "private", "Private", 0).await;
        insert_media_person(&db, "private", "p1", "Actor").await;

        assert!(!has_person_relation(&db, "p1", false).await.unwrap());
        assert!(has_person_relation(&db, "p1", true).await.unwrap());
    }

    #[tokio::test]
    async fn public_child_under_private_parent_does_not_expose_person() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_person(&db, "p1", "Actor").await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0).await;
        insert_media_item_with_parent(&db, "hidden-child", "Hidden Child", "hidden-parent", 1)
            .await;
        insert_media_person(&db, "hidden-child", "p1", "Actor").await;

        assert!(!has_person_relation(&db, "p1", false).await.unwrap());
        assert!(has_person_relation(&db, "p1", true).await.unwrap());
        assert!(
            fetch_tagged_items(
                &db,
                "p1",
                None,
                &[],
                &[],
                "SortName",
                "Ascending",
                10,
                0,
                false,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            count_tagged_items(&db, "p1", &[], &[], false)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn person_image_requires_visible_person_relation() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_person(&db, "p1", "Actor").await;
        insert_media_item_with_parent(&db, "hidden-parent", "Hidden Parent", "", 0).await;
        insert_media_item_with_parent(&db, "hidden-child", "Hidden Child", "hidden-parent", 1)
            .await;
        insert_media_person(&db, "hidden-child", "p1", "Actor").await;
        insert_media_item(&db, "p1", "Image Asset Placeholder", 1).await;

        let image_path = PathBuf::from("data").join("images").join(format!(
            "person-private-{}.png",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(image_path.parent().unwrap()).unwrap();
        std::fs::write(&image_path, b"png").unwrap();
        ImageAssets::insert(image_assets::ActiveModel {
            id: Set(format!("img-{}", uuid::Uuid::new_v4().simple())),
            item_id: Set("p1".to_string()),
            image_type: Set("Primary".to_string()),
            image_index: Set(0),
            path: Set(Some(image_path.to_string_lossy().to_string())),
            etag: Set(Some("tag".to_string())),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        assert_eq!(
            serve_person_image(&db, &HeaderMap::new(), "Actor", "Primary", 0, false)
                .await
                .into_response()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve_person_image(&db, &HeaderMap::new(), "Actor", "Primary", 0, true)
                .await
                .into_response()
                .status(),
            StatusCode::OK
        );

        std::fs::remove_file(image_path).unwrap();
    }

    async fn insert_person(db: &sea_orm::DatabaseConnection, id: &str, name: &str) {
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

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        is_public: i64,
    ) {
        insert_media_item_with_parent(db, id, title, "", is_public).await;
    }

    async fn insert_media_item_with_parent(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        parent_id: &str,
        is_public: i64,
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
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_media_person(
        db: &sea_orm::DatabaseConnection,
        item_id: &str,
        person_id: &str,
        person_type: &str,
    ) {
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person_id.to_string()),
            person_type: Set(person_type.to_string()),
            sort_order: Set(0),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}

/// Serve person image — GET /Persons/{name}/Images/{imageType}
pub async fn person_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((name, image_type)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    serve_person_image(&state.db, &headers, &name, &image_type, 0, is_admin).await
}

/// Serve person image with index — GET /Persons/{name}/Images/{imageType}/{index}
pub async fn person_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((name, first, second)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let (image_type, image_index) = if let Ok(index) = second.parse::<i64>() {
        (first, index)
    } else {
        (second, first.parse::<i64>().unwrap_or_default())
    };
    let (_, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    serve_person_image(
        &state.db,
        &headers,
        &name,
        &image_type,
        image_index,
        is_admin,
    )
    .await
}

async fn serve_person_image(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    name: &str,
    image_type: &str,
    image_index: i64,
    include_private: bool,
) -> Response {
    // Find person by name
    let person = match find_person_by_name(db, name).await {
        Ok(Some(p)) => p,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return internal_error(e),
    };
    match has_person_relation(db, &person.id, include_private).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }
    if wants_json_response(headers) {
        return ok_response();
    }

    // Find image asset
    let model = match find_person_image_asset(db, &person.id, image_type, image_index).await {
        Ok(m) => m,
        Err(e) => return internal_error(e),
    };

    let Some(model) = model else {
        return placeholder_image().await;
    };

    let etag = model.etag.as_deref().unwrap_or_default();
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_matches('"') == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let path = model.path.as_deref().unwrap_or_default();
    if !crate::jellyfin::images::image_storage_path_allowed(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(_) => return placeholder_image().await,
    };

    let content_type = crate::jellyfin::images::content_type_from_path(path);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        response_headers.insert(header::ETAG, value);
    }
    (response_headers, Body::from(bytes)).into_response()
}

async fn find_person_image_asset(
    db: &DatabaseConnection,
    person_id: &str,
    image_type: &str,
    image_index: i64,
) -> anyhow::Result<Option<image_assets::Model>> {
    Ok(ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(person_id))
        .filter(image_assets::Column::ImageType.eq(image_type))
        .filter(image_assets::Column::ImageIndex.eq(image_index))
        .one(db)
        .await?)
}
