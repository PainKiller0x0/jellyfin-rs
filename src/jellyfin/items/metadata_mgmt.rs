use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use axum::{
    Json,
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    sea_query::OnConflict,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        provider_ids::{self, Entity as ProviderIds},
    },
    jellyfin::{
        auth::query_user_id_or_request,
        common::{internal_error, strip_nulls},
        item_queries,
    },
    library::{models::media_source_json_with_streams, scanner::scan_media_library},
    playback::streaming::readable_media_path,
    util::{now_unix, stable_text_id},
};

const MAX_SUBTITLE_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_LYRICS_BYTES: u64 = 1024 * 1024;
const MAX_METADATA_WRITE_IDS: usize = 1000;
const MAX_METADATA_WRITE_ID_LEN: usize = 256;
const MAX_MERGE_VERSION_IDS: usize = 100;

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

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
    match subtitle_list_result_inner(&state.db, &item_id).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
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
    let visible = visible_media_item_sql("media_items");
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            &format!("SELECT title, path, runtime_ticks FROM media_items WHERE id = ? AND item_type = 'Audio' AND {visible}"),
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
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
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
            .and_then(|offset_ms| metadata.insert("Offset".to_string(), json!(offset_ms * 10_000))),
        "length" => parse_lrc_length(value)
            .and_then(|ticks| metadata.insert("Length".to_string(), json!(ticks))),
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
    let (seconds, fraction) = seconds_part.split_once('.').unwrap_or((seconds_part, ""));
    let seconds = seconds.parse::<i64>().ok()?;
    if hours < 0 || minutes < 0 || !(0..60).contains(&seconds) {
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
    let Some(item) = MediaItems::find_by_id(item_id.to_string()).one(db).await? else {
        anyhow::bail!("item not found");
    };
    let media_path = item.path;
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
    MediaStreams::insert(media_streams::ActiveModel {
        id: Set(stable_text_id(&format!("stream:{item_id}:{next_index}"))),
        item_id: Set(item_id.to_string()),
        stream_index: Set(next_index),
        stream_type: Set("Subtitle".to_string()),
        codec: Set(Some(format.to_string())),
        language: Set(Some(language.to_string())),
        title: Set(Some(title)),
        path: Set(Some(subtitle_path.to_string_lossy().to_string())),
        is_external: Set(1),
        created_at: Set(now),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::columns([
            media_streams::Column::ItemId,
            media_streams::Column::StreamIndex,
        ])
        .update_columns([
            media_streams::Column::StreamType,
            media_streams::Column::Codec,
            media_streams::Column::Language,
            media_streams::Column::Title,
            media_streams::Column::Path,
            media_streams::Column::IsExternal,
        ])
        .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

async fn next_subtitle_index(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<i64> {
    Ok(MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .order_by_desc(media_streams::Column::StreamIndex)
        .one(db)
        .await?
        .map(|stream| stream.stream_index)
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
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT ms.stream_index, ms.codec, ms.language, ms.title, ms.is_external FROM media_streams ms JOIN media_items mi ON mi.id = ms.item_id WHERE ms.item_id = ? AND ms.stream_type = 'Subtitle' AND mi.is_public = 1 AND (mi.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = mi.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = mi.parent_id AND parent.is_public = 1)) ORDER BY ms.stream_index ASC",
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

async fn subtitle_list_result_inner(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<Value>> {
    let exists = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT 1 AS found FROM media_items WHERE id = ? AND is_public = 1 AND (media_items.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = media_items.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = media_items.parent_id AND parent.is_public = 1))",
            vec![item_id.into()],
        ))
        .await?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let items = subtitle_list_inner(db, item_id).await?;
    Ok(Some(json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "StartIndex": 0,
    })))
}

pub async fn metadata_reset(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.as_ref().map(|Json(body)| body);
    let item_ids = match metadata_ids_from_query_or_body(
        &query,
        body,
        &["Ids", "ids"],
        MAX_METADATA_WRITE_IDS,
    ) {
        Ok(ids) => ids,
        Err(error) => return metadata_validation_error(error),
    };
    if item_ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    match metadata_reset_inner(&state, &item_ids).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
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
    let user_id = query_value(query, &["userId", "UserId"]);
    let favorite_only = query_value(query, &["isFavorite", "IsFavorite"])
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let mut values = Vec::new();
    let mut join = String::new();
    let mut filters = vec![visible_media_item_sql("mi")];

    if favorite_only {
        if let Some(user_id) = user_id.as_deref() {
            join.push_str(" JOIN user_data ud ON ud.item_id = mi.id AND ud.user_id = ?");
            values.push(user_id.into());
            filters.push("ud.is_favorite = 1".to_string());
        } else {
            filters.push("0 = 1".to_string());
        }
    }

    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &format!(
                "SELECT mi.item_type, CASE WHEN mi.item_type = 'Episode' THEN COUNT(DISTINCT (mi.parent_id, COALESCE(mi.season_number, 0), COALESCE(mi.episode_number, 0))) ELSE COUNT(*) END AS count FROM media_items mi{join} WHERE {} GROUP BY mi.item_type",
                filters.join(" AND ")
            ),
            values,
        ))
        .await
        .context("failed to count items")?;

    item_counts_response(db, &rows).await
}

