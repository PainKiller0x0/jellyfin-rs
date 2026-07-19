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
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        image_assets::{self, Entity as ImageAssets},
        libraries::Entity as Libraries,
    },
    jellyfin::common::{
        image as placeholder_image, internal_error, ok_response, wants_json_response,
    },
    library::image_processing::{
        EncodedImageFormat, ImageRequestOptions, create_collage, create_placeholder, process_image,
    },
    library::path_utils,
    util::{now_unix, stable_text_id},
};

mod remote;

pub use remote::{
    download_remote_image, image_by_name_remote, remote_images, remote_images_providers,
};

const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_BASE64_IMAGE_BODY_BYTES: usize = 14 * 1024 * 1024;
const MAX_REMOTE_IMAGE_URL_BODY_BYTES: usize = 4096;
const MAX_IMAGE_INDEX: i64 = 255;
const MAX_IMAGE_ID_BYTES: usize = 128;

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

#[derive(Deserialize)]
pub(crate) struct UserImagePath {
    user_id: String,
    image_type: Option<String>,
    index: Option<i64>,
}

pub async fn item_images(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(response) = ensure_visible_item_or_library(&state, &headers, &query, &item_id).await
    {
        return response;
    }
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
    if let Err(response) = ensure_visible_item_or_library(&state, &headers, &query, &item_id).await
    {
        return response;
    }
    serve_item_image(&state.db, &headers, &query, &item_id, &image_type, 0).await
}

pub async fn get_item_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((item_id, first, second)): Path<(String, String, String)>,
) -> Response {
    if let Err(response) = ensure_visible_item_or_library(&state, &headers, &query, &item_id).await
    {
        return response;
    }
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

pub async fn get_item_image_legacy_path(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(mut query): Query<HashMap<String, String>>,
    Path((
        item_id,
        image_type,
        image_index,
        _tag,
        format,
        max_width,
        max_height,
        _percent_played,
        _unplayed_count,
    )): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    if let Err(response) = ensure_visible_item_or_library(&state, &headers, &query, &item_id).await
    {
        return response;
    }
    query.entry("Format".to_string()).or_insert(format);
    query.entry("MaxWidth".to_string()).or_insert(max_width);
    query.entry("MaxHeight".to_string()).or_insert(max_height);
    serve_item_image(
        &state.db,
        &headers,
        &query,
        &item_id,
        &image_type,
        image_index.parse::<i64>().unwrap_or_default(),
    )
    .await
}

pub async fn upload_item_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let image_type = match canonical_image_type(&image_type) {
        Some(image_type) => image_type,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": "Unsupported image type" })),
            )
                .into_response();
        }
    };
    if let Some(image_url) = parse_image_url_body(&body) {
        match remote::download_and_cache_image(&state, &item_id, &image_type, &image_url).await {
            Ok(()) => return StatusCode::NO_CONTENT.into_response(),
            Err(error) if remote::is_rejected_remote_image_url(&error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "Error": error.to_string() })),
                )
                    .into_response();
            }
            Err(error) => return internal_error(error),
        }
    }
    match save_item_image(&state.db, &headers, &item_id, &image_type, 0, body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_write_error(error),
    }
}

pub async fn upload_item_image_with_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, first, second)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let (image_type, image_index) = match image_type_and_index(&first, &second) {
        Ok(value) => value,
        Err(error) => return image_write_error(error),
    };
    if let Some(image_url) = parse_image_url_body(&body) {
        match remote::download_and_cache_image(&state, &item_id, &image_type, &image_url).await {
            Ok(()) => return StatusCode::NO_CONTENT.into_response(),
            Err(error) if remote::is_rejected_remote_image_url(&error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "Error": error.to_string() })),
                )
                    .into_response();
            }
            Err(error) => return internal_error(error),
        }
    }
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
        Err(error) => image_write_error(error),
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
    Path((item_id, first, second)): Path<(String, String, String)>,
) -> Response {
    let (image_type, index) = match image_type_and_index(&first, &second) {
        Ok(value) => value,
        Err(error) => return image_write_error(error),
    };
    delete_item_image_inner(&state.db, &item_id, &image_type, index).await
}

