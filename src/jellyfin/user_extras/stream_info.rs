use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
};

/// DELETE /Users/{user_id}/TrackSelections/{track_type} — clear track selections
pub async fn clear_track_selections(
    State(_state): State<Arc<AppState>>,
    Path((_user_id, _track_type)): Path<(String, String)>,
) -> Response {
    // No-op for now - track selections are client-side
    StatusCode::NO_CONTENT.into_response()
}

/// GET /AudioCodecs — list audio codecs from media_streams
pub async fn audio_codecs(
    State(state): State<Arc<AppState>>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT DISTINCT codec FROM media_streams WHERE stream_type = 'Audio' AND codec IS NOT NULL AND codec <> '' ORDER BY codec ASC",
            vec![],
        ))
        .await
        .unwrap_or_default();

    let codecs: Vec<Value> = rows.iter()
        .filter_map(|r| r.get_opt_str("codec").ok().flatten().map(|c| json!({"Name": c, "Id": c})))
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /AudioLayouts — list audio channel layouts
pub async fn audio_layouts(
    State(state): State<Arc<AppState>>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT DISTINCT channels FROM media_streams WHERE stream_type = 'Audio' AND channels IS NOT NULL ORDER BY channels ASC",
            vec![],
        ))
        .await
        .unwrap_or_default();

    let layouts: Vec<Value> = rows.iter()
        .filter_map(|r| {
            r.get_opt_i64("channels").ok().flatten().map(|ch| {
                let name = match ch {
                    1 => "Mono".to_string(),
                    2 => "Stereo".to_string(),
                    6 => "5.1".to_string(),
                    8 => "7.1".to_string(),
                    n => format!("{}ch", n),
                };
                json!({"Name": name, "Id": ch})
            })
        })
        .collect();

    Json(json!({ "Items": layouts, "TotalRecordCount": layouts.len() })).into_response()
}

/// GET /SubtitleCodecs — list subtitle codecs
pub async fn subtitle_codecs(
    State(state): State<Arc<AppState>>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT DISTINCT codec FROM media_streams WHERE stream_type = 'Subtitle' AND codec IS NOT NULL AND codec <> '' ORDER BY codec ASC",
            vec![],
        ))
        .await
        .unwrap_or_default();

    let codecs: Vec<Value> = rows.iter()
        .filter_map(|r| r.get_opt_str("codec").ok().flatten().map(|c| json!({"Name": c, "Id": c})))
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /StreamLanguages — list stream languages
pub async fn stream_languages(
    State(state): State<Arc<AppState>>,
) -> Response {
    let backend = state.db.get_database_backend();
    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT DISTINCT language FROM media_streams WHERE language IS NOT NULL AND language <> '' ORDER BY language ASC",
            vec![],
        ))
        .await
        .unwrap_or_default();

    let langs: Vec<Value> = rows.iter()
        .filter_map(|r| r.get_opt_str("language").ok().flatten().map(|l| json!({"Name": l, "Id": l})))
        .collect();

    Json(json!({ "Items": langs, "TotalRecordCount": langs.len() })).into_response()
}

/// GET /ItemTypes — list item types
pub async fn item_types() -> Response {
    Json(json!([
        {"Name": "Movie", "Id": "Movie"},
        {"Name": "Series", "Id": "Series"},
        {"Name": "Season", "Id": "Season"},
        {"Name": "Episode", "Id": "Episode"},
        {"Name": "Video", "Id": "Video"},
        {"Name": "BoxSet", "Id": "BoxSet"},
        {"Name": "Playlist", "Id": "Playlist"},
    ]))
    .into_response()
}
