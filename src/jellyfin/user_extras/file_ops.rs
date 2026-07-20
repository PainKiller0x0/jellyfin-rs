use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde_json::json;
use std::{collections::HashMap, path::Path as FsPath};

use crate::{
    app::state::AppState,
    entities::media_streams::{self, Entity as MediaStreams},
    jellyfin::{
        auth::{query_user_id_or_request, request_user_id_and_admin_or_default},
        common::{internal_error, ok_response, strip_nulls, wants_json_response},
        item_queries,
    },
    library::{models::MediaItem, naming::parse_media_name},
    playback::streaming::{readable_media_path, stream_item_file},
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
        if wants_json_response(&headers) {
            return ok_response();
        }
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
        "WHERE media_items.path = ? AND media_items.is_public = 1 AND (media_items.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = media_items.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = media_items.parent_id AND parent.is_public = 1))"
    };
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            &item_queries::media_item_select_sql(where_clause),
            vec![user_id.into(), path.into()],
        ))
        .await?;
    row.map(|row| MediaItem::from_query_result(&row))
        .transpose()
        .map_err(Into::into)
}

pub async fn attachment_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_attachment_response(&state, &headers, &query, &item_id, &index).await
}

pub async fn attachment_stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, _media_source_id, index)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_attachment_response(&state, &headers, &query, &item_id, &index).await
}

async fn stream_attachment_response(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    item_id: &str,
    index: &str,
) -> Response {
    match visible_item_from_request(state, headers, query, item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return internal_error(error),
    }

    match attachment_path(&state.db, item_id, index).await {
        Ok(Some(path)) => {
            if !readable_media_path(&state.db, &path).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/octet-stream"),
                    );
                    headers.insert(
                        header::CONTENT_LENGTH,
                        HeaderValue::from_str(&bytes.len().to_string())
                            .unwrap_or_else(|_| HeaderValue::from_static("0")),
                    );
                    (headers, bytes).into_response()
                }
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn attachment_path(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    index: &str,
) -> anyhow::Result<Option<String>> {
    let index = index
        .trim()
        .trim_end_matches(".bin")
        .parse::<i64>()
        .unwrap_or(-1);
    let Some(stream) = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::StreamIndex.eq(index))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if stream.path.as_deref().map_or(true, str::is_empty) {
        return Ok(None);
    }
    if stream.stream_type == "Attachment"
        || (stream.stream_type == "Subtitle" && stream.is_external == 1)
    {
        Ok(stream.path)
    } else {
        Ok(None)
    }
}

/// HEAD /Items/{item_id}/Images/{image_type} — HEAD request for image (ETag caching)
pub async fn item_image_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    use crate::entities::{
        image_assets::{Column, Entity as ImageAssets},
        libraries::Entity as Libraries,
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => match Libraries::find_by_id(item_id.clone()).one(&state.db).await {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(error) => return internal_error(error.into()),
        },
        Err(error) => return internal_error(error),
    }

    let image_type = match canonical_head_image_type(&image_type) {
        Some(image_type) => image_type,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let fallback_type = if image_type == "Art" {
        Some("Backdrop")
    } else {
        None
    };
    let model = ImageAssets::find()
        .filter(Column::ItemId.eq(&item_id))
        .filter(Column::ImageType.eq(image_type))
        .filter(Column::ImageIndex.eq(0))
        .one(&state.db)
        .await;

    let model = match model {
        Ok(None) => {
            if let Some(fallback_type) = fallback_type {
                ImageAssets::find()
                    .filter(Column::ItemId.eq(&item_id))
                    .filter(Column::ImageType.eq(fallback_type))
                    .filter(Column::ImageIndex.eq(0))
                    .one(&state.db)
                    .await
            } else {
                Ok(None)
            }
        }
        other => other,
    };

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
                        axum::http::header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static(
                            crate::jellyfin::images::content_type_from_path(&path),
                        ),
                    );
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
        Ok(None) => StatusCode::OK.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