pub async fn user_avatar(Path(path): Path<UserImagePath>) -> Response {
    let Some(user_id) = user_avatar_path_user_id(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
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

pub async fn current_user_avatar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = crate::jellyfin::auth::request_user_id_or_default(&state, &headers, &query).await;
    user_avatar_with_head(user_id, headers, false).await
}

pub async fn current_user_avatar_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = crate::jellyfin::auth::request_user_id_or_default(&state, &headers, &query).await;
    user_avatar_with_head(user_id, headers, true).await
}

pub async fn upload_user_avatar(
    headers: HeaderMap,
    Path(path): Path<UserImagePath>,
    body: Bytes,
) -> Response {
    let Some(user_id) = user_avatar_path_user_id(&path) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Only primary user images are supported" })),
        )
            .into_response();
    };
    match save_user_avatar(&headers, &user_id, body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_write_error(error),
    }
}

pub async fn delete_user_avatar(Path(path): Path<UserImagePath>) -> Response {
    let Some(user_id) = user_avatar_path_user_id(&path) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Only primary user images are supported" })),
        )
            .into_response();
    };
    for path in user_avatar_paths(&user_id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn upload_current_user_avatar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let user_id = crate::jellyfin::auth::request_user_id_or_default(&state, &headers, &query).await;
    match save_user_avatar(&headers, &user_id, body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_write_error(error),
    }
}

pub async fn delete_current_user_avatar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = crate::jellyfin::auth::request_user_id_or_default(&state, &headers, &query).await;
    for path in user_avatar_paths(&user_id) {
        let _ = tokio::fs::remove_file(path).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn user_avatar_with_head(user_id: String, headers: HeaderMap, head: bool) -> Response {
    let Some(path) = find_user_avatar_path(&user_id).await else {
        return placeholder_image().await.into_response();
    };
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(_) => return placeholder_image().await.into_response(),
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let etag = stable_text_id(&format!("avatar:{user_id}:{}:{modified}", metadata.len()));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_matches('"') == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_from_path(&path.to_string_lossy())),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response_headers.insert(header::ETAG, value);
    }
    if head {
        return (response_headers, Body::empty()).into_response();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => (response_headers, Body::from(bytes)).into_response(),
        Err(_) => placeholder_image().await.into_response(),
    }
}

