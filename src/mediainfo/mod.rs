use std::path::{Path, PathBuf};

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};

use crate::entities::{
    media_items::{self, Entity as MediaItems},
    media_streams::{self, Entity as MediaStreams},
};
use crate::util::{now_unix, stable_item_id};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfoSidecar {
    pub media_streams: Vec<MediaStreamEntry>,
    pub chapters: Vec<ChapterEntry>,
    pub runtime_ticks: Option<i64>,
    pub container: Option<String>,
    pub size_bytes: Option<i64>,
    pub total_bitrate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaStreamEntry {
    pub stream_index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
    pub is_external: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterEntry {
    pub start_position_ticks: i64,
    pub name: String,
    pub marker_type: Option<String>,
}

/// Returns the path for the MediaInfo JSON sidecar file.
pub fn mediainfo_json_path(media_path: &Path, root_folder: Option<&str>) -> PathBuf {
    let stem = media_path.file_stem().unwrap_or_default();
    let parent = media_path.parent().unwrap_or(Path::new("."));

    if let Some(root) = root_folder {
        let drive_root = get_drive_root(parent);
        if let Ok(relative) = parent.strip_prefix(&drive_root) {
            let target_dir = Path::new(root).join(relative);
            return target_dir.join(format!("{}-mediainfo.json", stem.to_string_lossy()));
        }
    }

    parent.join(format!("{}-mediainfo.json", stem.to_string_lossy()))
}

/// Serialize media info from DB to a JSON sidecar file.
#[allow(dead_code)]
pub async fn serialize_mediainfo(
    db: &DatabaseConnection,
    item_id: &str,
    media_path: &Path,
    root_folder: Option<&str>,
) -> anyhow::Result<()> {
    let streams = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .order_by_asc(media_streams::Column::StreamIndex)
        .all(db)
        .await?;

    let item = MediaItems::find_by_id(item_id.to_string()).one(db).await?;
    let runtime_ticks = item.as_ref().and_then(|item| item.runtime_ticks);
    let container = item.as_ref().and_then(|item| item.container.clone());
    let size_bytes = item.as_ref().and_then(|item| item.size_bytes);

    // Get chapters
    let chapters = crate::chapters::get_chapters(db, item_id).await?;

    let sidecar = MediaInfoSidecar {
        media_streams: streams
            .iter()
            .map(|s| MediaStreamEntry {
                stream_index: s.stream_index,
                stream_type: s.stream_type.clone(),
                codec: s.codec.clone(),
                language: s.language.clone(),
                title: s.title.clone(),
                bit_rate: s.bit_rate,
                width: s.width,
                height: s.height,
                channels: s.channels,
                sample_rate: s.sample_rate,
                is_external: s.is_external != 0,
            })
            .collect(),
        chapters: chapters
            .iter()
            .map(|c| ChapterEntry {
                start_position_ticks: c.start_position_ticks,
                name: c.name.clone(),
                marker_type: c.marker_type.clone(),
            })
            .collect(),
        runtime_ticks,
        container,
        size_bytes,
        total_bitrate: None,
        width: None,
        height: None,
    };

    let json_path = mediainfo_json_path(media_path, root_folder);
    if let Some(parent) = json_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&sidecar)?;
    std::fs::write(&json_path, json)?;
    tracing::debug!("serialized mediainfo to {}", json_path.display());
    Ok(())
}

/// Deserialize media info from a JSON sidecar file and restore into DB.
/// Returns true if sidecar was found and restored.
pub async fn deserialize_mediainfo(
    db: &DatabaseConnection,
    item_id: &str,
    media_path: &Path,
    root_folder: Option<&str>,
) -> anyhow::Result<bool> {
    let json_path = mediainfo_json_path(media_path, root_folder);
    if !json_path.exists() {
        return Ok(false);
    }

    let json = std::fs::read_to_string(&json_path)?;
    let sidecar: MediaInfoSidecar = serde_json::from_str(&json)?;

    // Restore media streams
    if !sidecar.media_streams.is_empty() {
        MediaStreams::delete_many()
            .filter(media_streams::Column::ItemId.eq(item_id))
            .exec(db)
            .await?;

        let now = now_unix();
        for stream in &sidecar.media_streams {
            let id = stable_item_id(std::path::Path::new(&format!(
                "{item_id}:{}:{}",
                stream.stream_index, stream.stream_type
            )));
            MediaStreams::insert(media_streams::ActiveModel {
                id: Set(id),
                item_id: Set(item_id.to_string()),
                stream_index: Set(stream.stream_index),
                stream_type: Set(stream.stream_type.clone()),
                codec: Set(stream.codec.clone()),
                language: Set(stream.language.clone()),
                title: Set(stream.title.clone()),
                bit_rate: Set(stream.bit_rate),
                width: Set(stream.width),
                height: Set(stream.height),
                channels: Set(stream.channels),
                sample_rate: Set(stream.sample_rate),
                is_external: Set(stream.is_external as i64),
                created_at: Set(now),
                ..Default::default()
            })
            .exec_without_returning(db)
            .await?;
        }
    }

    // Restore item properties
    if sidecar.runtime_ticks.is_some()
        || sidecar.container.is_some()
        || sidecar.size_bytes.is_some()
    {
        if let Some(item) = MediaItems::find_by_id(item_id.to_string()).one(db).await? {
            let mut active: media_items::ActiveModel = item.into();
            if let Some(runtime_ticks) = sidecar.runtime_ticks {
                active.runtime_ticks = Set(Some(runtime_ticks));
            }
            if let Some(container) = sidecar.container.clone() {
                active.container = Set(Some(container));
            }
            if let Some(size_bytes) = sidecar.size_bytes {
                active.size_bytes = Set(Some(size_bytes));
            }
            active.update(db).await?;
        }
    }

    // Restore chapters
    if !sidecar.chapters.is_empty() {
        let chapters: Vec<crate::chapters::ChapterInfo> = sidecar
            .chapters
            .iter()
            .map(|c| crate::chapters::ChapterInfo {
                id: String::new(),
                item_id: item_id.to_string(),
                start_position_ticks: c.start_position_ticks,
                name: c.name.clone(),
                marker_type: c.marker_type.clone(),
                source: "mediainfo".to_string(),
            })
            .collect();
        crate::chapters::save_chapters(db, item_id, &chapters).await?;
    }

    tracing::debug!(
        "deserialized mediainfo for {item_id} from {}",
        json_path.display()
    );
    Ok(true)
}

fn get_drive_root(path: &Path) -> PathBuf {
    let mut components = path.components();
    match components.next() {
        Some(std::path::Component::Prefix(prefix)) => PathBuf::from(prefix.as_os_str()),
        Some(std::path::Component::RootDir) => PathBuf::from("/"),
        _ => PathBuf::from("/"),
    }
}
