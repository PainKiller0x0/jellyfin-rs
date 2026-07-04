use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState, db::row_ext::QueryResultExt, jellyfin::common::internal_error,
    util::now_unix,
};

pub async fn get_display_preferences(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Path(prefs_id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user_id = display_pref_user_id(&request_user_id, &query);
    let client = display_pref_client(&query);
    match display_preferences_inner(&state.db, &prefs_id, &user_id, &client).await {
        Ok(Some(prefs)) => Json(prefs).into_response(),
        Ok(None) => Json(default_display_preferences(&prefs_id, &client)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_display_preferences(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Path(prefs_id): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    let now = now_unix();
    let user_id = display_pref_user_id(&request_user_id, &query);
    let client = display_pref_client(&query);
    let mut prefs = body;
    if let Some(object) = prefs.as_object_mut() {
        object.entry("Id").or_insert_with(|| json!(prefs_id));
        object.entry("Client").or_insert_with(|| json!(client));
    }
    let prefs_json = prefs.to_string();
    let id = display_preferences_key(&prefs_id, &user_id, &client);
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
    user_id: &str,
    client: &str,
) -> anyhow::Result<Option<Value>> {
    let id = display_preferences_key(prefs_id, user_id, client);
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT preferences_json FROM display_preferences WHERE id IN (?, ?) ORDER BY CASE WHEN id = ? THEN 0 ELSE 1 END LIMIT 1",
            vec![
                id.clone().into(),
                legacy_display_preferences_key(prefs_id).into(),
                id.into(),
            ],
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

fn display_pref_user_id(
    request_user_id: &str,
    query: &std::collections::HashMap<String, String>,
) -> String {
    query
        .get("userId")
        .or_else(|| query.get("UserId"))
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| request_user_id.to_string())
}

fn display_pref_client(query: &std::collections::HashMap<String, String>) -> String {
    query
        .get("client")
        .or_else(|| query.get("Client"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn display_preferences_key(prefs_id: &str, user_id: &str, client: &str) -> String {
    crate::util::stable_text_id(&format!("display-prefs:{user_id}:{client}:{prefs_id}"))
}

fn legacy_display_preferences_key(prefs_id: &str) -> String {
    crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"))
}

fn default_display_preferences(prefs_id: &str, client: &str) -> Value {
    json!({
        "Id": prefs_id,
        "ViewType": null,
        "SortBy": null,
        "IndexBy": null,
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Vertical",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": true,
        "Client": client
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn display_preferences_key_scopes_user_and_client() {
        assert_ne!(
            display_preferences_key("home", "u1", "web"),
            display_preferences_key("home", "u2", "web")
        );
        assert_ne!(
            display_preferences_key("home", "u1", "web"),
            display_preferences_key("home", "u1", "mobile")
        );
    }

    #[test]
    fn display_pref_client_accepts_jellyfin_casing() {
        let mut query = HashMap::new();
        query.insert("Client".to_string(), "Web".to_string());
        assert_eq!(display_pref_client(&query), "Web");
    }

    #[test]
    fn display_pref_user_id_defaults_to_request_user() {
        let query = HashMap::new();
        assert_eq!(display_pref_user_id("u1", &query), "u1");
    }

    #[test]
    fn default_display_preferences_have_jellyfin_shape() {
        let prefs = default_display_preferences("home", "Web");
        assert_eq!(prefs["Id"], "home");
        assert_eq!(prefs["Client"], "Web");
        assert!(prefs["CustomPrefs"].as_object().is_some());
        assert_eq!(prefs["ScrollDirection"], "Vertical");
    }
}
