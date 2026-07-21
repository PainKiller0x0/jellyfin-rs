use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Context;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde_json::Value as JsonValue;

use crate::{
    entities::{
        libraries::Entity as Libraries,
        media_items::{self, Entity as MediaItems},
        media_streams::{Entity as MediaStreams, Model as MediaStreamModel},
    },
    library::models::{MediaItem, MediaStreamRow, child_video_source_json_for_item},
};

type EpisodeVersionKey = (String, Option<i64>, i64);

#[derive(Clone)]
struct VideoSourceRow {
    id: String,
    title: String,
    path: String,
    container: String,
    runtime_ticks: Option<i64>,
    size_bytes: Option<i64>,
}

impl From<media_items::Model> for VideoSourceRow {
    fn from(item: media_items::Model) -> Self {
        Self {
            id: item.id,
            title: item.title,
            path: item.path,
            container: item.container.unwrap_or_else(|| "bin".to_string()),
            runtime_ticks: item.runtime_ticks,
            size_bytes: item.size_bytes,
        }
    }
}

async fn visible_media_item(
    db: &sea_orm::DatabaseConnection,
    item: &media_items::Model,
) -> anyhow::Result<bool> {
    if item.is_public == 0 {
        return Ok(false);
    }
    if item.parent_id.is_empty() {
        return Ok(true);
    }
    if Libraries::find_by_id(item.parent_id.clone())
        .one(db)
        .await
        .context("failed to check parent library visibility")?
        .is_some()
    {
        return Ok(true);
    }
    Ok(MediaItems::find_by_id(item.parent_id.clone())
        .one(db)
        .await
        .context("failed to check parent item visibility")?
        .is_some_and(|parent| parent.is_public != 0))
}

