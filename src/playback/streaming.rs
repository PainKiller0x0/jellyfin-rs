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
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde_json::json;
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
    sync::{Mutex, OnceCell, RwLock},
    time::timeout,
};
use tokio_util::io::ReaderStream;

use crate::{
    app::state::AppState,
    entities::{
        library_paths,
        library_paths::Entity as LibraryPaths,
        media_streams::{self, Entity as MediaStreams},
    },
    jellyfin::{
        auth::{request_user_id_and_admin_or_default, request_user_id_or_default},
        common::{ok_response, wants_json_response},
        routes::{find_media_item, find_media_item_for_admin, internal_error, not_found},
    },
    library::{
        models::{MediaItem, rewrite_public_strm_target},
        path_utils,
    },
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
        None,
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
        None,
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
    Path((item_id, media_source_id, index, format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        Some(media_source_id),
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
    Path((item_id, media_source_id, index, format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        Some(media_source_id),
        index,
        format,
        None,
        headers,
        query,
        Method::HEAD,
    )
    .await
}

/// Subtitle streaming with a start position but without mediaSourceId.
///
/// Jellyfin Web can emit this compact form while progressively loading
/// embedded subtitles. Keep it equivalent to the mediaSourceId variant so
/// lightweight clients do not need to query and retain a media source id.
pub async fn stream_subtitle_with_ticks_no_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, index, start_ticks, format)): Path<(String, i64, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        None,
        index,
        format,
        Some(start_ticks),
        headers,
        query,
        Method::GET,
    )
    .await
}

