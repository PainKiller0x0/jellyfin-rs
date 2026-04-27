use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{
    app::state::AppState,
    jellyfin::common::{image as placeholder_image, internal_error},
    jellyfin::providers,
    library::images::upsert_image_asset,
    util::{now_unix, stable_text_id},
};

pub async fn item_images(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match item_images_inner(&state.db, &item_id).await {
        Ok(images) => Json(images).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn get_item_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
) -> Response {
    serve_item_image(&state.db, &headers, &item_id, &image_type, 0).await
}

pub async fn get_item_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, first, second)): Path<(String, String, String)>,
) -> Response {
    let (image_type, image_index) = if let Ok(index) = second.parse::<i64>() {
        (first, index)
    } else {
        (second, first.parse::<i64>().unwrap_or_default())
    };
    serve_item_image(&state.db, &headers, &item_id, &image_type, image_index).await
}

pub async fn upload_item_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    if let Some(image_url) = parse_image_url_body(&body) {
        match download_and_cache_image(&state, &item_id, &image_type, &image_url).await {
            Ok(()) => return StatusCode::NO_CONTENT.into_response(),
            Err(error) => return internal_error(error),
        }
    }
    match save_item_image(&state.db, &headers, &item_id, &image_type, 0, body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn upload_item_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, first, second)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    if let Some(image_url) = parse_image_url_body(&body) {
        match download_and_cache_image(&state, &item_id, &second, &image_url).await {
            Ok(()) => return StatusCode::NO_CONTENT.into_response(),
            Err(error) => return internal_error(error),
        }
    }
    let (image_type, image_index) = if let Ok(index) = first.parse::<i64>() {
        (second, index)
    } else {
        (first, second.parse::<i64>().unwrap_or_default())
    };
    match save_item_image(
        &state.db,
        &headers,
        &item_id,
        &image_type,
        image_index,
        body,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_item_image(
    State(state): State<Arc<AppState>>,
    Path((item_id, image_type)): Path<(String, String)>,
) -> Response {
    delete_item_image_inner(&state.db, &item_id, &image_type, 0).await
}

pub async fn delete_item_image_with_index(
    State(state): State<Arc<AppState>>,
    Path((item_id, image_type, index)): Path<(String, String, i64)>,
) -> Response {
    delete_item_image_inner(&state.db, &item_id, &image_type, index).await
}

pub async fn remote_images(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let image_type = query
        .get("Type")
        .or_else(|| query.get("type"))
        .map(String::as_str)
        .unwrap_or("Primary");
    let include_all_languages = query
        .get("IncludeAllLanguages")
        .or_else(|| query.get("includeAllLanguages"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let provider_name = query
        .get("ProviderName")
        .or_else(|| query.get("providerName"))
        .filter(|value| !value.is_empty())
        .map(String::as_str);

    if provider_name.is_some_and(|name| name.eq_ignore_ascii_case("Local")) {
        return Json(json!({
            "Images": [],
            "TotalRecordCount": 0,
            "Providers": ["Local"]
        }))
        .into_response();
    }

    let mut all_images: Vec<Value> = Vec::new();

    let tmdb_id = if let Some(api_key) = state.tmdb_api_key.as_deref().filter(|key| !key.is_empty())
    {
        let tmdb_id = lookup_tmdb_id(&state.db, &item_id).await.ok().flatten();
        match tmdb_id {
            Some(ref tmdb_id) => {
                let images_result =
                    fetch_remote_images_by_type(&state, api_key, &item_id, tmdb_id).await;
                match images_result {
                    Ok(images) => all_images.extend(images),
                    Err(error) => {
                        tracing::warn!("TMDb images fetch failed for {item_id}: {error:#}")
                    }
                }
                Some(tmdb_id.clone())
            }
            None => {
                if let Ok(Some(found)) = search_tmdb_id_by_item(&state, &item_id).await {
                    match providers::tmdb_movie_images(&state.http_client, api_key, &found).await {
                        Ok(images) => all_images.extend(images),
                        Err(error) => {
                            tracing::warn!("TMDb images search failed for {item_id}: {error:#}")
                        }
                    }
                    Some(found)
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    if !include_all_languages {
        all_images.retain(|image| {
            image
                .get("Language")
                .and_then(Value::as_str)
                .is_none_or(|lang| lang.is_empty() || lang.eq_ignore_ascii_case("en"))
        });
    }

    if image_type.eq_ignore_ascii_case("Backdrop") {
        all_images.retain(|image| {
            image
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|type_| type_.eq_ignore_ascii_case("Backdrop"))
        });
    } else {
        all_images.retain(|image| {
            image
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|type_| type_.eq_ignore_ascii_case("Primary"))
        });
    }

    let total = all_images.len();
    let providers: Vec<&str> = if tmdb_id.is_some() {
        vec!["TheMovieDb"]
    } else {
        vec!["Local"]
    };

    Json(json!({
        "Images": all_images,
        "TotalRecordCount": total,
        "Providers": providers
    }))
    .into_response()
}

async fn lookup_tmdb_id(db: &AnyPool, item_id: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query(
        "SELECT provider_item_id FROM provider_ids WHERE item_id = ? AND provider = 'Tmdb'",
    )
    .bind(item_id)
    .fetch_optional(db)
    .await
    .context("failed to look up TMDb id")?;
    Ok(row.and_then(|row| row.try_get("provider_item_id").ok()))
}

async fn search_tmdb_id_by_item(state: &AppState, item_id: &str) -> anyhow::Result<Option<String>> {
    let api_key = state
        .tmdb_api_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .context("no TMDb API key configured")?;
    let row = sqlx::query("SELECT title, production_year, item_type FROM media_items WHERE id = ?")
        .bind(item_id)
        .fetch_optional(&state.db)
        .await
        .context("failed to fetch item for TMDb search")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let name: String = row.try_get("title")?;
    let year: Option<i64> = row.try_get("production_year")?;
    let item_type: String = row.try_get("item_type")?;

    let results = if item_type.eq_ignore_ascii_case("Series") {
        providers::tmdb_tv_search(&state.http_client, api_key, &name, year).await?
    } else {
        providers::tmdb_movie_search(&state.http_client, api_key, &name, year).await?
    };

    Ok(results
        .first()
        .and_then(|result| result.get("ProviderIds"))
        .and_then(|providers| providers.get("Tmdb"))
        .and_then(Value::as_str)
        .map(ToString::to_string))
}

async fn fetch_remote_images_by_type(
    state: &AppState,
    api_key: &str,
    item_id: &str,
    tmdb_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let row = sqlx::query("SELECT item_type FROM media_items WHERE id = ?")
        .bind(item_id)
        .fetch_optional(&state.db)
        .await
        .context("failed to fetch item type for images")?;

    let is_series = row
        .and_then(|row| row.try_get::<String, _>("item_type").ok())
        .is_some_and(|t| t.eq_ignore_ascii_case("Series"));

    if is_series {
        let response: TmdbTvImageResponse = state
            .http_client
            .get(format!("https://api.themoviedb.org/3/tv/{tmdb_id}/images"))
            .query(&[("api_key", api_key)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut images = Vec::new();
        for poster in &response.posters {
            images.push(build_remote_image("Primary", &poster.file_path, poster.width, poster.height, poster.vote_average, poster.vote_count, poster.iso_639_1.as_deref()));
        }
        for backdrop in &response.backdrops {
            images.push(build_remote_image("Backdrop", &backdrop.file_path, backdrop.width, backdrop.height, backdrop.vote_average, backdrop.vote_count, backdrop.iso_639_1.as_deref()));
        }
        Ok(images)
    } else {
        providers::tmdb_movie_images(&state.http_client, api_key, tmdb_id).await
    }
}

fn build_remote_image(
    image_type: &str,
    file_path: &str,
    width: Option<i64>,
    height: Option<i64>,
    vote_average: Option<f64>,
    vote_count: Option<i64>,
    iso_639_1: Option<&str>,
) -> Value {
    let thumbnail_url = if image_type == "Primary" {
        format!("https://image.tmdb.org/t/p/w342{file_path}")
    } else {
        format!("https://image.tmdb.org/t/p/w780{file_path}")
    };
    let full_url = format!("https://image.tmdb.org/t/p/original{file_path}");
    let mut image = json!({
        "ProviderName": "TheMovieDb",
        "Url": full_url,
        "ThumbnailUrl": thumbnail_url,
        "Height": height,
        "Width": width,
        "CommunityRating": vote_average,
        "VoteCount": vote_count,
        "Type": image_type,
    });
    if let Some(lang) = iso_639_1 {
        if !lang.is_empty() {
            image["Language"] = json!(lang);
        }
    }
    image
}

pub async fn user_avatar(Path(user_id): Path<String>) -> Response {
    let path = PathBuf::from("data")
        .join("avatars")
        .join(format!("{}_primary.png", sanitize_file_part(&user_id)));
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
            headers.insert(
                header::ETAG,
                HeaderValue::from_str(&stable_text_id(&format!("avatar:{user_id}")))
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            (headers, Body::from(bytes)).into_response()
        }
        Err(_) => placeholder_image().await.into_response(),
    }
}

pub async fn download_remote_image(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let image_type = query
        .get("Type")
        .or_else(|| query.get("type"))
        .map(String::as_str)
        .unwrap_or("Primary");
    let Some(image_url) = query
        .get("ImageUrl")
        .or_else(|| query.get("imageUrl"))
        .filter(|value| !value.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "ImageUrl is required" })),
        )
            .into_response();
    };

    match download_and_cache_image(&state, &item_id, image_type, image_url).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn serve_item_image(
    db: &AnyPool,
    headers: &HeaderMap,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let row = match sqlx::query("SELECT path, etag, size_bytes FROM image_assets WHERE item_id = ? AND image_type = ? AND image_index = ?")
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .fetch_optional(db)
        .await
        .with_context(|| format!("failed to find image asset: {item_id}:{image_type}:{image_index}"))
    {
        Ok(row) => row,
        Err(error) => return internal_error(error),
    };

    let Some(row) = row else {
        return placeholder_image().await.into_response();
    };
    let etag: String = match row.try_get("etag") {
        Ok(etag) => etag,
        Err(error) => return internal_error(error.into()),
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_matches('"') == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let path: String = match row.try_get("path") {
        Ok(path) => path,
        Err(error) => return internal_error(error.into()),
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "Error": format!("failed to read image file: {error}") })),
            )
                .into_response();
        }
    };

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_from_path(&path)),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response_headers.insert(header::ETAG, value);
    }
    (response_headers, Body::from(bytes)).into_response()
}

