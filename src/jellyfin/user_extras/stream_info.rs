use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::{app::state::AppState, db::row_ext::QueryResultExt};

fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

/// DELETE /Users/{user_id}/TrackSelections/{track_type} — clear track selections
pub async fn clear_track_selections(
    State(_state): State<Arc<AppState>>,
    Path((_user_id, _track_type)): Path<(String, String)>,
) -> Response {
    // No-op for now - track selections are client-side
    StatusCode::NO_CONTENT.into_response()
}

/// GET /AudioCodecs — list audio codecs from media_streams
pub async fn audio_codecs(State(state): State<Arc<AppState>>) -> Response {
    let rows = public_stream_rows(
        &state.db,
        "media_streams.codec AS value",
        "media_streams.stream_type = 'Audio' AND media_streams.codec IS NOT NULL AND media_streams.codec <> ''",
        "media_streams.codec ASC",
    )
    .await;

    let codecs: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            r.get_opt_str("value")
                .ok()
                .flatten()
                .map(|c| json!({"Name": c, "Id": c}))
        })
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /AudioLayouts — list audio channel layouts
pub async fn audio_layouts(State(state): State<Arc<AppState>>) -> Response {
    let rows = public_stream_rows(
        &state.db,
        "media_streams.channels AS value",
        "media_streams.stream_type = 'Audio' AND media_streams.channels IS NOT NULL",
        "media_streams.channels ASC",
    )
    .await;

    let layouts: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            r.get_opt_i64("value").ok().flatten().map(|ch| {
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
pub async fn subtitle_codecs(State(state): State<Arc<AppState>>) -> Response {
    let rows = public_stream_rows(
        &state.db,
        "media_streams.codec AS value",
        "media_streams.stream_type = 'Subtitle' AND media_streams.codec IS NOT NULL AND media_streams.codec <> ''",
        "media_streams.codec ASC",
    )
    .await;

    let codecs: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            r.get_opt_str("value")
                .ok()
                .flatten()
                .map(|c| json!({"Name": c, "Id": c}))
        })
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /StreamLanguages — list stream languages
pub async fn stream_languages(State(state): State<Arc<AppState>>) -> Response {
    let rows = public_stream_rows(
        &state.db,
        "media_streams.language AS value",
        "media_streams.language IS NOT NULL AND media_streams.language <> ''",
        "media_streams.language ASC",
    )
    .await;

    let langs: Vec<Value> = rows
        .iter()
        .filter_map(|r| {
            r.get_opt_str("value")
                .ok()
                .flatten()
                .map(|l| json!({"Name": l, "Id": l}))
        })
        .collect();

    Json(json!({ "Items": langs, "TotalRecordCount": langs.len() })).into_response()
}

async fn public_stream_rows(
    db: &sea_orm::DatabaseConnection,
    select_expr: &str,
    filter: &str,
    order_by: &str,
) -> Vec<sea_orm::QueryResult> {
    let visible = visible_media_item_sql("media_items");
    let sql = format!(
        "SELECT DISTINCT {select_expr} FROM media_streams JOIN media_items ON media_items.id = media_streams.item_id WHERE {visible} AND {filter} ORDER BY {order_by}"
    );
    db.query_all(crate::db::helpers::pg_statement(&sql, vec![]))
        .await
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::public_stream_rows;
    use crate::db::row_ext::QueryResultExt;
    use sea_orm::ConnectionTrait;

    #[tokio::test]
    async fn stream_filters_hide_private_media_values() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, parent_id, item_type, is_folder, public, codec, lang, channels) in [
            ("public", "", "Movie", 0_i64, 1_i64, "aac", "eng", 2_i64),
            ("private", "", "Movie", 0_i64, 0_i64, "dts", "jpn", 8_i64),
            (
                "private-parent",
                "",
                "Series",
                1_i64,
                0_i64,
                "flac",
                "fra",
                6_i64,
            ),
            (
                "hidden-child",
                "private-parent",
                "Episode",
                0_i64,
                1_i64,
                "opus",
                "spa",
                1_i64,
            ),
        ] {
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES (?, ?, ?, '', ?, ?, ?, ?, 1, 1, 1)",
                vec![
                    id.into(),
                    id.into(),
                    id.into(),
                    parent_id.into(),
                    item_type.into(),
                    is_folder.into(),
                    public.into(),
                ],
            ))
            .await
            .unwrap();
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, channels, is_external, created_at) VALUES (?, ?, 0, 'Audio', ?, ?, ?, 0, 1)",
                vec![
                    format!("{id}-audio").into(),
                    id.into(),
                    codec.into(),
                    lang.into(),
                    channels.into(),
                ],
            ))
            .await
            .unwrap();
        }

        let codecs = public_stream_rows(
            &db,
            "media_streams.codec AS value",
            "media_streams.stream_type = 'Audio' AND media_streams.codec IS NOT NULL AND media_streams.codec <> ''",
            "media_streams.codec ASC",
        )
        .await;
        assert_eq!(codecs.len(), 1);
        assert_eq!(codecs[0].get_str("value").unwrap(), "aac");

        let languages = public_stream_rows(
            &db,
            "media_streams.language AS value",
            "media_streams.language IS NOT NULL AND media_streams.language <> ''",
            "media_streams.language ASC",
        )
        .await;
        assert_eq!(languages.len(), 1);
        assert_eq!(languages[0].get_str("value").unwrap(), "eng");

        let layouts = public_stream_rows(
            &db,
            "media_streams.channels AS value",
            "media_streams.stream_type = 'Audio' AND media_streams.channels IS NOT NULL",
            "media_streams.channels ASC",
        )
        .await;
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].get_i64("value").unwrap(), 2);
    }
}