pub(crate) async fn media_streams_for_item(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<JsonValue>> {
    let models = MediaStreams::find()
        .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
        .filter(
            crate::entities::media_streams::Column::StreamType
                .ne(crate::library::storage::MEDIA_PROBE_FAILURE_STREAM_TYPE),
        )
        .order_by_asc(crate::entities::media_streams::Column::StreamIndex)
        .all(db)
        .await
        .with_context(|| format!("failed to list media streams for item: {item_id}"))?;
    Ok(models
        .iter()
        .map(|model| media_stream_model_to_json(model, item_id))
        .collect())
}

pub(crate) async fn media_streams_for_items(
    db: &sea_orm::DatabaseConnection,
    item_ids: &[String],
) -> anyhow::Result<HashMap<String, Vec<JsonValue>>> {
    let mut stream_map: HashMap<String, Vec<JsonValue>> = HashMap::new();
    if item_ids.is_empty() {
        return Ok(stream_map);
    }

    for chunk in item_ids.chunks(500) {
        let models = MediaStreams::find()
            .filter(
                crate::entities::media_streams::Column::ItemId
                    .is_in(chunk.iter().map(|id| id.as_str())),
            )
            .filter(
                crate::entities::media_streams::Column::StreamType
                    .ne(crate::library::storage::MEDIA_PROBE_FAILURE_STREAM_TYPE),
            )
            .order_by_asc(crate::entities::media_streams::Column::ItemId)
            .order_by_asc(crate::entities::media_streams::Column::StreamIndex)
            .all(db)
            .await
            .with_context(|| "failed to list media streams for episode versions")?;

        for model in &models {
            let item_id = model.item_id.clone();
            let stream = media_stream_model_to_json(model, &item_id);
            stream_map.entry(item_id).or_default().push(stream);
        }
    }

    Ok(stream_map)
}

fn media_stream_model_to_json(model: &MediaStreamModel, item_id: &str) -> JsonValue {
    MediaStreamRow {
        stream_index: model.stream_index,
        stream_type: model.stream_type.clone(),
        codec: model.codec.clone(),
        profile: model.profile.clone(),
        codec_tag: model.codec_tag.clone(),
        language: model.language.clone(),
        title: model.title.clone(),
        comment: model.comment.clone(),
        bit_rate: model.bit_rate,
        width: model.width,
        height: model.height,
        aspect_ratio: model.aspect_ratio.clone(),
        average_frame_rate: model.average_frame_rate,
        real_frame_rate: model.real_frame_rate,
        reference_frame_rate: model.reference_frame_rate,
        channels: model.channels,
        channel_layout: model.channel_layout.clone(),
        sample_rate: model.sample_rate,
        bit_depth: model.bit_depth,
        ref_frames: model.ref_frames,
        is_interlaced: model.is_interlaced != 0,
        is_avc: model.is_avc.map(|value| value != 0),
        is_anamorphic: model.is_anamorphic.map(|value| value != 0),
        pixel_format: model.pixel_format.clone(),
        level: model.level,
        color_range: model.color_range.clone(),
        color_space: model.color_space.clone(),
        color_transfer: model.color_transfer.clone(),
        color_primaries: model.color_primaries.clone(),
        time_base: model.time_base.clone(),
        codec_time_base: model.codec_time_base.clone(),
        nal_length_size: model.nal_length_size.clone(),
        rotation: model.rotation,
        video_range: model.video_range.clone(),
        video_range_type: model.video_range_type.clone(),
        hdr10_plus_present_flag: model.hdr10_plus_present_flag.map(|value| value != 0),
        is_default: model.is_default != 0,
        is_forced: model.is_forced != 0,
        is_hearing_impaired: model.is_hearing_impaired != 0,
        is_original: model.is_original.map(|value| value != 0),
        path: model.path.clone(),
        is_external: model.is_external != 0,
    }
    .to_jellyfin_json(item_id)
}

/// For Movie folders, find child Video items and return their media sources
pub(crate) async fn child_video_sources(
    db: &sea_orm::DatabaseConnection,
    parent_id: &str,
    include_private: bool,
) -> anyhow::Result<Vec<JsonValue>> {
    let rows = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(parent_id))
        .filter(media_items::Column::ItemType.eq("Video"))
        .order_by_asc(media_items::Column::Title)
        .all(db)
        .await
        .with_context(|| format!("failed to find video children for: {parent_id}"))?;

    let mut sources = Vec::new();
    for row in rows {
        if !include_private && !visible_media_item(db, &row).await? {
            continue;
        }
        let video_id = row.id;
        let title = row.title;
        let path = row.path;
        let source_name = media_source_file_name(&path, &title);
        let container = row.container.unwrap_or_else(|| "bin".to_string());
        let size = row.size_bytes;
        let runtime_ticks = row.runtime_ticks;

        // Add media streams for this video
        let streams = media_streams_for_item(db, &video_id)
            .await
            .unwrap_or_default();

        let source = child_video_source_json_for_item(
            parent_id,
            &video_id,
            &source_name,
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

/// For Movie/Episode folders, batch load their child Video files as MediaSourceInfo entries.
pub(crate) async fn batch_child_video_sources(
    db: &sea_orm::DatabaseConnection,
    parent_ids: &[String],
    include_private: bool,
    include_streams: bool,
) -> anyhow::Result<HashMap<String, Vec<JsonValue>>> {
    let mut source_map: HashMap<String, Vec<JsonValue>> = HashMap::new();
    if parent_ids.is_empty() {
        return Ok(source_map);
    }

    let mut source_rows: Vec<(String, VideoSourceRow)> = Vec::new();
    for chunk in parent_ids.chunks(500) {
        let rows = MediaItems::find()
            .filter(media_items::Column::ParentId.is_in(chunk.iter().cloned()))
            .filter(media_items::Column::ItemType.eq("Video"))
            .order_by_asc(media_items::Column::ParentId)
            .order_by_asc(media_items::Column::Title)
            .all(db)
            .await
            .with_context(|| "failed to batch find child video sources")?;
        for row in rows {
            if !include_private && !visible_media_item(db, &row).await? {
                continue;
            }
            source_rows.push((row.parent_id.clone(), VideoSourceRow::from(row)));
        }
    }

    let stream_map = if include_streams {
        let mut source_ids = source_rows
            .iter()
            .map(|(_, row)| row.id.clone())
            .collect::<Vec<_>>();
        source_ids.sort();
        source_ids.dedup();
        media_streams_for_items(db, &source_ids)
            .await
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    for (parent_id, row) in source_rows {
        source_map
            .entry(parent_id.clone())
            .or_default()
            .push(child_video_source_json_for_item(
                &parent_id,
                &row.id,
                &media_source_file_name(&row.path, &row.title),
                &row.path,
                &row.container,
                row.size_bytes,
                row.runtime_ticks,
                stream_map.get(&row.id).cloned().unwrap_or_default(),
            ));
    }

    Ok(source_map)
}

/// For episode rows that represent one file/version, return all sibling files
/// for the same season/episode as MediaSourceInfo entries.
pub(crate) async fn episode_version_sources(
    db: &sea_orm::DatabaseConnection,
    item: &MediaItem,
    include_private: bool,
) -> anyhow::Result<Vec<JsonValue>> {
    if item.item_type != "Episode" || item.is_folder || item.episode_number.is_none() {
        return Ok(Vec::new());
    }

    let mut query = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(&item.parent_id))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .filter(media_items::Column::EpisodeNumber.eq(item.episode_number.unwrap_or_default()));
    query = if let Some(season_number) = item.season_number {
        query.filter(media_items::Column::SeasonNumber.eq(season_number))
    } else {
        query.filter(media_items::Column::SeasonNumber.is_null())
    };
    let rows = query
        .all(db)
        .await
        .with_context(|| format!("failed to find episode versions for: {}", item.id))?;
    let mut visible_rows = Vec::new();
    for row in rows {
        if include_private || visible_media_item(db, &row).await? {
            visible_rows.push(row);
        }
    }

    let mut rows = visible_rows
        .into_iter()
        .map(VideoSourceRow::from)
        .collect::<Vec<_>>();
    sort_episode_version_sources_for_item(&mut rows, &item.id);

    let mut sources = Vec::new();
    for row in rows {
        let source_id = row.id;
        let title = row.title;
        let path = row.path;
        let source_name = media_source_file_name(&path, &title);
        let container = row.container;
        let size = row.size_bytes;
        let runtime_ticks = row.runtime_ticks;
        let streams = media_streams_for_item(db, &source_id)
            .await
            .unwrap_or_default();
        sources.push(child_video_source_json_for_item(
            &item.id,
            &source_id,
            &source_name,
            &path,
            &container,
            size,
            runtime_ticks,
            streams,
        ));
    }

    Ok(sources)
}

pub(crate) async fn batch_episode_version_sources(
    db: &sea_orm::DatabaseConnection,
    items: &[MediaItem],
    include_private: bool,
    include_streams: bool,
) -> anyhow::Result<HashMap<String, Vec<JsonValue>>> {
    let episode_items: Vec<&MediaItem> = items
        .iter()
        .filter(|item| {
            item.item_type == "Episode" && !item.is_folder && item.episode_number.is_some()
        })
        .collect();
    if episode_items.is_empty() {
        return Ok(HashMap::new());
    }

    let mut episode_keys = episode_items
        .iter()
        .filter_map(|item| episode_version_key(item))
        .collect::<Vec<_>>();
    episode_keys.sort();
    episode_keys.dedup();

    let episode_key_set = episode_keys.iter().cloned().collect::<HashSet<_>>();
    let parent_ids = episode_keys
        .iter()
        .map(|(parent_id, _, _)| parent_id.clone())
        .collect::<HashSet<_>>();
    let rows = MediaItems::find()
        .filter(media_items::Column::ParentId.is_in(parent_ids))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .all(db)
        .await
        .with_context(|| "failed to batch find episode versions")?;

    let mut grouped: HashMap<EpisodeVersionKey, Vec<VideoSourceRow>> = HashMap::new();
    for row in rows {
        if !include_private && !visible_media_item(db, &row).await? {
            continue;
        }
        let Some(episode_number) = row.episode_number else {
            continue;
        };
        let key = (row.parent_id.clone(), row.season_number, episode_number);
        if episode_key_set.contains(&key) {
            grouped
                .entry(key)
                .or_default()
                .push(VideoSourceRow::from(row));
        }
    }

    let stream_map = if include_streams {
        let mut source_ids: Vec<String> = grouped
            .values()
            .flat_map(|rows| rows.iter().map(|row| row.id.clone()))
            .collect();
        source_ids.sort();
        source_ids.dedup();
        media_streams_for_items(db, &source_ids)
            .await
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    let mut source_map: HashMap<String, Vec<JsonValue>> = HashMap::new();
    for item in episode_items {
        let Some(key) = episode_version_key(item) else {
            continue;
        };
        let Some(rows) = grouped.get(&key) else {
            continue;
        };

        let mut rows = rows.clone();
        sort_episode_version_sources_for_item(&mut rows, &item.id);

        let sources = rows
            .iter()
            .map(|row| {
                child_video_source_json_for_item(
                    &item.id,
                    &row.id,
                    &media_source_file_name(&row.path, &row.title),
                    &row.path,
                    &row.container,
                    row.size_bytes,
                    row.runtime_ticks,
                    stream_map.get(&row.id).cloned().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        if !sources.is_empty() {
            source_map.insert(item.id.clone(), sources);
        }
    }

    Ok(source_map)
}

fn episode_version_key(item: &MediaItem) -> Option<EpisodeVersionKey> {
    Some((
        item.parent_id.clone(),
        item.season_number,
        item.episode_number?,
    ))
}

fn sort_episode_version_sources_for_item(rows: &mut [VideoSourceRow], item_id: &str) {
    rows.sort_by(|a, b| {
        let a_rank = if a.id == item_id { 0 } else { 1 };
        let b_rank = if b.id == item_id { 0 } else { 1 };
        a_rank
            .cmp(&b_rank)
            .then_with(|| compare_size_desc_nulls_last(a.size_bytes, b.size_bytes))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn compare_size_desc_nulls_last(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn media_source_file_name(path: &str, fallback_title: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback_title)
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::{
        batch_episode_version_sources, child_video_sources, episode_version_sources,
        media_streams_for_item,
    };
    use crate::entities::{
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use crate::library::models::MediaItem;
    use sea_orm::{DatabaseConnection, EntityTrait, Set};

    #[tokio::test]
    async fn child_video_sources_hide_private_versions_unless_requested() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, parent_id, item_type, is_folder, is_public) in [
            ("movie", "", "Movie", 1_i64, 1_i64),
            ("public", "movie", "Video", 0_i64, 1_i64),
            ("private", "movie", "Video", 0_i64, 0_i64),
            ("private-parent", "", "Movie", 1_i64, 0_i64),
            ("hidden-child", "private-parent", "Video", 0_i64, 1_i64),
        ] {
            insert_test_media_item(
                &db,
                id,
                id,
                &format!("/tmp/{id}.mkv"),
                "",
                parent_id,
                item_type,
                is_folder,
                is_public,
                None,
                None,
                None,
            )
            .await;
        }

        assert_eq!(
            child_video_sources(&db, "movie", false)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            child_video_sources(&db, "movie", true).await.unwrap().len(),
            2
        );
        assert_eq!(
            child_video_sources(&db, "private-parent", false)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            child_video_sources(&db, "private-parent", true)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn media_streams_hide_probe_failure_markers() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_test_media_item(
            &db,
            "movie",
            "Movie",
            "/tmp/movie.mkv",
            "movies",
            "movies",
            "Movie",
            0,
            1,
            Some(100),
            None,
            None,
        )
        .await;
        for (id, index, stream_type) in [
            ("stream-video", 0_i64, "Video"),
            (
                "stream-failure",
                -1_i64,
                crate::library::storage::MEDIA_PROBE_FAILURE_STREAM_TYPE,
            ),
        ] {
            MediaStreams::insert(media_streams::ActiveModel {
                id: Set(id.to_string()),
                item_id: Set("movie".to_string()),
                stream_index: Set(index),
                stream_type: Set(stream_type.to_string()),
                created_at: Set(1),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }

        let streams = media_streams_for_item(&db, "movie").await.unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0]["Type"], "Video");
    }

    #[tokio::test]
    async fn episode_version_sources_group_sibling_files_by_episode_number() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        for (id, title, parent_id, item_type, is_public, path, season, episode, size) in [
            (
                "season",
                "Season 1",
                "series",
                "Season",
                1_i64,
                "/tmp/show/Season 1",
                Some(1_i64),
                None,
                None,
            ),
            (
                "ep-1080",
                "Pilot",
                "season",
                "Episode",
                1_i64,
                "/tmp/show/Season 1/Show.S01E01.1080p.mkv",
                Some(1),
                Some(1),
                Some(100_i64),
            ),
            (
                "ep-2160",
                "Pilot",
                "season",
                "Episode",
                1_i64,
                "/tmp/show/Season 1/Show.S01E01.2160p.HDR.mkv",
                Some(1),
                Some(1),
                Some(200),
            ),
            (
                "ep-private",
                "Pilot",
                "season",
                "Episode",
                0_i64,
                "/tmp/show/Season 1/Show.S01E01.2160p.DV.mkv",
                Some(1),
                Some(1),
                Some(300),
            ),
            (
                "ep-2",
                "Second",
                "season",
                "Episode",
                1_i64,
                "/tmp/show/Season 1/Show.S01E02.1080p.mkv",
                Some(1),
                Some(2),
                Some(400),
            ),
        ] {
            insert_test_media_item(
                &db,
                id,
                title,
                path,
                "tv",
                parent_id,
                item_type,
                if item_type == "Season" { 1 } else { 0 },
                is_public,
                size,
                season,
                episode,
            )
            .await;
        }

        let item = episode_item("ep-1080", 100);

        let public_sources = episode_version_sources(&db, &item, false).await.unwrap();
        assert_eq!(
            public_sources
                .iter()
                .map(|source| source["Id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["ep-1080", "ep-2160"]
        );
        assert_eq!(public_sources[0]["Name"], "Show.S01E01.1080p.mkv");
        assert_eq!(public_sources[1]["Name"], "Show.S01E01.2160p.HDR.mkv");

        let admin_sources = episode_version_sources(&db, &item, true).await.unwrap();
        assert_eq!(admin_sources.len(), 3);
        assert!(
            admin_sources
                .iter()
                .any(|source| source["Id"] == "ep-private")
        );

        let mut second_item = episode_item("ep-2", 400);
        second_item.title = "Second".to_string();
        second_item.path = "/tmp/show/Season 1/Show.S01E02.1080p.mkv".to_string();
        second_item.episode_number = Some(2);

        let batch_sources = batch_episode_version_sources(&db, &[item, second_item], false, false)
            .await
            .unwrap();
        assert_eq!(
            batch_sources
                .get("ep-1080")
                .unwrap()
                .iter()
                .map(|source| source["Id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["ep-1080", "ep-2160"]
        );
        assert_eq!(
            batch_sources
                .get("ep-2")
                .unwrap()
                .iter()
                .map(|source| source["Id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["ep-2"]
        );
    }

    fn episode_item(id: &str, size_bytes: i64) -> MediaItem {
        MediaItem {
            id: id.to_string(),
            title: "Pilot".to_string(),
            path: format!("/tmp/show/Season 1/Show.S01E01.1080p.{id}.mkv"),
            library_id: "tv".to_string(),
            collection_type: "tvshows".to_string(),
            parent_id: "season".to_string(),
            item_type: "Episode".to_string(),
            is_folder: false,
            container: Some("mkv".to_string()),
            overview: None,
            official_rating: None,
            extended_video_type: None,
            production_year: None,
            premiere_date: None,
            runtime_ticks: None,
            size_bytes: Some(size_bytes),
            season_number: Some(1),
            episode_number: Some(1),
            community_rating: None,
            critic_rating: None,
            created_at: 1,
            modified_at: 1,
            is_public: true,
            is_favorite: false,
            played: false,
            playback_position_ticks: 0,
            played_percentage: None,
            play_count: 0,
            last_played_at: None,
            image_tags: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_test_media_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        size_bytes: Option<i64>,
        season_number: Option<i64>,
        episode_number: Option<i64>,
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
            container: Set(Some("mkv".to_string())),
            size_bytes: Set(size_bytes),
            season_number: Set(season_number),
            episode_number: Set(episode_number),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}
