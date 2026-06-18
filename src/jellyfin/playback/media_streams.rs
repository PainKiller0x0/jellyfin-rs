use anyhow::Context;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde_json::Value as JsonValue;

use crate::{
    db::row_ext::QueryResultExt,
    entities::media_streams::Entity as MediaStreams,
    library::models::{MediaStreamRow, child_video_source_json},
};

pub(crate) async fn media_streams_for_item(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let models = MediaStreams::find()
        .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
        .order_by_asc(crate::entities::media_streams::Column::StreamIndex)
        .all(db)
        .await
        .with_context(|| format!("failed to list media streams for item: {item_id}"))?;
    let streams: Vec<_> = models
        .iter()
        .map(|m| MediaStreamRow {
            stream_index: m.stream_index,
            stream_type: m.stream_type.clone(),
            codec: m.codec.clone(),
            language: m.language.clone(),
            title: m.title.clone(),
            bit_rate: m.bit_rate,
            width: m.width,
            height: m.height,
            channels: m.channels,
            sample_rate: m.sample_rate,
            path: m.path.clone(),
            is_external: m.is_external != 0,
        })
        .collect();
    Ok(streams
        .into_iter()
        .map(|s| s.to_jellyfin_json(item_id))
        .collect())
}

/// For Movie folders, find child Video items and return their media sources
pub(crate) async fn child_video_sources(
    db: &sea_orm::DatabaseConnection,
    parent_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT media_items.id, media_items.title, media_items.path, media_items.container, media_items.runtime_ticks, media_items.size_bytes FROM media_items WHERE media_items.parent_id = ? AND media_items.item_type = 'Video' ORDER BY media_items.title ASC",
            vec![parent_id.into()],
        ))
        .await
        .with_context(|| format!("failed to find video children for: {parent_id}"))?;

    let mut sources = Vec::new();
    for row in &rows {
        let video_id: String = row.get_str("id")?;
        let title: String = row.get_str("title")?;
        let path: String = row.get_str("path")?;
        let container = row
            .get_opt_str("container")?
            .unwrap_or_else(|| "bin".to_string());
        let size = row.get_opt_i64("size_bytes")?;
        let runtime_ticks = row.get_opt_i64("runtime_ticks")?;

        // Add media streams for this video
        let streams = media_streams_for_item(db, &video_id)
            .await
            .unwrap_or_default();

        let source = child_video_source_json(
            &video_id,
            &title,
            &path,
            &container,
            size,
            runtime_ticks,
            streams,
        );
        sources.push(source);
    }

    Ok(sources)
}

pub async fn subtitle_stream_path(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    stream_index: i64,
) -> anyhow::Result<Option<String>> {
    let model = MediaStreams::find()
        .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
        .filter(crate::entities::media_streams::Column::StreamIndex.eq(stream_index))
        .filter(crate::entities::media_streams::Column::StreamType.eq("Subtitle"))
        .filter(crate::entities::media_streams::Column::IsExternal.eq(1))
        .one(db)
        .await
        .with_context(|| format!("failed to find subtitle stream: {item_id}:{stream_index}"))?;
    Ok(model.and_then(|m| m.path))
}