pub async fn stream_subtitle_with_ticks_no_source_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, index, start_ticks, format)): Path<(String, i64, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(
        state,
        item_id,
        None,
        index,
        format,
        Some(start_ticks),
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
    Path((item_id, media_source_id, index, start_ticks, format)): Path<(
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
        Some(media_source_id),
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
    Path((item_id, media_source_id, index, start_ticks, format)): Path<(
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
        Some(media_source_id),
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
    _media_source_id: Option<String>,
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
    let (bytes, is_external) = match path {
        Some(path) => {
            if !readable_media_path(&state.db, &path).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => (bytes, true),
                Err(error) => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "Error": format!("failed to read subtitle file: {error}") })),
                    )
                        .into_response();
                }
            }
        }
        None => {
            // Jellyfin Web may request Stream.js through a native track/XHR
            // path that the Jellium fetch shim cannot intercept. For remote
            // STRM sources, return only the requested bounded window. Do not
            // start a full-track background extraction: it competes with video
            // playback for the same remote source and can outlive a seek.
            let bytes = if format.eq_ignore_ascii_case("js") {
                let window_start_ticks = start_ticks.unwrap_or(0).max(0);
                match cached_embedded_subtitle_window(
                    &state,
                    &item,
                    index,
                    window_start_ticks,
                    EMBEDDED_SUBTITLE_TIMEOUT,
                )
                .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(item_id = %item_id, index, "embedded subtitle extraction failed: {error:#}");
                        return StatusCode::NOT_FOUND.into_response();
                    }
                }
            } else {
                match cached_embedded_subtitle(&state, &item, index, EMBEDDED_SUBTITLE_TIMEOUT)
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(item_id = %item_id, index, "embedded subtitle extraction failed: {error:#}");
                        return StatusCode::NOT_FOUND.into_response();
                    }
                }
            };
            (bytes, false)
        }
    };
    let Some((content_type, bytes)) = subtitle_response_payload(&format, bytes, is_external)
    else {
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

fn subtitle_response_payload(
    format: &str,
    bytes: Vec<u8>,
    is_external: bool,
) -> Option<(&'static str, Vec<u8>)> {
    if format.eq_ignore_ascii_case("js") {
        Some((
            "application/json; charset=utf-8",
            vtt_to_track_events(&bytes),
        ))
    } else if format.eq_ignore_ascii_case("vtt") {
        Some(("text/vtt; charset=utf-8", bytes))
    } else if is_external
        && (format.eq_ignore_ascii_case("ass") || format.eq_ignore_ascii_case("ssa"))
    {
        Some(("text/x-ssa; charset=utf-8", bytes))
    } else {
        None
    }
}

const EMBEDDED_SUBTITLE_TIMEOUT: Duration = Duration::from_secs(30);
// A remote STRM can service byte ranges quickly, but FFmpeg still has to walk
// the selected subtitle stream until the requested output window is complete.
// A 120-second window made a seek wait tens of seconds on Quark-backed media.
// Twenty seconds is small enough to return promptly and the WebView client
// requests the next window ahead of the playback position.
const EMBEDDED_SUBTITLE_WINDOW_SECONDS: u64 = 20;
const EMBEDDED_SUBTITLE_WINDOW_BUCKET_TICKS: i64 = 5 * 10_000_000;
const MAX_EMBEDDED_SUBTITLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EMBEDDED_SUBTITLE_CACHE_ENTRIES: usize = 32;
const DEFAULT_SUBTITLE_CACHE_DIR: &str = "/data/subtitle-cache";
const DEFAULT_SUBTITLE_PREWARM_INTERVAL_SECONDS: u64 = 6 * 60 * 60;
const DEFAULT_SUBTITLE_PREWARM_INITIAL_DELAY_SECONDS: u64 = 30;
const DEFAULT_SUBTITLE_PREWARM_MAX_ITEMS: usize = 32;
const DEFAULT_SUBTITLE_PREWARM_TIMEOUT_SECONDS: u64 = 15;
const DEFAULT_SUBTITLE_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;

type EmbeddedSubtitleCacheKey = (String, i64, i64, i64);
static EMBEDDED_SUBTITLE_CACHE: OnceLock<RwLock<HashMap<EmbeddedSubtitleCacheKey, Vec<u8>>>> =
    OnceLock::new();
static EMBEDDED_SUBTITLE_INFLIGHT: OnceLock<
    Mutex<HashMap<EmbeddedSubtitleCacheKey, Arc<OnceCell<Result<Vec<u8>, String>>>>>,
> = OnceLock::new();

fn embedded_subtitle_cache() -> &'static RwLock<HashMap<EmbeddedSubtitleCacheKey, Vec<u8>>> {
    EMBEDDED_SUBTITLE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn embedded_subtitle_inflight()
-> &'static Mutex<HashMap<EmbeddedSubtitleCacheKey, Arc<OnceCell<Result<Vec<u8>, String>>>>> {
    EMBEDDED_SUBTITLE_INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn cached_embedded_subtitle(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    extraction_timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    cached_embedded_subtitle_range(state, item, index, None, extraction_timeout).await
}

async fn cached_embedded_subtitle_window(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    start_ticks: i64,
    extraction_timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    // Browsers report a seek position with sub-second precision. Treat nearby
    // requests as the same five-second subtitle window so the `seeking` and
    // `seeked` events cannot launch duplicate FFmpeg processes.
    let start_ticks = normalize_embedded_subtitle_window_start_ticks(start_ticks);
    if let Some(bytes) = read_persistent_subtitle_cache(item, index).await? {
        return Ok(bytes);
    }
    let full_key = (item.id.clone(), item.modified_at, index, -1);
    if let Some(bytes) = embedded_subtitle_cache()
        .read()
        .await
        .get(&full_key)
        .cloned()
    {
        return Ok(bytes);
    }
    cached_embedded_subtitle_range(state, item, index, Some(start_ticks), extraction_timeout).await
}

fn normalize_embedded_subtitle_window_start_ticks(start_ticks: i64) -> i64 {
    start_ticks
        .max(0)
        .div_euclid(EMBEDDED_SUBTITLE_WINDOW_BUCKET_TICKS)
        * EMBEDDED_SUBTITLE_WINDOW_BUCKET_TICKS
}

async fn cached_embedded_subtitle_range(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    start_ticks: Option<i64>,
    extraction_timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let cache_start_ticks = start_ticks.unwrap_or(-1);
    let key = (item.id.clone(), item.modified_at, index, cache_start_ticks);
    let cache_path = persistent_subtitle_cache_path_for(item, index, start_ticks);
    loop {
        if let Some(bytes) = read_persistent_subtitle_cache_path(&cache_path).await? {
            return Ok(bytes);
        }
        if let Some(bytes) = embedded_subtitle_cache().read().await.get(&key).cloned() {
            return Ok(bytes);
        }

        // WebView2 can issue multiple subtitle requests at the same time. Only
        // one FFmpeg process should inspect the remote STRM source for a key.
        let cell = {
            let mut inflight = embedded_subtitle_inflight().lock().await;
            inflight
                .entry(key.clone())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let result = cell
            .get_or_init(|| async {
                let _permit = state
                    .queue_manager
                    .tier2_semaphore
                    .acquire()
                    .await
                    .map_err(|error| format!("subtitle extraction queue closed: {error}"))?;
                extract_embedded_subtitle(state, item, index, start_ticks, extraction_timeout)
                    .await
                    .map_err(|error| format!("{error:#}"))
            })
            .await
            .clone()
            .map_err(|error| anyhow::anyhow!(error));
        if let Ok(bytes) = &result {
            let mut cache = embedded_subtitle_cache().write().await;
            if cache.len() >= MAX_EMBEDDED_SUBTITLE_CACHE_ENTRIES {
                if let Some(oldest) = cache.keys().next().cloned() {
                    cache.remove(&oldest);
                }
            }
            cache.insert(key.clone(), bytes.clone());
            if let Err(error) = write_persistent_subtitle_cache_path(&cache_path, bytes).await {
                tracing::warn!(
                    item_id = %item.id,
                    index,
                    "failed to persist embedded subtitle cache: {error:#}"
                );
            }
        }
        let mut inflight = embedded_subtitle_inflight().lock().await;
        if inflight
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &cell))
        {
            inflight.remove(&key);
        }
        return result;
    }
}

