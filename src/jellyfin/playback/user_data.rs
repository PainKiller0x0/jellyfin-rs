use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
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
    jellyfin::{auth::request_user_id_or_default, common::internal_error},
    util::{now_unix, unix_to_jellyfin_date},
};

pub async fn favorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", true).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn unfavorite_item(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match set_user_data_flag_json(&state.db, &user_id, &item_id, "is_favorite", false).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn mark_played(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "played", true).await
}

pub async fn mark_unplayed(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    set_user_data_flag(&state.db, &user_id, &item_id, "played", false).await
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
        Err(error) => internal_error(error),
    }
}

pub async fn set_rating(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
    Json(body): Json<JsonValue>,
) -> Response {
    let rating = body
        .get("Likes")
        .and_then(JsonValue::as_bool)
        .map(|likes| if likes { 1.0 } else { -1.0 })
        .or_else(|| body.get("Rating").and_then(JsonValue::as_f64))
        .unwrap_or(0.0);
    match upsert_user_data_simple(&state.db, &user_id, &item_id, |active| {
        active.rating = Set(Some(rating));
    })
    .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
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
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn set_user_data_flag(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> Response {
    match upsert_user_data_flag(db, user_id, item_id, field, value).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn set_user_data_flag_json(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    field: &str,
    value: bool,
) -> anyhow::Result<JsonValue> {
    upsert_user_data_flag(db, user_id, item_id, field, value).await?;
    // Fetch the updated user data and return it
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT is_favorite, played, playback_position_ticks, played_percentage, play_count, last_played_at FROM user_data WHERE user_id = ? AND item_id = ?",
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
            Ok(json!({
                "ItemId": item_id,
                "Key": item_id,
                "IsFavorite": is_favorite,
                "Played": played,
                "PlaybackPositionTicks": playback_position_ticks,
                "PlayedPercentage": played_percentage,
                "PlayCount": play_count,
                "LastPlayedDate": last_played_at.map(unix_to_jellyfin_date),
                "Rating": null,
                "Likes": null,
                "UnplayedItemCount": null,
            }))
        }
        None => Ok(json!({
            "ItemId": item_id,
            "Key": item_id,
            "IsFavorite": value,
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
        "played" => {
            let now = now_unix();
            let backend = db.get_database_backend();
            db.execute(crate::db::helpers::portable_statement(
                backend,
                r#"INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, played_percentage, play_count, last_played_at, updated_at) VALUES (?, ?, ?, 0, NULL, COALESCE((SELECT play_count FROM user_data WHERE user_id = ? AND item_id = ?), 0) + 1, ?, ?) ON CONFLICT(user_id, item_id) DO UPDATE SET played = excluded.played, playback_position_ticks = excluded.playback_position_ticks, played_percentage = excluded.played_percentage, play_count = COALESCE(user_data.play_count, 0) + 1, last_played_at = excluded.last_played_at, updated_at = excluded.updated_at"#,
                vec![
                    user_id.into(),
                    item_id.into(),
                    value_int.into(),
                    user_id.into(),
                    item_id.into(),
                    now.into(),
                    now.into(),
                ],
            ))
            .await
            .with_context(|| format!("failed to update user data flag for item: {item_id}"))?;
            Ok(())
        }
        _ => anyhow::bail!("unsupported user data flag: {field}"),
    }
}

async fn upsert_user_data_simple(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    apply: impl FnOnce(&mut user_data::ActiveModel),
) -> anyhow::Result<()> {
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

    let is_favorite = body
        .get("IsFavorite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let played = body
        .get("Played")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let playback_position_ticks = body
        .get("PlaybackPositionTicks")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let play_count = body.get("PlayCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let last_played_at = if body.get("LastPlayedDate").is_some() {
        Some(now)
    } else {
        None
    };

    match UserData::find_by_id((user_id.clone(), item_id.clone()))
        .one(&state.db)
        .await
    {
        Ok(Some(model)) => {
            let mut active: user_data::ActiveModel = model.into();
            active.is_favorite = Set(if is_favorite { 1 } else { 0 });
            active.played = Set(if played { 1 } else { 0 });
            active.playback_position_ticks = Set(playback_position_ticks);
            active.play_count = Set(play_count);
            active.last_played_at = Set(last_played_at);
            active.updated_at = Set(now);
            if let Err(e) = active.update(&state.db).await {
                return internal_error(e.into());
            }
        }
        Ok(None) => {
            let active = user_data::ActiveModel {
                user_id: Set(user_id.clone()),
                item_id: Set(item_id.clone()),
                is_favorite: Set(if is_favorite { 1 } else { 0 }),
                played: Set(if played { 1 } else { 0 }),
                playback_position_ticks: Set(playback_position_ticks),
                play_count: Set(play_count),
                last_played_at: Set(last_played_at),
                updated_at: Set(now),
                ..Default::default()
            };
            if let Err(e) = UserData::insert(active).exec(&state.db).await {
                return internal_error(e.into());
            }
        }
        Err(error) => return internal_error(error.into()),
    }

    // Return updated user data
    match UserData::find_by_id((user_id, item_id))
        .one(&state.db)
        .await
    {
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
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}
