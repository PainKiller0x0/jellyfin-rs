use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        common::{internal_error, strip_nulls},
        item_queries,
    },
    library::{models::media_source_json_with_streams, scanner::scan_media_library},
    playback::streaming::readable_media_path,
    util::{now_unix, stable_text_id},
};

const MAX_SUBTITLE_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_LYRICS_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
pub struct UploadSubtitleRequest {
    #[serde(rename = "Data")]
    data: String,
    #[serde(rename = "Format")]
    format: String,
    #[serde(rename = "Language")]
    language: String,
    #[serde(rename = "IsForced", default)]
    is_forced: bool,
    #[serde(rename = "IsHearingImpaired", default)]
    is_hearing_impaired: bool,
}

#[derive(Deserialize)]
pub struct UploadLyricsQuery {
    #[serde(rename = "fileName", alias = "FileName")]
    file_name: String,
}

pub async fn item_subtitles(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match subtitle_list_inner(&state.db, &item_id).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn upload_subtitle(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Json(request): Json<UploadSubtitleRequest>,
) -> Response {
    match upload_subtitle_inner(&state.db, &item_id, request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("not found") => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error)
            if error.to_string().contains("required")
                || error.to_string().contains("unsupported")
                || error.to_string().contains("too large") =>
        {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn item_lyrics(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match item_lyrics_inner(&state.db, &item_id).await {
        Ok(Some(lyrics)) => Json(lyrics).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Lyrics not found" })),
        )
            .into_response(),
        Err(error) if error.to_string().contains("too large") => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Lyrics file is too large" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn upload_lyrics(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<UploadLyricsQuery>,
    body: Bytes,
) -> Response {
    match upload_lyrics_inner(&state.db, &item_id, &query.file_name, body).await {
        Ok(lyrics) => Json(lyrics).into_response(),
        Err(error) if error.to_string().contains("not found") => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error)
            if error.to_string().contains("required")
                || error.to_string().contains("unsupported")
                || error.to_string().contains("too large")
                || error.to_string().contains("empty") =>
        {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn delete_lyrics(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match delete_lyrics_inner(&state.db, &item_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("not found") => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn remote_lyrics_unavailable() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "Error": "Lyrics not found" })),
    )
        .into_response()
}

async fn item_lyrics_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<Value>> {
    let backend = db.get_database_backend();
    let Some(row) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT title, path, runtime_ticks FROM media_items WHERE id = ? AND item_type = 'Audio' AND is_public = 1",
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(None);
    };

    let title = row.get_str("title")?;
    let path = row.get_str("path")?;
    if !readable_media_path(db, &path).await {
        return Ok(None);
    }
    let runtime_ticks = row.get_opt_i64("runtime_ticks")?;
    let Some((text, is_lrc)) = read_lyric_sidecar(FsPath::new(&path)).await? else {
        return Ok(None);
    };

    Ok(lyrics_value_from_text(&title, runtime_ticks, &text, is_lrc))
}

async fn upload_lyrics_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    file_name: &str,
    body: Bytes,
) -> anyhow::Result<Value> {
    if body.is_empty() {
        anyhow::bail!("Lyrics file is empty");
    }
    if body.len() as u64 > MAX_LYRICS_BYTES {
        anyhow::bail!("Lyrics file is too large");
    }
    let is_lrc = lyric_file_is_lrc(file_name)?;
    let text = std::str::from_utf8(&body)
        .context("Lyrics file must be UTF-8 text")?
        .to_string();
    let Some((title, runtime_ticks, media_path)) = lyric_item_info(db, item_id).await? else {
        anyhow::bail!("item not found");
    };
    let lyric_path = lyric_sidecar_path(&media_path, is_lrc);
    tokio::fs::write(&lyric_path, text.as_bytes())
        .await
        .context("failed to write lyrics file")?;
    lyrics_value_from_text(&title, runtime_ticks, &text, is_lrc)
        .ok_or_else(|| anyhow::anyhow!("Lyrics file is empty"))
}

async fn delete_lyrics_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<()> {
    let Some((_title, _runtime_ticks, media_path)) = lyric_item_info(db, item_id).await? else {
        anyhow::bail!("item not found");
    };
    let mut removed = false;
    for (path, _) in lyric_sidecar_candidates(&media_path) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to delete lyrics file"),
        }
    }
    if removed {
        Ok(())
    } else {
        anyhow::bail!("lyrics not found")
    }
}

async fn lyric_item_info(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<(String, Option<i64>, PathBuf)>> {
    let backend = db.get_database_backend();
    let Some(row) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT title, path, runtime_ticks FROM media_items WHERE id = ? AND item_type = 'Audio'",
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let path = row.get_str("path")?;
    if !readable_media_path(db, &path).await {
        return Ok(None);
    }
    Ok(Some((
        row.get_str("title")?,
        row.get_opt_i64("runtime_ticks")?,
        PathBuf::from(path),
    )))
}

async fn read_lyric_sidecar(media_path: &FsPath) -> anyhow::Result<Option<(String, bool)>> {
    for (path, is_lrc) in lyric_sidecar_candidates(media_path) {
        let Ok(metadata) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_LYRICS_BYTES {
            anyhow::bail!("lyrics file is too large");
        }
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return Ok(None);
        };
        return Ok(Some((text, is_lrc)));
    }
    Ok(None)
}

