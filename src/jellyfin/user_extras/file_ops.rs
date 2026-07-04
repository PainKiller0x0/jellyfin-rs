use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::json;
use std::{collections::HashMap, path::Path as FsPath};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        auth::{query_user_id_or_request, request_user_id_and_admin_or_default},
        common::{internal_error, strip_nulls},
        item_queries,
    },
    library::{models::MediaItem, naming::parse_media_name},
    playback::streaming::readable_media_path,
};

/// GET /Videos/{item_id}/{media_source_id}/Attachments/{index}/Stream — font attachments
pub async fn item_by_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = query
        .get("Path")
        .or_else(|| query.get("path"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (user_id, is_admin) = request_user_id_and_admin_or_default(&state, &headers, &query).await;
    match item_by_file_inner(&state.db, &user_id, path, is_admin).await {
        Ok(Some(item)) => Json(strip_nulls(item.to_jellyfin_json())).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_by_file_inner(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    path: &str,
    include_private: bool,
) -> anyhow::Result<Option<MediaItem>> {
    if !readable_media_path(db, path).await {
        return Ok(None);
    }
    let where_clause = if include_private {
        "WHERE media_items.path = ?"
    } else {
        "WHERE media_items.path = ? AND media_items.is_public = 1"
    };
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            &item_queries::media_item_select_sql(where_clause),
            vec![user_id.into(), path.into()],
        ))
        .await?;
    row.map(|row| MediaItem::from_query_result(&row))
        .transpose()
        .map_err(Into::into)
}

pub async fn attachment_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }

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
                if !readable_media_path(&state.db, &path).await {
                    return StatusCode::NOT_FOUND.into_response();
                }
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
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    use crate::entities::image_assets::{Column, Entity as ImageAssets};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }

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
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    item_image_head(
        State(state),
        headers,
        Path((item_id, image_type)),
        Query(query),
    )
    .await
}

