use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::user_data::{self, Entity as UserData},
    jellyfin::{auth::request_user_id_or_default, common::internal_error, item_queries},
    util::{now_unix, unix_to_jellyfin_date},
};

const USER_DATA_TARGET_NOT_FOUND: &str = "user data target not found";

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
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT is_favorite, played, playback_position_ticks, played_percentage, play_count, last_played_at, rating FROM user_data WHERE user_id = ? AND item_id = ?",
            vec![user_id.into(), item_id.into()],
        ))
        .await?;

    match row {
        Some(row) => {
            let is_favorite = row.get_i64("is_favorite")? != 0;
            let played = row.get_i64("played")? != 0;
            let playback_position_ticks = row.get_i64("playback_position_ticks")?;
            let played_percentage = row.get_f64("played_percentage")?;
            let play_count = row.get_i64("play_count")?;
            let last_played_at = row.get_opt_i64("last_played_at")?;
            let rating = row.get_f64("rating")?;
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
        None => Ok(json!({
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

async fn upsert_played_flag(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    played: bool,
) -> anyhow::Result<()> {
    ensure_user_data_target_visible(db, item_id).await?;
    let now = now_unix();
    match UserData::find_by_id((user_id.to_string(), item_id.to_string()))
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
                item_id: Set(item_id.to_string()),
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
    Ok(())
}

async fn upsert_user_data_simple(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    apply: impl FnOnce(&mut user_data::ActiveModel),
) -> anyhow::Result<()> {
    ensure_user_data_target_visible(db, item_id).await?;
    let now = now_unix();
    match UserData::find_by_id((user_id.to_string(), item_id.to_string()))
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
                item_id: Set(item_id.to_string()),
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
    Ok(())
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
    Ok(db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "SELECT 1 AS found FROM people p JOIN media_people mp ON mp.person_id = p.id JOIN media_items mi ON mi.id = mp.item_id WHERE p.id = ? AND mi.is_public = 1 AND (mi.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = mi.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = mi.parent_id AND parent.is_public = 1)) LIMIT 1",
            vec![item_id.into()],
        ))
        .await?
        .is_some())
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
    let existing = UserData::find_by_id((user_id, item_id.clone()))
        .one(&state.db)
        .await;
    match existing {
        Ok(Some(model)) => {
            let data = json!({
                "ItemId": model.item_id,
                "Key": model.item_id,
                "IsFavorite": model.is_favorite != 0,
                "Played": model.played != 0,
                "PlaybackPositionTicks": model.playback_position_ticks,
                "PlayedPercentage": model.played_percentage,
                "PlayCount": model.play_count,
                "LastPlayedDate": model.last_played_at.map(unix_to_jellyfin_date),
                "Rating": model.rating,
                "Likes": null,
                "UnplayedItemCount": null,
            });
            Json(data).into_response()
        }
        Ok(None) => Json(json!({
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
        }))
        .into_response(),
        Err(error) => internal_error(error.into()),
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
    use crate::db::row_ext::QueryResultExt;
    use sea_orm::Set;
    use sea_orm::{ConnectionTrait, Database};
    use serde_json::json;
    use std::collections::HashMap;

    #[tokio::test]
    async fn marking_unplayed_does_not_increment_play_count() {
        let db = seeded_db().await;
        let backend = db.get_database_backend();

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

        let row = db
            .query_one(crate::db::helpers::portable_statement(
                backend,
                "SELECT played, play_count, last_played_at FROM user_data WHERE user_id = 'u1' AND item_id = 'i1'",
                vec![],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("played").unwrap(), 0);
        assert_eq!(row.get_i64("play_count").unwrap(), 0);
        assert_eq!(row.get_opt_i64("last_played_at").unwrap(), None);
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
        let db = seeded_db().await;
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
        let db = seeded_db().await;
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
        let db = seeded_db().await;
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES ('private', 'Private', '/tmp/private.mkv', '', '', 'Movie', 0, 0, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO people (id, name, created_at) VALUES ('p1', 'Person', 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_people (item_id, person_id, person_type, sort_order) VALUES ('i1', 'p1', 'Actor', 0)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES ('private-parent', 'Private Parent', '/tmp/private-parent', '', '', 'Movie', 1, 0, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES ('public-child', 'Public Child', '/tmp/public-child.mkv', '', 'private-parent', 'Movie', 0, 1, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO people (id, name, created_at) VALUES ('p2', 'Hidden Person', 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_people (item_id, person_id, person_type, sort_order) VALUES ('public-child', 'p2', 'Actor', 0)",
            vec![],
        ))
        .await
        .unwrap();

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

    async fn seeded_db() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES ('u1', 'u1', 'u1', 0, 0, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES ('i1', 'Movie', '/tmp/i1.mkv', '', '', 'Movie', 0, 1, 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db
    }
}
