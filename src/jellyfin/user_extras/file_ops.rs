use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{app::state::AppState, db::row_ext::QueryResultExt};

/// GET /Artists/{name}/Images/{image_type} — serve artist image
pub async fn artist_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((name, image_type)): Path<(String, String)>,
) -> Response {
    // Look up artist by name and serve their image
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM people WHERE name = ?",
            vec![name.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            if let Ok(person_id) = r.get_str("id") {
                return crate::jellyfin::persons::person_image(
                    State(state),
                    headers,
                    Path((person_id, image_type)),
                )
                .await;
            }
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /Videos/{item_id}/{media_source_id}/Attachments/{index}/Stream — font attachments
pub async fn attachment_stream(
    State(state): State<Arc<AppState>>,
    Path((item_id, _media_source_id, index)): Path<(String, String, String)>,
) -> Response {
    // Look for external subtitle attachment files
    let backend = state.db.get_database_backend();
    let row = state.db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT path FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' AND is_external = 1 AND stream_index = ?",
            vec![item_id.into(), index.parse::<i64>().unwrap_or(0).into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            if let Ok(path) = r.get_str("path") {
                match tokio::fs::read(&path).await {
                    Ok(bytes) => {
                        return (
                            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                            bytes,
                        )
                            .into_response();
                    }
                    Err(_) => return StatusCode::NOT_FOUND.into_response(),
                }
            }
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// HEAD /Items/{item_id}/Images/{image_type} — HEAD request for image (ETag caching)
pub async fn item_image_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
) -> Response {
    use crate::entities::image_assets::{Column, Entity as ImageAssets};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let model = ImageAssets::find()
        .filter(Column::ItemId.eq(&item_id))
        .filter(Column::ImageType.eq(&image_type))
        .filter(Column::ImageIndex.eq(0))
        .one(&state.db)
        .await;

    match model {
        Ok(Some(m)) => {
            let etag = m.etag.unwrap_or_default();
            if headers
                .get(axum::http::header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim_matches('"') == etag)
            {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            let path = m.path.unwrap_or_default();
            match tokio::fs::metadata(&path).await {
                Ok(meta) => {
                    let mut resp_headers = axum::http::HeaderMap::new();
                    resp_headers.insert(
                        axum::http::header::CONTENT_LENGTH,
                        axum::http::HeaderValue::from_str(&meta.len().to_string()).unwrap(),
                    );
                    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
                        resp_headers.insert(axum::http::header::ETAG, v);
                    }
                    (resp_headers, StatusCode::OK).into_response()
                }
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// HEAD /Items/{item_id}/Images/{image_type}/{index} — HEAD for indexed image
pub async fn item_image_index_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type, _index)): Path<(String, String, i64)>,
) -> Response {
    item_image_head(State(state), headers, Path((item_id, image_type))).await
}

/// GET /Items/{item_id}/Download — download item file
pub async fn download_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT path, title, container FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            if let Ok(path) = r.get_str("path") {
                let title = r.get_opt_str("title").ok().flatten().unwrap_or_default();
                let container = r
                    .get_opt_str("container")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                match tokio::fs::read(&path).await {
                    Ok(bytes) => {
                        let filename = format!("{}.{}", title, container);
                        return (
                            [
                                (
                                    axum::http::header::CONTENT_TYPE,
                                    "application/octet-stream".to_string(),
                                ),
                                (
                                    axum::http::header::CONTENT_DISPOSITION,
                                    format!("attachment; filename=\"{}\"", filename),
                                ),
                            ],
                            bytes,
                        )
                            .into_response();
                    }
                    Err(_) => return StatusCode::NOT_FOUND.into_response(),
                }
            }
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /Items/{item_id}/File — get item file info
pub async fn item_file_info(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT path, title, container, size_bytes FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            let path = r.get_opt_str("path").ok().flatten().unwrap_or_default();
            let title = r.get_opt_str("title").ok().flatten().unwrap_or_default();
            let container = r
                .get_opt_str("container")
                .ok()
                .flatten()
                .unwrap_or_default();
            let size = r.get_opt_i64("size_bytes").ok().flatten();
            Json(json!({
                "Path": path,
                "Name": format!("{}.{}", title, container),
                "Size": size,
            }))
            .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /Videos/{item_id}/AdditionalParts — multi-part video support
pub async fn video_additional_parts(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    // Check if this video has additional parts (same parent folder, same type)
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT parent_id, item_type FROM media_items WHERE id = ?",
            vec![item_id.clone().into()],
        ))
        .await;

    match row {
        Ok(Some(r)) => {
            let parent_id = r
                .get_opt_str("parent_id")
                .ok()
                .flatten()
                .unwrap_or_default();
            let item_type = r
                .get_opt_str("item_type")
                .ok()
                .flatten()
                .unwrap_or_default();

            let rows = state.db
                .query_all(crate::db::helpers::portable_statement(
                    backend,
                    "SELECT id, title, path, container, size_bytes FROM media_items WHERE parent_id = ? AND item_type = ? AND id <> ? ORDER BY title ASC",
                    vec![parent_id.into(), item_type.into(), item_id.into()],
                ))
                .await
                .unwrap_or_default();

            let parts: Vec<Value> = rows
                .iter()
                .filter_map(|r| {
                    let id = r.get_str("id").ok()?;
                    let title = r.get_str("title").ok()?;
                    let path = r.get_str("path").ok()?;
                    let container = r.get_opt_str("container").ok().flatten()?;
                    let size = r.get_opt_i64("size_bytes").ok().flatten();
                    Some(json!({
                        "Id": id,
                        "Name": title,
                        "Path": path,
                        "Container": container,
                        "Size": size,
                    }))
                })
                .collect();

            Json(json!({ "Items": parts, "TotalRecordCount": parts.len() })).into_response()
        }
        _ => Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response(),
    }
}
