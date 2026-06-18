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
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::image_assets::{self, Entity as ImageAssets},
    jellyfin::common::{image as placeholder_image, internal_error},
    library::image_processing::{
        EncodedImageFormat, ImageRequestOptions, create_collage, create_placeholder, process_image,
    },
    util::{now_unix, stable_text_id},
};

mod remote;

pub use remote::{download_remote_image, remote_images, remote_images_providers};

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
    Query(query): Query<HashMap<String, String>>,
    Path((item_id, image_type)): Path<(String, String)>,
) -> Response {
    serve_item_image(&state.db, &headers, &query, &item_id, &image_type, 0).await
}

pub async fn get_item_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((item_id, first, second)): Path<(String, String, String)>,
) -> Response {
    let (image_type, image_index) = if let Ok(index) = second.parse::<i64>() {
        (first, index)
    } else {
        (second, first.parse::<i64>().unwrap_or_default())
    };
    serve_item_image(
        &state.db,
        &headers,
        &query,
        &item_id,
        &image_type,
        image_index,
    )
    .await
}

pub async fn upload_item_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    if let Some(image_url) = parse_image_url_body(&body) {
        match remote::download_and_cache_image(&state, &item_id, &image_type, &image_url).await {
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
        match remote::download_and_cache_image(&state, &item_id, &second, &image_url).await {
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

pub async fn user_avatar(Path(user_id): Path<String>) -> Response {
    let Some(path) = find_user_avatar_path(&user_id).await else {
        return placeholder_image().await.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type_from_path(&path.to_string_lossy())),
            );
            if let Ok(metadata) = tokio::fs::metadata(&path).await {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or_default();
                let etag =
                    stable_text_id(&format!("avatar:{user_id}:{}:{modified}", metadata.len()));
                if let Ok(value) = HeaderValue::from_str(&etag) {
                    headers.insert(header::ETAG, value);
                }
            }
            (headers, Body::from(bytes)).into_response()
        }
        Err(_) => placeholder_image().await.into_response(),
    }
}