fn lyric_sidecar_candidates(media_path: &FsPath) -> [(PathBuf, bool); 2] {
    [
        (media_path.with_extension("lrc"), true),
        (media_path.with_extension("txt"), false),
    ]
}

fn lyric_sidecar_path(media_path: &FsPath, is_lrc: bool) -> PathBuf {
    media_path.with_extension(if is_lrc { "lrc" } else { "txt" })
}

fn lyric_file_is_lrc(file_name: &str) -> anyhow::Result<bool> {
    let extension = FsPath::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "lrc" => Ok(true),
        "txt" => Ok(false),
        _ => anyhow::bail!("unsupported lyrics format"),
    }
}

fn lyrics_value_from_text(
    fallback_title: &str,
    runtime_ticks: Option<i64>,
    text: &str,
    is_lrc: bool,
) -> Option<Value> {
    let mut metadata = Map::new();
    let lines = if is_lrc {
        parse_lrc(text, &mut metadata)
    } else {
        parse_plain_lyrics(text)
    };
    if lines.is_empty() {
        return None;
    }

    if !metadata.contains_key("Title") {
        metadata.insert("Title".to_string(), json!(fallback_title));
    }
    if let Some(runtime_ticks) = runtime_ticks.filter(|value| *value > 0) {
        metadata
            .entry("Length".to_string())
            .or_insert_with(|| json!(runtime_ticks));
    }
    let is_synced = lines.iter().any(|line| {
        line.get("Start")
            .and_then(Value::as_i64)
            .is_some_and(|start| start >= 0)
    });
    metadata.insert("IsSynced".to_string(), json!(is_synced));

    Some(crate::jellyfin::common::strip_nulls(json!({
        "Metadata": Value::Object(metadata),
        "Lyrics": lines,
    })))
}

fn parse_plain_lyrics(text: &str) -> Vec<Value> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| json!({ "Text": line }))
        .collect()
}

fn parse_lrc(text: &str, metadata: &mut Map<String, Value>) -> Vec<Value> {
    let mut timed = Vec::new();
    let mut plain = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || parse_lrc_metadata(line, metadata) {
            continue;
        }
        let (starts, lyric_text) = parse_lrc_line(line);
        if starts.is_empty() {
            if !line.starts_with('[') {
                plain.push(json!({ "Text": line }));
            }
            continue;
        }
        for start in starts {
            timed.push((start, lyric_text.clone()));
        }
    }

    if timed.is_empty() {
        return plain;
    }
    timed.sort_by_key(|(start, _)| *start);
    timed
        .into_iter()
        .map(|(start, text)| json!({ "Text": text, "Start": start }))
        .collect()
}

fn parse_lrc_metadata(line: &str, metadata: &mut Map<String, Value>) -> bool {
    if !line.starts_with('[') || !line.ends_with(']') {
        return false;
    }
    let Some((key, value)) = line[1..line.len() - 1].split_once(':') else {
        return true;
    };
    if parse_lrc_timestamp(line[1..line.len() - 1].trim()).is_some() {
        return false;
    }
    let value = value.trim();
    match key.trim().to_ascii_lowercase().as_str() {
        "ar" => metadata.insert("Artist".to_string(), json!(value)),
        "al" => metadata.insert("Album".to_string(), json!(value)),
        "ti" => metadata.insert("Title".to_string(), json!(value)),
        "au" => metadata.insert("Author".to_string(), json!(value)),
        "by" => metadata.insert("By".to_string(), json!(value)),
        "re" => metadata.insert("Creator".to_string(), json!(value)),
        "ve" => metadata.insert("Version".to_string(), json!(value)),
        "offset" => value
            .parse::<i64>()
            .ok()
            .map(|offset_ms| metadata.insert("Offset".to_string(), json!(offset_ms * 10_000)))
            .flatten(),
        "length" => parse_lrc_length(value)
            .map(|ticks| metadata.insert("Length".to_string(), json!(ticks)))
            .flatten(),
        _ => None,
    };
    true
}

fn parse_lrc_line(line: &str) -> (Vec<i64>, String) {
    let mut starts = Vec::new();
    let mut rest = line;
    while let Some(after_open) = rest.strip_prefix('[') {
        let Some(end) = after_open.find(']') else {
            break;
        };
        let token = &after_open[..end];
        let Some(start) = parse_lrc_timestamp(token) else {
            break;
        };
        starts.push(start);
        rest = &after_open[end + 1..];
    }
    (starts, rest.trim_start().to_string())
}

fn parse_lrc_length(value: &str) -> Option<i64> {
    parse_lrc_timestamp(value).or_else(|| {
        value
            .trim()
            .parse::<i64>()
            .ok()
            .map(|seconds| seconds * 10_000_000)
    })
}

fn parse_lrc_timestamp(token: &str) -> Option<i64> {
    let parts = token.trim().split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds_part) = match parts.as_slice() {
        [minutes, seconds] => (0_i64, minutes.parse::<i64>().ok()?, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<i64>().ok()?,
            minutes.parse::<i64>().ok()?,
            *seconds,
        ),
        _ => return None,
    };
    let (seconds, fraction) = seconds_part
        .split_once('.')
        .map(|(seconds, fraction)| (seconds, fraction))
        .unwrap_or((seconds_part, ""));
    let seconds = seconds.parse::<i64>().ok()?;
    if hours < 0 || minutes < 0 || seconds < 0 || seconds >= 60 {
        return None;
    }
    let fraction = fraction
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(3)
        .collect::<String>();
    let milliseconds = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction.parse::<i64>().ok()?,
    };
    Some(((hours * 3600 + minutes * 60 + seconds) * 1000 + milliseconds) * 10_000)
}