async fn item_counts_response(
    db: &sea_orm::DatabaseConnection,
    rows: &[sea_orm::QueryResult],
) -> anyhow::Result<Value> {
    let mut movie_count = 0;
    let mut series_count = 0;
    let mut episode_count = 0;
    let mut game_count = 0;
    let mut trailer_count = 0;
    let mut song_count = 0;
    let mut album_count = 0;
    let mut music_video_count = 0;
    let mut box_set_count = 0;
    let mut book_count = 0;
    let mut game_system_count = 0;
    let mut item_count = 0;

    for row in rows {
        let item_type: String = row.get_str("item_type")?;
        let count: i64 = row.get_i64("count")?;
        item_count += count;
        match item_type.as_str() {
            "Movie" => movie_count += count,
            "Series" => series_count += count,
            "Episode" => episode_count += count,
            "Game" => game_count += count,
            "GameSystem" => game_system_count += count,
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
        .query_all(crate::db::helpers::pg_statement(
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
        "GameCount": game_count,
        "ArtistCount": artist_count,
        "ProgramCount": 0,
        "GameSystemCount": game_system_count,
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
    let row = db
        .query_one(crate::db::helpers::pg_statement(
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
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.as_ref().map(|Json(body)| body);
    let item_ids =
        match metadata_ids_from_query_or_body(&query, body, &["Ids", "ids"], MAX_MERGE_VERSION_IDS)
        {
            Ok(ids) => ids,
            Err(error) => return metadata_validation_error(error),
        };
    if item_ids.len() < 2 {
        return metadata_validation_error((
            StatusCode::BAD_REQUEST,
            "Need at least 2 items to merge",
        ));
    }

    match merge_versions_inner(&state.db, &item_ids).await {
        Ok(MergeVersionsResult::Merged) => StatusCode::NO_CONTENT.into_response(),
        Ok(MergeVersionsResult::MissingItem) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Item not found" })),
        )
            .into_response(),
        Ok(MergeVersionsResult::InvalidItem) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Ids must reference video items" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }

    // Find the parent of the first item — this becomes the target parent
    // If the first item doesn't have a parent (it's a top-level item), create a folder for it
    // The first item IS the folder — move others into it

    // Move all other items to be children of the target parent
}

/// GET /Videos/ActiveEncodings — list active transcodings (stub)
fn metadata_ids_from_query_or_body(
    query: &HashMap<String, String>,
    body: Option<&Value>,
    keys: &[&str],
    max_ids: usize,
) -> Result<Vec<String>, (StatusCode, &'static str)> {
    if let Some(ids) = query_value(query, keys) {
        return normalize_metadata_ids(ids.split(',').map(str::to_string).collect(), max_ids);
    }
    let value = keys.iter().find_map(|key| {
        body.and_then(|body| {
            body.as_object().and_then(|object| {
                object
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(key))
                    .map(|(_, value)| value)
            })
        })
    });
    metadata_ids_from_value(value, max_ids)
}

fn metadata_ids_from_value(
    value: Option<&Value>,
    max_ids: usize,
) -> Result<Vec<String>, (StatusCode, &'static str)> {
    match value {
        Some(Value::Array(items)) => {
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                let Some(id) = item.as_str() else {
                    return Err((StatusCode::BAD_REQUEST, "Ids must contain strings"));
                };
                ids.push(id.to_string());
            }
            normalize_metadata_ids(ids, max_ids)
        }
        Some(Value::String(ids)) => {
            normalize_metadata_ids(ids.split(',').map(str::to_string).collect(), max_ids)
        }
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err((
            StatusCode::BAD_REQUEST,
            "Ids must be an array or CSV string",
        )),
    }
}

fn normalize_metadata_ids(
    ids: Vec<String>,
    max_ids: usize,
) -> Result<Vec<String>, (StatusCode, &'static str)> {
    if ids.len() > max_ids {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "Too many item ids"));
    }
    let mut normalized = Vec::new();
    for id in ids {
        let id = id.trim();
        if id.is_empty() || normalized.iter().any(|existing| existing == id) {
            continue;
        }
        if id.len() > MAX_METADATA_WRITE_ID_LEN
            || id.contains('\0')
            || id.chars().any(char::is_control)
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid item id"));
        }
        normalized.push(id.to_string());
    }
    Ok(normalized)
}

fn metadata_validation_error(error: (StatusCode, &'static str)) -> Response {
    (error.0, Json(json!({ "Error": error.1 }))).into_response()
}

async fn metadata_reset_inner(state: &AppState, item_ids: &[String]) -> anyhow::Result<usize> {
    let now = now_unix();
    let mut reset_count = 0;
    for item_id in item_ids {
        let Some(item) = MediaItems::find_by_id(item_id.clone())
            .one(&state.db)
            .await
            .with_context(|| format!("failed to load item for metadata reset: {item_id}"))?
        else {
            continue;
        };
        let mut active: media_items::ActiveModel = item.into();
        active.overview = Set(None);
        active.production_year = Set(None);
        active.premiere_date = Set(None);
        active.updated_at = Set(now);
        active
            .update(&state.db)
            .await
            .with_context(|| format!("failed to reset metadata: {item_id}"))?;
        reset_count += 1;

        MediaPeople::delete_many()
            .filter(media_people::Column::ItemId.eq(item_id))
            .exec(&state.db)
            .await
            .with_context(|| format!("failed to clear media_people: {item_id}"))?;
        MediaGenres::delete_many()
            .filter(media_genres::Column::ItemId.eq(item_id))
            .exec(&state.db)
            .await
            .with_context(|| format!("failed to clear media_genres: {item_id}"))?;
        MediaTags::delete_many()
            .filter(media_tags::Column::ItemId.eq(item_id))
            .exec(&state.db)
            .await
            .with_context(|| format!("failed to clear media_tags: {item_id}"))?;
        MediaStudios::delete_many()
            .filter(media_studios::Column::ItemId.eq(item_id))
            .exec(&state.db)
            .await
            .with_context(|| format!("failed to clear media_studios: {item_id}"))?;
        ProviderIds::delete_many()
            .filter(provider_ids::Column::ItemId.eq(item_id))
            .exec(&state.db)
            .await
            .with_context(|| format!("failed to clear provider_ids: {item_id}"))?;

        crate::jellyfin::system::log_activity(
            state,
            "Metadata reset",
            "MetadataReset",
            None,
            Some(item_id),
        )
        .await;
    }
    Ok(reset_count)
}

#[derive(Debug, PartialEq, Eq)]
enum MergeVersionsResult {
    Merged,
    MissingItem,
    InvalidItem,
}

struct MergeItem {
    id: String,
    parent_id: String,
    item_type: String,
    is_folder: bool,
}

async fn merge_versions_inner(
    db: &sea_orm::DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<MergeVersionsResult> {
    let items = merge_version_items(db, item_ids).await?;
    if items.len() != item_ids.len() {
        return Ok(MergeVersionsResult::MissingItem);
    }
    if items.iter().any(|item| !mergeable_video_item(item)) {
        return Ok(MergeVersionsResult::InvalidItem);
    }

    let first = &items[0];
    let target_parent = if first.parent_id.is_empty() || first.parent_id == first.id {
        first.id.clone()
    } else {
        first.parent_id.clone()
    };

    let now = now_unix();
    for id in &item_ids[1..] {
        if let Some(item) = MediaItems::find_by_id(id.clone()).one(db).await? {
            let mut active: media_items::ActiveModel = item.into();
            active.parent_id = Set(target_parent.clone());
            active.updated_at = Set(now);
            active
                .update(db)
                .await
                .with_context(|| format!("failed to merge video version: {id}"))?;
        }
    }

    Ok(MergeVersionsResult::Merged)
}

async fn merge_version_items(
    db: &sea_orm::DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<Vec<MergeItem>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = MediaItems::find()
        .filter(media_items::Column::Id.is_in(item_ids.to_vec()))
        .all(db)
        .await
        .context("failed to load video versions")?;
    let mut items = Vec::new();
    for id in item_ids {
        if let Some(row) = rows.iter().find(|row| row.id == *id) {
            items.push(MergeItem {
                id: row.id.clone(),
                parent_id: row.parent_id.clone(),
                item_type: row.item_type.clone(),
                is_folder: row.is_folder != 0,
            });
        }
    }
    Ok(items)
}

fn mergeable_video_item(item: &MergeItem) -> bool {
    !item.is_folder
        && matches!(
            item.item_type.as_str(),
            "Video" | "Movie" | "Episode" | "Trailer"
        )
}

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
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
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
    let Some(item) = item_queries::find_media_item(db, "", item_id).await? else {
        return Ok(None);
    };
    let parent_id = if item.parent_id.is_empty() {
        item.id.as_str()
    } else {
        item.parent_id.as_str()
    };
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &item_queries::media_item_select_sql(&format!(
                "WHERE media_items.parent_id = ? AND media_items.id <> ? AND media_items.item_type = 'Video' AND {} ORDER BY media_items.title ASC",
                visible_media_item_sql("media_items")
            )),
            vec!["".into(), parent_id.into(), item.id.as_str().into()],
        ))
        .await?;
    let items = item_queries::decode_media_items(&rows)?;
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
    let now = now_unix();
    let alternates = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(&item.parent_id))
        .filter(media_items::Column::Id.ne(&item.id))
        .filter(media_items::Column::ItemType.eq("Video"))
        .all(db)
        .await?;
    for alternate in alternates {
        let mut active: media_items::ActiveModel = alternate.into();
        active.parent_id = Set(String::new());
        active.updated_at = Set(now);
        active.update(db).await?;
    }
    Ok(true)
}

