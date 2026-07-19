use std::path::{Path, PathBuf};

use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::{Deserialize, Serialize};

use crate::db::helpers::pg_statement;
use crate::db::row_ext::QueryResultExt;
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
    // Get media streams via raw SQL
    let stream_rows = db
        .query_all(pg_statement(
            "SELECT stream_index, stream_type, codec, language, title, bit_rate, width, height, channels, sample_rate, is_external FROM media_streams WHERE item_id = ? ORDER BY stream_index ASC",
            vec![item_id.into()],
        ))
        .await?;

    // Get runtime and container
    let item_row = db
        .query_one(pg_statement(
            "SELECT runtime_ticks, container, size_bytes FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await?;

    let runtime_ticks = item_row
        .as_ref()
        .and_then(|r| r.get_i64("runtime_ticks").ok());
    let container = item_row.as_ref().and_then(|r| r.get_str("container").ok());
    let size_bytes = item_row.as_ref().and_then(|r| r.get_i64("size_bytes").ok());

    // Get chapters
    let chapters = crate::chapters::get_chapters(db, item_id).await?;

    let sidecar = MediaInfoSidecar {
        media_streams: stream_rows
            .iter()
            .map(|s| MediaStreamEntry {
                stream_index: s.get_i64("stream_index").unwrap_or(0),
                stream_type: s.get_str("stream_type").unwrap_or_default(),
                codec: s.get_str("codec").ok(),
                language: s.get_str("language").ok(),
                title: s.get_str("title").ok(),
                bit_rate: s.get_i64("bit_rate").ok(),
                width: s.get_i64("width").ok(),
                height: s.get_i64("height").ok(),
                channels: s.get_i64("channels").ok(),
                sample_rate: s.get_i64("sample_rate").ok(),
                is_external: s.get_i64("is_external").unwrap_or(0) != 0,
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
        db.execute(pg_statement(
            "DELETE FROM media_streams WHERE item_id = ?",
            vec![item_id.into()],
        ))
        .await?;

        let now = now_unix();
        for stream in &sidecar.media_streams {
            let id = stable_item_id(std::path::Path::new(&format!(
                "{item_id}:{}:{}",
                stream.stream_index, stream.stream_type
            )));
            db.execute(pg_statement(
                "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, bit_rate, width, height, channels, sample_rate, is_external, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                vec![
                    id.into(),
                    item_id.into(),
                    stream.stream_index.into(),
                    stream.stream_type.clone().into(),
                    stream.codec.clone().into(),
                    stream.language.clone().into(),
                    stream.title.clone().into(),
                    stream.bit_rate.into(),
                    stream.width.into(),
                    stream.height.into(),
                    stream.channels.into(),
                    stream.sample_rate.into(),
                    (stream.is_external as i64).into(),
                    now.into(),
                ],
            ))
            .await?;
        }
    }

    // Restore item properties
    let mut updates = Vec::new();
    let mut params: Vec<sea_orm::Value> = Vec::new();

    if let Some(rt) = sidecar.runtime_ticks {
        updates.push("runtime_ticks = ?");
        params.push(rt.into());
    }
    if let Some(ref c) = sidecar.container {
        updates.push("container = ?");
        params.push(c.clone().into());
    }
    if let Some(sb) = sidecar.size_bytes {
        updates.push("size_bytes = ?");
        params.push(sb.into());
    }

    if !updates.is_empty() {
        params.push(item_id.into());
        let sql = format!("UPDATE media_items SET {} WHERE id = ?", updates.join(", "));
        db.execute(pg_statement(&sql, params)).await?;
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