async fn serve_item_image(
    db: &DatabaseConnection,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let image_type = match canonical_image_type(image_type) {
        Some(image_type) => image_type,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    if wants_json_response(headers) {
        return ok_response();
    }
    let options = image_options_from_query(query, image_type);
    let model = match find_item_image_asset(db, item_id, image_type, image_index)
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
    if !image_storage_path_allowed(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }

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

async fn ensure_visible_item_or_library(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    item_id: &str,
) -> Result<(), Response> {
    match crate::jellyfin::user_extras::visible_item_from_request(state, headers, query, item_id)
        .await
    {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(error) => return Err(internal_error(error)),
    }

    match Libraries::find_by_id(item_id.to_string())
        .one(&state.db)
        .await
    {
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            let (_, is_admin) =
                crate::jellyfin::auth::request_user_id_and_admin_or_default(state, headers, query)
                    .await;
            match crate::jellyfin::persons::has_person_relation(&state.db, item_id, is_admin).await
            {
                Ok(true) => Ok(()),
                Ok(false) => Err(StatusCode::NOT_FOUND.into_response()),
                Err(error) => Err(internal_error(error)),
            }
        }
        Err(error) => Err(internal_error(error.into())),
    }
}

async fn find_item_image_asset(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> anyhow::Result<Option<image_assets::Model>> {
    if let Some(model) = find_item_image_asset_by_type(db, item_id, image_type, image_index).await?
    {
        return Ok(Some(model));
    }
    if image_type.eq_ignore_ascii_case("Art") {
        return find_item_image_asset_by_type(db, item_id, "Backdrop", image_index).await;
    }
    Ok(None)
}

async fn find_item_image_asset_by_type(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> anyhow::Result<Option<image_assets::Model>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT id, item_id, image_type, image_index, path, etag, width, height, size_bytes, created_at, updated_at FROM image_assets WHERE item_id = ? AND image_type = ? AND CAST(image_index AS TEXT) = ? LIMIT 1",
            vec![item_id.into(), image_type.into(), image_index.to_string().into()],
        ))
        .await
        .context("failed to query item image asset")?;

    row.map(|row| {
        Ok(image_assets::Model {
            id: row.get_str("id")?,
            item_id: row.get_str("item_id")?,
            image_type: row.get_str("image_type")?,
            image_index: row.get_i64("image_index")?,
            path: row.get_opt_str("path")?,
            etag: row.get_opt_str("etag")?,
            width: row.get_opt_i64("width")?,
            height: row.get_opt_i64("height")?,
            size_bytes: row.get_opt_i64("size_bytes")?,
            created_at: row.get_i64("created_at")?,
            updated_at: row.get_i64("updated_at")?,
        })
    })
    .transpose()
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
    let sql = format!(
        "SELECT image_assets.path FROM image_assets JOIN media_items mi ON mi.id = image_assets.item_id WHERE mi.parent_id = ? AND {} AND image_assets.image_type = ? ORDER BY mi.title ASC, image_assets.image_index ASC LIMIT 4",
        visible_media_item_sql("mi")
    );
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &sql,
            vec![item_id.into(), preferred_type.into()],
        ))
        .await
        .context("failed to find child images for collage")?;

    let mut images = Vec::new();
    for row in &rows {
        let path: String = row.get_str("path")?;
        if image_storage_path_allowed(&path) {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                images.push(bytes);
            }
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
    let item_id = validate_image_id(item_id, "item id")?;
    let image_type = canonical_image_type(image_type)
        .ok_or_else(|| anyhow::anyhow!("Unsupported image type"))?;
    let image_index = validate_image_index(image_index)?;
    if !image_item_exists(db, item_id).await? {
        anyhow::bail!("item not found");
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let (bytes, extension) = decode_image_body(content_type, &body)?;
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
    db.execute(crate::db::helpers::pg_statement(
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
    let user_id = validate_image_id(user_id, "user id")?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream");
    let (bytes, extension) = decode_image_body(content_type, &body)?;
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
    let item_id = match validate_image_id(item_id, "item id") {
        Ok(item_id) => item_id,
        Err(error) => return image_write_error(error),
    };
    let image_type = match canonical_image_type(image_type) {
        Some(image_type) => image_type,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": "Unsupported image type" })),
            )
                .into_response();
        }
    };
    let image_index = match validate_image_index(image_index) {
        Ok(image_index) => image_index,
        Err(error) => return image_write_error(error),
    };
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
                if image_storage_path_allowed(&path) {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

fn decode_image_body(content_type: &str, body: &[u8]) -> anyhow::Result<(Vec<u8>, &'static str)> {
    if body.is_empty() {
        anyhow::bail!("image body is empty");
    }
    let content_type = normalized_content_type(content_type);
    if supported_image_content_type(&content_type) || content_type == "application/octet-stream" {
        if body.len() > MAX_IMAGE_BYTES {
            anyhow::bail!("image is too large");
        }
        let extension = detect_image_extension(body)
            .ok_or_else(|| anyhow::anyhow!("unsupported image content type"))?;
        return Ok((body.to_vec(), extension));
    }
    if body.len() > MAX_BASE64_IMAGE_BODY_BYTES {
        anyhow::bail!("image is too large");
    }
    if content_type.starts_with("image/") {
        anyhow::bail!("unsupported image content type");
    }
    let text = std::str::from_utf8(body).context("image body is not valid utf-8")?;
    let encoded = text
        .split_once(',')
        .map_or(text, |(_, encoded)| encoded)
        .trim();
    let bytes = general_purpose::STANDARD
        .decode(encoded)
        .context("failed to decode base64 image body")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("image is too large");
    }
    let extension = detect_image_extension(&bytes)
        .ok_or_else(|| anyhow::anyhow!("unsupported image content type"))?;
    Ok((bytes, extension))
}

fn extension_from_content_type(content_type: &str) -> Option<&'static str> {
    match normalized_content_type(content_type).as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn supported_image_content_type(content_type: &str) -> bool {
    extension_from_content_type(content_type).is_some()
}

fn normalized_content_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

pub(super) fn detect_image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1A\n") {
        Some("png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
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
    content_type_from_extension(
        std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default(),
    )
}

pub(super) fn content_type_from_extension(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

pub(crate) fn image_storage_path_allowed(path: &str) -> bool {
    path_utils::path_within_roots(
        path,
        &[PathBuf::from("data")
            .join("images")
            .to_string_lossy()
            .to_string()],
    )
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

fn user_avatar_path_user_id(path: &UserImagePath) -> Option<String> {
    if path
        .image_type
        .as_deref()
        .is_some_and(|image_type| !image_type.eq_ignore_ascii_case("Primary"))
    {
        return None;
    }
    if path.index.is_some_and(|index| index != 0) {
        return None;
    }
    Some(path.user_id.clone())
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
    if body.len() > MAX_REMOTE_IMAGE_URL_BODY_BYTES {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let parsed: JsonValue = serde_json::from_str(text).ok()?;
    parsed
        .get("Url")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty() && url.len() <= 2048)
        .map(ToString::to_string)
}

fn is_image_too_large(error: &anyhow::Error) -> bool {
    error.to_string().contains("image is too large")
}

fn image_write_error(error: anyhow::Error) -> Response {
    let message = error.to_string();
    if is_image_too_large(&error) {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Image is too large" })),
        )
            .into_response();
    }
    if message.contains("not found") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Item not found" })),
        )
            .into_response();
    }
    if message.contains("empty")
        || message.contains("invalid")
        || message.contains("required")
        || message.contains("unsupported")
        || message.contains("failed to decode base64")
        || message.contains("not valid utf-8")
    {
        return (StatusCode::BAD_REQUEST, Json(json!({ "Error": message }))).into_response();
    }
    internal_error(error)
}

fn image_type_and_index(first: &str, second: &str) -> anyhow::Result<(&'static str, i64)> {
    if let Ok(index) = first.parse::<i64>() {
        let image_type = canonical_image_type(second)
            .ok_or_else(|| anyhow::anyhow!("Unsupported image type"))?;
        return Ok((image_type, validate_image_index(index)?));
    }
    let image_type =
        canonical_image_type(first).ok_or_else(|| anyhow::anyhow!("Unsupported image type"))?;
    let index = second
        .trim()
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("invalid image index"))?;
    Ok((image_type, validate_image_index(index)?))
}