pub async fn upload_user_avatar(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    body: Bytes,
) -> Response {
    match save_user_avatar(&headers, &user_id, body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_user_avatar(Path(user_id): Path<String>) -> Response {
    for path in user_avatar_paths(&user_id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn serve_item_image(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let options = image_options_from_query(query, image_type);
    let model = match ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq(image_type))
        .filter(image_assets::Column::ImageIndex.eq(image_index))
        .one(db)
        .await
        .with_context(|| {
            format!("failed to find image asset: {item_id}:{image_type}:{image_index}")
        }) {
        Ok(row) => row,
        Err(error) => return internal_error(error),
    };

    let Some(model) = model else {
        return dynamic_image_response(db, item_id, image_type, &options).await;
    };
    let etag = model.etag.unwrap_or_default();
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_matches('"') == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let path = model.path.unwrap_or_default();

    // Check processed image cache before doing expensive re-processing
    if should_process(query) {
        let cache_key = format!(
            "{}_{}_{}_{}_{}_{}",
            item_id,
            image_type,
            image_index,
            options.width.unwrap_or(0),
            options.height.unwrap_or(0),
            options.quality,
        );
        let cache_ext = match options.format {
            EncodedImageFormat::Jpeg => "jpg",
            EncodedImageFormat::Png => "png",
            EncodedImageFormat::Webp => "webp",
        };
        let cache_path = format!("data/image_cache/{cache_key}.{cache_ext}");

        if let Ok(cached_bytes) = tokio::fs::read(&cache_path).await {
            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(options.format.content_type()),
            );
            response_headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&cached_bytes.len().to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );
            if let Ok(value) = HeaderValue::from_str(&etag) {
                response_headers.insert(header::ETAG, value);
            }
            return (response_headers, Body::from(cached_bytes)).into_response();
        }

        // Cache miss — read, process, save to cache
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
        match process_image(&bytes, &options) {
            Ok(processed) => {
                // Save to cache (ignore errors)
                let _ = tokio::fs::create_dir_all("data/image_cache").await;
                let _ = tokio::fs::write(&cache_path, &processed).await;
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(options.format.content_type()),
                );
                response_headers.insert(
                    header::CONTENT_LENGTH,
                    HeaderValue::from_str(&processed.len().to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("0")),
                );
                if let Ok(value) = HeaderValue::from_str(&etag) {
                    response_headers.insert(header::ETAG, value);
                }
                return (response_headers, Body::from(processed)).into_response();
            }
            Err(error) => return internal_error(error),
        }
    }

    // No processing needed — serve original file directly
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

    let content_type = content_type_from_path(&path);

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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

async fn dynamic_image_response(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
    options: &ImageRequestOptions,
) -> Response {
    let width = options
        .width
        .unwrap_or_else(|| default_image_size(image_type).0);
    let height = options
        .height
        .unwrap_or_else(|| default_image_size(image_type).1);
    let bytes = match collage_source_images(db, item_id, image_type).await {
        Ok(images) if !images.is_empty() => create_collage(&images, width, height, options),
        Ok(_) => create_placeholder(width, height, item_id, options),
        Err(error) => return internal_error(error),
    };
    match bytes {
        Ok(bytes) => image_response(
            bytes,
            options.format.content_type(),
            dynamic_etag(item_id, image_type, width, height),
        ),
        Err(error) => internal_error(error),
    }
}

async fn collage_source_images(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
) -> anyhow::Result<Vec<Vec<u8>>> {
    let preferred_type = if image_type.eq_ignore_ascii_case("Thumb") {
        "Backdrop"
    } else {
        "Primary"
    };
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT image_assets.path FROM image_assets JOIN media_items ON media_items.id = image_assets.item_id WHERE media_items.parent_id = ? AND image_assets.image_type = ? ORDER BY media_items.title ASC, image_assets.image_index ASC LIMIT 4"#,
            vec![item_id.into(), preferred_type.into()],
        ))
        .await
        .context("failed to find child images for collage")?;

    let mut images = Vec::new();
    for row in &rows {
        let path: String = row.get_str("path")?;
        if let Ok(bytes) = tokio::fs::read(&path).await {
            images.push(bytes);
        }
    }
    Ok(images)
}

fn image_response(bytes: Vec<u8>, content_type: &'static str, etag: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    (headers, Body::from(bytes)).into_response()
}

fn dynamic_etag(item_id: &str, image_type: &str, width: u32, height: u32) -> String {
    stable_text_id(&format!(
        "dynamic-image:{item_id}:{image_type}:{width}:{height}"
    ))
}

fn default_image_size(image_type: &str) -> (u32, u32) {
    if image_type.eq_ignore_ascii_case("Primary") {
        (600, 600)
    } else if image_type.eq_ignore_ascii_case("Thumb")
        || image_type.eq_ignore_ascii_case("Backdrop")
    {
        (960, 540)
    } else {
        (600, 600)
    }
}

async fn save_item_image(
    db: &DatabaseConnection,
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
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, size_bytes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET path = excluded.path, etag = excluded.etag, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#,
        vec![
            stable_text_id(&format!("image-asset:{item_id}:{image_type}:{image_index}")).into(),
            item_id.into(),
            image_type.into(),
            image_index.into(),
            path.to_string_lossy().to_string().into(),
            etag.into(),
            i64::try_from(bytes.len()).unwrap_or(i64::MAX).into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to upsert image asset")?;

    Ok(())
}

async fn save_user_avatar(headers: &HeaderMap, user_id: &str, body: Bytes) -> anyhow::Result<()> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let bytes = decode_image_body(content_type, &body)?;
    let extension = extension_from_content_type(content_type).unwrap_or("png");
    let directory = PathBuf::from("data").join("avatars");
    tokio::fs::create_dir_all(&directory)
        .await
        .context("failed to create avatar directory")?;

    for path in user_avatar_paths(user_id) {
        let _ = tokio::fs::remove_file(path).await;
    }

    let path = directory.join(format!(
        "{}_primary.{}",
        sanitize_file_part(user_id),
        extension
    ));
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to write avatar file: {}", path.display()))?;
    Ok(())
}