fn subtitle_cache_dir() -> std::path::PathBuf {
    std::env::var_os("JELLYFIN_RS_SUBTITLE_CACHE_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_SUBTITLE_CACHE_DIR))
}

fn persistent_subtitle_cache_path_for(
    item: &MediaItem,
    index: i64,
    start_ticks: Option<i64>,
) -> std::path::PathBuf {
    let key = format!(
        "{}:{}:{}:{}",
        item.id,
        item.modified_at,
        index,
        start_ticks.unwrap_or(-1).max(-1)
    );
    let encoded = URL_SAFE_NO_PAD.encode(key.as_bytes());
    subtitle_cache_dir().join(format!("{encoded}.vtt"))
}

fn persistent_subtitle_cache_path(item: &MediaItem, index: i64) -> std::path::PathBuf {
    persistent_subtitle_cache_path_for(item, index, None)
}

async fn read_persistent_subtitle_cache(
    item: &MediaItem,
    index: i64,
) -> anyhow::Result<Option<Vec<u8>>> {
    read_persistent_subtitle_cache_path(&persistent_subtitle_cache_path(item, index)).await
}

async fn read_persistent_subtitle_cache_path(
    path: &std::path::Path,
) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(&path).await {
        Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
        Ok(_) => {
            let _ = fs::remove_file(path).await;
            Ok(None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn write_persistent_subtitle_cache_path(
    path: &std::path::Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    // The cache key is already part of `path`, so the temporary file is also
    // unique per item/track. This avoids collisions when two tracks finish
    // extraction during the same process.
    let temp_path = path.with_extension("vtt.tmp");
    fs::write(&temp_path, bytes).await?;
    if let Err(error) = fs::rename(&temp_path, &path).await {
        let _ = fs::remove_file(&temp_path).await;
        return Err(error.into());
    }
    Ok(())
}

fn subtitle_prewarm_interval() -> Duration {
    let seconds = std::env::var("JELLYFIN_RS_SUBTITLE_PREWARM_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SUBTITLE_PREWARM_INTERVAL_SECONDS);
    Duration::from_secs(seconds)
}

fn subtitle_prewarm_initial_delay() -> Duration {
    let seconds = std::env::var("JELLYFIN_RS_SUBTITLE_PREWARM_INITIAL_DELAY_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SUBTITLE_PREWARM_INITIAL_DELAY_SECONDS);
    Duration::from_secs(seconds)
}

fn subtitle_prewarm_max_items() -> usize {
    std::env::var("JELLYFIN_RS_SUBTITLE_PREWARM_MAX_ITEMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SUBTITLE_PREWARM_MAX_ITEMS)
}

fn subtitle_prewarm_timeout() -> Duration {
    let seconds = std::env::var("JELLYFIN_RS_SUBTITLE_PREWARM_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SUBTITLE_PREWARM_TIMEOUT_SECONDS);
    Duration::from_secs(seconds)
}

fn subtitle_cache_max_bytes() -> u64 {
    std::env::var("JELLYFIN_RS_SUBTITLE_CACHE_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SUBTITLE_CACHE_MAX_BYTES)
}

async fn prune_persistent_subtitle_cache() -> anyhow::Result<()> {
    let mut directory = match fs::read_dir(subtitle_cache_dir()).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut entries = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("vtt") {
            continue;
        }
        let metadata = entry.metadata().await?;
        entries.push((
            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            metadata.len(),
            path,
        ));
    }
    let mut total_bytes = entries.iter().map(|(_, size, _)| *size).sum::<u64>();
    if total_bytes <= subtitle_cache_max_bytes() {
        return Ok(());
    }
    entries.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in entries {
        if total_bytes <= subtitle_cache_max_bytes() {
            break;
        }
        if fs::remove_file(path).await.is_ok() {
            total_bytes = total_bytes.saturating_sub(size);
        }
    }
    Ok(())
}

/// Start the persistent embedded-subtitle prewarm loop.
///
/// The loop is intentionally incremental: it only extracts tracks whose
/// versioned cache file is missing, and the shared tier-2 semaphore keeps it
/// from competing with other light media operations.
pub fn start_embedded_subtitle_cache_scheduler(state: Arc<AppState>) {
    let interval = subtitle_prewarm_interval();
    if interval.is_zero() {
        tracing::info!("embedded subtitle cache prewarm disabled");
        return;
    }

    tokio::spawn(async move {
        tokio::time::sleep(subtitle_prewarm_initial_delay()).await;
        loop {
            match prewarm_embedded_subtitle_cache(&state, subtitle_prewarm_max_items()).await {
                Ok(0) => tracing::debug!("embedded subtitle cache prewarm found no missing tracks"),
                Ok(count) => tracing::info!(count, "embedded subtitle cache prewarm completed"),
                Err(error) => tracing::warn!("embedded subtitle cache prewarm failed: {error:#}"),
            }
            if let Err(error) = prune_persistent_subtitle_cache().await {
                tracing::warn!("embedded subtitle cache cleanup failed: {error:#}");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

async fn prewarm_embedded_subtitle_cache(
    state: &Arc<AppState>,
    max_items: usize,
) -> anyhow::Result<usize> {
    // `media_items.has_subtitles` is a metadata hint and can be stale for
    // existing libraries. The stream table is authoritative for embedded
    // subtitle tracks, so use it as the prewarm source.
    let candidates = MediaStreams::find()
        .filter(media_streams::Column::StreamType.eq("Subtitle"))
        .filter(media_streams::Column::IsExternal.eq(0_i64))
        .order_by_desc(media_streams::Column::CreatedAt)
        .limit((max_items.saturating_mul(8).max(64)) as u64)
        .all(&state.db)
        .await?;
    let user_id = state.user_id.to_string();
    let mut warmed = 0;

    for stream in candidates {
        if warmed >= max_items {
            break;
        }
        let Some(item) = find_media_item_for_admin(&state.db, &user_id, &stream.item_id).await?
        else {
            continue;
        };
        if !item.is_public || item.is_folder {
            continue;
        }
        let result = cached_embedded_subtitle_window(
            state,
            &item,
            stream.stream_index,
            0,
            subtitle_prewarm_timeout(),
        )
        .await;
        match result {
            Ok(_) => warmed += 1,
            Err(error) => {
                tracing::warn!(
                    item_id = %item.id,
                    index = stream.stream_index,
                    "embedded subtitle prewarm failed: {error:#}"
                );
            }
        }
    }
    Ok(warmed)
}

async fn extract_embedded_subtitle(
    state: &Arc<AppState>,
    item: &MediaItem,
    index: i64,
    start_ticks: Option<i64>,
    extraction_timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    let source = match playback_target_for_item(item)? {
        PlaybackTarget::RemoteUrl(url) => url,
        PlaybackTarget::LocalPath(path) => path.to_string_lossy().into_owned(),
    };
    let mut command = Command::new(&state.sa_config.ffmpeg_path);
    // An HTTP client cancels a seek as soon as it moves again. Ensure its
    // remote FFmpeg extraction is terminated with that request instead of
    // continuing to consume the SmartStrm/Quark connection in the background.
    command.kill_on_drop(true);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin");
    if let Some(start_ticks) = start_ticks {
        command
            .arg("-ss")
            .arg(format!("{:.3}", start_ticks as f64 / 10_000_000.0));
    }
    command.arg("-i").arg(source);
    if start_ticks.is_some() {
        command
            .arg("-t")
            .arg(EMBEDDED_SUBTITLE_WINDOW_SECONDS.to_string());
    }
    command
        .arg("-map")
        .arg(format!("0:{index}"))
        .arg("-c:s")
        .arg("webvtt")
        .arg("-f")
        .arg("webvtt")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = timeout(extraction_timeout, command.output())
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
    Ok(match start_ticks {
        Some(start_ticks) => shift_vtt_timestamps(&output.stdout, start_ticks),
        None => output.stdout,
    })
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
        let cue_text = strip_subtitle_markup(&lines[text_start..index].join("\n"));
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

fn strip_subtitle_markup(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in value.chars() {
        match ch {
            '<' if !in_tag => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                if tag.trim_start().to_ascii_lowercase().starts_with("br") {
                    output.push('\n');
                }
            }
            _ if in_tag => tag.push(ch),
            _ => output.push(ch),
        }
    }
    output
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
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
    let media_source_id = media_source_id.or_else(|| query_media_source_id(&query));
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
        Ok(PlaybackTarget::RemoteUrl(url)) => {
            if remote_stream_proxy_requested(&query) {
                return remote_stream_proxy(
                    &url,
                    &request_headers,
                    method,
                    media_content_type(&item),
                    item.runtime_ticks,
                )
                .await;
            }
            return remote_stream_redirect(&url);
        }
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

fn query_media_source_id(query: &HashMap<String, String>) -> Option<String> {
    query
        .iter()
        .find(|(key, value)| key.eq_ignore_ascii_case("mediaSourceId") && !value.trim().is_empty())
        .map(|(_, value)| value.trim().to_string())
}

fn playback_target_for_item(item: &MediaItem) -> anyhow::Result<PlaybackTarget> {
    let path = std::path::Path::new(&item.path);
    if crate::strm::is_strm_path(path) {
        let target = crate::strm::resolve_strm_path(path)?;
        let target_text = target.to_string_lossy().to_string();
        if crate::strm::is_remote_url(&target_text) {
            return Ok(PlaybackTarget::RemoteUrl(rewrite_public_strm_target(
                &target_text,
            )));
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

/// Proxy a remote STRM target through the server when a client cannot follow
/// the cloud-drive redirect itself.  The default remains a redirect so
/// clients such as VidHub keep the existing direct-link performance.  Jellium
/// opts in with `JellyfinRsProxy=1`, which keeps the signed cloud-drive URL and
/// any IP-bound session on the VPS instead of exposing it to the desktop.
async fn remote_stream_proxy(
    url: &str,
    request_headers: &HeaderMap,
    method: Method,
    content_type: &'static str,
    runtime_ticks: Option<i64>,
) -> Response {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Follow redirects below so the cloud-drive Referer can be
            // re-applied after SmartStrm crosses from its local endpoint to
            // the CDN host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build remote stream proxy client")
    });

    let reqwest_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET);
    let has_client_range = request_headers.contains_key(header::RANGE);
    let mut current_url = url.to_string();
    let mut upstream = None;
    for _ in 0..8 {
        let mut request = client.request(reqwest_method.clone(), &current_url);
        for name in [
            header::RANGE,
            header::IF_RANGE,
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
            header::ACCEPT,
            header::USER_AGENT,
        ] {
            if let Some(value) = request_headers.get(&name) {
                request = request.header(name.as_str(), value.as_bytes());
            }
        }
        // Xunlei's preview endpoint returns an unbounded 200 response when the
        // client omits Range. Desktop players then classify it as a live stream
        // and report a one-second duration. Seed the first GET with an open
        // range so the upstream returns 206 plus the complete Content-Range.
        if method == Method::GET && !has_client_range {
            request = request.header(header::RANGE, "bytes=0-");
        }
        // Re-apply the provider Referer on every redirect. reqwest strips or
        // does not synthesize it when the redirect crosses hosts.
        if let Some(referer) =
            remote_stream_referer(&current_url).or_else(|| remote_stream_referer(url))
        {
            request = request.header(header::REFERER, referer);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(url = %redacted_remote_stream_url(url), "remote STRM proxy request failed: {error}");
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "Error": "remote media proxy request failed" })),
                )
                    .into_response();
            }
        };
        if !response.status().is_redirection() {
            upstream = Some(response);
            break;
        }
        let Some(location) = response.headers().get(header::LOCATION) else {
            upstream = Some(response);
            break;
        };
        let Ok(location) = location.to_str() else {
            upstream = Some(response);
            break;
        };
        let Some(next_url) = response.url().join(location).ok() else {
            upstream = Some(response);
            break;
        };
        current_url = next_url.to_string();
    }
    let Some(upstream) = upstream else {
        tracing::warn!(url = %redacted_remote_stream_url(url), "remote STRM proxy redirect limit exceeded");
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "Error": "remote media proxy redirect limit exceeded" })),
        )
            .into_response();
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for name in [
        header::ACCEPT_RANGES,
        header::CACHE_CONTROL,
        header::CONTENT_DISPOSITION,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::CONTENT_TYPE,
        header::ETAG,
        header::LAST_MODIFIED,
    ] {
        if name == header::CONTENT_TYPE {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        } else if let Some(value) = upstream.headers().get(name.as_str())
            && let Ok(value) = HeaderValue::from_bytes(value.as_bytes())
        {
            headers.insert(name, value);
        }
    }
    if let Some(runtime_ticks) = runtime_ticks.filter(|value| *value > 0) {
        let duration_seconds = runtime_ticks as f64 / 10_000_000.0;
        if let Ok(value) = HeaderValue::from_str(&format!("{duration_seconds:.3}")) {
            // The upstream is an MPEG-TS preview stream and does not carry a
            // fixed duration in its initial packets.  Jellyfin already knows
            // the duration from its media metadata, so expose it using the
            // standard HTTP hint understood by some desktop players.
            headers.insert(
                header::HeaderName::from_static("content-duration"),
                value.clone(),
            );
            headers.insert(header::HeaderName::from_static("x-content-duration"), value);
        }
    }

    if method == Method::HEAD {
        return (status, headers, Body::empty()).into_response();
    }

    (status, headers, Body::from_stream(upstream.bytes_stream())).into_response()
}

fn remote_stream_proxy_requested(query: &HashMap<String, String>) -> bool {
    query.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("JellyfinRsProxy")
            && matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
    })
}

fn remote_stream_referer(url: &str) -> Option<&'static str> {
    let folded = url.to_ascii_lowercase();
    if folded.contains("xunlei") || folded.contains("thunder") {
        Some("http://pan.xunlei.com/")
    } else if folded.contains("quark") || folded.contains("myquark") {
        Some("http://pan.quark.cn/")
    } else {
        None
    }
}

fn redacted_remote_stream_url(url: &str) -> String {
    reqwest::Url::parse(url)
        .map(|mut parsed| {
            if parsed.query().is_some() {
                parsed.set_query(Some("<redacted>"));
            }
            parsed.to_string()
        })
        .unwrap_or_else(|_| "<invalid-url>".to_string())
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
    use std::collections::HashMap;

    use super::{
        PlaybackTarget, normalize_embedded_subtitle_window_start_ticks, parse_range_header,
        playback_target_for_item, query_media_source_id, remote_stream_proxy_requested,
        remote_stream_redirect, remote_stream_referer, shift_vtt_timestamps,
        subtitle_response_payload, vtt_to_track_events,
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
    fn remote_strm_proxy_requires_explicit_opt_in() {
        let mut query = HashMap::new();
        assert!(!remote_stream_proxy_requested(&query));
        query.insert("JellyfinRsProxy".to_string(), "1".to_string());
        assert!(remote_stream_proxy_requested(&query));
        query.insert("jellyfinrsproxy".to_string(), "true".to_string());
        assert!(remote_stream_proxy_requested(&query));
        query.insert("JellyfinRsProxy".to_string(), "0".to_string());
        assert!(remote_stream_proxy_requested(&query));
    }

    #[test]
    fn stream_query_preserves_selected_media_source_id() {
        let mut query = HashMap::from([
            ("JellyfinRsProxy".to_string(), "1".to_string()),
            ("MEDIASOURCEID".to_string(), "version-2".to_string()),
        ]);
        assert_eq!(query_media_source_id(&query).as_deref(), Some("version-2"));

        query.insert("mediaSourceId".to_string(), "  ".to_string());
        assert_eq!(query_media_source_id(&query).as_deref(), Some("version-2"));
    }

    #[test]
    fn remote_strm_proxy_selects_cloud_drive_referer() {
        assert_eq!(
            remote_stream_referer("http://127.0.0.1:8024/smartstrm_fid/myquark/file"),
            Some("http://pan.quark.cn/")
        );
        assert_eq!(
            remote_stream_referer("http://127.0.0.1:8024/smartstrm_fid/xunlei_123/file"),
            Some("http://pan.xunlei.com/")
        );
        assert_eq!(remote_stream_referer("https://example.test/file"), None);
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
        assert_eq!(events[0]["Text"], "hello");
        assert_eq!(events[1]["StartPositionTicks"], 102_400_000);
    }

    #[test]
    fn external_ass_subtitles_are_served_in_their_original_format() {
        let bytes = b"[Script Info]\nTitle: test\n".to_vec();
        let (content_type, response) = subtitle_response_payload("ass", bytes.clone(), true)
            .expect("external ASS should be supported");

        assert_eq!(content_type, "text/x-ssa; charset=utf-8");
        assert_eq!(response, bytes);

        assert!(subtitle_response_payload("ass", b"ass".to_vec(), false).is_none());
        assert!(subtitle_response_payload("ssa", b"ssa".to_vec(), true).is_some());
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

    #[test]
    fn embedded_subtitle_window_uses_a_shared_five_second_bucket() {
        assert_eq!(normalize_embedded_subtitle_window_start_ticks(0), 0);
        assert_eq!(
            normalize_embedded_subtitle_window_start_ticks(49_999_999),
            0
        );
        assert_eq!(
            normalize_embedded_subtitle_window_start_ticks(50_000_000),
            50_000_000
        );
        assert_eq!(
            normalize_embedded_subtitle_window_start_ticks(5_733_660_000),
            5_700_000_000
        );
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