fn canonical_image_type(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "primary" => Some("Primary"),
        "art" => Some("Art"),
        "backdrop" => Some("Backdrop"),
        "banner" => Some("Banner"),
        "logo" => Some("Logo"),
        "thumb" => Some("Thumb"),
        "disc" => Some("Disc"),
        "box" => Some("Box"),
        "boxrear" | "box_rear" | "box-rear" => Some("BoxRear"),
        "screenshot" => Some("Screenshot"),
        "menu" => Some("Menu"),
        "chapter" => Some("Chapter"),
        "profile" => Some("Profile"),
        _ => None,
    }
}

fn validate_image_index(index: i64) -> anyhow::Result<i64> {
    if (0..=MAX_IMAGE_INDEX).contains(&index) {
        Ok(index)
    } else {
        anyhow::bail!("invalid image index")
    }
}

fn validate_image_id<'a>(value: &'a str, label: &str) -> anyhow::Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("{label} is required");
    }
    if value.len() > MAX_IMAGE_ID_BYTES {
        anyhow::bail!("{label} is too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!("invalid {label}");
    }
    Ok(value)
}

async fn image_item_exists(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    Ok(db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT id FROM media_items WHERE id = ? UNION ALL SELECT id FROM libraries WHERE id = ? LIMIT 1",
            vec![item_id.into(), item_id.into()],
        ))
        .await
        .context("failed to validate image item")?
        .is_some())
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
    add_art_tag_fallback(&mut tags);
    Ok(JsonValue::Object(tags))
}

