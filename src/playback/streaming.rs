use std::{
    collections::HashMap,
    io::SeekFrom,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{EntityTrait, QueryOrder};
use serde_json::json;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
    sync::RwLock,
    time::timeout,
};
use tokio_util::io::ReaderStream;

use crate::{
    app::state::AppState,
    entities::{library_paths, library_paths::Entity as LibraryPaths},
    jellyfin::{
        auth::{request_user_id_and_admin_or_default, request_user_id_or_default},
        common::{ok_response, wants_json_response},
        routes::{find_media_item, find_media_item_for_admin, internal_error, not_found},
    },
    library::{models::MediaItem, path_utils},
};

pub async fn stream_video(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

pub async fn stream_video_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

pub async fn stream_video_simple(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

pub async fn stream_video_simple_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

pub async fn stream_video_with_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id, _container)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::GET,
        None,
    )
    .await
}

pub async fn stream_video_with_source_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id, _container)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::HEAD,
        None,
    )
    .await
}

pub async fn stream_video_with_source_simple(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::GET,
        None,
    )
    .await
}

pub async fn stream_video_with_source_simple_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::HEAD,
        None,
    )
    .await
}

pub async fn stream_video_original(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

pub async fn stream_video_original_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

pub async fn stream_video_original_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

pub async fn stream_video_original_container_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

pub async fn stream_video_original_with_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::GET,
        None,
    )
    .await
}

pub async fn stream_video_original_with_source_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::HEAD,
        None,
    )
    .await
}

pub async fn stream_video_original_with_source_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id, _container)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::GET,
        None,
    )
    .await
}

pub async fn stream_video_original_with_source_container_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, media_source_id, _container)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        Some(media_source_id),
        headers,
        query,
        Method::HEAD,
        None,
    )
    .await
}

pub async fn stream_audio(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

pub async fn stream_audio_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

pub async fn stream_subtitle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, index, format)): Path<(String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        None,
        headers,
        query,
        Method::GET,
    )
    .await
}

pub async fn stream_subtitle_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, index, format)): Path<(String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        None,
        headers,
        query,
        Method::HEAD,
    )
    .await
}

/// Subtitle streaming with mediaSourceId path segment for Emby client compatibility.
pub async fn stream_subtitle_with_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // media_source_id is ignored; route to the same handler
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        None,
        headers,
        query,
        Method::GET,
    )
    .await
}

pub async fn stream_subtitle_with_source_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        None,
        headers,
        query,
        Method::HEAD,
    )
    .await
}

/// Subtitle streaming with mediaSourceId and start position ticks (Emby compatibility).
pub async fn stream_subtitle_with_ticks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, start_ticks, format)): Path<(
        String,
        String,
        i64,
        i64,
        String,
    )>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        Some(start_ticks),
        headers,
        query,
        Method::GET,
    )
    .await
}

pub async fn stream_subtitle_with_ticks_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, start_ticks, format)): Path<(
        String,
        String,
        i64,
        i64,
        String,
    )>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        index,
        format,
        Some(start_ticks),
        headers,
        query,
        Method::HEAD,
    )
    .await
}

