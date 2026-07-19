use std::{collections::HashMap, io::SeekFrom, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::json;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
};
use tokio_util::io::ReaderStream;

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
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
    Path((item_id, index, _format)): Path<(String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(state, item_id, index, headers, query, Method::GET).await
}

pub async fn stream_subtitle_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, index, _format)): Path<(String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(state, item_id, index, headers, query, Method::HEAD).await
}

/// Subtitle streaming with mediaSourceId path segment for Emby client compatibility.
pub async fn stream_subtitle_with_source(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, _format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // media_source_id is ignored; route to the same handler
    stream_subtitle_item(state, item_id, index, headers, query, Method::GET).await
}

pub async fn stream_subtitle_with_source_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, _format)): Path<(String, String, i64, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(state, item_id, index, headers, query, Method::HEAD).await
}

/// Subtitle streaming with mediaSourceId and start position ticks (Emby compatibility).
pub async fn stream_subtitle_with_ticks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, _start_ticks, _format)): Path<(
        String,
        String,
        i64,
        i64,
        String,
    )>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(state, item_id, index, headers, query, Method::GET).await
}

pub async fn stream_subtitle_with_ticks_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index, _start_ticks, _format)): Path<(
        String,
        String,
        i64,
        i64,
        String,
    )>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_subtitle_item(state, item_id, index, headers, query, Method::HEAD).await
}

async fn stream_subtitle_item(
    state: Arc<AppState>,
    item_id: String,
    index: i64,
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
    match item {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }
    if wants_json_response(&request_headers) {
        return ok_response();
    }

    let path = match crate::jellyfin::routes::subtitle_stream_path(&state.db, &item_id, index).await
    {
        Ok(Some(path)) => path,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    };
    if !readable_media_path(&state.db, &path).await {
        return StatusCode::NOT_FOUND.into_response();
    }
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
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT path FROM library_paths",
            vec![],
        ))
        .await?;
    rows.iter()
        .map(|row| row.get_str("path").map_err(Into::into))
        .collect()
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
    use super::{PlaybackTarget, playback_target_for_item};
    use crate::library::models::MediaItem;

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

    fn media_item(path: &str) -> MediaItem {
        MediaItem {
            id: "item".to_string(),
            title: "Item".to_string(),
            path: path.to_string(),
            library_id: "movies".to_string(),
            collection_type: "movies".to_string(),
            parent_id: "movies".to_string(),
            item_type: "Video".to_string(),
            is_folder: false,
            container: Some("mp4".to_string()),
            overview: None,
            official_rating: None,
            extended_video_type: None,
            production_year: None,
            premiere_date: None,
            runtime_ticks: None,
            size_bytes: None,
            season_number: None,
            episode_number: None,
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
        }
    }
}
