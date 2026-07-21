use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
    sea_query::OnConflict,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::row_ext::QueryResultExt;
use crate::entities::{
    chapters::{self, Entity as Chapters},
    image_assets::{self, Entity as ImageAssets},
    media_items::{self, Entity as MediaItems},
};
use crate::util::{now_unix, stable_item_id, stable_text_id};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterInfo {
    pub id: String,
    pub item_id: String,
    pub start_position_ticks: i64,
    pub name: String,
    pub marker_type: Option<String>,
    pub source: String,
    #[serde(default)]
    pub image_path: Option<String>,
    #[serde(default)]
    pub image_date_modified: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryChapterImageOptions {
    pub enabled: bool,
    pub extract_during_scan: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ChapterImageScanSettings {
    libraries: HashMap<String, LibraryChapterImageOptions>,
    resolution: Option<(u32, u32)>,
}

impl ChapterImageScanSettings {
    pub fn library_options(&self, library_id: &str) -> LibraryChapterImageOptions {
        self.libraries.get(library_id).copied().unwrap_or_default()
    }
}

pub async fn chapter_image_scan_settings(db: &DatabaseConnection) -> ChapterImageScanSettings {
    let libraries = crate::db::settings::find_by_prefix(db, "LibraryOptions.")
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|setting| {
            let library_id = setting.key.strip_prefix("LibraryOptions.")?.to_string();
            let value = serde_json::from_str::<Value>(&setting.value).ok()?;
            Some((
                library_id,
                LibraryChapterImageOptions {
                    enabled: value
                        .get("EnableChapterImageExtraction")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    extract_during_scan: value
                        .get("ExtractChapterImagesDuringLibraryScan")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
            ))
        })
        .collect();
    let resolution = crate::db::settings::get(db, "server_config")
        .await
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|value| {
            value
                .get("ChapterImageResolution")
                .and_then(Value::as_str)
                .and_then(chapter_image_resolution)
        });
    ChapterImageScanSettings {
        libraries,
        resolution,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn refresh_chapter_images(
    db: &DatabaseConnection,
    ffmpeg_path: &str,
    item_id: &str,
    library_id: &str,
    media_path: &Path,
    video_type: Option<&str>,
    iso_type: Option<&str>,
    modified_at: i64,
    runtime_ticks: i64,
    has_video_stream: bool,
    is_shortcut: bool,
    during_scan: bool,
    settings: &ChapterImageScanSettings,
) -> anyhow::Result<bool> {
    let chapters = Chapters::find()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::MarkerType.is_null())
        .order_by_asc(chapters::Column::StartPositionTicks)
        .all(db)
        .await?;
    if chapters.is_empty() {
        return Ok(true);
    }

    let options = settings.library_options(library_id);
    let folder = chapter_image_folder(item_id);
    let mut saved_images = chapter_image_files(&folder).await;
    if !options.enabled {
        clear_chapter_images(db, item_id, &saved_images).await?;
        return Ok(true);
    }

    let eligible = !is_shortcut && has_video_stream && runtime_ticks > 0;
    let average_duration = average_chapter_duration(
        &chapters
            .iter()
            .map(|chapter| chapter.start_position_ticks)
            .collect::<Vec<_>>(),
    );
    let extract_requested = !during_scan || options.extract_during_scan;
    let extract_images =
        extract_requested && eligible && (chapters.len() < 2 || average_duration >= 10_000_000);
    if extract_requested && !extract_images && chapters.len() >= 2 {
        tracing::info!(
            "skipping chapter image extraction for {item_id}; average chapter duration is below one second or media is ineligible"
        );
    }

    let mut success = true;
    let mut current = Vec::new();
    for (index, chapter) in chapters.iter().enumerate() {
        if chapter.start_position_ticks >= runtime_ticks {
            break;
        }
        let path = chapter_image_path(&folder, modified_at, chapter.start_position_ticks);
        let exists = saved_images.iter().any(|saved| paths_equal(saved, &path));
        if !exists && extract_images {
            let offset = if chapter.start_position_ticks == 0 {
                runtime_ticks.min(15 * 10_000_000)
            } else {
                chapter.start_position_ticks
            };
            if let Err(error) = extract_video_image(
                ffmpeg_path,
                media_path,
                video_type,
                iso_type,
                offset,
                settings.resolution,
                &path,
            )
            .await
            {
                tracing::error!("chapter image extraction failed for {item_id}: {error:#}");
                success = false;
                break;
            }
            saved_images.push(path.clone());
        }
        if path.is_file() {
            let modified = file_modified_unix(&path).unwrap_or_else(now_unix);
            let etag = stable_text_id(&format!(
                "chapter-image:{item_id}:{}:{modified}",
                chapter.start_position_ticks
            ));
            current.push((index as i64, chapter.id.clone(), path, modified, etag));
        }
    }

    persist_chapter_images(db, item_id, &current).await?;
    delete_dead_chapter_images(&saved_images, current.iter().map(|(_, _, path, _, _)| path)).await;
    Ok(success)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChapterImageRefreshStats {
    pub processed: usize,
    pub failed: usize,
}

pub async fn refresh_all_chapter_images(
    db: &DatabaseConnection,
    ffmpeg_path: &str,
) -> anyhow::Result<ChapterImageRefreshStats> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT mi.id, mi.library_id, mi.path, mi.video_type, mi.iso_type,
                      mi.modified_at, COALESCE(mi.runtime_ticks, 0) AS runtime_ticks,
                      CASE WHEN EXISTS (
                          SELECT 1 FROM media_streams ms
                          WHERE ms.item_id = mi.id AND ms.stream_type = 'Video'
                      ) THEN 1 ELSE 0 END AS has_video_stream
               FROM media_items mi
               WHERE mi.is_folder = 0
                 AND EXISTS (SELECT 1 FROM chapters chapter WHERE chapter.item_id = mi.id)
               ORDER BY mi.id"#,
            vec![],
        ))
        .await
        .context("failed to list chapter image candidates")?;
    let settings = chapter_image_scan_settings(db).await;
    let mut stats = ChapterImageRefreshStats::default();
    for row in &rows {
        stats.processed += 1;
        let item_id = row.get_str("id")?;
        let path = row.get_str("path")?;
        let is_shortcut = Path::new(&path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"));
        match refresh_chapter_images(
            db,
            ffmpeg_path,
            &item_id,
            &row.get_str("library_id")?,
            Path::new(&path),
            row.get_opt_str("video_type")?.as_deref(),
            row.get_opt_str("iso_type")?.as_deref(),
            row.get_i64("modified_at")?,
            row.get_i64("runtime_ticks").unwrap_or_default(),
            row.get_i64("has_video_stream").unwrap_or_default() != 0,
            is_shortcut,
            false,
            &settings,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => stats.failed += 1,
            Err(error) => {
                stats.failed += 1;
                tracing::warn!("failed to refresh chapter images for {path}: {error:#}");
            }
        }
    }
    Ok(stats)
}

fn average_chapter_duration(chapters: &[i64]) -> i64 {
    if chapters.len() < 2 {
        return 0;
    }
    chapters
        .windows(2)
        .map(|window| window[1].saturating_sub(window[0]))
        .sum::<i64>()
        / i64::try_from(chapters.len()).unwrap_or(i64::MAX)
}

fn chapter_image_resolution(value: &str) -> Option<(u32, u32)> {
    match value {
        "P144" => Some((256, 144)),
        "P240" => Some((426, 240)),
        "P360" => Some((640, 360)),
        "P480" => Some((854, 480)),
        "P720" => Some((1280, 720)),
        "P1080" => Some((1920, 1080)),
        "P1440" => Some((2560, 1440)),
        "P2160" => Some((3840, 2160)),
        _ => None,
    }
}

fn chapter_image_folder(item_id: &str) -> PathBuf {
    let data_dir = std::env::var_os("JELLYFIN_RS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    let prefix = item_id.get(..2).unwrap_or(item_id);
    data_dir
        .join("metadata")
        .join(prefix)
        .join(item_id)
        .join("chapters")
}

fn chapter_image_path(folder: &Path, modified_at: i64, position_ticks: i64) -> PathBuf {
    folder.join(format!("{modified_at}_{position_ticks}.jpg"))
}

async fn chapter_image_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(mut entries) = tokio::fs::read_dir(folder).await else {
        return Vec::new();
    };
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn file_modified_unix(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

async fn clear_chapter_images(
    db: &DatabaseConnection,
    item_id: &str,
    images: &[PathBuf],
) -> anyhow::Result<()> {
    Chapters::update_many()
        .col_expr(
            chapters::Column::ImagePath,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            chapters::Column::ImageDateModified,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .filter(chapters::Column::ItemId.eq(item_id))
        .exec(db)
        .await?;
    ImageAssets::delete_many()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq("Chapter"))
        .exec(db)
        .await?;
    delete_dead_chapter_images(images, std::iter::empty::<&PathBuf>()).await;
    Ok(())
}

async fn persist_chapter_images(
    db: &DatabaseConnection,
    item_id: &str,
    images: &[(i64, String, PathBuf, i64, String)],
) -> anyhow::Result<()> {
    Chapters::update_many()
        .col_expr(
            chapters::Column::ImagePath,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            chapters::Column::ImageDateModified,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .filter(chapters::Column::ItemId.eq(item_id))
        .exec(db)
        .await?;
    ImageAssets::delete_many()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq("Chapter"))
        .exec(db)
        .await?;
    let now = now_unix();
    for (index, chapter_id, path, modified, etag) in images {
        Chapters::update_many()
            .col_expr(
                chapters::Column::ImagePath,
                sea_orm::sea_query::Expr::value(Some(path.to_string_lossy().to_string())),
            )
            .col_expr(
                chapters::Column::ImageDateModified,
                sea_orm::sea_query::Expr::value(Some(*modified)),
            )
            .col_expr(
                chapters::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(chapters::Column::Id.eq(chapter_id))
            .exec(db)
            .await?;
        let size_bytes = path
            .metadata()
            .ok()
            .and_then(|metadata| i64::try_from(metadata.len()).ok());
        ImageAssets::insert(image_assets::ActiveModel {
            id: Set(stable_text_id(&format!("chapter:{item_id}:{index}"))),
            item_id: Set(item_id.to_string()),
            image_type: Set("Chapter".to_string()),
            image_index: Set(*index),
            path: Set(Some(path.to_string_lossy().to_string())),
            etag: Set(Some(etag.clone())),
            width: Set(None),
            height: Set(None),
            size_bytes: Set(size_bytes),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            OnConflict::column(image_assets::Column::Id)
                .update_columns([
                    image_assets::Column::Path,
                    image_assets::Column::Etag,
                    image_assets::Column::SizeBytes,
                    image_assets::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

async fn delete_dead_chapter_images<'a>(
    images: &[PathBuf],
    current: impl Iterator<Item = &'a PathBuf>,
) {
    let current = current.collect::<Vec<_>>();
    for image in images {
        let extension = image
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp"
        ) || current.iter().any(|path| paths_equal(path, image))
        {
            continue;
        }
        if let Err(error) = tokio::fs::remove_file(image).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "failed to delete dead chapter image {}: {error}",
                    image.display()
                );
            }
        }
    }
}

async fn extract_video_image(
    ffmpeg_path: &str,
    media_path: &Path,
    video_type: Option<&str>,
    iso_type: Option<&str>,
    offset_ticks: i64,
    resolution: Option<(u32, u32)>,
    output_path: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temp_path = output_path.with_extension(format!("{}.tmp.jpg", std::process::id()));
    let first = run_ffmpeg_image_extract(
        ffmpeg_path,
        media_path,
        video_type,
        iso_type,
        offset_ticks,
        resolution,
        &temp_path,
        true,
    )
    .await;
    if first.is_err() {
        run_ffmpeg_image_extract(
            ffmpeg_path,
            media_path,
            video_type,
            iso_type,
            offset_ticks,
            resolution,
            &temp_path,
            false,
        )
        .await?;
    }
    tokio::fs::rename(&temp_path, output_path)
        .await
        .with_context(|| format!("failed to publish chapter image {}", output_path.display()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_ffmpeg_image_extract(
    ffmpeg_path: &str,
    media_path: &Path,
    video_type: Option<&str>,
    iso_type: Option<&str>,
    offset_ticks: i64,
    resolution: Option<(u32, u32)>,
    output_path: &Path,
    thumbnail: bool,
) -> anyhow::Result<()> {
    let mut command = tokio::process::Command::new(ffmpeg_path);
    command.kill_on_drop(true);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{:.7}", offset_ticks as f64 / 10_000_000.0));
    match (video_type, iso_type) {
        (Some("Dvd"), _) => {
            command
                .arg("-f")
                .arg("dvdvideo")
                .arg("-title")
                .arg("1")
                .arg("-i")
                .arg(media_path);
        }
        (Some("BluRay"), _) | (Some("Iso"), Some("BluRay")) => {
            command
                .arg("-i")
                .arg(format!("bluray:{}", media_path.to_string_lossy()));
        }
        _ => {
            command.arg("-i").arg(media_path);
        }
    }
    command.arg("-map").arg("0:v:0").arg("-frames:v").arg("1");
    let mut filters = vec!["scale=round(iw*sar/2)*2:round(ih/2)*2".to_string()];
    if thumbnail {
        filters.push("thumbnail=n=24".to_string());
    }
    command.arg("-vf").arg(filters.join(","));
    if let Some((width, height)) = resolution {
        command.arg("-s").arg(format!("{width}x{height}"));
    }
    command.arg("-q:v").arg("2").arg("-y").arg(output_path);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .context("ffmpeg chapter image extraction timed out")??;
    if !output.status.success() {
        anyhow::bail!(
            "ffmpeg exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !output_path.is_file() {
        anyhow::bail!("ffmpeg did not create a chapter image");
    }
    Ok(())
}

/// Get all chapters for an item, ordered by start_position_ticks.
pub async fn get_chapters(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<ChapterInfo>> {
    let chapters = Chapters::find()
        .filter(chapters::Column::ItemId.eq(item_id))
        .order_by_asc(chapters::Column::StartPositionTicks)
        .all(db)
        .await?;

    Ok(chapters.into_iter().map(ChapterInfo::from).collect())
}

/// Save chapters for an item. Deletes existing chapters and inserts new ones.
pub async fn save_chapters(
    db: &DatabaseConnection,
    item_id: &str,
    chapters: &[ChapterInfo],
) -> anyhow::Result<()> {
    Chapters::delete_many()
        .filter(chapters::Column::ItemId.eq(item_id))
        .exec(db)
        .await?;

    let now = now_unix();
    for ch in chapters {
        let id = if ch.id.is_empty() {
            stable_item_id(std::path::Path::new(&format!(
                "{}:{}:{}",
                item_id, ch.start_position_ticks, ch.name
            )))
        } else {
            ch.id.clone()
        };
        Chapters::insert(chapter_active_model(
            id,
            item_id.to_string(),
            ch.start_position_ticks,
            ch.name.clone(),
            ch.marker_type.clone(),
            ch.source.clone(),
            ch.image_path.clone(),
            ch.image_date_modified,
            now,
        ))
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

/// Save chapters from one probe/source without touching manual markers or sidecar chapters.
pub async fn save_source_chapters(
    db: &DatabaseConnection,
    item_id: &str,
    source: &str,
    chapters: &[ChapterInfo],
) -> anyhow::Result<()> {
    Chapters::delete_many()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::Source.eq(source))
        .filter(chapters::Column::MarkerType.is_null())
        .exec(db)
        .await?;

    let now = now_unix();
    for ch in chapters {
        Chapters::insert(chapter_active_model(
            stable_item_id(std::path::Path::new(&format!(
                "{}:{}:{}:{}",
                source, item_id, ch.start_position_ticks, ch.name
            ))),
            item_id.to_string(),
            ch.start_position_ticks,
            ch.name.clone(),
            None,
            source.to_string(),
            None,
            None,
            now,
        ))
        .exec_without_returning(db)
        .await?;
    }

    Ok(())
}

/// Clear all intro/credits markers for an item.
#[allow(dead_code)]
pub async fn clear_intro_credits_markers(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<()> {
    Chapters::delete_many()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::MarkerType.is_not_null())
        .exec(db)
        .await?;
    Ok(())
}

/// Get intro markers (start, end) for an item.
pub async fn get_intro_markers(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<(i64, i64)>> {
    let markers = Chapters::find()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::MarkerType.is_in(["IntroStart", "IntroEnd"]))
        .all(db)
        .await?;

    let start = markers
        .iter()
        .find(|marker| marker.marker_type.as_deref() == Some("IntroStart"))
        .map(|marker| marker.start_position_ticks);
    let end = markers
        .iter()
        .find(|marker| marker.marker_type.as_deref() == Some("IntroEnd"))
        .map(|marker| marker.start_position_ticks);

    match (start, end) {
        (Some(start), Some(end)) if start < end => Ok(Some((start, end))),
        _ => Ok(None),
    }
}

/// Update intro markers for an episode and propagate to siblings in the same season.
#[allow(dead_code)]
pub async fn update_intro_for_season(
    db: &DatabaseConnection,
    episode_id: &str,
    intro_start: i64,
    intro_end: i64,
    source: &str,
) -> anyhow::Result<()> {
    if intro_start >= intro_end {
        return Ok(());
    }

    // Find all episodes in the same season
    let season_id = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .map(|item| item.parent_id);

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(season_id))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .order_by_asc(media_items::Column::EpisodeNumber)
        .all(db)
        .await?;

    let now = now_unix();
    for episode in &episodes {
        let ep_id = episode.id.clone();

        // Check if this episode already has markers with a non-behavior source
        let existing = get_intro_markers(db, &ep_id).await?;
        if ep_id != episode_id {
            if let Some((_s, _e)) = existing {
                // Skip if it already has markers (from any source)
                continue;
            }
        }

        // Remove existing intro markers
        Chapters::delete_many()
            .filter(chapters::Column::ItemId.eq(&ep_id))
            .filter(chapters::Column::MarkerType.is_in(["IntroStart", "IntroEnd"]))
            .exec(db)
            .await?;

        // Insert new markers
        let start_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroStart:{intro_start}"
        )));
        let end_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroEnd:{intro_end}"
        )));

        Chapters::insert_many([
            chapter_active_model(
                start_id,
                ep_id.clone(),
                intro_start,
                "IntroStart".to_string(),
                Some("IntroStart".to_string()),
                source.to_string(),
                None,
                None,
                now,
            ),
            chapter_active_model(
                end_id,
                ep_id,
                intro_end,
                "IntroEnd".to_string(),
                Some("IntroEnd".to_string()),
                source.to_string(),
                None,
                None,
                now,
            ),
        ])
        .exec_without_returning(db)
        .await?;
    }

    Ok(())
}

/// Update credits marker for an episode and propagate to siblings in the same season.
#[allow(dead_code)]
pub async fn update_credits_for_season(
    db: &DatabaseConnection,
    episode_id: &str,
    credits_start: i64,
    source: &str,
) -> anyhow::Result<()> {
    let season_id = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .map(|item| item.parent_id);

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(season_id))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .order_by_asc(media_items::Column::EpisodeNumber)
        .all(db)
        .await?;

    // Calculate credits duration from the source episode
    let source_runtime = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .and_then(|item| item.runtime_ticks)
        .unwrap_or(0);

    let credits_duration = source_runtime - credits_start;
    if credits_duration <= 0 {
        return Ok(());
    }

    let now = now_unix();
    for episode in &episodes {
        let ep_id = episode.id.clone();
        let ep_runtime = episode.runtime_ticks.unwrap_or(0);
        let ep_credits_start = ep_runtime - credits_duration;

        // Remove existing credits marker
        Chapters::delete_many()
            .filter(chapters::Column::ItemId.eq(&ep_id))
            .filter(chapters::Column::MarkerType.eq("CreditsStart"))
            .exec(db)
            .await?;

        let marker_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:CreditsStart:{ep_credits_start}"
        )));
        Chapters::insert(chapter_active_model(
            marker_id,
            ep_id,
            ep_credits_start,
            "CreditsStart".to_string(),
            Some("CreditsStart".to_string()),
            source.to_string(),
            None,
            None,
            now,
        ))
        .exec_without_returning(db)
        .await?;
    }

    Ok(())
}