async fn audiobooks_next_up_inner(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
) -> anyhow::Result<Vec<crate::library::models::MediaItem>> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &item_queries::media_item_select_sql(&format!(
                "WHERE media_items.item_type = 'Audio' AND media_items.is_folder = 0 AND {} AND COALESCE(user_data.played, 0) = 0 AND COALESCE(user_data.playback_position_ticks, 0) > 0 ORDER BY user_data.updated_at DESC",
                visible_media_item_sql("media_items")
            )),
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
        MAX_MERGE_VERSION_IDS, MergeVersionsResult, UploadSubtitleRequest, alternate_sources_inner,
        audiobooks_next_up, audiobooks_next_up_inner, available_recording_options,
        available_recording_options_value, delete_alternate_sources_inner, delete_lyrics_inner,
        item_counts_inner, item_lyrics_inner, lyrics_value_from_text, merge_versions_inner,
        metadata_editor_info_inner, metadata_ids_from_query_or_body, metadata_reset_inner,
        normalize_metadata_ids, parse_lrc_timestamp, stop_encodings, subtitle_format,
        subtitle_list_inner, subtitle_list_result_inner, subtitle_provider_info, subtitle_suffix,
        upload_lyrics_inner, upload_subtitle_inner,
    };
    use crate::entities::{
        libraries::{self, Entity as Libraries},
        library_paths::{self, Entity as LibraryPaths},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        people::{self, Entity as People},
        provider_ids::{self, Entity as ProviderIds},
        user_data::{self, Entity as UserData},
        users::{self, Entity as Users},
    };
    use axum::body::{Bytes, to_bytes};
    use axum::extract::{Extension, Query, State};
    use axum::response::IntoResponse;
    use base64::{Engine as _, engine::general_purpose};
    use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    #[tokio::test]
    async fn lyrics_report_missing() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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

        let public_child_path = dir.join("public-child.mp3");
        let public_child_lyric_path = dir.join("public-child.lrc");
        std::fs::write(&public_child_path, b"audio").unwrap();
        std::fs::write(&public_child_lyric_path, "[00:02.00]Still hidden").unwrap();
        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            &dir.join("private-parent").to_string_lossy(),
            "music",
            "",
            "MusicAlbum",
            1,
            0,
            None,
            None,
            None,
        )
        .await;
        insert_audio_item_with_parent(
            &db,
            "public-child",
            "Public Child",
            &public_child_path,
            "private-parent",
            true,
        )
        .await;
        insert_library_path(&db, &dir).await;
        assert_eq!(item_lyrics_inner(&db, "public-child").await.unwrap(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn lyrics_upload_and_delete_sidecar_file() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        insert_audio_item_with_parent(db, id, title, path, "", is_public).await;
    }

    async fn insert_audio_item_with_parent(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &std::path::Path,
        parent_id: &str,
        is_public: bool,
    ) {
        insert_library(db, "music", "Music", "music").await;
        insert_media_item(
            db,
            id,
            title,
            &path.to_string_lossy(),
            "music",
            parent_id,
            "Audio",
            0,
            i64::from(is_public),
            None,
            None,
            None,
        )
        .await;
    }

    async fn insert_library_path(db: &DatabaseConnection, path: &std::path::Path) {
        LibraryPaths::insert(library_paths::ActiveModel {
            id: Set(crate::util::stable_text_id(&format!(
                "library-path:{}",
                path.to_string_lossy()
            ))),
            library_id: Set("music".to_string()),
            path: Set(path.to_string_lossy().to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_library(db: &DatabaseConnection, id: &str, name: &str, collection_type: &str) {
        Libraries::insert(libraries::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            collection_type: Set(collection_type.to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .on_conflict_do_nothing()
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_media_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        container: Option<&str>,
        overview: Option<&str>,
        production_year: Option<i64>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(library_id.to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(is_public),
            container: Set(container.map(ToString::to_string)),
            overview: Set(overview.map(ToString::to_string)),
            production_year: Set(production_year),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_subtitle_stream(
        db: &DatabaseConnection,
        id: &str,
        item_id: &str,
        stream_index: i64,
        codec: &str,
        language: &str,
        title: &str,
        is_external: i64,
    ) {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            stream_index: Set(stream_index),
            stream_type: Set("Subtitle".to_string()),
            codec: Set(Some(codec.to_string())),
            language: Set(Some(language.to_string())),
            title: Set(Some(title.to_string())),
            is_interlaced: Set(0),
            is_default: Set(0),
            is_forced: Set(0),
            is_hearing_impaired: Set(0),
            is_external: Set(is_external),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_user(db: &DatabaseConnection, id: &str, username: &str, display_name: &str) {
        Users::insert(users::ActiveModel {
            id: Set(id.to_string()),
            username: Set(username.to_string()),
            display_name: Set(display_name.to_string()),
            is_admin: Set(0),
            is_disabled: Set(0),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_user_data(
        db: &DatabaseConnection,
        user_id: &str,
        item_id: &str,
        is_favorite: i64,
        played: i64,
        playback_position_ticks: i64,
        play_count: i64,
        updated_at: i64,
    ) {
        UserData::insert(user_data::ActiveModel {
            user_id: Set(user_id.to_string()),
            item_id: Set(item_id.to_string()),
            is_favorite: Set(is_favorite),
            played: Set(played),
            playback_position_ticks: Set(playback_position_ticks),
            play_count: Set(play_count),
            updated_at: Set(updated_at),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_person(db: &DatabaseConnection, id: &str, name: &str) {
        People::insert(people::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_media_person(
        db: &DatabaseConnection,
        item_id: &str,
        person_id: &str,
        person_type: &str,
    ) {
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person_id.to_string()),
            person_type: Set(person_type.to_string()),
            role: Set(None),
            sort_order: Set(0),
        })
        .exec_without_returning(db)
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_audio_item(
            &db,
            "song",
            "Song",
            &std::env::temp_dir().join("private-song.mp3"),
            false,
        )
        .await;
        insert_subtitle_stream(&db, "s1", "song", 2, "srt", "eng", "English", 0).await;

        assert!(subtitle_list_inner(&db, "song").await.unwrap().is_empty());
        assert!(
            subtitle_list_result_inner(&db, "song")
                .await
                .unwrap()
                .is_none()
        );

        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            "/tmp/private-parent",
            "",
            "",
            "Movie",
            1,
            0,
            None,
            None,
            None,
        )
        .await;
        insert_media_item(
            &db,
            "public-child",
            "Public Child",
            "/tmp/public-child.mkv",
            "",
            "private-parent",
            "Video",
            0,
            1,
            None,
            None,
            None,
        )
        .await;
        insert_subtitle_stream(&db, "s2", "public-child", 3, "srt", "eng", "English", 0).await;
        assert!(
            subtitle_list_inner(&db, "public-child")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            subtitle_list_result_inner(&db, "public-child")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn subtitle_list_result_reports_empty_public_items() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_audio_item(
            &db,
            "song",
            "Song",
            &std::env::temp_dir().join("public-song.mp3"),
            true,
        )
        .await;

        let value = subtitle_list_result_inner(&db, "song")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["TotalRecordCount"], 0);
        assert_eq!(value["StartIndex"], 0);
        assert!(value["Items"].as_array().unwrap().is_empty());
        assert!(
            subtitle_list_result_inner(&db, "missing")
                .await
                .unwrap()
                .is_none()
        );
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

    #[test]
    fn metadata_write_ids_accept_query_and_body_shapes() {
        let mut query = HashMap::new();
        query.insert("ids".to_string(), " a,b,a ,, c ".to_string());
        let ids =
            metadata_ids_from_query_or_body(&query, None, &["Ids", "ids"], MAX_MERGE_VERSION_IDS)
                .unwrap();
        assert_eq!(ids, vec!["a", "b", "c"]);

        let body = json!({ "Ids": ["v1", "v2"] });
        let ids =
            metadata_ids_from_query_or_body(&HashMap::new(), Some(&body), &["Ids", "ids"], 10)
                .unwrap();
        assert_eq!(ids, vec!["v1", "v2"]);

        assert!(normalize_metadata_ids(vec!["bad\nid".to_string()], 10).is_err());
        assert!(
            normalize_metadata_ids(
                vec!["x".to_string(); MAX_MERGE_VERSION_IDS + 1],
                MAX_MERGE_VERSION_IDS
            )
            .is_err()
        );
        assert!(
            metadata_ids_from_query_or_body(
                &HashMap::new(),
                Some(&json!({ "Ids": [1] })),
                &["Ids", "ids"],
                10
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn metadata_reset_and_merge_versions_are_limited_and_persisted() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let state = test_state(db);
        for (id, title, item_type, parent_id, overview, production_year) in [
            ("v1", "1080p", "Video", "movie", "overview", 2024_i64),
            ("v2", "720p", "Video", "", "overview", 2024_i64),
            ("audio", "Song", "Audio", "", "overview", 2024_i64),
        ] {
            insert_media_item(
                &state.db,
                id,
                title,
                &format!("/tmp/{id}"),
                "",
                parent_id,
                item_type,
                0,
                1,
                None,
                Some(overview),
                Some(production_year),
            )
            .await;
        }
        crate::db::provider_ids::upsert(&state.db, "v1", "Tmdb", "1")
            .await
            .unwrap();

        assert_eq!(
            metadata_reset_inner(&state, &["v1".to_string(), "missing".to_string()])
                .await
                .unwrap(),
            1
        );
        let item = MediaItems::find_by_id("v1".to_string())
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        assert!(item.overview.is_none());
        assert!(item.production_year.is_none());
        assert!(
            ProviderIds::find()
                .filter(provider_ids::Column::ItemId.eq("v1"))
                .one(&state.db)
                .await
                .unwrap()
                .is_none()
        );

        assert_eq!(
            merge_versions_inner(&state.db, &["v1".to_string(), "v2".to_string()])
                .await
                .unwrap(),
            MergeVersionsResult::Merged
        );
        let item = MediaItems::find_by_id("v2".to_string())
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.parent_id, "movie");
        assert_eq!(
            merge_versions_inner(&state.db, &["v1".to_string(), "missing".to_string()])
                .await
                .unwrap(),
            MergeVersionsResult::MissingItem
        );
        assert_eq!(
            merge_versions_inner(&state.db, &["v1".to_string(), "audio".to_string()])
                .await
                .unwrap(),
            MergeVersionsResult::InvalidItem
        );
    }

    #[tokio::test]
    async fn alternate_sources_return_and_remove_video_versions() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, title, parent_id, is_public) in [
            ("parent", "Movie", "", 1_i64),
            ("v1", "1080p", "parent", 1),
            ("v2", "720p", "parent", 1),
            ("private", "Private", "parent", 0),
            ("hidden-parent", "Hidden Parent", "", 0),
            ("hidden-child", "Hidden Child", "hidden-parent", 1),
        ] {
            insert_media_item(
                &db,
                id,
                title,
                &format!("/tmp/{id}.mkv"),
                "",
                parent_id,
                "Video",
                0,
                is_public,
                Some("mkv"),
                None,
                None,
            )
            .await;
        }

        let sources = alternate_sources_inner(&db, "v1").await.unwrap().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["Id"], "v2");
        assert_eq!(sources[0]["DirectStreamUrl"], "/Videos/v2/stream");
        assert_eq!(alternate_sources_inner(&db, "private").await.unwrap(), None);
        assert_eq!(
            alternate_sources_inner(&db, "hidden-child").await.unwrap(),
            None
        );

        assert!(delete_alternate_sources_inner(&db, "v1").await.unwrap());
        let sources = alternate_sources_inner(&db, "v1").await.unwrap().unwrap();
        assert!(sources.is_empty());
    }

    #[tokio::test]
    async fn audiobooks_next_up_uses_audio_resume_progress() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let dir = std::env::temp_dir();
        insert_user(&db, "u1", "alice", "Alice").await;
        insert_audio_item(&db, "a1", "Chapter 1", &dir.join("a1.mp3"), true).await;
        insert_audio_item(&db, "a2", "Chapter 2", &dir.join("a2.mp3"), true).await;
        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            &dir.join("private-parent").to_string_lossy(),
            "music",
            "",
            "MusicAlbum",
            1,
            0,
            None,
            None,
            None,
        )
        .await;
        insert_audio_item_with_parent(
            &db,
            "hidden-child",
            "Hidden Child",
            &dir.join("hidden-child.mp3"),
            "private-parent",
            true,
        )
        .await;
        insert_user_data(&db, "u1", "a1", 0, 0, 42, 0, 10).await;
        insert_user_data(&db, "u1", "a2", 0, 1, 100, 1, 20).await;
        insert_user_data(&db, "u1", "hidden-child", 0, 0, 99, 0, 30).await;

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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let dir = std::env::temp_dir();
        insert_user(&db, "u1", "alice", "Alice").await;
        insert_audio_item(&db, "a1", "Chapter 1", &dir.join("a1.mp3"), true).await;
        insert_audio_item(&db, "a2", "Chapter 2", &dir.join("a2.mp3"), true).await;
        insert_audio_item(&db, "private", "Private", &dir.join("private.mp3"), false).await;
        insert_media_item(
            &db,
            "hidden-parent",
            "Hidden Parent",
            &dir.join("hidden-parent").to_string_lossy(),
            "music",
            "",
            "MusicAlbum",
            1,
            0,
            None,
            None,
            None,
        )
        .await;
        insert_audio_item_with_parent(
            &db,
            "hidden-child",
            "Hidden Child",
            &dir.join("hidden-child.mp3"),
            "hidden-parent",
            true,
        )
        .await;
        for (id, updated_at) in [
            ("a1", 10_i64),
            ("a2", 20),
            ("private", 30),
            ("hidden-child", 40),
        ] {
            insert_user_data(&db, "u1", id, 0, 0, 42, 0, updated_at).await;
        }

        let state = Arc::new(test_state(db));
        let mut query = HashMap::new();
        query.insert("UserId".to_string(), "u1".to_string());
        query.insert("StartIndex".to_string(), "1".to_string());
        query.insert("Limit".to_string(), "1".to_string());
        let response = audiobooks_next_up(State(state), Extension("u1".to_string()), Query(query))
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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_media_item(
            &db, "i1", "Movie", "D:/movie", "movies", "", "Movie", 1, 1, None, None, None,
        )
        .await;

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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_user(&db, "u1", "alice", "Alice").await;
        for (id, title, item_type, is_folder) in [
            ("m1", "Movie", "Movie", 1_i64),
            ("s1", "Series", "Series", 1_i64),
            ("e1", "Episode", "Episode", 0_i64),
            ("a1", "Song", "Audio", 0_i64),
        ] {
            insert_media_item(
                &db,
                id,
                title,
                &format!("D:/{id}"),
                "",
                "",
                item_type,
                is_folder,
                1,
                None,
                None,
                None,
            )
            .await;
        }
        insert_user_data(&db, "u1", "m1", 1, 0, 0, 0, 1).await;
        insert_person(&db, "p1", "Artist").await;
        insert_media_person(&db, "a1", "p1", "Artist").await;

        let counts = item_counts_inner(&db, &HashMap::new()).await.unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["SeriesCount"], 1);
        assert_eq!(counts["EpisodeCount"], 1);
        assert_eq!(counts["GameCount"], 0);
        assert_eq!(counts["GameSystemCount"], 0);
        assert_eq!(counts["SongCount"], 1);
        assert_eq!(counts["ArtistCount"], 1);
        assert_eq!(counts["ItemCount"], 4);

        let mut query = HashMap::new();
        query.insert("userId".to_string(), "u1".to_string());
        let counts = item_counts_inner(&db, &query).await.unwrap();
        assert_eq!(counts["MovieCount"], 1);
        assert_eq!(counts["SeriesCount"], 1);
        assert_eq!(counts["EpisodeCount"], 1);
        assert_eq!(counts["ItemCount"], 4);

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
            tmdb_proxy_url: Arc::new(RwLock::new(None)),
            tmdb_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            douban_cookie: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            admin_http_log_seq: std::sync::atomic::AtomicU64::new(0),
            admin_http_logs: RwLock::new(std::collections::VecDeque::new()),
            playback_distribution: RwLock::new(crate::app::state::PlaybackDistribution::default()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
