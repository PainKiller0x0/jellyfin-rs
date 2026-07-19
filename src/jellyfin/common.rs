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
    Json(json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 }))
}

pub async fn empty_array() -> impl IntoResponse {
    Json(Vec::<serde_json::Value>::new())
}

pub async fn no_content() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub async fn ok() -> impl IntoResponse {
    StatusCode::OK
}

pub fn ok_response() -> Response {
    StatusCode::OK.into_response()
}

pub fn wants_json_response(headers: &HeaderMap) -> bool {
    let Some(accept) = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let values = accept
        .split(',')
        .filter_map(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    values
        .iter()
        .any(|value| value == "application/json" || value.ends_with("+json"))
        && !values.iter().any(|value| {
            value == "*/*"
                || value.starts_with("image/")
                || value.starts_with("audio/")
                || value.starts_with("video/")
                || value == "application/octet-stream"
        })
}

pub fn internal_error(error: anyhow::Error) -> Response {
    tracing::warn!("request failed: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "Error": "Internal server error" })),
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

pub async fn image_or_empty(headers: HeaderMap) -> Response {
    if wants_json_response(&headers) {
        return ok_response();
    }
    image().await
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
        JsonValue::Array(arr) => JsonValue::Array(arr.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
    use serde_json::Value;

    use super::{empty_list, internal_error};

    #[tokio::test]
    async fn empty_list_has_query_result_shape() {
        let response = empty_list().await.into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert!(value["Items"].as_array().unwrap().is_empty());
        assert_eq!(value["TotalRecordCount"], 0);
        assert_eq!(value["StartIndex"], 0);
    }

    #[tokio::test]
    async fn internal_error_does_not_echo_details() {
        let response = internal_error(anyhow::anyhow!(
            "database failed at D:/private/media/movie.mkv"
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["Error"], "Internal server error");
        assert!(!String::from_utf8_lossy(&body).contains("D:/private"));
    }
}