async fn item_images_inner(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let models = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .order_by_asc(image_assets::Column::ImageType)
        .order_by_asc(image_assets::Column::ImageIndex)
        .all(db)
        .await
        .context("failed to list item images")?;
    models
        .iter()
        .map(|m| {
            Ok(json!({
                "Filename": m.path.as_deref().and_then(|path| std::path::Path::new(path).file_name()).and_then(|name| name.to_str()),
                "ImageType": m.image_type,
                "ImageIndex": m.image_index,
                "Size": m.size_bytes,
            }))
        })
        .collect()
}

async fn delete_item_image_inner(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let path = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq(image_type))
        .filter(image_assets::Column::ImageIndex.eq(image_index))
        .one(db)
        .await
        .ok()
        .flatten()
        .and_then(|m| m.path);

    match ImageAssets::delete_many()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq(image_type))
        .filter(image_assets::Column::ImageIndex.eq(image_index))
        .exec(db)
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

pub fn content_type_from_path(path: &str) -> &'static str {
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

async fn find_user_avatar_path(user_id: &str) -> Option<PathBuf> {
    for path in user_avatar_paths(user_id) {
        if tokio::fs::metadata(&path).await.is_ok() {
            return Some(path);
        }
    }
    None
}

fn user_avatar_paths(user_id: &str) -> Vec<PathBuf> {
    let file_stem = format!("{}_primary", sanitize_file_part(user_id));
    ["png", "jpg", "jpeg", "webp", "gif"]
        .into_iter()
        .map(|extension| {
            PathBuf::from("data")
                .join("avatars")
                .join(format!("{file_stem}.{extension}"))
        })
        .collect()
}

fn image_options_from_query(
    query: &HashMap<String, String>,
    _image_type: &str,
) -> ImageRequestOptions {
    ImageRequestOptions {
        width: query_u32(query, &["Width", "width", "MaxWidth", "maxWidth"]),
        height: query_u32(query, &["Height", "height", "MaxHeight", "maxHeight"]),
        quality: query_u8(query, &["Quality", "quality"])
            .unwrap_or(90)
            .clamp(1, 100),
        format: image_format_from_query(query).unwrap_or(EncodedImageFormat::Png),
    }
}

fn should_process(query: &HashMap<String, String>) -> bool {
    [
        "Width",
        "width",
        "Height",
        "height",
        "MaxWidth",
        "maxWidth",
        "MaxHeight",
        "maxHeight",
        "Format",
        "format",
        "Quality",
        "quality",
    ]
    .iter()
    .any(|key| query.contains_key(*key))
}

fn query_u32(query: &HashMap<String, String>, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| query.get(*key))
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(4096))
}

fn query_u8(query: &HashMap<String, String>, keys: &[&str]) -> Option<u8> {
    keys.iter()
        .find_map(|key| query.get(*key))
        .and_then(|value| value.parse::<u8>().ok())
}

fn image_format_from_query(query: &HashMap<String, String>) -> Option<EncodedImageFormat> {
    let format = query
        .get("Format")
        .or_else(|| query.get("format"))?
        .trim()
        .to_ascii_lowercase();
    match format.as_str() {
        "jpg" | "jpeg" => Some(EncodedImageFormat::Jpeg),
        "png" => Some(EncodedImageFormat::Png),
        "webp" => Some(EncodedImageFormat::Webp),
        _ => None,
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
    let parsed: JsonValue = serde_json::from_str(text).ok()?;
    parsed
        .get("Url")
        .and_then(JsonValue::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(ToString::to_string)
}

#[allow(dead_code)]
pub async fn item_image_tags(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<JsonValue> {
    let models = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .order_by_asc(image_assets::Column::ImageType)
        .order_by_asc(image_assets::Column::ImageIndex)
        .all(db)
        .await
        .context("failed to load image tags")?;

    let mut tags = serde_json::Map::new();
    for m in &models {
        let etag = m.etag.as_deref().unwrap_or_default();
        tags.entry(m.image_type.clone())
            .or_insert_with(|| json!(etag));
    }
    Ok(JsonValue::Object(tags))
}
