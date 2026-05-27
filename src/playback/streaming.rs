use std::{collections::HashMap, io::SeekFrom, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};
use tokio_util::io::ReaderStream;

use crate::{
    app::state::AppState,
    jellyfin::{
        auth::request_user_id_or_default,
        routes::{find_media_item, internal_error, not_found},
    },
    library::models::MediaItem,
};

pub async fn stream_video(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::GET).await
}

pub async fn stream_video_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::HEAD).await
}

pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::GET).await
}

pub async fn stream_audio_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::HEAD).await
}

pub async fn stream_subtitle(
    State(state): State<Arc<AppState>>,
    Path((item_id, index, _format)): Path<(String, i64, String)>,
) -> Response {
    stream_subtitle_item(state, item_id, index, Method::GET).await
}

pub async fn stream_subtitle_head(
    State(state): State<Arc<AppState>>,
    Path((item_id, index, _format)): Path<(String, i64, String)>,
) -> Response {
    stream_subtitle_item(state, item_id, index, Method::HEAD).await
}

/// Subtitle streaming with mediaSourceId path segment for Emby client compatibility.
pub async fn stream_subtitle_with_source(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index, _format)): Path<(String, String, i64, String)>,
) -> Response {
    // media_source_id is ignored; route to the same handler
    stream_subtitle_item(state, item_id, index, Method::GET).await
}

pub async fn stream_subtitle_with_source_head(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index, _format)): Path<(String, String, i64, String)>,
) -> Response {
    stream_subtitle_item(state, item_id, index, Method::HEAD).await
}

/// Subtitle streaming with mediaSourceId and start position ticks (Emby compatibility).
pub async fn stream_subtitle_with_ticks(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index, _start_ticks, _format)): Path<(String, String, i64, i64, String)>,
) -> Response {
    stream_subtitle_item(state, item_id, index, Method::GET).await
}

pub async fn stream_subtitle_with_ticks_head(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index, _start_ticks, _format)): Path<(String, String, i64, i64, String)>,
) -> Response {
    stream_subtitle_item(state, item_id, index, Method::HEAD).await
}

async fn stream_subtitle_item(
    state: Arc<AppState>,
    item_id: String,
    index: i64,
    method: Method,
) -> Response {
    let path = match crate::jellyfin::routes::subtitle_stream_path(&state.db, &item_id, index).await
    {
        Ok(Some(path)) => path,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": format!("failed to read subtitle file: {error}") })),
            )
                .into_response();
        }
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if method == Method::HEAD {
        (StatusCode::OK, headers, Body::empty()).into_response()
    } else {
        (StatusCode::OK, headers, Body::from(bytes)).into_response()
    }
}

async fn stream_media_item(
    state: Arc<AppState>,
    item_id: String,
    request_headers: HeaderMap,
    query: HashMap<String, String>,
    method: Method,
) -> Response {
    let user_id = request_user_id_or_default(&state, &request_headers, &query).await;
    let item = match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return not_found().await.into_response(),
        Err(error) => return internal_error(error),
    };
    if item.is_folder {
        return StatusCode::NOT_FOUND.into_response();
    }

    let mut file = match File::open(&item.path).await {
        Ok(file) => file,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": format!("failed to read media file: {error}") })),
            )
                .into_response();
        }
    };
    let metadata = match file.metadata().await {
        Ok(metadata) => metadata,
        Err(error) => {
            return internal_error(anyhow::anyhow!(error).context("failed to read media metadata"));
        }
    };
    let file_size = metadata.len();
    if file_size == 0 {
        return StatusCode::NO_CONTENT.into_response();
    }

    let range = request_headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_range_header(value, file_size));
    let byte_range = range.unwrap_or(ByteRange {
        start: 0,
        end: file_size - 1,
    });
    let content_length = byte_range.len();

    if let Err(error) = file.seek(SeekFrom::Start(byte_range.start)).await {
        return internal_error(anyhow::anyhow!(error).context("failed to seek media file"));
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(media_content_type(&item))
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&item.id).unwrap_or_else(|_| HeaderValue::from_static("jellyfin-rs")),
    );

    let status = if range.is_some() {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!(
                "bytes {}-{}/{}",
                byte_range.start, byte_range.end, file_size
            ))
            .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
        );
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    if method == Method::HEAD {
        return (status, headers, Body::empty()).into_response();
    }
    let stream = ReaderStream::new(file.take(content_length));
    (status, headers, Body::from_stream(stream)).into_response()
}

#[derive(Clone, Copy)]
struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

fn parse_range_header(value: &str, file_size: u64) -> Option<ByteRange> {
    let range = value.strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(file_size);
        return Some(ByteRange {
            start: file_size - suffix,
            end: file_size - 1,
        });
    }
    let start = start.parse::<u64>().ok()?;
    if start >= file_size {
        return None;
    }
    let end = if end.is_empty() {
        file_size - 1
    } else {
        end.parse::<u64>().ok()?.min(file_size - 1)
    };
    (start <= end).then_some(ByteRange { start, end })
}

fn media_content_type(item: &MediaItem) -> &'static str {
    match item.container.as_deref().unwrap_or_default() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/aac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// GET /Audio/{id}/stream — alias for stream_audio
pub async fn stream_audio_simple(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::GET).await
}

/// HEAD /Audio/{id}/stream — alias for stream_audio_head
pub async fn stream_audio_simple_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::HEAD).await
}

/// GET /Audio/{id}/stream.{Container} — audio stream with container specified
pub async fn stream_audio_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::GET).await
}

/// HEAD /Audio/{id}/stream.{Container} — HEAD for audio stream with container
pub async fn stream_audio_container_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, headers, query, Method::HEAD).await
}