fn canonical_head_image_type(value: &str) -> Option<&'static str> {
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
    download_item_response(state, headers, item_id, query, Method::GET).await
}

/// HEAD /Items/{item_id}/Download — download item file metadata
pub async fn download_item_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    download_item_response(state, headers, item_id, query, Method::HEAD).await
}

async fn download_item_response(
    state: Arc<AppState>,
    headers: HeaderMap,
    item_id: String,
    query: HashMap<String, String>,
    method: Method,
) -> Response {
    match visible_item_from_request(&state, &headers, &query, &item_id).await {
        Ok(Some(item)) => {
            let filename =
                safe_download_filename(&item.title, item.container.as_deref().unwrap_or_default());
            stream_item_file(state, item_id, headers, query, method, Some(filename)).await
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

/// GET /Items/{item_id}/File — stream the original item file
pub async fn item_file_info(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_item_file(state, item_id, headers, query, Method::GET, None).await
}

/// HEAD /Items/{item_id}/File — original item file metadata
pub async fn item_file_info_head(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    stream_item_file(state, item_id, headers, query, Method::HEAD, None).await
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
    let Some(item) = item_queries::find_media_item(db, user_id, item_id).await? else {
        return Ok(Vec::new());
    };
    let Some(stack) = stack_info(&item) else {
        return Ok(Vec::new());
    };

    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &item_queries::media_item_select_sql(
                "WHERE media_items.parent_id = ? AND media_items.item_type = ? AND media_items.id <> ? AND media_items.is_public = 1 AND (EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = ?) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = ? AND parent.is_public = 1)) ORDER BY media_items.title ASC",
            ),
            vec![
                user_id.into(),
                item.parent_id.as_str().into(),
                item.item_type.as_str().into(),
                item.id.as_str().into(),
                item.parent_id.as_str().into(),
                item.parent_id.as_str().into(),
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
    use super::{additional_parts, attachment_path, item_by_file_inner, safe_download_filename};
    use crate::entities::{
        libraries::{self, Entity as Libraries},
        library_paths::{self, Entity as LibraryPaths},
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use sea_orm::{EntityTrait, Set};

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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_item(
            &db,
            "movie1",
            "Movie",
            "D:/Movie",
            "movies",
            "movies",
            "Movie",
            1,
            1,
            Some("mkv"),
        )
        .await;
        for (id, title, path, is_public) in [
            ("p1", "Movie", "D:/Movie/Movie CD1.mkv", 1),
            ("p2", "Movie", "D:/Movie/Movie CD2.mkv", 1),
            ("p3", "Movie", "D:/Movie/Movie CD3.mkv", 0),
            ("trailer", "Trailer", "D:/Movie/trailers/Trailer.mkv", 1),
        ] {
            insert_item(
                &db,
                id,
                title,
                path,
                "movies",
                "movie1",
                "Video",
                0,
                is_public,
                Some("mkv"),
            )
            .await;
        }

        let parts = additional_parts(&db, "u1", "p1").await.unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].id, "p2");
        assert!(additional_parts(&db, "u1", "p2").await.unwrap().is_empty());
        assert!(additional_parts(&db, "u1", "p3").await.unwrap().is_empty());

        insert_item(
            &db,
            "private-parent",
            "Private Parent",
            "D:/Hidden",
            "movies",
            "movies",
            "Movie",
            1,
            0,
            Some("mkv"),
        )
        .await;
        for (id, path) in [
            ("hidden-p1", "D:/Hidden/Hidden CD1.mkv"),
            ("hidden-p2", "D:/Hidden/Hidden CD2.mkv"),
        ] {
            insert_item(
                &db,
                id,
                "Hidden",
                path,
                "movies",
                "private-parent",
                "Video",
                0,
                1,
                Some("mkv"),
            )
            .await;
        }
        assert!(
            additional_parts(&db, "u1", "hidden-p1")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn attachment_path_accepts_attachments_and_external_subtitles_only() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_item(
            &db,
            "movie",
            "Movie",
            "D:/Movie/movie.mkv",
            "",
            "",
            "Video",
            0,
            1,
            Some("mkv"),
        )
        .await;
        for (id, index, stream_type, path, is_external) in [
            ("font", 1_i64, "Attachment", "D:/Movie/font.ttf", 0_i64),
            ("subtitle", 2_i64, "Subtitle", "D:/Movie/sub.srt", 1_i64),
            (
                "embedded",
                3_i64,
                "Subtitle",
                "D:/Movie/embedded.srt",
                0_i64,
            ),
            ("missing-path", 4_i64, "Attachment", "", 0_i64),
        ] {
            insert_media_stream(
                &db,
                id,
                "movie",
                index,
                stream_type,
                None,
                None,
                path,
                is_external,
            )
            .await;
        }

        assert_eq!(
            attachment_path(&db, "movie", "1").await.unwrap().as_deref(),
            Some("D:/Movie/font.ttf")
        );
        assert_eq!(
            attachment_path(&db, "movie", "2.bin")
                .await
                .unwrap()
                .as_deref(),
            Some("D:/Movie/sub.srt")
        );
        assert!(attachment_path(&db, "movie", "3").await.unwrap().is_none());
        assert!(attachment_path(&db, "movie", "4").await.unwrap().is_none());
        assert!(
            attachment_path(&db, "movie", "not-a-number")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn item_by_file_requires_known_library_path() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
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
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_library_path(&db, "lp1", "movies", &dir.to_string_lossy()).await;
        insert_item(
            &db,
            "movie",
            "Movie",
            &media_path.to_string_lossy(),
            "movies",
            "",
            "Movie",
            0,
            1,
            Some("mkv"),
        )
        .await;

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
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-private-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let media_path = dir.join("private.mkv");
        std::fs::write(&media_path, b"private").unwrap();
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_library_path(&db, "lp1", "movies", &dir.to_string_lossy()).await;
        insert_item(
            &db,
            "private",
            "Private",
            &media_path.to_string_lossy(),
            "movies",
            "",
            "Movie",
            0,
            0,
            Some("mkv"),
        )
        .await;

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

    #[tokio::test]
    async fn item_by_file_hides_items_under_private_parent() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-private-parent-file-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let media_path = dir.join("child.mkv");
        std::fs::write(&media_path, b"child").unwrap();
        insert_library(&db, "movies", "Movies", "movies").await;
        insert_library_path(&db, "lp1", "movies", &dir.to_string_lossy()).await;
        insert_item(
            &db,
            "private-parent",
            "Private Parent",
            "D:/hidden",
            "movies",
            "",
            "Movie",
            1,
            0,
            Some("mkv"),
        )
        .await;
        insert_item(
            &db,
            "public-child",
            "Public Child",
            &media_path.to_string_lossy(),
            "movies",
            "private-parent",
            "Video",
            0,
            1,
            Some("mkv"),
        )
        .await;

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
            "public-child"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    async fn insert_library(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        name: &str,
        collection_type: &str,
    ) {
        Libraries::insert(libraries::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            collection_type: Set(collection_type.to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_library_path(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        library_id: &str,
        path: &str,
    ) {
        LibraryPaths::insert(library_paths::ActiveModel {
            id: Set(id.to_string()),
            library_id: Set(library_id.to_string()),
            path: Set(path.to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_item(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        container: Option<&str>,
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
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_media_stream(
        db: &sea_orm::DatabaseConnection,
        id: &str,
        item_id: &str,
        stream_index: i64,
        stream_type: &str,
        codec: Option<&str>,
        language: Option<&str>,
        path: &str,
        is_external: i64,
    ) {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            stream_index: Set(stream_index),
            stream_type: Set(stream_type.to_string()),
            codec: Set(codec.map(ToString::to_string)),
            language: Set(language.map(ToString::to_string)),
            path: Set(Some(path.to_string())),
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
}
