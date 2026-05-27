use std::{collections::HashMap, sync::Arc};

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
};

/// GET /Items/{item_id}/Intros — get intros (returns empty, not supported)
pub async fn item_intros() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Items/{item_id}/LocalTrailers — get local trailers
pub async fn item_local_trailers(
    State(_state): State<Arc<AppState>>,
    Path(_item_id): Path<String>,
) -> Response {
    Json(json!([])).into_response()
}

/// GET /Items/{item_id}/SpecialFeatures — get special features
pub async fn item_special_features(
    State(_state): State<Arc<AppState>>,
    Path(_item_id): Path<String>,
) -> Response {
    Json(json!([])).into_response()
}

/// GET /Items/{item_id}/ThemeSongs — theme songs (empty for video server)
pub async fn item_theme_songs() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Items/{item_id}/ThemeVideos — theme videos (empty)
pub async fn item_theme_videos() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Items/{item_id}/ThemeMedia — theme media (empty)
pub async fn item_theme_media() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /MediaSegments/{item_id} — chapter markers (intro/credits segments)
pub async fn media_segments(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let mut segments = Vec::new();

    // Get intro markers
    if let Ok(Some((intro_start, intro_end))) =
        crate::chapters::get_intro_markers(&state.db, &item_id).await
    {
        segments.push(json!({
            "Type": "Intro",
            "StartTicks": intro_start,
            "EndTicks": intro_end
        }));
    }

    // Get credits marker
    if let Ok(Some(credits_start)) =
        crate::chapters::get_credits_marker(&state.db, &item_id).await
    {
        // Get runtime to compute end ticks
        let runtime = state
            .db
            .query_one(crate::db::helpers::portable_statement(
                state.db.get_database_backend(),
                "SELECT runtime_ticks FROM media_items WHERE id = ?",
                vec![item_id.clone().into()],
            ))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get_i64("runtime_ticks").ok())
            .unwrap_or(0);

        segments.push(json!({
            "Type": "Credits",
            "StartTicks": credits_start,
            "EndTicks": runtime
        }));
    }

    Json(json!({ "Segments": segments })).into_response()
}

/// GET /Items/{item_id}/InstantMix — instant mix from item
pub async fn item_instant_mix(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let limit = query
        .get("Limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16);

    // Use similar items logic (same genre-based approach)
    let backend = state.db.get_database_backend();
    let sql = r#"SELECT mg_rel.item_id FROM media_genres mg_src JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id AND mg_src.item_id <> mg_rel.item_id WHERE mg_src.item_id = ? GROUP BY mg_rel.item_id ORDER BY COUNT(*) DESC LIMIT ?"#;
    let similar_rows = state.db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            sql,
            vec![item_id.into(), (limit as i64).into()],
        ))
        .await
        .unwrap_or_default();

    let ids: Vec<String> = similar_rows.iter()
        .filter_map(|r| r.get_opt_str("item_id").ok().flatten())
        .collect();

    if ids.is_empty() {
        return Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response();
    }

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let item_sql = format!(
        "{} WHERE media_items.id IN ({})",
        crate::jellyfin::item_queries::media_item_select_sql(""),
        placeholders
    );
    let mut vals: Vec<sea_orm::Value> = vec![user_id.into()];
    for id in &ids { vals.push(id.as_str().into()); }

    let rows = state.db
        .query_all(crate::db::helpers::portable_statement(backend, &item_sql, vals))
        .await
        .unwrap_or_default();

    let items = crate::jellyfin::item_queries::decode_media_items(&rows).unwrap_or_default();
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|i| crate::jellyfin::common::strip_nulls(i.to_jellyfin_json())).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

/// GET /Items/{id}/CriticReviews — critic reviews
pub async fn item_critic_reviews() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Items/{id}/ThumbnailSet — trickplay thumbnail set
pub async fn thumbnail_set(
    State(_state): State<Arc<AppState>>,
    Path(_item_id): Path<String>,
) -> Response {
    // Not implemented - would need trickplay image generation
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Items/{item_id}/RemoteSearch/Subtitles/{language} — search remote subtitles
pub async fn remote_subtitle_search(
    State(_state): State<Arc<AppState>>,
    Path((_item_id, _param)): Path<(String, String)>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    // Return empty - remote subtitle providers not implemented
    Json(json!([])).into_response()
}

/// POST /Items/{item_id}/RemoteSearch/Subtitles/{subtitle_id} — download remote subtitle
pub async fn download_remote_subtitle(
    State(_state): State<Arc<AppState>>,
    Path((_item_id, _param)): Path<(String, String)>,
) -> Response {
    // Not implemented - would need subtitle provider integration
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Genres/{name}/Images/{image_type} — genre image
pub async fn genre_image(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type)): Path<(String, String)>,
) -> Response {
    // Genre images not stored
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Studios/{name}/Images/{image_type} — studio image
pub async fn studio_image(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type)): Path<(String, String)>,
) -> Response {
    // Studio images not stored
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Users/{id}/Items/{id}/Intros — per-user intros
pub async fn user_item_intros() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

/// GET /Users/{id}/Items/{id}/LocalTrailers — per-user local trailers
pub async fn user_item_local_trailers() -> Response {
    Json(json!([])).into_response()
}