async fn stream_subtitle_item(
    state: Arc<AppState>,
    item_id: String,
    index: i64,
    format: String,
    start_ticks: Option<i64>,
    request_headers: HeaderMap,
    query: HashMap<String, String>,
    method: Method,
) -> Response {
    let (user_id, is_admin) =
        request_user_id_and_admin_or_default(&state, &request_headers, &query).await;
    let item = if is_admin {
        find_media_item_for_admin(&state.db, &user_id, &item_id).await
    } else {
        find_media_item(&state.db, &user_id, &item_id).await
    };
    let item = match item {
        Ok(Some(item)) => item,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    let path = match crate::jellyfin::routes::subtitle_stream_path(&state.db, &item_id, index).await
    {
        Ok(path) => path,
        Err(error) => return internal_error(error),
    };
    let bytes = match path {
        Some(path) => {
            if !readable_media_path(&state.db, &path).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "Error": format!("failed to read subtitle file: {error}") })),
                    )
                        .into_response();
                }
            }
        }
        None => match cached_embedded_subtitle(&state, &item, index, start_ticks).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(item_id = %item_id, index, "embedded subtitle extraction failed: {error:#}");
                return StatusCode::NOT_FOUND.into_response();
            }
        },
    };
    let (content_type, bytes) = if format.eq_ignore_ascii_case("js") {
        (
            "application/json; charset=utf-8",
            vtt_to_track_events(&bytes),
        )
    } else if format.eq_ignore_ascii_case("vtt") {
        ("text/vtt; charset=utf-8", bytes)
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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

const EMBEDDED_SUBTITLE_TIMEOUT: Duration = Duration::from_secs(30);
const EMBEDDED_SUBTITLE_WINDOW_SECONDS: u64 = 30;
const MAX_EMBEDDED_SUBTITLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EMBEDDED_SUBTITLE_CACHE_ENTRIES: usize = 32;

type EmbeddedSubtitleCacheKey = (String, i64, i64, i64);
static EMBEDDED_SUBTITLE_CACHE: OnceLock<RwLock<HashMap<EmbeddedSubtitleCacheKey, Vec<u8>>>> =
    OnceLock::new();

fn embedded_subtitle_cache() -> &'static RwLock<HashMap<EmbeddedSubtitleCacheKey, Vec<u8>>> {
    EMBEDDED_SUBTITLE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

async fn cached_embedded_subtitle(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    start_ticks: Option<i64>,
) -> anyhow::Result<Vec<u8>> {
    let start_ticks = start_ticks.unwrap_or_default().max(0);
    let key = (item.id.clone(), item.modified_at, index, start_ticks);
    if let Some(bytes) = embedded_subtitle_cache().read().await.get(&key).cloned() {
        return Ok(bytes);
    }
    let bytes = extract_embedded_subtitle(state, item, index, start_ticks).await?;
    let mut cache = embedded_subtitle_cache().write().await;
    if cache.len() >= MAX_EMBEDDED_SUBTITLE_CACHE_ENTRIES {
        if let Some(oldest) = cache.keys().next().cloned() {
            cache.remove(&oldest);
        }
    }
    cache.insert(key, bytes.clone());
    Ok(bytes)
}

async fn extract_embedded_subtitle(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    start_ticks: i64,
) -> anyhow::Result<Vec<u8>> {
    let source = match playback_target_for_item(item)? {
        PlaybackTarget::RemoteUrl(url) => url,
        PlaybackTarget::LocalPath(path) => path.to_string_lossy().into_owned(),
    };
    let mut command = Command::new(&state.sa_config.ffmpeg_path);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-ss")
        .arg(format!("{:.3}", start_ticks as f64 / 10_000_000.0))
        .arg("-i")
        .arg(source)
        .arg("-t")
        .arg(EMBEDDED_SUBTITLE_WINDOW_SECONDS.to_string())
        .arg("-map")
        .arg(format!("0:{index}"))
        .arg("-c:s")
        .arg("webvtt")
        .arg("-f")
        .arg("webvtt")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = timeout(EMBEDDED_SUBTITLE_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("embedded subtitle extraction timed out"))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg exited with {}: {}", output.status, stderr.trim());
    }
    if output.stdout.len() > MAX_EMBEDDED_SUBTITLE_BYTES {
        anyhow::bail!("embedded subtitle output is too large");
    }
    if output.stdout.is_empty() {
        anyhow::bail!("ffmpeg returned an empty subtitle stream");
    }
    Ok(shift_vtt_timestamps(&output.stdout, start_ticks))
}

fn shift_vtt_timestamps(bytes: &[u8], start_ticks: i64) -> Vec<u8> {
    if start_ticks <= 0 {
        return bytes.to_vec();
    }
    let offset_seconds = start_ticks as f64 / 10_000_000.0;
    let text = String::from_utf8_lossy(bytes).replace('\r', "");
    let mut shifted = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            shifted.push('\n');
        }
        let Some((start, end_and_settings)) = line.split_once("-->") else {
            shifted.push_str(line);
            continue;
        };
        let Some(end_token) = end_and_settings.split_whitespace().next() else {
            shifted.push_str(line);
            continue;
        };
        let Some(start_seconds) = vtt_timestamp_seconds(start.trim()) else {
            shifted.push_str(line);
            continue;
        };
        let Some(end_seconds) = vtt_timestamp_seconds(end_token) else {
            shifted.push_str(line);
            continue;
        };
        let settings = &end_and_settings[end_token.len()..];
        shifted.push_str(&format!(
            "{} --> {}{}",
            format_vtt_timestamp(start_seconds + offset_seconds),
            format_vtt_timestamp(end_seconds + offset_seconds),
            settings
        ));
    }
    shifted.into_bytes()
}

