use std::{collections::HashSet, sync::Arc};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{DatabaseConnection, EntityTrait};
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    entities::{
        libraries::Entity as Libraries,
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    },
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
pub async fn audio_codecs(State(state): State<Arc<AppState>>) -> Response {
    let values =
        public_stream_text_values(&state.db, Some("Audio"), |stream| stream.codec.as_deref()).await;

    let codecs: Vec<Value> = values
        .iter()
        .map(|codec| json!({"Name": codec, "Id": codec}))
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /AudioLayouts — list audio channel layouts
pub async fn audio_layouts(State(state): State<Arc<AppState>>) -> Response {
    let values = public_stream_channel_values(&state.db).await;

    let layouts: Vec<Value> = values
        .iter()
        .map(|ch| {
            let name = match ch {
                1 => "Mono".to_string(),
                2 => "Stereo".to_string(),
                6 => "5.1".to_string(),
                8 => "7.1".to_string(),
                n => format!("{}ch", n),
            };
            json!({"Name": name, "Id": ch})
        })
        .collect();

    Json(json!({ "Items": layouts, "TotalRecordCount": layouts.len() })).into_response()
}

/// GET /SubtitleCodecs — list subtitle codecs
pub async fn subtitle_codecs(State(state): State<Arc<AppState>>) -> Response {
    let values = public_stream_text_values(&state.db, Some("Subtitle"), |stream| {
        stream.codec.as_deref()
    })
    .await;

    let codecs: Vec<Value> = values
        .iter()
        .map(|codec| json!({"Name": codec, "Id": codec}))
        .collect();

    Json(json!({ "Items": codecs, "TotalRecordCount": codecs.len() })).into_response()
}

/// GET /StreamLanguages — list stream languages
pub async fn stream_languages(State(state): State<Arc<AppState>>) -> Response {
    let values =
        public_stream_text_values(&state.db, None, |stream| stream.language.as_deref()).await;

    let langs: Vec<Value> = values
        .iter()
        .map(|language| json!({"Name": language, "Id": language}))
        .collect();

    Json(json!({ "Items": langs, "TotalRecordCount": langs.len() })).into_response()
}

async fn public_stream_text_values<F>(
    db: &DatabaseConnection,
    stream_type: Option<&str>,
    value_for_stream: F,
) -> Vec<String>
where
    F: Fn(&media_streams::Model) -> Option<&str>,
{
    let mut values = public_streams(db)
        .await
        .into_iter()
        .filter(|stream| stream_type.is_none_or(|stream_type| stream.stream_type == stream_type))
        .filter_map(|stream| {
            value_for_stream(&stream)
                .map(str::trim)
                .map(ToString::to_string)
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

async fn public_stream_channel_values(db: &DatabaseConnection) -> Vec<i64> {
    let mut values = public_streams(db)
        .await
        .into_iter()
        .filter(|stream| stream.stream_type == "Audio")
        .filter_map(|stream| stream.channels)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

async fn public_streams(db: &DatabaseConnection) -> Vec<media_streams::Model> {
    let streams = MediaStreams::find().all(db).await.unwrap_or_default();
    let items = MediaItems::find().all(db).await.unwrap_or_default();
    let libraries = Libraries::find().all(db).await.unwrap_or_default();
    let library_ids = libraries
        .into_iter()
        .map(|library| library.id)
        .collect::<HashSet<_>>();
    let item_by_id = items
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<std::collections::HashMap<_, _>>();

    streams
        .into_iter()
        .filter(|stream| {
            item_by_id
                .get(&stream.item_id)
                .is_some_and(|item| visible_media_item(item, &item_by_id, &library_ids))
        })
        .collect()
}

fn visible_media_item(
    item: &media_items::Model,
    item_by_id: &std::collections::HashMap<String, media_items::Model>,
    library_ids: &HashSet<String>,
) -> bool {
    item.is_public != 0
        && (item.parent_id.is_empty()
            || library_ids.contains(&item.parent_id)
            || item_by_id
                .get(&item.parent_id)
                .is_some_and(|parent| parent.is_public != 0))
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
    use super::{public_stream_channel_values, public_stream_text_values};
    use crate::entities::{
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use sea_orm::{EntityTrait, Set};

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
            MediaItems::insert(media_items::ActiveModel {
                id: Set(id.to_string()),
                title: Set(id.to_string()),
                path: Set(id.to_string()),
                library_id: Set(String::new()),
                parent_id: Set(parent_id.to_string()),
                item_type: Set(item_type.to_string()),
                is_folder: Set(is_folder),
                is_public: Set(public),
                modified_at: Set(1),
                created_at: Set(1),
                updated_at: Set(1),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
            MediaStreams::insert(media_streams::ActiveModel {
                id: Set(format!("{id}-audio")),
                item_id: Set(id.to_string()),
                stream_index: Set(0),
                stream_type: Set("Audio".to_string()),
                codec: Set(Some(codec.to_string())),
                language: Set(Some(lang.to_string())),
                channels: Set(Some(channels)),
                is_external: Set(0),
                created_at: Set(1),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }

        let codecs =
            public_stream_text_values(&db, Some("Audio"), |stream| stream.codec.as_deref()).await;
        assert_eq!(codecs.len(), 1);
        assert_eq!(codecs[0], "aac");

        let languages =
            public_stream_text_values(&db, None, |stream| stream.language.as_deref()).await;
        assert_eq!(languages.len(), 1);
        assert_eq!(languages[0], "eng");

        let layouts = public_stream_channel_values(&db).await;
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0], 2);
    }
}