async fn save_item_image(
    db: &AnyPool,
    headers: &HeaderMap,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    body: Bytes,
) -> anyhow::Result<()> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let bytes = decode_image_body(content_type, &body)?;
    let extension = extension_from_content_type(content_type).unwrap_or("bin");
    let etag = stable_text_id(&format!(
        "image:{item_id}:{image_type}:{image_index}:{}",
        now_unix()
    ));
    let relative_path = PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&relative_path)
        .await
        .context("failed to create image directory")?;
    let path = relative_path.join(format!(
        "{}_{}_{}.{}",
        sanitize_file_part(item_id),
        sanitize_file_part(image_type),
        image_index,
        extension
    ));
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to write image file: {}", path.display()))?;

    let now = now_unix();
    sqlx::query(r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, size_bytes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET path = excluded.path, etag = excluded.etag, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#)
        .bind(stable_text_id(&format!("image-asset:{item_id}:{image_type}:{image_index}")))
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .bind(path.to_string_lossy().to_string())
        .bind(etag)
        .bind(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .context("failed to upsert image asset")?;

    Ok(())
}

async fn download_and_cache_image(
    state: &AppState,
    item_id: &str,
    image_type: &str,
    image_url: &str,
) -> anyhow::Result<()> {
    let response = state
        .http_client
        .get(image_url)
        .send()
        .await?
        .error_for_status()?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let bytes = response.bytes().await?;
    let extension = content_type
        .as_deref()
        .and_then(extension_from_content_type)
        .or_else(|| extension_from_url(image_url))
        .unwrap_or("bin");

    let directory = PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&directory)
        .await
        .context("failed to create image cache directory")?;
    let path = directory.join(format!(
        "{}_{}_remote.{}",
        sanitize_file_part(item_id),
        sanitize_file_part(image_type),
        extension
    ));
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to write remote image file: {}", path.display()))?;
    upsert_image_asset(
        &state.db,
        item_id,
        image_type,
        0,
        path.to_string_lossy().as_ref(),
        i64::try_from(bytes.len()).ok(),
    )
    .await
}