async fn upload_subtitle_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    request: UploadSubtitleRequest,
) -> anyhow::Result<()> {
    let format = subtitle_format(&request.format)?;
    let language = request.language.trim();
    if language.is_empty() {
        anyhow::bail!("Language is required");
    }
    let bytes = general_purpose::STANDARD
        .decode(request.data.trim())
        .context("Data is not valid base64")?;
    if bytes.is_empty() {
        anyhow::bail!("Data is required");
    }
    if bytes.len() > MAX_SUBTITLE_UPLOAD_BYTES {
        anyhow::bail!("Data is too large");
    }

    let backend = db.get_database_backend();
    let Some(item) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT path FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await?
    else {
        anyhow::bail!("item not found");
    };
    let media_path = item.get_str("path")?;
    if !readable_media_path(db, &media_path).await {
        anyhow::bail!("item not found");
    }
    let media_path = FsPath::new(&media_path);
    let directory = media_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("item path not found"))?;
    let stem = media_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(item_id);

    let next_index = next_subtitle_index(db, item_id).await?;
    let suffix = subtitle_suffix(language, request.is_forced, request.is_hearing_impaired);
    let subtitle_path = directory.join(format!("{stem}.{suffix}.{next_index}.{format}"));
    tokio::fs::write(&subtitle_path, bytes)
        .await
        .context("failed to write subtitle file")?;

    let now = now_unix();
    let title = subtitle_title(language, request.is_forced, request.is_hearing_impaired);
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, path, is_external, created_at) VALUES (?, ?, ?, 'Subtitle', ?, ?, ?, ?, 1, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = 'Subtitle', codec = excluded.codec, language = excluded.language, title = excluded.title, path = excluded.path, is_external = 1"#,
        vec![
            stable_text_id(&format!("stream:{item_id}:{next_index}")).into(),
            item_id.into(),
            next_index.into(),
            format.into(),
            language.into(),
            title.into(),
            subtitle_path.to_string_lossy().to_string().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn next_subtitle_index(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<i64> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT COALESCE(MAX(stream_index), -1) AS max_index FROM media_streams WHERE item_id = ?",
            vec![item_id.into()],
        ))
        .await?;
    Ok(row
        .and_then(|row| row.get_i64("max_index").ok())
        .unwrap_or(-1)
        + 1)
}

fn subtitle_format(format: &str) -> anyhow::Result<&'static str> {
    match format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "srt" => Ok("srt"),
        "vtt" | "webvtt" => Ok("vtt"),
        "ass" => Ok("ass"),
        "ssa" => Ok("ssa"),
        _ => anyhow::bail!("unsupported subtitle format"),
    }
}

fn subtitle_suffix(language: &str, is_forced: bool, is_hearing_impaired: bool) -> String {
    let mut suffix = sanitize_subtitle_part(language);
    if is_forced {
        suffix.push_str(".forced");
    }
    if is_hearing_impaired {
        suffix.push_str(".sdh");
    }
    suffix
}

fn subtitle_title(language: &str, is_forced: bool, is_hearing_impaired: bool) -> String {
    let mut title = language.to_string();
    if is_forced {
        title.push_str(" Forced");
    }
    if is_hearing_impaired {
        title.push_str(" SDH");
    }
    title
}

fn sanitize_subtitle_part(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect::<String>();
    if sanitized.is_empty() {
        "und".to_string()
    } else {
        sanitized
    }
}

async fn subtitle_list_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT ms.stream_index, ms.codec, ms.language, ms.title, ms.is_external FROM media_streams ms JOIN media_items mi ON mi.id = ms.item_id WHERE ms.item_id = ? AND ms.stream_type = 'Subtitle' AND mi.is_public = 1 ORDER BY ms.stream_index ASC",
            vec![item_id.into()],
        ))
        .await
        .context("failed to list subtitles")?;

    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Index": row.get_i64("stream_index")?,
                "Codec": row.get_opt_str("codec")?,
                "Language": row.get_opt_str("language")?,
                "DisplayTitle": row.get_opt_str("title")?,
                "IsExternal": row.get_i64("is_external").unwrap_or_default() != 0,
            }))
        })
        .collect()
}

