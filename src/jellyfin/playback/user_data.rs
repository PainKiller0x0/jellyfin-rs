use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        media_people::{self, Entity as MediaPeople},
        user_data::{self, Entity as UserData},
    },
    jellyfin::{auth::request_user_id_or_default, common::internal_error, item_queries},
    util::{now_unix, unix_to_jellyfin_date},
};

const USER_DATA_TARGET_NOT_FOUND: &str = "user data target not found";

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

pub async fn favorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", true).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn unfavorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", false).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn current_user_favorite_item(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", true).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn current_user_unfavorite_item(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", false).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn mark_played(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "played", true).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn mark_unplayed(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "played", false).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn current_user_mark_played(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    mark_played(State(state), Path((user_id, item_id))).await
}

pub async fn current_user_mark_unplayed(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    mark_unplayed(State(state), Path((user_id, item_id))).await
}

pub async fn hide_from_resume(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let hide = query
        .get("Hide")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true"))
        .unwrap_or(true);
    if !hide {
        // Unhide: no-op since we don't track hide state separately
        return StatusCode::NO_CONTENT.into_response();
    }
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.playback_position_ticks = Set(0);
    })
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => user_data_error(error),
    }
}

pub async fn set_rating(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let body_json = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<JsonValue>(&body) {
            Ok(value) => Some(value),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    let Some(rating) = rating_from_request(&query, body_json.as_ref()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.rating = Set(Some(rating));
    })
    .await
    {
        Ok(_) => match user_data_json(&state.db, &user_id, &item_id).await {
            Ok(data) => Json(data).into_response(),
            Err(error) => internal_error(error),
        },
        Err(error) => user_data_error(error),
    }
}

pub async fn delete_rating(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.rating = Set(None);
    })
    .await
    {
        Ok(_) => match user_data_json(&state.db, &user_id, &item_id).await {
            Ok(data) => Json(data).into_response(),
            Err(error) => internal_error(error),
        },
        Err(error) => user_data_error(error),
    }
}

pub async fn current_user_set_rating(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
    body: Bytes,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    set_rating(State(state), Path((user_id, item_id)), Query(query), body).await
}

pub async fn current_user_delete_rating(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    delete_rating(State(state), Path((user_id, item_id))).await
}