fn vtt_to_track_events(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes).replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    let mut events = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].contains("-->") {
            index += 1;
            continue;
        }
        let Some((start, end)) = parse_vtt_timing(lines[index]) else {
            index += 1;
            continue;
        };
        index += 1;
        let text_start = index;
        while index < lines.len() && !lines[index].trim().is_empty() {
            index += 1;
        }
        let cue_text = lines[text_start..index].join("\n");
        if !cue_text.is_empty() {
            events.push(json!({
                "Text": cue_text,
                "StartPositionTicks": start,
                "EndPositionTicks": end,
            }));
        }
        index += 1;
    }
    serde_json::to_vec(&json!({ "TrackEvents": events }))
        .unwrap_or_else(|_| br#"{"TrackEvents":[]}"#.to_vec())
}

fn parse_vtt_timing(line: &str) -> Option<(i64, i64)> {
    let (start, end) = line.split_once("-->")?;
    let start = vtt_timestamp_ticks(start.trim().split_whitespace().next()?)?;
    let end = vtt_timestamp_ticks(end.trim().split_whitespace().next()?)?;
    (end > start).then_some((start, end))
}

fn vtt_timestamp_ticks(value: &str) -> Option<i64> {
    Some((vtt_timestamp_seconds(value)? * 10_000_000.0).round() as i64)
}

