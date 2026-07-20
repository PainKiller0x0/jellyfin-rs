use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{EntityTrait, Set, sea_query::OnConflict};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    entities::display_preferences::{self, Entity as DisplayPreferences},
    jellyfin::common::internal_error,
    util::now_unix,
};

const MAX_DISPLAY_PREFERENCES_JSON_BYTES: usize = 64 * 1024;
const MAX_DISPLAY_PREFERENCES_ID_LEN: usize = 128;
const MAX_DISPLAY_PREFERENCES_CLIENT_LEN: usize = 128;

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
    let prefs_json = match serialize_display_preferences(body, &prefs_id, &client) {
        Ok(value) => value,
        Err(error) => return validation_error_response(error.0, error.1),
    };
    let id = display_preferences_key(&prefs_id, &user_id, &client);
    match DisplayPreferences::insert(display_preferences::ActiveModel {
        id: Set(id),
        user_id: Set(user_id),
        preferences_json: Set(prefs_json),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::column(display_preferences::Column::Id)
            .update_columns([
                display_preferences::Column::PreferencesJson,
                display_preferences::Column::UserId,
                display_preferences::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(&state.db)
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
    let model = if let Some(model) = DisplayPreferences::find_by_id(id)
        .one(db)
        .await
        .context("failed to load display preferences")?
    {
        Some(model)
    } else {
        DisplayPreferences::find_by_id(legacy_display_preferences_key(prefs_id))
            .one(db)
            .await
            .context("failed to load legacy display preferences")?
    };

    match model {
        Some(model) => Ok(Some(serde_json::from_str(&model.preferences_json)?)),
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
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DISPLAY_PREFERENCES_CLIENT_LEN)
        .collect::<String>()
}

fn serialize_display_preferences(
    mut prefs: Value,
    prefs_id: &str,
    client: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let Some(object) = prefs.as_object_mut() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Display preferences must be a JSON object",
        ));
    };
    let prefs_id = sanitize_display_preference_part(prefs_id, MAX_DISPLAY_PREFERENCES_ID_LEN);
    let client = sanitize_display_preference_part(client, MAX_DISPLAY_PREFERENCES_CLIENT_LEN);
    object.entry("Id").or_insert_with(|| json!(prefs_id));
    object.entry("Client").or_insert_with(|| json!(client));
    let serialized = prefs.to_string();
    if serialized.len() > MAX_DISPLAY_PREFERENCES_JSON_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "Display preferences are too large",
        ));
    }
    Ok(serialized)
}

fn sanitize_display_preference_part(value: &str, max_len: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max_len)
        .collect::<String>()
}

fn validation_error_response(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({ "Error": message }))).into_response()
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
        query.insert("Client".to_string(), "Web\nBad".to_string());
        assert_eq!(display_pref_client(&query), "WebBad");

        query.insert("Client".to_string(), "x".repeat(160));
        assert_eq!(
            display_pref_client(&query).chars().count(),
            MAX_DISPLAY_PREFERENCES_CLIENT_LEN
        );
    }

    #[test]
    fn display_preferences_payload_is_object_and_limited() {
        let serialized =
            serialize_display_preferences(json!({ "SortBy": "SortName" }), "home", "Web").unwrap();
        let value: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(value["Id"], "home");
        assert_eq!(value["Client"], "Web");

        assert!(serialize_display_preferences(json!([]), "home", "Web").is_err());
        assert!(
            serialize_display_preferences(
                json!({
                    "CustomPrefs": {
                        "large": "x".repeat(MAX_DISPLAY_PREFERENCES_JSON_BYTES)
                    }
                }),
                "home",
                "Web"
            )
            .is_err()
        );
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