/// GET /Items/{item_id}/Download — download item file
pub async fn download_item(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(item)) => {
            if !readable_media_path(&state.db, &item.path).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            let container = item.container.unwrap_or_default();
            match tokio::fs::read(&item.path).await {
                Ok(bytes) => {
                    let filename = safe_download_filename(&item.title, &container);
                    (
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
                        .into_response()
                }
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

fn safe_download_filename(title: &str, container: &str) -> String {
    let mut stem = sanitize_download_part(title);
    if stem.is_empty() {
        stem = "download".to_string();
    }
    let extension = sanitize_download_part(container);
    if extension.is_empty() {
        stem
    } else {
        format!("{stem}.{extension}")
    }
}

fn sanitize_download_part(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        .collect::<String>()
        .trim_matches([' ', '.'])
        .to_string()
}

/// GET /Items/{item_id}/File — get item file info
pub async fn item_file_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(item)) => {
            let container = item.container.unwrap_or_default();
            Json(json!({
                "Path": item.path,
                "Name": format!("{}.{}", item.title, container),
                "Size": item.size_bytes,
            }))
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub(crate) async fn visible_item_from_request(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    item_id: &str,
) -> anyhow::Result<Option<MediaItem>> {
    let (user_id, is_admin) = request_user_id_and_admin_or_default(state, headers, query).await;
    if is_admin {
        item_queries::find_media_item_for_admin(&state.db, &user_id, item_id).await
    } else {
        item_queries::find_media_item(&state.db, &user_id, item_id).await
    }
}

/// GET /Videos/{item_id}/AdditionalParts — multi-part video support
pub async fn video_additional_parts(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    match additional_parts(&state.db, &user_id, &item_id).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({
                "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(),
                "TotalRecordCount": total,
                "StartIndex": 0,
            }))
            .into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn additional_parts(
    db: &sea_orm::DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let sql = item_queries::media_item_select_sql(
        "WHERE media_items.id = ? AND media_items.is_public = 1",
    );
    let Some(row) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![user_id.into(), item_id.into()],
        ))
        .await?
    else {
        return Ok(Vec::new());
    };
    let item = MediaItem::from_query_result(&row)?;
    let Some(stack) = stack_info(&item) else {
        return Ok(Vec::new());
    };

    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &item_queries::media_item_select_sql(
                "WHERE media_items.parent_id = ? AND media_items.item_type = ? AND media_items.id <> ? AND media_items.is_public = 1 ORDER BY media_items.title ASC",
            ),
            vec![
                user_id.into(),
                item.parent_id.as_str().into(),
                item.item_type.as_str().into(),
                item.id.as_str().into(),
            ],
        ))
        .await?;
    let mut parts = item_queries::decode_media_items(&rows)?
        .into_iter()
        .filter(|candidate| {
            stack_info(candidate)
                .map(|candidate_stack| candidate_stack.0 == stack.0 && candidate_stack.1 > stack.1)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    parts.sort_by_key(|item| stack_info(item).map(|(_, part)| part).unwrap_or(i64::MAX));
    Ok(parts)
}

fn stack_info(item: &MediaItem) -> Option<(String, i64)> {
    let parsed = parse_media_name(FsPath::new(&item.path), &item.collection_type);
    Some((parsed.stack_key?, parsed.stack_part?))
}

#[cfg(test)]
mod tests {
    use super::{additional_parts, item_by_file_inner, safe_download_filename};
    use sea_orm::{ConnectionTrait, Database};

    #[test]
    fn download_filename_rejects_header_breaking_characters() {
        assert_eq!(
            safe_download_filename("movie\"\r\nbad", "mkv"),
            "moviebad.mkv"
        );
        assert_eq!(safe_download_filename("../secret", "m/p4"), "secret.mp4");
        assert_eq!(safe_download_filename("", ""), "download");
    }

    #[tokio::test]
    async fn additional_parts_returns_only_later_stack_parts() {
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
        for (id, title, path, is_public) in [
            ("p1", "Movie", "D:/Movie/Movie CD1.mkv", 1),
            ("p2", "Movie", "D:/Movie/Movie CD2.mkv", 1),
            ("p3", "Movie", "D:/Movie/Movie CD3.mkv", 0),
            ("trailer", "Trailer", "D:/Movie/trailers/Trailer.mkv", 1),
        ] {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, container, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', 'movie1', 'Video', 0, ?, 'mkv', 1, 1, 1)",
                vec![id.into(), title.into(), path.into(), is_public.into()],
            ))
            .await
            .unwrap();
        }

        let parts = additional_parts(&db, "u1", "p1").await.unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].id, "p2");
        assert!(additional_parts(&db, "u1", "p2").await.unwrap().is_empty());
        assert!(additional_parts(&db, "u1", "p3").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn item_by_file_requires_known_library_path() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-item-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let media_path = dir.join("movie.mkv");
        let other_path = dir.with_file_name(format!(
            "{}-other.mkv",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&media_path, b"movie").unwrap();
        std::fs::write(&other_path, b"other").unwrap();
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
            "INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, 1)",
            vec![
                "lp1".into(),
                "movies".into(),
                dir.to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', '', 'Movie', 0, 'mkv', 1, 1, 1)",
            vec![
                "movie".into(),
                "Movie".into(),
                media_path.to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();

        let item = item_by_file_inner(&db, "u1", &media_path.to_string_lossy(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.id, "movie");
        assert!(
            item_by_file_inner(&db, "u1", &other_path.to_string_lossy(), false)
                .await
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_file(&other_path).unwrap();
    }

    #[tokio::test]
    async fn item_by_file_hides_private_items_unless_admin() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-private-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let media_path = dir.join("private.mkv");
        std::fs::write(&media_path, b"private").unwrap();
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
            "INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, 1)",
            vec![
                "lp1".into(),
                "movies".into(),
                dir.to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, container, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', '', 'Movie', 0, 0, 'mkv', 1, 1, 1)",
            vec![
                "private".into(),
                "Private".into(),
                media_path.to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();

        assert!(
            item_by_file_inner(&db, "u1", &media_path.to_string_lossy(), false)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            item_by_file_inner(&db, "u1", &media_path.to_string_lossy(), true)
                .await
                .unwrap()
                .unwrap()
                .id,
            "private"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