fn vtt_timestamp_seconds(value: &str) -> Option<f64> {
    let parts: Vec<&str> = value.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0.0, minutes.parse::<f64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<f64>().ok()?,
            minutes.parse::<f64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let seconds = seconds.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn format_vtt_timestamp(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let hours = (seconds / 3600.0).floor() as u64;
    let minutes = ((seconds - hours as f64 * 3600.0) / 60.0).floor() as u64;
    let remainder = seconds - hours as f64 * 3600.0 - minutes as f64 * 60.0;
    format!("{hours:02}:{minutes:02}:{remainder:06.3}")
}

async fn stream_media_item(
    state: Arc<AppState>,
    item_id: String,
    media_source_id: Option<String>,
    request_headers: HeaderMap,
    query: HashMap<String, String>,
    method: Method,
    download_filename: Option<String>,
) -> Response {
    let user_id = request_user_id_or_default(&state, &request_headers, &query).await;
    let item =
        match find_stream_media_item(&state.db, &user_id, &item_id, media_source_id.as_deref())
            .await
        {
            Ok(Some(item)) => item,
            Ok(None) => return not_found().await.into_response(),
            Err(error) => return internal_error(error),
        };
    if item.is_folder {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !readable_media_path(&state.db, &item.path).await {
        tracing::warn!(
            item_id = %item.id,
            path = %item.path,
            "media item path is outside configured library paths or is not visible"
        );
        return StatusCode::NOT_FOUND.into_response();
    }
    if method != Method::HEAD && wants_json_response(&request_headers) {
        return ok_response();
    }

    let playback_target = match playback_target_for_item(&item) {
        Ok(PlaybackTarget::RemoteUrl(url)) => return remote_stream_redirect(&url),
        Ok(PlaybackTarget::LocalPath(path)) => path,
        Err(error) => {
            tracing::warn!(
                item_id = %item.id,
                path = %item.path,
                "failed to resolve playback target: {error:#}"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let mut file = match File::open(&playback_target).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(
                item_id = %item.id,
                path = %playback_target.display(),
                "failed to open media file for playback: {error}"
            );
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
    if let Some(filename) = download_filename {
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        );
    }

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

pub(crate) async fn stream_item_file(
    state: Arc<AppState>,
    item_id: String,
    request_headers: HeaderMap,
    query: HashMap<String, String>,
    method: Method,
    download_filename: Option<String>,
) -> Response {
    stream_media_item(
        state,
        item_id,
        None,
        request_headers,
        query,
        method,
        download_filename,
    )
    .await
}

enum PlaybackTarget {
    LocalPath(std::path::PathBuf),
    RemoteUrl(String),
}

async fn find_stream_media_item(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
    media_source_id: Option<&str>,
) -> anyhow::Result<Option<MediaItem>> {
    if let Some(media_source_id) =
        media_source_id.filter(|id| preferred_media_source_id(item_id, id))
    {
        match find_stream_media_item_by_id(db, user_id, media_source_id).await? {
            Some(item) => return Ok(Some(item)),
            None => {
                tracing::warn!(
                    item_id,
                    media_source_id,
                    "media source id was not playable; falling back to item id"
                );
            }
        }
    }
    find_stream_media_item_by_id(db, user_id, item_id).await
}

async fn find_stream_media_item_by_id(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let Some(item) = find_media_item(db, user_id, item_id).await? else {
        return Ok(None);
    };
    if !item.is_folder {
        return Ok(Some(item));
    }
    crate::jellyfin::items::find_first_playable_child(db, user_id, &item.id).await
}

fn preferred_media_source_id(item_id: &str, media_source_id: &str) -> bool {
    let media_source_id = media_source_id.trim();
    !media_source_id.is_empty()
        && media_source_id != item_id
        && !matches!(
            media_source_id.to_ascii_lowercase().as_str(),
            "default" | "main" | "primary"
        )
}

fn playback_target_for_item(item: &MediaItem) -> anyhow::Result<PlaybackTarget> {
    let path = std::path::Path::new(&item.path);
    if crate::strm::is_strm_path(path) {
        let target = crate::strm::resolve_strm_path(path)?;
        let target_text = target.to_string_lossy().to_string();
        if crate::strm::is_remote_url(&target_text) {
            return Ok(PlaybackTarget::RemoteUrl(target_text));
        }
        return Ok(PlaybackTarget::LocalPath(target));
    }
    Ok(PlaybackTarget::LocalPath(std::path::PathBuf::from(
        &item.path,
    )))
}

fn remote_stream_redirect(url: &str) -> Response {
    let mut headers = HeaderMap::new();
    match HeaderValue::from_str(url) {
        Ok(location) => {
            headers.insert(header::LOCATION, location);
            (StatusCode::TEMPORARY_REDIRECT, headers).into_response()
        }
        Err(error) => internal_error(anyhow::anyhow!(error).context("invalid STRM URL")),
    }
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

pub(crate) async fn readable_media_path(db: &sea_orm::DatabaseConnection, path: &str) -> bool {
    match library_roots(db).await {
        Ok(roots) => path_utils::path_within_roots(path, &roots),
        Err(error) => {
            tracing::warn!("failed to check media path roots: {error:#}");
            false
        }
    }
}

async fn library_roots(db: &sea_orm::DatabaseConnection) -> anyhow::Result<Vec<String>> {
    {
        let cache = library_roots_cache().read().await;
        if let Some(cached) = cache.as_ref()
            && cached.loaded_at.elapsed() <= LIBRARY_ROOTS_CACHE_TTL
        {
            return Ok(cached.roots.clone());
        }
    }

    // Recheck while holding the write lock so concurrent HEAD/GET/seek
    // requests perform at most one database refresh per short TTL window.
    let mut cache = library_roots_cache().write().await;
    if let Some(cached) = cache.as_ref()
        && cached.loaded_at.elapsed() <= LIBRARY_ROOTS_CACHE_TTL
    {
        return Ok(cached.roots.clone());
    }

    let paths = LibraryPaths::find()
        .order_by_asc(library_paths::Column::Path)
        .all(db)
        .await?;
    let roots: Vec<String> = paths.into_iter().map(|path| path.path).collect();
    *cache = Some(CachedLibraryRoots {
        loaded_at: Instant::now(),
        roots: roots.clone(),
    });
    Ok(roots)
}

const LIBRARY_ROOTS_CACHE_TTL: Duration = Duration::from_secs(3);

struct CachedLibraryRoots {
    loaded_at: Instant,
    roots: Vec<String>,
}

static LIBRARY_ROOTS_CACHE: OnceLock<RwLock<Option<CachedLibraryRoots>>> = OnceLock::new();

fn library_roots_cache() -> &'static RwLock<Option<CachedLibraryRoots>> {
    LIBRARY_ROOTS_CACHE.get_or_init(|| RwLock::new(None))
}

/// GET /Audio/{id}/stream — alias for stream_audio
pub async fn stream_audio_simple(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

/// HEAD /Audio/{id}/stream — alias for stream_audio_head
pub async fn stream_audio_simple_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

/// GET /Audio/{id}/stream.{Container} — audio stream with container specified
pub async fn stream_audio_container(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::GET, None).await
}

/// HEAD /Audio/{id}/stream.{Container} — HEAD for audio stream with container
pub async fn stream_audio_container_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _container)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_media_item(state, item_id, None, headers, query, Method::HEAD, None).await
}

#[cfg(test)]
mod tests {
    use super::{
        PlaybackTarget, parse_range_header, playback_target_for_item, remote_stream_redirect,
        shift_vtt_timestamps, vtt_to_track_events,
    };
    use crate::library::models::MediaItem;
    use axum::http::{StatusCode, header};

    #[test]
    fn playback_target_resolves_remote_strm_url() {
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-strm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("movie.strm");
        std::fs::write(&path, "https://example.test/movie.mp4").unwrap();

        let target = playback_target_for_item(&media_item(&path.to_string_lossy())).unwrap();
        match target {
            PlaybackTarget::RemoteUrl(url) => assert_eq!(url, "https://example.test/movie.mp4"),
            PlaybackTarget::LocalPath(_) => panic!("expected remote STRM target"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn playback_target_resolves_uppercase_remote_strm_url() {
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-strm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("movie.strm");
        std::fs::write(&path, "HTTPS://example.test/movie.mp4?token=1").unwrap();

        let target = playback_target_for_item(&media_item(&path.to_string_lossy())).unwrap();
        match target {
            PlaybackTarget::RemoteUrl(url) => {
                assert_eq!(url, "HTTPS://example.test/movie.mp4?token=1")
            }
            PlaybackTarget::LocalPath(_) => panic!("expected remote STRM target"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remote_strm_playback_is_a_redirect_without_body_buffering() {
        let response = remote_stream_redirect("https://smartstrm.example/movie.mkv?sign=x");

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://smartstrm.example/movie.mkv?sign=x"
        );
    }

    #[test]
    fn range_parser_covers_browser_seek_shapes() {
        assert_eq!(parse_range_header("bytes=0-99", 1_000).unwrap().len(), 100);
        assert_eq!(parse_range_header("bytes=900-", 1_000).unwrap().start, 900);
        assert_eq!(parse_range_header("bytes=-100", 1_000).unwrap().start, 900);
        assert!(parse_range_header("bytes=1000-", 1_000).is_none());
        assert!(parse_range_header("bytes=0-1,2-3", 1_000).is_none());
    }

    #[test]
    fn embedded_vtt_is_exposed_as_jellyfin_track_events() {
        let body = vtt_to_track_events(
            b"WEBVTT\n\n00:00.400 --> 00:03.900\n<b>hello</b>\n\n00:10.240 --> 00:12.910\nworld\n",
        );
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let events = value["TrackEvents"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["StartPositionTicks"], 4_000_000);
        assert_eq!(events[0]["EndPositionTicks"], 39_000_000);
        assert_eq!(events[0]["Text"], "<b>hello</b>");
        assert_eq!(events[1]["StartPositionTicks"], 102_400_000);
    }

    #[test]
    fn embedded_vtt_window_is_shifted_back_to_absolute_time() {
        let shifted = shift_vtt_timestamps(
            b"WEBVTT\n\n00:00.400 --> 00:03.900\nhello\n",
            180 * 10_000_000,
        );
        let text = String::from_utf8(shifted).unwrap();
        assert!(text.contains("00:03:00.400 --> 00:03:03.900"));
        assert!(text.contains("hello"));
    }

    fn media_item(path: &str) -> MediaItem {
        MediaItem {
            id: "item".to_string(),
            title: "Item".to_string(),
            path: path.to_string(),
            library_id: "movies".to_string(),
            collection_type: "movies".to_string(),
            parent_id: "movies".to_string(),
            item_type: "Video".to_string(),
            extra_type: None,
            video_type: None,
            iso_type: None,
            video_3d_format: None,
            is_folder: false,
            container: Some("mp4".to_string()),
            overview: None,
            official_rating: None,
            custom_rating: None,
            extended_video_type: None,
            original_title: None,
            sort_name: None,
            forced_sort_name: None,
            lock_data: false,
            locked_fields: Vec::new(),
            tagline: None,
            collection_name: None,
            original_language: None,
            series_status: None,
            home_page_url: None,
            remote_trailers: Vec::new(),
            production_locations: Vec::new(),
            production_year: None,
            premiere_date: None,
            end_date: None,
            runtime_ticks: None,
            display_order: None,
            size_bytes: None,
            season_number: None,
            episode_number: None,
            episode_number_end: None,
            community_rating: None,
            critic_rating: None,
            created_at: 1,
            modified_at: 1,
            is_public: true,
            is_favorite: false,
            played: false,
            playback_position_ticks: 0,
            played_percentage: None,
            play_count: 0,
            last_played_at: None,
            image_tags: None,
            ..Default::default()
        }
    }
}