async fn item_images_inner(db: &AnyPool, item_id: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query("SELECT image_type, image_index, path, size_bytes FROM image_assets WHERE item_id = ? ORDER BY image_type ASC, image_index ASC")
        .bind(item_id)
        .fetch_all(db)
        .await
        .context("failed to list item images")?;
    rows.into_iter()
        .map(|row| -> anyhow::Result<serde_json::Value> {
            let path: Option<String> = row.try_get("path")?;
            Ok(json!({
                "Filename": path.as_deref().and_then(|path| std::path::Path::new(path).file_name()).and_then(|name| name.to_str()),
                "ImageType": row.try_get::<String, _>("image_type")?,
                "ImageIndex": row.try_get::<i64, _>("image_index")?,
                "Size": row.try_get::<Option<i64>, _>("size_bytes")?,
            }))
        })
        .collect()
}

async fn delete_item_image_inner(
    db: &AnyPool,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let path: Option<String> = match sqlx::query(
        "SELECT path FROM image_assets WHERE item_id = ? AND image_type = ? AND image_index = ?",
    )
    .bind(item_id)
    .bind(image_type)
    .bind(image_index)
    .fetch_optional(db)
    .await
    {
        Ok(Some(row)) => row.try_get("path").ok(),
        _ => None,
    };

    match sqlx::query(
        "DELETE FROM image_assets WHERE item_id = ? AND image_type = ? AND image_index = ?",
    )
    .bind(item_id)
    .bind(image_type)
    .bind(image_index)
    .execute(db)
    .await
    {
        Ok(_) => {
            if let Some(path) = path {
                let _ = tokio::fs::remove_file(&path).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

fn decode_image_body(content_type: &str, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    if content_type.starts_with("image/") || content_type == "application/octet-stream" {
        return Ok(body.to_vec());
    }
    let text = std::str::from_utf8(body).context("image body is not valid utf-8")?;
    let encoded = text
        .split_once(',')
        .map_or(text, |(_, encoded)| encoded)
        .trim();
    general_purpose::STANDARD
        .decode(encoded)
        .context("failed to decode base64 image body")
}

fn extension_from_content_type(content_type: &str) -> Option<&'static str> {
    match content_type.split(';').next().unwrap_or_default().trim() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn extension_from_url(url: &str) -> Option<&'static str> {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("jpg")
    } else if path.ends_with(".png") {
        Some("png")
    } else if path.ends_with(".webp") {
        Some("webp")
    } else if path.ends_with(".gif") {
        Some("gif")
    } else {
        None
    }
}

fn content_type_from_path(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

fn sanitize_file_part(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn parse_image_url_body(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let parsed: Value = serde_json::from_str(text).ok()?;
    parsed
        .get("Url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(ToString::to_string)
}

pub async fn item_image_tags(db: &AnyPool, item_id: &str) -> anyhow::Result<Value> {
    let rows = sqlx::query(
        "SELECT image_type, etag FROM image_assets WHERE item_id = ? ORDER BY image_type ASC, image_index ASC",
    )
    .bind(item_id)
    .fetch_all(db)
    .await
    .context("failed to load image tags")?;

    let mut tags = serde_json::Map::new();
    for row in rows {
        let image_type: String = row.try_get("image_type")?;
        let etag: String = row.try_get("etag")?;
        tags.entry(image_type).or_insert_with(|| json!(etag));
    }
    Ok(Value::Object(tags))
}

#[derive(Deserialize)]
struct TmdbTvImageResponse {
    #[serde(default)]
    posters: Vec<TmdbTvImage>,
    #[serde(default)]
    backdrops: Vec<TmdbTvImage>,
}

#[derive(Deserialize)]
struct TmdbTvImage {
    file_path: String,
    width: Option<i64>,
    height: Option<i64>,
    vote_average: Option<f64>,
    vote_count: Option<i64>,
    iso_639_1: Option<String>,
}