async fn set_user_data_flag_json(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> anyhow::Result<JsonValue> {
    upsert_user_data_flag(db, user_id, item_id, field, value).await?;
    user_data_json(db, user_id, item_id).await
}

async fn user_data_json(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<JsonValue> {
    let item_ids = playback_user_data_item_ids(db, item_id).await?;
    let rows = UserData::find()
        .filter(user_data::Column::UserId.eq(user_id))
        .filter(user_data::Column::ItemId.is_in(item_ids))
        .all(db)
        .await?;

    match rows.is_empty() {
        false => {
            let is_favorite = rows.iter().any(|row| row.is_favorite != 0);
            let played = rows.iter().any(|row| row.played != 0);
            let playback_position_ticks = rows
                .iter()
                .map(|row| row.playback_position_ticks)
                .max()
                .unwrap_or_default();
            let played_percentage = rows
                .iter()
                .filter_map(|row| row.played_percentage)
                .max_by(f64::total_cmp);
            let play_count = rows
                .iter()
                .map(|row| row.play_count)
                .max()
                .unwrap_or_default();
            let last_played_at = rows.iter().filter_map(|row| row.last_played_at).max();
            let rating = rows
                .iter()
                .filter_map(|row| row.rating)
                .max_by(f64::total_cmp);
            Ok(json!({
                "ItemId": item_id,
                "Key": item_id,
                "IsFavorite": is_favorite,
                "Played": played,
                "PlaybackPositionTicks": playback_position_ticks,
                "PlayedPercentage": played_percentage,
                "PlayCount": play_count,
                "LastPlayedDate": last_played_at.map(unix_to_jellyfin_date),
                "Rating": rating,
                "Likes": null,
                "UnplayedItemCount": null,
            }))
        }
        _ => Ok(json!({
            "ItemId": item_id,
            "Key": item_id,
            "IsFavorite": false,
            "Played": false,
            "PlaybackPositionTicks": 0,
            "PlayedPercentage": null,
            "PlayCount": 0,
            "LastPlayedDate": null,
            "Rating": null,
            "Likes": null,
            "UnplayedItemCount": null,
        })),
    }
}

fn rating_from_request(query: &HashMap<String, String>, body: Option<&JsonValue>) -> Option<f64> {
    query_bool_any(query, &["likes", "Likes"])
        .map(|likes| if likes { 1.0 } else { -1.0 })
        .or_else(|| {
            query
                .get("rating")
                .or_else(|| query.get("Rating"))
                .and_then(|value| value.parse::<f64>().ok())
        })
        .or_else(|| {
            body.and_then(|body| {
                body.get("Likes")
                    .and_then(JsonValue::as_bool)
                    .map(|likes| if likes { 1.0 } else { -1.0 })
                    .or_else(|| body.get("Rating").and_then(JsonValue::as_f64))
            })
        })
}

fn query_bool_any(query: &HashMap<String, String>, keys: &[&str]) -> Option<bool> {
    let value = query
        .iter()
        .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
        .map(|(_, value)| value.trim().to_ascii_lowercase())?;
    match value.as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn optional_f64(body: &JsonValue, key: &str) -> Option<Option<f64>> {
    match body.get(key)? {
        JsonValue::Null => Some(None),
        value => value.as_f64().map(Some),
    }
}

fn optional_jellyfin_date(body: &JsonValue, key: &str) -> Option<Option<i64>> {
    match body.get(key)? {
        JsonValue::Null => Some(None),
        JsonValue::String(value) => chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|date| Some(date.timestamp())),
        _ => None,
    }
}

fn apply_user_item_data_update(active: &mut user_data::ActiveModel, body: &JsonValue, now: i64) {
    if let Some(value) = body.get("IsFavorite").and_then(JsonValue::as_bool) {
        active.is_favorite = Set(if value { 1 } else { 0 });
    }
    if let Some(value) = body.get("Played").and_then(JsonValue::as_bool) {
        active.played = Set(if value { 1 } else { 0 });
        if value && !body.get("LastPlayedDate").is_some_and(JsonValue::is_null) {
            active.last_played_at = Set(Some(now));
        } else if !value {
            active.last_played_at = Set(None);
        }
    }
    if let Some(value) = body
        .get("PlaybackPositionTicks")
        .and_then(JsonValue::as_i64)
    {
        active.playback_position_ticks = Set(value.max(0));
    }
    if let Some(value) = optional_f64(body, "PlayedPercentage") {
        active.played_percentage = Set(value);
    }
    if let Some(value) = body.get("PlayCount").and_then(JsonValue::as_i64) {
        active.play_count = Set(value.max(0));
    }
    if let Some(value) = optional_jellyfin_date(body, "LastPlayedDate") {
        active.last_played_at = Set(value);
    }
    if let Some(value) = optional_f64(body, "Rating").or_else(|| {
        body.get("Likes")
            .and_then(JsonValue::as_bool)
            .map(|likes| Some(if likes { 1.0 } else { -1.0 }))
    }) {
        active.rating = Set(value);
    }
}

async fn upsert_user_data_flag(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> anyhow::Result<()> {
    let value_int = if value { 1 } else { 0 };

    match field {
        "is_favorite" => {
            upsert_user_data_simple(db, user_id, item_id, |active| {
                active.is_favorite = Set(value_int);
            })
            .await
        }
        "played" => upsert_played_flag(db, user_id, item_id, value)
            .await
            .with_context(|| format!("failed to update user data flag for item: {item_id}")),
        _ => anyhow::bail!("unsupported user data flag: {field}"),
    }
}

pub(crate) async fn upsert_played_flag(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    played: bool,
) -> anyhow::Result<()> {
    let item_ids = playback_user_data_item_ids(db, item_id).await?;
    let now = now_unix();
    for target_id in item_ids {
        ensure_user_data_target_visible(db, &target_id).await?;
        match UserData::find_by_id((user_id.to_string(), target_id.clone()))
            .one(db)
            .await?
        {
            Some(model) => {
                let play_count = if played {
                    model.play_count.saturating_add(1)
                } else {
                    0
                };
                let mut active: user_data::ActiveModel = model.into();
                active.played = Set(if played { 1 } else { 0 });
                active.playback_position_ticks = Set(0);
                active.played_percentage = Set(None);
                active.play_count = Set(play_count);
                active.last_played_at = Set(played.then_some(now));
                active.updated_at = Set(now);
                active.update(db).await?;
            }
            None => {
                let active = user_data::ActiveModel {
                    user_id: Set(user_id.to_string()),
                    item_id: Set(target_id),
                    played: Set(if played { 1 } else { 0 }),
                    playback_position_ticks: Set(0),
                    played_percentage: Set(None),
                    play_count: Set(if played { 1 } else { 0 }),
                    last_played_at: Set(played.then_some(now)),
                    updated_at: Set(now),
                    ..Default::default()
                };
                UserData::insert(active).exec(db).await?;
            }
        }
    }
    Ok(())
}

async fn upsert_user_data_simple(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    mut apply: impl FnMut(&mut user_data::ActiveModel),
) -> anyhow::Result<()> {
    let item_ids = playback_user_data_item_ids(db, item_id).await?;
    let now = now_unix();
    for target_id in item_ids {
        ensure_user_data_target_visible(db, &target_id).await?;
        match UserData::find_by_id((user_id.to_string(), target_id.clone()))
            .one(db)
            .await?
        {
            Some(model) => {
                let mut active: user_data::ActiveModel = model.into();
                apply(&mut active);
                active.updated_at = Set(now);
                active.update(db).await?;
            }
            None => {
                let mut active = user_data::ActiveModel {
                    user_id: Set(user_id.to_string()),
                    item_id: Set(target_id),
                    is_favorite: Set(0),
                    played: Set(0),
                    playback_position_ticks: Set(0),
                    play_count: Set(0),
                    updated_at: Set(now),
                    ..Default::default()
                };
                apply(&mut active);
                UserData::insert(active).exec(db).await?;
            }
        }
    }
    Ok(())
}

pub(crate) async fn playback_user_data_item_ids(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<String>> {
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT mi.id, mi.parent_id, mi.item_type, mi.is_folder, mi.season_number, mi.episode_number, parent.item_type AS parent_item_type, parent.is_folder AS parent_is_folder FROM media_items mi LEFT JOIN media_items parent ON parent.id = mi.parent_id WHERE mi.id = ?",
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(vec![item_id.to_string()]);
    };

    let item_type = row.get_str("item_type")?;
    let parent_id = row.get_str("parent_id")?;
    let is_folder = row.get_i64("is_folder")? != 0;

    if item_type == "Episode" && !is_folder {
        let Some(episode_number) = row.get_opt_i64("episode_number")? else {
            return Ok(vec![item_id.to_string()]);
        };
        let season_number = row.get_opt_i64("season_number")?;
        let season_clause = if season_number.is_some() {
            "mi.season_number = ?"
        } else {
            "mi.season_number IS NULL"
        };
        let visible = visible_media_item_sql("mi");
        let sql = format!(
            "SELECT mi.id FROM media_items mi WHERE mi.parent_id = ? AND mi.item_type = 'Episode' AND mi.is_folder = 0 AND {season_clause} AND mi.episode_number = ? AND {visible} ORDER BY mi.id ASC"
        );
        let mut values: Vec<sea_orm::Value> = vec![parent_id.into()];
        if let Some(season_number) = season_number {
            values.push(season_number.into());
        }
        values.push(episode_number.into());
        return item_ids_from_query(db, &sql, values, item_id).await;
    }

    if (item_type == "Movie" || item_type == "Episode") && is_folder {
        let parent_visible = visible_media_item_sql("parent");
        let child_visible = visible_media_item_sql("child");
        let sql = format!(
            "SELECT parent.id FROM media_items parent WHERE parent.id = ? AND {parent_visible} UNION SELECT child.id FROM media_items child JOIN media_items parent ON parent.id = child.parent_id WHERE child.parent_id = ? AND child.item_type = 'Video' AND {child_visible} AND {parent_visible} ORDER BY id ASC"
        );
        return item_ids_from_query(db, &sql, vec![item_id.into(), item_id.into()], item_id).await;
    }

    if item_type == "Video" && !parent_id.is_empty() {
        let parent_item_type = row.get_opt_str("parent_item_type")?.unwrap_or_default();
        let parent_is_folder = row.get_i64("parent_is_folder").unwrap_or(0) != 0;
        if parent_is_folder && (parent_item_type == "Movie" || parent_item_type == "Episode") {
            let parent_visible = visible_media_item_sql("parent");
            let child_visible = visible_media_item_sql("child");
            let sql = format!(
                "SELECT parent.id FROM media_items parent WHERE parent.id = ? AND {parent_visible} UNION SELECT child.id FROM media_items child JOIN media_items parent ON parent.id = child.parent_id WHERE child.parent_id = ? AND child.item_type = 'Video' AND {child_visible} AND {parent_visible} ORDER BY id ASC"
            );
            return item_ids_from_query(
                db,
                &sql,
                vec![parent_id.clone().into(), parent_id.into()],
                item_id,
            )
            .await;
        }
    }

    Ok(vec![item_id.to_string()])
}

async fn item_ids_from_query(
    db: &sea_orm::DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
    fallback_item_id: &str,
) -> anyhow::Result<Vec<String>> {
    let mut item_ids = db
        .query_all(crate::db::helpers::pg_statement(sql, values))
        .await?
        .iter()
        .filter_map(|row| row.get_opt_str("id").ok().flatten())
        .collect::<Vec<_>>();
    if item_ids.is_empty() {
        item_ids.push(fallback_item_id.to_string());
    }
    item_ids.sort();
    item_ids.dedup();
    Ok(item_ids)
}

async fn ensure_user_data_target_visible(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<()> {
    if public_media_item_exists(db, item_id).await? || public_person_exists(db, item_id).await? {
        Ok(())
    } else {
        anyhow::bail!(USER_DATA_TARGET_NOT_FOUND)
    }
}

async fn public_media_item_exists(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<bool> {
    Ok(item_queries::find_media_item(db, "", item_id)
        .await?
        .is_some())
}

async fn public_person_exists(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<bool> {
    let links = MediaPeople::find()
        .filter(media_people::Column::PersonId.eq(item_id))
        .all(db)
        .await?;
    for link in links {
        if item_queries::find_media_item(db, "", &link.item_id)
            .await?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn user_data_error(error: anyhow::Error) -> Response {
    if error.to_string().contains(USER_DATA_TARGET_NOT_FOUND) {
        StatusCode::NOT_FOUND.into_response()
    } else {
        internal_error(error)
    }
}

/// GET /UserItems/{item_id}/UserData — returns user data for an item
pub async fn get_user_item_data(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    match user_data_json(&state.db, &user_id, &item_id).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => internal_error(error),
    }
}

/// POST /UserItems/{item_id}/UserData — update user data for an item
pub async fn update_user_item_data(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Json(body): Json<JsonValue>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &headers, &query).await;
    let now = now_unix();

    if let Err(error) = upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        apply_user_item_data_update(active, &body, now);
    })
    .await
    {
        return user_data_error(error);
    }

    match user_data_json(&state.db, &user_id, &item_id).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_user_item_data_update, rating_from_request, set_user_data_flag_json,
        upsert_user_data_simple, user_data_json,
    };
    use crate::entities::{
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        people::{self, Entity as People},
        user_data::Entity as UserData,
        users::{self, Entity as Users},
    };
    use sea_orm::{EntityTrait, Set};
    use serde_json::json;
    use std::collections::HashMap;

    #[tokio::test]
    async fn marking_unplayed_does_not_increment_play_count() {
        let Some(db) = seeded_db().await else {
            return;
        };

        let played = set_user_data_flag_json(&db, "u1", "i1", "played", true)
            .await
            .unwrap();
        assert_eq!(played["Played"], true);
        assert_eq!(played["PlayCount"], 1);
        assert!(played["LastPlayedDate"].is_string());

        let unplayed = set_user_data_flag_json(&db, "u1", "i1", "played", false)
            .await
            .unwrap();
        assert_eq!(unplayed["Played"], false);
        assert_eq!(unplayed["PlayCount"], 0);
        assert_eq!(unplayed["PlaybackPositionTicks"], 0);
        assert!(unplayed["LastPlayedDate"].is_null());

        let row = UserData::find_by_id(("u1".to_string(), "i1".to_string()))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.played, 0);
        assert_eq!(row.play_count, 0);
        assert_eq!(row.last_played_at, None);
    }

    #[test]
    fn rating_request_accepts_query_and_body_shapes() {
        let mut query = HashMap::new();
        query.insert("Likes".to_string(), "false".to_string());
        assert_eq!(rating_from_request(&query, None), Some(-1.0));

        query.clear();
        query.insert("rating".to_string(), "4.5".to_string());
        assert_eq!(rating_from_request(&query, None), Some(4.5));

        assert_eq!(
            rating_from_request(&HashMap::new(), Some(&json!({ "Likes": true }))),
            Some(1.0)
        );
    }

    #[tokio::test]
    async fn user_data_json_includes_rating() {
        let Some(db) = seeded_db().await else {
            return;
        };
        upsert_user_data_simple(&db, "u1", "i1", |active| {
            active.rating = Set(Some(4.5));
        })
        .await
        .unwrap();

        let data = user_data_json(&db, "u1", "i1").await.unwrap();
        assert_eq!(data["Rating"], 4.5);
    }

    #[tokio::test]
    async fn user_data_update_preserves_omitted_fields() {
        let Some(db) = seeded_db().await else {
            return;
        };
        upsert_user_data_simple(&db, "u1", "i1", |active| {
            active.is_favorite = Set(1);
            active.played = Set(1);
            active.playback_position_ticks = Set(10);
            active.play_count = Set(3);
            active.last_played_at = Set(Some(100));
            active.rating = Set(Some(4.5));
        })
        .await
        .unwrap();

        upsert_user_data_simple(&db, "u1", "i1", |active| {
            apply_user_item_data_update(active, &json!({ "PlaybackPositionTicks": 250 }), 200);
        })
        .await
        .unwrap();

        let data = user_data_json(&db, "u1", "i1").await.unwrap();
        assert_eq!(data["IsFavorite"], true);
        assert_eq!(data["Played"], true);
        assert_eq!(data["PlaybackPositionTicks"], 250);
        assert_eq!(data["PlayCount"], 3);
        assert_eq!(data["Rating"], 4.5);
        assert_eq!(data["LastPlayedDate"], "1970-01-01T00:01:40Z");
    }

    #[tokio::test]
    async fn user_data_writes_require_visible_media_or_person() {
        let Some(db) = seeded_db().await else {
            return;
        };
        insert_media_item(&db, "private", "Private", "/tmp/private.mkv", "", 0, 0).await;
        insert_person(&db, "p1", "Person").await;
        link_person(&db, "i1", "p1").await;
        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            "/tmp/private-parent",
            "",
            1,
            0,
        )
        .await;
        insert_media_item(
            &db,
            "public-child",
            "Public Child",
            "/tmp/public-child.mkv",
            "private-parent",
            0,
            1,
        )
        .await;
        insert_person(&db, "p2", "Hidden Person").await;
        link_person(&db, "public-child", "p2").await;

        set_user_data_flag_json(&db, "u1", "i1", "is_favorite", true)
            .await
            .unwrap();
        set_user_data_flag_json(&db, "u1", "p1", "is_favorite", true)
            .await
            .unwrap();
        assert!(
            set_user_data_flag_json(&db, "u1", "private", "is_favorite", true)
                .await
                .is_err()
        );
        assert!(
            set_user_data_flag_json(&db, "u1", "public-child", "is_favorite", true)
                .await
                .is_err()
        );
        assert!(
            set_user_data_flag_json(&db, "u1", "p2", "is_favorite", true)
                .await
                .is_err()
        );
        assert!(
            set_user_data_flag_json(&db, "u1", "missing", "is_favorite", true)
                .await
                .is_err()
        );
    }

    async fn seeded_db() -> Option<sea_orm::DatabaseConnection> {
        let Some(db) = crate::db::test_db().await else {
            return None;
        };
        Users::insert(users::ActiveModel {
            id: Set("u1".to_string()),
            username: Set("u1".to_string()),
            display_name: Set("u1".to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        insert_media_item(&db, "i1", "Movie", "/tmp/i1.mkv", "", 0, 1).await;
        Some(db)
    }

    async fn insert_media_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        parent_id: &str,
        is_folder: i64,
        is_public: i64,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(String::new()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(is_folder),
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

    async fn link_person(db: &sea_orm::DatabaseConnection, item_id: &str, person_id: &str) {
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person_id.to_string()),
            person_type: Set("Actor".to_string()),
            sort_order: Set(0),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}
