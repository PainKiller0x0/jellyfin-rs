use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::internal_error,
    util::now_unix,
};

pub async fn get_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
) -> Response {
    match display_preferences_inner(&state.db, &prefs_id).await {
        Ok(Some(prefs)) => Json(prefs).into_response(),
        Ok(None) => Json(json!({ "Id": prefs_id, "CustomPrefs": {} })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let now = now_unix();
    let prefs_json = body.to_string();
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let default_user_id = state.user_id.to_string();
    let user_id = body
        .get("UserId")
        .and_then(Value::as_str)
        .unwrap_or(&default_user_id);
    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO display_preferences (id, user_id, preferences_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET preferences_json = excluded.preferences_json, user_id = excluded.user_id, updated_at = excluded.updated_at"#,
            vec![id.into(), user_id.into(), prefs_json.into(), now.into(), now.into()],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

async fn display_preferences_inner(
    db: &sea_orm::DatabaseConnection,
    prefs_id: &str,
) -> anyhow::Result<Option<Value>> {
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT preferences_json FROM display_preferences WHERE id = ?",
            vec![id.into()],
        ))
        .await
        .context("failed to load display preferences")?;
    match row {
        Some(row) => {
            let json_str: String = row.get_str("preferences_json")?;
            Ok(Some(serde_json::from_str(&json_str)?))
        }
        None => Ok(None),
    }
}