pub async fn metadata_reset(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let item_ids: Vec<&str> = body
        .get("Ids")
        .and_then(Value::as_str)
        .map(|ids| {
            ids.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if item_ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let now = now_unix();
    let backend = state.db.get_database_backend();
    for item_id in &item_ids {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET overview = NULL, production_year = NULL, updated_at = ? WHERE id = ?",
                vec![now.into(), (*item_id).into()],
            ))
            .await;

        for table in [
            "media_people",
            "media_genres",
            "media_tags",
            "media_studios",
            "provider_ids",
        ] {
            let _ = state
                .db
                .execute(crate::db::helpers::portable_statement(
                    backend,
                    &format!("DELETE FROM {table} WHERE item_id = ?"),
                    vec![(*item_id).into()],
                ))
                .await;
        }

        crate::jellyfin::system::log_activity(
            &state,
            "Metadata reset",
            "MetadataReset",
            None,
            Some(item_id),
        )
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn item_counts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match item_counts_inner(&state.db, &query).await {
        Ok(counts) => Json(counts).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_counts_inner(
    db: &sea_orm::DatabaseConnection,
    query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let user_id = query
        .get("userId")
        .or_else(|| query.get("UserId"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let favorite_only = query
        .get("isFavorite")
        .or_else(|| query.get("IsFavorite"))
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let mut values = Vec::new();
    let mut join = String::new();
    let mut filters = vec!["mi.is_folder = 0".to_string()];

    if let Some(user_id) = user_id.as_deref() {
        join.push_str(" JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ?");
        values.push(user_id.into());
        if favorite_only {
            filters.push("ud.is_favorite = 1".to_string());
        }
    }

    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                "SELECT mi.item_type, COUNT(*) AS count FROM media_items mi{join} WHERE {} GROUP BY mi.item_type",
                filters.join(" AND ")
            ),
            values,
        ))
        .await
        .context("failed to count items")?;

    let mut movie_count = 0;
    let mut series_count = 0;
    let mut episode_count = 0;
    let mut trailer_count = 0;
    let mut song_count = 0;
    let mut album_count = 0;
    let mut music_video_count = 0;
    let mut box_set_count = 0;
    let mut book_count = 0;
    let mut item_count = 0;

    for row in &rows {
        let item_type: String = row.get_str("item_type")?;
        let count: i64 = row.get_i64("count")?;
        item_count += count;
        match item_type.as_str() {
            "Movie" => movie_count += count,
            "Series" => series_count += count,
            "Episode" => episode_count += count,
            "Trailer" => trailer_count += count,
            "Audio" => song_count += count,
            "MusicAlbum" => album_count += count,
            "MusicVideo" => music_video_count += count,
            "BoxSet" | "CollectionFolder" => box_set_count += count,
            "Book" => book_count += count,
            _ => {}
        }
    }

    let artist_count = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT COUNT(DISTINCT person_id) AS count FROM media_people WHERE LOWER(person_type) IN ('artist', 'albumartist')",
            vec![],
        ))
        .await
        .context("failed to count artists")?
        .first()
        .and_then(|row| row.get_i64("count").ok())
        .unwrap_or_default();

    Ok(json!({
        "MovieCount": movie_count,
        "SeriesCount": series_count,
        "EpisodeCount": episode_count,
        "ArtistCount": artist_count,
        "ProgramCount": 0,
        "TrailerCount": trailer_count,
        "SongCount": song_count,
        "AlbumCount": album_count,
        "MusicVideoCount": music_video_count,
        "BoxSetCount": box_set_count,
        "BookCount": book_count,
        "ItemCount": item_count
    }))
}

pub async fn scan_handler(State(state): State<Arc<AppState>>) -> Response {
    tokio::spawn(async move {
        let start = now_unix();
        let result = scan_media_library(&state).await;
        let end = now_unix();
        let (status, message) = match &result {
            Ok(count) => ("Completed", Some(format!("Scanned {count} items"))),
            Err(error) => ("Failed", Some(format!("{error:#}"))),
        };
        crate::jellyfin::system::upsert_task_result(
            &state,
            "scan-library",
            status,
            start,
            end,
            message.as_deref(),
        )
        .await;
        crate::jellyfin::system::log_activity(&state, "Library scan", "LibraryScan", None, None)
            .await;
    });
    Json(json!({ "Scanning": true })).into_response()
}

pub async fn external_id_infos() -> Response {
    Json(external_id_infos_value()).into_response()
}

