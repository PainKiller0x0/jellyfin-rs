use axum::{
    Json,
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use serde_json::{Map, Value as JsonValue, json};

pub async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({ "Error": "Not found" })))
}

pub async fn empty_list() -> impl IntoResponse {
    Json(json!({ "Items": [], "TotalRecordCount": 0 }))
}

pub async fn empty_array() -> impl IntoResponse {
    Json(Vec::<serde_json::Value>::new())
}

pub async fn empty_object() -> impl IntoResponse {
    Json(json!({}))
}

pub async fn no_content() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub fn internal_error(error: anyhow::Error) -> Response {
    tracing::warn!("request failed: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "Error": format!("{error:#}") })),
    )
        .into_response()
}

pub async fn image() -> Response {
    let bytes = general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lpNcWQAAAABJRU5ErkJggg==")
        .expect("embedded PNG must decode");
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(
        header::ETAG,
        HeaderValue::from_static("jellyfin-rs-placeholder"),
    );
    (headers, Body::from(bytes)).into_response()
}

/// Recursively remove null fields from a JSON object.
/// Matches Jellyfin's DefaultIgnoreCondition = WhenWritingNull behavior.
pub fn strip_nulls(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let cleaned: Map<String, JsonValue> = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect();
            JsonValue::Object(cleaned)
        }
        JsonValue::Array(arr) => {
            JsonValue::Array(arr.into_iter().map(strip_nulls).collect())
        }
        other => other,
    }
}