pub(crate) fn add_art_tag_fallback(tags: &mut serde_json::Map<String, JsonValue>) {
    if !tags.contains_key("Art") {
        if let Some(backdrop) = tags.get("Backdrop").cloned() {
            tags.insert("Art".to_string(), backdrop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_IMAGE_BYTES, add_art_tag_fallback, canonical_image_type, collage_source_images,
        decode_image_body, image_storage_path_allowed, image_type_and_index, is_image_too_large,
        item_images_inner,
    };
    use sea_orm::ConnectionTrait;
    use serde_json::json;
    use std::fs;

    #[test]
    fn image_storage_path_allowed_rejects_sibling_directory() {
        let image_dir = std::path::PathBuf::from("data").join("images");
        let sibling_dir = std::path::PathBuf::from("data").join("images-other");
        fs::create_dir_all(&image_dir).unwrap();
        fs::create_dir_all(&sibling_dir).unwrap();
        let image = image_dir.join(format!("{}.png", uuid::Uuid::new_v4()));
        let sibling = sibling_dir.join(format!("{}.png", uuid::Uuid::new_v4()));
        fs::write(&image, b"ok").unwrap();
        fs::write(&sibling, b"no").unwrap();

        assert!(image_storage_path_allowed(&image.to_string_lossy()));
        assert!(!image_storage_path_allowed(&sibling.to_string_lossy()));

        let _ = fs::remove_file(image);
        let _ = fs::remove_file(sibling);
    }

    #[test]
    fn image_body_size_is_limited() {
        let oversized = vec![0_u8; MAX_IMAGE_BYTES + 1];
        let error = decode_image_body("image/png", &oversized).unwrap_err();
        assert!(is_image_too_large(&error));

        let base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, oversized);
        let error = decode_image_body("text/plain", base64.as_bytes()).unwrap_err();
        assert!(is_image_too_large(&error));
    }

    #[test]
    fn image_upload_rejects_unsupported_types_and_indexes() {
        assert_eq!(canonical_image_type("backdrop"), Some("Backdrop"));
        assert!(canonical_image_type("../bad").is_none());
        assert_eq!(
            image_type_and_index("Backdrop", "2").unwrap(),
            ("Backdrop", 2)
        );
        assert_eq!(
            image_type_and_index("2", "Backdrop").unwrap(),
            ("Backdrop", 2)
        );
        assert!(image_type_and_index("Backdrop", "-1").is_err());

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        let error = decode_image_body("image/svg+xml", svg).unwrap_err();
        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn art_tag_falls_back_to_backdrop_tag() {
        let mut tags = serde_json::Map::new();
        tags.insert("Backdrop".to_string(), json!("backdrop-tag"));
        add_art_tag_fallback(&mut tags);
        assert_eq!(tags.get("Art"), Some(&json!("backdrop-tag")));
    }

    #[tokio::test]
    async fn private_item_images_still_require_item_visibility_gate() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', '', 'Movie', 0, 0, 1, 1, 1)",
            vec!["private".into(), "Private".into(), "/tmp/private.mkv".into()],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, created_at, updated_at) VALUES (?, ?, 'Primary', 0, 'data/images/private.png', 'tag', 1, 1)",
            vec!["img1".into(), "private".into()],
        ))
        .await
        .unwrap();

        assert!(
            crate::jellyfin::item_queries::find_media_item(&db, "u1", "private")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(item_images_inner(&db, "private").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn collage_sources_hide_items_under_private_parents() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, parent_id, is_public) in [
            ("album", "", 1),
            ("public-child", "album", 1),
            ("private-parent", "", 0),
            ("hidden-child", "private-parent", 1),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, 'Movie', 1, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    format!("/tmp/{id}").into(),
                    parent_id.into(),
                    is_public.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let image_dir = std::path::PathBuf::from("data").join("images");
        fs::create_dir_all(&image_dir).unwrap();
        let public_image = image_dir.join(format!("public-{}.png", uuid::Uuid::new_v4()));
        let hidden_image = image_dir.join(format!("hidden-{}.png", uuid::Uuid::new_v4()));
        fs::write(&public_image, b"public").unwrap();
        fs::write(&hidden_image, b"hidden").unwrap();
        for (asset_id, item_id, path) in [
            ("public-image", "public-child", &public_image),
            ("hidden-image", "hidden-child", &hidden_image),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, created_at, updated_at) VALUES (?, ?, 'Primary', 0, ?, ?, 1, 1)",
                vec![
                    format!("{asset_id}-{}", uuid::Uuid::new_v4()).into(),
                    item_id.into(),
                    path.to_string_lossy().to_string().into(),
                    asset_id.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let images = collage_source_images(&db, "album", "Primary")
            .await
            .unwrap();
        assert_eq!(images, vec![b"public".to_vec()]);
        assert!(
            collage_source_images(&db, "private-parent", "Primary")
                .await
                .unwrap()
                .is_empty()
        );

        let _ = fs::remove_file(public_image);
        let _ = fs::remove_file(hidden_image);
    }
}
