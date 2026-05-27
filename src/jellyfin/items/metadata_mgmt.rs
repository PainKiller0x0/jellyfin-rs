use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::internal_error,
    library::scanner::scan_media_library,
    util::now_unix,
};

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

async fn subtitle_list_inner(db: &sea_orm::DatabaseConnection, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT stream_index, codec, language, title, is_external FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' ORDER BY stream_index ASC",
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
    _query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT library_id, item_type, COUNT(*) AS count FROM media_items WHERE is_folder = 0 GROUP BY library_id, item_type"#,
            vec![],
        ))
        .await
        .context("failed to count items")?;

    let mut counts = serde_json::Map::new();
    for row in &rows {
        let library_id: String = row.get_str("library_id")?;
        let item_type: String = row.get_str("item_type")?;
        let count: i64 = row.get_i64("count")?;
        if let Some(obj) = counts
            .entry(library_id)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            let existing = obj.get(&item_type).and_then(Value::as_i64).unwrap_or(0);
            obj.insert(item_type, json!(existing + count));
        }
    }

    Ok(Value::Object(counts))
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
    Json(json!([
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
    ]))
    .into_response()
}

/// POST /Videos/MergeVersions — merge multiple video items into one multi-version item
pub async fn merge_versions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let Some(ids) = body.get("Ids").and_then(Value::as_array) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "Error": "Ids is required" }))).into_response();
    };
    let item_ids: Vec<String> = ids.iter().filter_map(|v| v.as_str().map(ToString::to_string)).collect();
    if item_ids.len() < 2 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "Error": "Need at least 2 items to merge" }))).into_response();
    }

    let backend = state.db.get_database_backend();
    // Find the parent of the first item — this becomes the target parent
    let first_id = &item_ids[0];
    let parent_row = state.db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT parent_id, item_type FROM media_items WHERE id = ?",
        vec![first_id.as_str().into()],
    )).await;

    let (parent_id, item_type) = match parent_row {
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
    StatusCode::NO_CONTENT.into_response()
}

/// GET /Videos/{id}/AlternateSources — alternate video sources (stub)
pub async fn alternate_sources() -> Response {
    Json(json!([])).into_response()
}

/// DELETE /Videos/{id}/AlternateSources — delete alternate source (stub)
pub async fn delete_alternate_source() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// GET /AudioBooks/NextUp — audiobooks next up (stub)
pub async fn audiobooks_next_up() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /LiveTv/AvailableRecordingOptions — recording options (stub)
pub async fn available_recording_options() -> Response {
    Json(json!({})).into_response()
}

/// GET /Providers/Subtitles/Subtitles/{id} — subtitle provider (stub)
pub async fn subtitle_provider_info() -> Response {
    Json(json!({})).into_response()
}