pub async fn metadata_editor_info(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match metadata_editor_info_inner(&state.db, &item_id).await {
        Ok(Some(info)) => Json(info).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

fn external_id_infos_value() -> Value {
    json!([
        {
            "Name": "TheMovieDb",
            "Key": "Tmdb",
            "Website": "https://www.themoviedb.org/",
            "UrlFormatString": "https://www.themoviedb.org/movie/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "TheTVDB",
            "Key": "Tvdb",
            "Website": "https://thetvdb.com/",
            "UrlFormatString": "https://thetvdb.com/?id={0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "IMDb",
            "Key": "IMDB",
            "Website": "https://www.imdb.com/",
            "UrlFormatString": "https://www.imdb.com/title/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album",
            "Key": "MusicBrainzAlbum",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album Artist",
            "Key": "MusicBrainzAlbumArtist",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/artist/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Release Group",
            "Key": "MusicBrainzReleaseGroup",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release-group/{0}",
            "IsSupportedAsIdentifier": true
        }
    ])
}

async fn metadata_editor_info_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<Value>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT media_items.item_type, libraries.collection_type
               FROM media_items
               LEFT JOIN libraries ON libraries.id = media_items.library_id
               WHERE media_items.id = ?"#,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to load metadata editor info: {item_id}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let content_type = row
        .get_opt_str("collection_type")?
        .filter(|value| !value.is_empty())
        .or_else(|| content_type_for_item_type(&row.get_str("item_type").unwrap_or_default()));
    Ok(Some(json!({
        "ParentalRatingOptions": [],
        "Countries": [],
        "Cultures": [],
        "ExternalIdInfos": external_id_infos_value(),
        "ContentType": content_type,
        "ContentTypeOptions": content_type_options(),
    })))
}

fn content_type_for_item_type(item_type: &str) -> Option<String> {
    match item_type {
        "Movie" => Some("movies".to_string()),
        "Series" | "Season" | "Episode" => Some("tvshows".to_string()),
        "Audio" | "MusicAlbum" | "MusicArtist" => Some("music".to_string()),
        "MusicVideo" => Some("musicvideos".to_string()),
        "Trailer" => Some("trailers".to_string()),
        "BoxSet" => Some("boxsets".to_string()),
        "Book" => Some("books".to_string()),
        "Photo" => Some("photos".to_string()),
        "Playlist" => Some("playlists".to_string()),
        "Folder" => Some("folders".to_string()),
        _ => None,
    }
}

fn content_type_options() -> Value {
    json!([
        { "Name": "Movies", "Value": "movies" },
        { "Name": "TV Shows", "Value": "tvshows" },
        { "Name": "Music", "Value": "music" },
        { "Name": "Music Videos", "Value": "musicvideos" },
        { "Name": "Trailers", "Value": "trailers" },
        { "Name": "Home Videos", "Value": "homevideos" },
        { "Name": "Box Sets", "Value": "boxsets" },
        { "Name": "Books", "Value": "books" },
        { "Name": "Photos", "Value": "photos" },
        { "Name": "Live TV", "Value": "livetv" },
        { "Name": "Playlists", "Value": "playlists" },
        { "Name": "Folders", "Value": "folders" },
    ])
}

/// POST /Videos/MergeVersions — merge multiple video items into one multi-version item
pub async fn merge_versions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let Some(ids) = body.get("Ids").and_then(Value::as_array) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Ids is required" })),
        )
            .into_response();
    };
    let item_ids: Vec<String> = ids
        .iter()
        .filter_map(|v| v.as_str().map(ToString::to_string))
        .collect();
    if item_ids.len() < 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Need at least 2 items to merge" })),
        )
            .into_response();
    }

    let backend = state.db.get_database_backend();
    // Find the parent of the first item — this becomes the target parent
    let first_id = &item_ids[0];
    let parent_row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT parent_id, item_type FROM media_items WHERE id = ?",
            vec![first_id.as_str().into()],
        ))
        .await;

    let (parent_id, _item_type) = match parent_row {
        Ok(Some(r)) => {
            let pid = r.get_str("parent_id").unwrap_or_default();
            let it = r.get_str("item_type").unwrap_or_default();
            (pid, it)
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    // If the first item doesn't have a parent (it's a top-level item), create a folder for it
    let target_parent = if parent_id.is_empty() || parent_id == *first_id {
        // The first item IS the folder — move others into it
        first_id.clone()
    } else {
        parent_id
    };

    // Move all other items to be children of the target parent
    let now = crate::util::now_unix();
    for id in &item_ids[1..] {
        let _ = state.db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET parent_id = ?, updated_at = ? WHERE id = ? AND item_type = 'Video'",
            vec![target_parent.clone().into(), now.into(), id.as_str().into()],
        )).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// GET /Videos/ActiveEncodings — list active transcodings (stub)
pub async fn active_encodings() -> Response {
    Json(json!([])).into_response()
}

/// DELETE /Videos/ActiveEncodings — stop all encodings (stub)
pub async fn stop_encodings() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "Error": "Active encodings are not available" })),
    )
        .into_response()
}

/// GET /Videos/{id}/AlternateSources — alternate video sources (stub)
pub async fn alternate_sources(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match alternate_sources_inner(&state.db, &item_id).await {
        Ok(Some(sources)) => Json(sources).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// DELETE /Videos/{id}/AlternateSources — delete alternate source (stub)
pub async fn delete_alternate_source(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match delete_alternate_sources_inner(&state.db, &item_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /AudioBooks/NextUp — audiobooks next up (stub)
pub async fn audiobooks_next_up(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id =
        query_value(&query, &["UserId", "userId"]).unwrap_or_else(|| state.user_id.to_string());
    let limit = query_value(&query, &["Limit", "limit"])
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(25)
        .min(200);
    let start_index = query_value(&query, &["StartIndex", "startIndex"])
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    match audiobooks_next_up_inner(&state.db, &user_id).await {
        Ok(items) => {
            let total = items.len();
            let page = items
                .into_iter()
                .skip(start_index)
                .take(limit)
                .map(|item| strip_nulls(item.to_jellyfin_json()))
                .collect::<Vec<_>>();
            Json(json!({
                "Items": page,
                "TotalRecordCount": total,
                "StartIndex": start_index
            }))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// GET /LiveTv/AvailableRecordingOptions — recording options (stub)
pub async fn available_recording_options() -> Response {
    Json(available_recording_options_value()).into_response()
}

/// GET /Providers/Subtitles/Subtitles/{id} — subtitle provider (stub)
pub async fn subtitle_provider_info() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "Error": "Subtitle not found" })),
    )
        .into_response()
}

fn available_recording_options_value() -> Value {
    json!({
        "CanRecord": false,
        "CanRecordSeries": false,
        "CanCancel": false,
        "CanDelete": false,
        "SupportsPadding": false,
        "SupportsPrePadding": false,
        "SupportsPostPadding": false,
        "PrePaddingSeconds": 0,
        "PostPaddingSeconds": 0,
        "RecordingFolders": [],
        "MovieRecordingFolders": [],
        "SeriesRecordingFolders": [],
        "Defaults": {
            "PrePaddingSeconds": 0,
            "PostPaddingSeconds": 0,
            "Priority": 0,
            "RecordAnyChannel": false,
            "RecordAnyTime": false,
            "RecordNewOnly": false,
            "SkipEpisodesInLibrary": false
        }
    })
}

async fn alternate_sources_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<Vec<Value>>> {
    let Some(item) = item_queries::find_media_item_for_admin(db, "", item_id).await? else {
        return Ok(None);
    };
    let parent_id = if item.parent_id.is_empty() {
        item.id.as_str()
    } else {
        item.parent_id.as_str()
    };
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &item_queries::media_item_select_sql(
                "WHERE media_items.parent_id = ? AND media_items.id <> ? AND media_items.item_type = 'Video' ORDER BY media_items.title ASC",
            ),
            vec!["".into(), parent_id.into(), item.id.as_str().into()],
        ))
        .await?;
    let items = item_queries::decode_media_items_for_admin(&rows)?;
    let mut sources = Vec::with_capacity(items.len());
    for item in items {
        let streams = crate::jellyfin::playback::media_streams_for_item(db, &item.id)
            .await
            .unwrap_or_default();
        sources.push(media_source_json_with_streams(&item, streams));
    }
    Ok(Some(sources))
}

