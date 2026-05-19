use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use sea_orm::ConnectionTrait;

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME, SERVER_NAME},
    jellyfin::common::internal_error,
    util::{hash_password, now_unix},
};

#[derive(Deserialize)]
pub struct StartupConfigurationRequest {
    #[serde(rename = "ServerName")]
    server_name: Option<String>,
    #[serde(rename = "UICulture")]
    ui_culture: Option<String>,
    #[serde(rename = "MetadataCountryCode")]
    metadata_country_code: Option<String>,
    #[serde(rename = "PreferredMetadataLanguage")]
    preferred_metadata_language: Option<String>,
}

#[derive(Deserialize)]
pub struct StartupUserRequest {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Password")]
    password: String,
}

#[derive(Deserialize)]
pub struct RemoteAccessRequest {
    #[serde(rename = "EnableRemoteAccess")]
    enable_remote_access: bool,
}

type Response = axum::response::Response;

pub async fn startup_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ServerName": super::app_setting(&state.db, "ServerName", SERVER_NAME).await,
        "UICulture": super::app_setting(&state.db, "UICulture", "zh-CN").await,
        "MetadataCountryCode": super::app_setting(&state.db, "MetadataCountryCode", "CN").await,
        "PreferredMetadataLanguage": super::app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        "EnableRemoteAccess": super::app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
    }))
    .into_response()
}

pub async fn update_startup_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartupConfigurationRequest>,
) -> Response {
    for (key, value) in [
        ("ServerName", request.server_name),
        ("UICulture", request.ui_culture),
        ("MetadataCountryCode", request.metadata_country_code),
        (
            "PreferredMetadataLanguage",
            request.preferred_metadata_language,
        ),
    ] {
        if let Some(value) = value {
            if let Err(error) = super::set_app_setting(&state.db, key, value.trim()).await {
                return internal_error(error);
            }
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn startup_user(State(state): State<Arc<AppState>>) -> Response {
    match super::first_admin_user(&state.db).await {
        Ok(Some((id, name))) => {
            Json(json!({ "Id": id, "Name": name, "Password": "" })).into_response()
        }
        Ok(None) => Json(json!({ "Name": DEFAULT_USER_NAME, "Password": "" })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_startup_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartupUserRequest>,
) -> Response {
    let username = request.name.trim();
    if username.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Name is required" })),
        )
            .into_response();
    }

    let now = now_unix();
    let password_hash = match hash_password(&request.password) {
        Ok(hash) => hash,
        Err(error) => return internal_error(error),
    };
    let user_id = state.user_id.to_string();

    let backend = state.db.get_database_backend();
    match state.db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 1, ?, ?) ON CONFLICT(id) DO UPDATE SET username = excluded.username, password_hash = excluded.password_hash, display_name = excluded.display_name, is_admin = 1, is_disabled = 0, updated_at = excluded.updated_at"#,
        vec![
            user_id.clone().into(),
            username.into(),
            password_hash.into(),
            username.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    {
        Ok(_) => Json(json!({ "Id": user_id, "Name": username })).into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn update_remote_access(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RemoteAccessRequest>,
) -> Response {
    match super::set_app_setting(
        &state.db,
        "EnableRemoteAccess",
        if request.enable_remote_access {
            "true"
        } else {
            "false"
        },
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn complete_startup(State(state): State<Arc<AppState>>) -> Response {
    match super::set_app_setting(&state.db, "StartupWizardCompleted", "true").await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn server_configuration(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ServerName": super::app_setting(&state.db, "ServerName", SERVER_NAME).await,
        "UICulture": super::app_setting(&state.db, "UICulture", "zh-CN").await,
        "MetadataCountryCode": super::app_setting(&state.db, "MetadataCountryCode", "CN").await,
        "PreferredMetadataLanguage": super::app_setting(&state.db, "PreferredMetadataLanguage", "zh-CN").await,
        "EnableRemoteAccess": super::app_setting_bool(&state.db, "EnableRemoteAccess", false).await,
        "CastReceiverApplications": [
            {
                "Id": "",
                "Name": "Disabled"
            }
        ]
    }))
    .into_response()
}

pub async fn update_server_configuration(
    State(state): State<Arc<AppState>>,
    Json(request): Json<Value>,
) -> Response {
    for key in [
        "ServerName",
        "UICulture",
        "MetadataCountryCode",
        "PreferredMetadataLanguage",
    ] {
        if let Some(value) = request.get(key).and_then(Value::as_str) {
            if let Err(error) = super::set_app_setting(&state.db, key, value.trim()).await {
                return internal_error(error);
            }
        }
    }

    if let Some(value) = request.get("EnableRemoteAccess").and_then(Value::as_bool) {
        if let Err(error) = super::set_app_setting(
            &state.db,
            "EnableRemoteAccess",
            if value { "true" } else { "false" },
        )
        .await
        {
            return internal_error(error);
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn default_metadata_options() -> impl IntoResponse {
    Json(json!({
        "ItemType": "Movie",
        "DisabledMetadataSavers": [],
        "LocalMetadataReaderOrder": [],
        "DisabledMetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "DisabledImageFetchers": [],
        "ImageFetcherOrder": []
    }))
}

pub async fn named_configuration(Path(key): Path<String>) -> impl IntoResponse {
    let value = match key.as_str() {
        "encoding" => json!({
            "EnableHardwareEncoding": false,
            "EnableThrottling": false,
            "EncodingThreadCount": -1
        }),
        "network" => json!({
            "EnableRemoteAccess": false,
            "LocalNetworkSubnets": [],
            "LocalNetworkAddresses": [],
            "KnownProxies": [],
            "PublishedServerUriBySubnet": []
        }),
        "livetv" => json!({
            "ListingProviders": [],
            "TunerHosts": []
        }),
        _ => json!({}),
    };
    Json(value)
}

pub async fn update_named_configuration() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub async fn configuration_pages(
    Query(_query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    Json(Vec::<Value>::new())
}

pub async fn dashboard_configuration_page() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}