impl From<chapters::Model> for ChapterInfo {
    fn from(model: chapters::Model) -> Self {
        Self {
            id: model.id,
            item_id: model.item_id,
            start_position_ticks: model.start_position_ticks,
            name: model.name,
            marker_type: model.marker_type,
            source: model.source,
            image_path: model.image_path,
            image_date_modified: model.image_date_modified,
        }
    }
}

fn chapter_active_model(
    id: String,
    item_id: String,
    start_position_ticks: i64,
    name: String,
    marker_type: Option<String>,
    source: String,
    image_path: Option<String>,
    image_date_modified: Option<i64>,
    now: i64,
) -> chapters::ActiveModel {
    chapters::ActiveModel {
        id: Set(id),
        item_id: Set(item_id),
        start_position_ticks: Set(start_position_ticks),
        name: Set(name),
        marker_type: Set(marker_type),
        source: Set(source),
        image_path: Set(image_path),
        image_date_modified: Set(image_date_modified),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        average_chapter_duration, chapter_image_scan_settings, extract_video_image,
        persist_chapter_images,
    };
    use crate::entities::{
        chapters::{self, Entity as Chapters},
        image_assets::Entity as ImageAssets,
        libraries::{self, Entity as Libraries},
        media_items::{self, Entity as MediaItems},
    };
    use image::GenericImageView;
    use sea_orm::{DatabaseConnection, EntityTrait, Set};
    use std::{path::Path, process::Command};

    #[test]
    fn average_duration_matches_jellyfin_chapter_manager() {
        assert_eq!(average_chapter_duration(&[]), 0);
        assert_eq!(average_chapter_duration(&[0]), 0);
        assert_eq!(
            average_chapter_duration(&[0, 30_000_000, 90_000_000]),
            30_000_000
        );
    }

    #[tokio::test]
    async fn chapter_image_settings_read_library_and_server_configuration() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        crate::db::settings::set(
            &db,
            "LibraryOptions.movies",
            r#"{"EnableChapterImageExtraction":true,"ExtractChapterImagesDuringLibraryScan":true}"#,
        )
        .await
        .unwrap();
        crate::db::settings::set(&db, "server_config", r#"{"ChapterImageResolution":"P360"}"#)
            .await
            .unwrap();

        let settings = chapter_image_scan_settings(&db).await;
        let options = settings.library_options("movies");
        assert!(options.enabled);
        assert!(options.extract_during_scan);
        assert_eq!(settings.resolution, Some((640, 360)));
        assert_eq!(settings.library_options("missing"), Default::default());
    }

    #[tokio::test]
    async fn chapter_images_are_persisted_for_standard_image_routes() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(&db).await;
        Chapters::insert(chapters::ActiveModel {
            id: Set("chapter-1".to_string()),
            item_id: Set("movie".to_string()),
            start_position_ticks: Set(0),
            name: Set("Chapter 1".to_string()),
            marker_type: Set(None),
            source: Set("ffprobe".to_string()),
            image_path: Set(None),
            image_date_modified: Set(None),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("jellyfin-rs-chapter-{}.jpg", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"jpeg").unwrap();

        persist_chapter_images(
            &db,
            "movie",
            &[(
                0,
                "chapter-1".to_string(),
                path.clone(),
                42,
                "etag".to_string(),
            )],
        )
        .await
        .unwrap();

        let chapter = Chapters::find_by_id("chapter-1")
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chapter.image_path.as_deref(), path.to_str());
        assert_eq!(chapter.image_date_modified, Some(42));
        let image = ImageAssets::find_by_id(crate::util::stable_text_id("chapter:movie:0"))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(image.item_id, "movie");
        assert_eq!(image.image_type, "Chapter");
        assert_eq!(image.image_index, 0);
        assert_eq!(image.etag.as_deref(), Some("etag"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn ffmpeg_chapter_extraction_honors_requested_resolution() {
        let ffmpeg =
            std::env::var("JELLYFIN_RS_SA_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string());
        if Command::new(&ffmpeg).arg("-version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "jellyfin-rs-chapter-extract-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("input.mp4");
        let output = root.join("chapter.jpg");
        let generated = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=320x180:d=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&input)
            .status();
        if !generated.is_ok_and(|status| status.success()) {
            let _ = std::fs::remove_dir_all(root);
            return;
        }

        extract_video_image(
            &ffmpeg,
            Path::new(&input),
            None,
            None,
            5_000_000,
            Some((256, 144)),
            &output,
        )
        .await
        .unwrap();
        assert_eq!(image::open(&output).unwrap().dimensions(), (256, 144));
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn insert_media_item(db: &DatabaseConnection) {
        Libraries::insert(libraries::ActiveModel {
            id: Set("movies".to_string()),
            name: Set("Movies".to_string()),
            collection_type: Set("movies".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
        MediaItems::insert(media_items::ActiveModel {
            id: Set("movie".to_string()),
            title: Set("Movie".to_string()),
            path: Set("/tmp/movie.mkv".to_string()),
            library_id: Set("movies".to_string()),
            parent_id: Set("movies".to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(0),
            is_public: Set(1),
            created_at: Set(1),
            modified_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }
}