async fn delete_alternate_sources_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<bool> {
    let Some(item) = item_queries::find_media_item_for_admin(db, "", item_id).await? else {
        return Ok(false);
    };
    if item.parent_id.is_empty() {
        return Ok(true);
    }
    db.execute(crate::db::helpers::portable_statement(
        db.get_database_backend(),
        "UPDATE media_items SET parent_id = '', updated_at = ? WHERE parent_id = ? AND id <> ? AND item_type = 'Video'",
        vec![now_unix().into(), item.parent_id.into(), item.id.into()],
    ))
    .await?;
    Ok(true)
}

async fn audiobooks_next_up_inner(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<crate::library::models::MediaItem>> {
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &item_queries::media_item_select_sql(
                "WHERE media_items.item_type = 'Audio' AND media_items.is_folder = 0 AND media_items.is_public = 1 AND COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC",
            ),
            vec![user_id.into()],
        ))
        .await?;
    item_queries::decode_media_items(&rows)
}

fn query_value(query: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    query
        .iter()
        .find(|(key, _)| keys.iter().any(|wanted| key.eq_ignore_ascii_case(wanted)))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        UploadSubtitleRequest, alternate_sources_inner, audiobooks_next_up,
        audiobooks_next_up_inner, available_recording_options, available_recording_options_value,
        delete_alternate_sources_inner, delete_lyrics_inner, item_counts_inner, item_lyrics_inner,
        lyrics_value_from_text, metadata_editor_info_inner, parse_lrc_timestamp, stop_encodings,
        subtitle_format, subtitle_list_inner, subtitle_provider_info, subtitle_suffix,
        upload_lyrics_inner, upload_subtitle_inner,
    };
    use axum::body::{Bytes, to_bytes};
    use axum::extract::{Query, State};
    use axum::response::IntoResponse;
    use base64::{Engine as _, engine::general_purpose};
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[tokio::test]
    async fn lyrics_report_missing() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        assert_eq!(item_lyrics_inner(&db, "missing").await.unwrap(), None);
    }

    #[test]
    fn lrc_parser_returns_synced_jellyfin_shape() {
        let value = lyrics_value_from_text(
            "Fallback",
            Some(30_000_000),
            "[ar:Artist]\n[ti:Song]\n[00:01.50][00:02.00]First\n[00:03]Second",
            true,
        )
        .unwrap();
        assert_eq!(value["Metadata"]["Artist"], "Artist");
        assert_eq!(value["Metadata"]["Title"], "Song");
        assert_eq!(value["Metadata"]["IsSynced"], true);
        assert_eq!(value["Metadata"]["Length"], 30_000_000);
        assert_eq!(
            value["Lyrics"][0],
            json!({ "Text": "First", "Start": 15_000_000 })
        );
        assert_eq!(
            value["Lyrics"][1],
            json!({ "Text": "First", "Start": 20_000_000 })
        );
        assert_eq!(
            value["Lyrics"][2],
            json!({ "Text": "Second", "Start": 30_000_000 })
        );
    }

    #[test]
    fn plain_lyrics_are_unsynced_lines() {
        let value =
            lyrics_value_from_text("Track", None, "first line\n\nsecond line\n", false).unwrap();
        assert_eq!(value["Metadata"]["Title"], "Track");
        assert_eq!(value["Metadata"]["IsSynced"], false);
        assert_eq!(value["Lyrics"][0], json!({ "Text": "first line" }));
        assert_eq!(value["Lyrics"][1], json!({ "Text": "second line" }));
    }

    #[test]
    fn lrc_timestamp_parses_centiseconds_and_hours() {
        assert_eq!(parse_lrc_timestamp("01:02.34"), Some(623_400_000));
        assert_eq!(parse_lrc_timestamp("1:02:03.004"), Some(37_230_040_000));
        assert_eq!(parse_lrc_timestamp("bad"), None);
    }

    #[tokio::test]
    async fn lyrics_are_loaded_from_sidecar_file() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-lyrics-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("song.mp3");
        let lyric_path = dir.join("song.lrc");
        std::fs::write(&audio_path, b"audio").unwrap();
        std::fs::write(&lyric_path, "[00:01.00]Hello").unwrap();

        insert_audio_item(&db, "song", "Song", &audio_path, true).await;
        insert_library_path(&db, &dir).await;

        let value = item_lyrics_inner(&db, "song").await.unwrap().unwrap();
        assert_eq!(
            value["Lyrics"][0],
            json!({ "Text": "Hello", "Start": 10_000_000 })
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn lyrics_require_item_path_inside_library_root() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-lyrics-outside-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("song.mp3");
        let lyric_path = dir.join("song.lrc");
        std::fs::write(&audio_path, b"audio").unwrap();
        std::fs::write(&lyric_path, "[00:01.00]Hidden").unwrap();
        insert_audio_item(&db, "song", "Song", &audio_path, true).await;

        assert_eq!(item_lyrics_inner(&db, "song").await.unwrap(), None);
        let upload = upload_lyrics_inner(&db, "song", "song.lrc", Bytes::from_static(b"New")).await;
        assert!(upload.is_err());
        assert_eq!(
            std::fs::read_to_string(&lyric_path).unwrap(),
            "[00:01.00]Hidden"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn lyrics_hide_private_audio_items() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-private-lyrics-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("song.mp3");
        let lyric_path = dir.join("song.lrc");
        std::fs::write(&audio_path, b"audio").unwrap();
        std::fs::write(&lyric_path, "[00:01.00]Hidden").unwrap();
        insert_audio_item(&db, "song", "Song", &audio_path, false).await;

        assert_eq!(item_lyrics_inner(&db, "song").await.unwrap(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn lyrics_upload_and_delete_sidecar_file() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-lyrics-upload-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("song.mp3");
        std::fs::write(&audio_path, b"audio").unwrap();
        insert_audio_item(&db, "song", "Song", &audio_path, true).await;
        insert_library_path(&db, &dir).await;

        let value = upload_lyrics_inner(
            &db,
            "song",
            "../ignored.lrc",
            Bytes::from_static(b"[00:02.00]Hi"),
        )
        .await
        .unwrap();
        assert_eq!(
            value["Lyrics"][0],
            json!({ "Text": "Hi", "Start": 20_000_000 })
        );
        assert!(dir.join("song.lrc").exists());
        assert!(!dir.join("ignored.lrc").exists());

        delete_lyrics_inner(&db, "song").await.unwrap();
        assert!(!dir.join("song.lrc").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn subtitle_upload_requires_item_path_inside_library_root() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-subtitle-upload-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("song.mp3");
        std::fs::write(&audio_path, b"audio").unwrap();
        insert_audio_item(&db, "song", "Song", &audio_path, true).await;

        let result = upload_subtitle_inner(
            &db,
            "song",
            UploadSubtitleRequest {
                data: general_purpose::STANDARD.encode(b"1\n00:00:00,000 --> 00:00:01,000\nHi"),
                format: "srt".to_string(),
                language: "eng".to_string(),
                is_forced: false,
                is_hearing_impaired: false,
            },
        )
        .await;

        assert!(result.is_err());
        assert!(!dir.join("song.eng.0.srt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    async fn insert_audio_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &std::path::Path,
        is_public: bool,
    ) {
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT OR IGNORE INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["music".into(), "Music".into(), "music".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, '', 'Audio', 0, ?, 1, 1, 1)",
            vec![
                id.into(),
                title.into(),
                path.to_string_lossy().to_string().into(),
                "music".into(),
                i64::from(is_public).into(),
            ],
        ))
        .await
        .unwrap();
    }

    async fn insert_library_path(db: &DatabaseConnection, path: &std::path::Path) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, 'music', ?, 1)",
            vec![
                crate::util::stable_text_id(&format!(
                    "library-path:{}",
                    path.to_string_lossy()
                ))
                .into(),
                path.to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();
    }

    #[test]
    fn subtitle_format_is_whitelisted() {
        assert_eq!(subtitle_format("srt").unwrap(), "srt");
        assert_eq!(subtitle_format(".webvtt").unwrap(), "vtt");
        assert!(subtitle_format("../exe").is_err());
    }

    #[test]
    fn subtitle_suffix_sanitizes_language() {
        assert_eq!(subtitle_suffix("../en", true, true), "en.forced.sdh");
        assert_eq!(subtitle_suffix("中文", false, false), "und");
    }

    #[tokio::test]
    async fn subtitle_list_hides_private_items() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        insert_audio_item(
            &db,
            "song",
            "Song",
            &std::env::temp_dir().join("private-song.mp3"),
            false,
        )
        .await;
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, is_external, created_at) VALUES ('s1', 'song', 2, 'Subtitle', 'srt', 'eng', 'English', 0, 1)",
            vec![],
        ))
        .await
        .unwrap();

        assert!(subtitle_list_inner(&db, "song").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn subtitle_provider_reports_missing() {
        assert_eq!(
            subtitle_provider_info().await.into_response().status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn unavailable_video_management_reports_not_implemented() {
        assert_eq!(
            stop_encodings().await.into_response().status(),
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
    }

    #[tokio::test]
    async fn available_recording_options_report_disabled_shape() {
        assert_eq!(
            available_recording_options().await.into_response().status(),
            axum::http::StatusCode::OK
        );
        let options = available_recording_options_value();
        assert_eq!(options["CanRecord"], false);
        assert_eq!(options["CanRecordSeries"], false);
        assert_eq!(options["RecordingFolders"], json!([]));
        assert_eq!(options["Defaults"]["PrePaddingSeconds"], 0);
    }

    #[tokio::test]
    async fn alternate_sources_return_and_remove_video_versions() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        for (id, title, parent_id) in [
            ("parent", "Movie", ""),
            ("v1", "1080p", "parent"),
            ("v2", "720p", "parent"),
        ] {
            db.execute(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, 'Video', 0, 'mkv', 1, 1, 1)",
                vec![id.into(), title.into(), format!("/tmp/{id}.mkv").into(), parent_id.into()],
            ))
            .await
            .unwrap();
        }

        let sources = alternate_sources_inner(&db, "v1").await.unwrap().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["Id"], "v2");
        assert_eq!(sources[0]["DirectStreamUrl"], "/Videos/v2/stream.mkv");

        assert!(delete_alternate_sources_inner(&db, "v1").await.unwrap());
        let sources = alternate_sources_inner(&db, "v1").await.unwrap().unwrap();
        assert!(sources.is_empty());
    }

    #[tokio::test]
    async fn audiobooks_next_up_uses_audio_resume_progress() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec!["u1".into(), "alice".into(), "Alice".into()],
        ))
        .await
        .unwrap();
        insert_audio_item(&db, "a1", "Chapter 1", &dir.join("a1.mp3"), true).await;
        insert_audio_item(&db, "a2", "Chapter 2", &dir.join("a2.mp3"), true).await;
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, updated_at) VALUES (?, ?, 0, 42, 0, 10)",
            vec!["u1".into(), "a1".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, updated_at) VALUES (?, ?, 1, 100, 1, 20)",
            vec!["u1".into(), "a2".into()],
        ))
        .await
        .unwrap();

        let items = audiobooks_next_up_inner(&db, "u1").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "a1");
        assert!(
            audiobooks_next_up_inner(&db, "u2")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn audiobooks_next_up_returns_query_result_page() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir();
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec!["u1".into(), "alice".into(), "Alice".into()],
        ))
        .await
        .unwrap();
        insert_audio_item(&db, "a1", "Chapter 1", &dir.join("a1.mp3"), true).await;
        insert_audio_item(&db, "a2", "Chapter 2", &dir.join("a2.mp3"), true).await;
        insert_audio_item(&db, "private", "Private", &dir.join("private.mp3"), false).await;
        for (id, updated_at) in [("a1", 10_i64), ("a2", 20), ("private", 30)] {
            db.execute(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "INSERT INTO user_data (user_id, item_id, played, playback_position_ticks, play_count, updated_at) VALUES (?, ?, 0, 42, 0, ?)",
                vec!["u1".into(), id.into(), updated_at.into()],
            ))
            .await
            .unwrap();
        }

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("UserId".to_string(), "u1".to_string());
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = audiobooks_next_up(State(state), Query(query))
            .await
            .into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["TotalRecordCount"], 2);
        assert_eq!(value["StartIndex"], 1);
        let items = value["Items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["Id"], "a1");
    }

    #[tokio::test]
    async fn metadata_editor_info_has_jellyfin_shape() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, 1, 1)",
            vec!["movies".into(), "Movies".into(), "movies".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, '', 'Movie', 1, 1, 1, 1)",
            vec!["i1".into(), "Movie".into(), "D:/movie".into(), "movies".into()],
        ))
        .await
        .unwrap();

        let info = metadata_editor_info_inner(&db, "i1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info["ContentType"], "movies");
        assert!(info["ContentTypeOptions"].as_array().unwrap().len() >= 3);
        assert!(info["ExternalIdInfos"].as_array().unwrap().len() >= 3);
        assert!(
            metadata_editor_info_inner(&db, "missing")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn item_counts_have_jellyfin_shape_and_user_filters() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let backend = db.get_database_backend();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO users (id, username, display_name, is_admin, is_disabled, created_at, updated_at) VALUES (?, ?, ?, 0, 0, 1, 1)",
            vec!["u1".into(), "alice".into(), "Alice".into()],
        ))
        .await
        .unwrap();
        for (id, title, item_type) in [
            ("m1", "Movie", "Movie"),
            ("s1", "Series", "Series"),
            ("e1", "Episode", "Episode"),
            ("a1", "Song", "Audio"),
        ] {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', ?, 0, 1, 1, 1)",
                vec![
                    id.into(),
                    title.into(),
                    format!("D:/{id}").into(),
                    item_type.into(),
                ],
            ))
            .await
            .unwrap();
        }
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO user_data (user_id, item_id, is_favorite, played, playback_position_ticks, play_count, updated_at) VALUES (?, ?, 1, 0, 0, 0, 1)",
            vec!["u1".into(), "m1".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO people (id, name, created_at) VALUES (?, ?, 1)",
            vec!["p1".into(), "Artist".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_people (item_id, person_id, person_type, sort_order) VALUES (?, ?, 'Artist', 0)",
            vec!["a1".into(), "p1".into()],
        ))
        .await
        .unwrap();

        let counts = item_counts_inner(&db, &HashMap::new()).await.unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["SeriesCount"], 1);
        assert_eq!(counts["EpisodeCount"], 1);
        assert_eq!(counts["SongCount"], 1);
        assert_eq!(counts["ArtistCount"], 1);
        assert_eq!(counts["ItemCount"], 4);

        let mut query = HashMap::new();
        query.insert("userId".to_string(), "u1".to_string());
        query.insert("isFavorite".to_string(), "true".to_string());
        let counts = item_counts_inner(&db, &query).await.unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["ItemCount"], 1);
    }

    fn test_state(db: DatabaseConnection) -> crate::app::state::AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        crate::app::state::AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"test"),
            access_token: "test-token".to_string(),
            db,
            media_dirs: Vec::new(),
            http_client: reqwest::Client::new(),
            tmdb_api_key: RwLock::new(None),
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
