use std::path::{Path, PathBuf};

use anyhow::Context;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set, sea_query::OnConflict,
};

use crate::entities::image_assets::{self, Entity as ImageAssets};
use crate::library::metadata::ParsedImage;
use crate::library::probe::ProbedStream;
use crate::util::{now_unix, stable_text_id};

const MAX_NFO_REMOTE_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub async fn validate_image_assets(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<usize> {
    let assets = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .all(db)
        .await?;
    let mut removed = 0;
    for asset in assets {
        let missing = asset
            .path
            .as_deref()
            .is_some_and(|path| !Path::new(path).is_file());
        if missing {
            ImageAssets::delete_by_id(asset.id).exec(db).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub async fn upsert_sidecar_images(
    db: &DatabaseConnection,
    item_path: &Path,
    item_id: &str,
) -> anyhow::Result<()> {
    for (image_type, image_index, path) in sidecar_image_candidates(item_path) {
        if !path.exists() || !path.is_file() {
            continue;
        }
        let size_bytes = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|metadata| i64::try_from(metadata.len()).ok());
        upsert_image_asset(
            db,
            item_id,
            image_type,
            image_index,
            path.to_string_lossy().as_ref(),
            size_bytes,
        )
        .await?;
    }
    Ok(())
}

pub async fn upsert_nfo_images(
    db: &DatabaseConnection,
    item_id: &str,
    images: &[ParsedImage],
) -> anyhow::Result<()> {
    for image in images {
        match nfo_image_local_path(&image.path) {
            Some(path) => {
                if !path.exists() || !path.is_file() {
                    tracing::warn!(
                        "NFO image path does not exist for item {}: {}",
                        item_id,
                        image.path
                    );
                    continue;
                }
                let size_bytes = tokio::fs::metadata(&path)
                    .await
                    .ok()
                    .and_then(|metadata| i64::try_from(metadata.len()).ok());
                upsert_image_asset(
                    db,
                    item_id,
                    &image.image_type,
                    0,
                    path.to_string_lossy().as_ref(),
                    size_bytes,
                )
                .await?;
            }
            None if is_remote_image_url(&image.path) => {
                match download_nfo_remote_image(item_id, &image.image_type, &image.path).await {
                    Ok((path, size_bytes)) => {
                        upsert_image_asset(
                            db,
                            item_id,
                            &image.image_type,
                            0,
                            path.to_string_lossy().as_ref(),
                            Some(size_bytes),
                        )
                        .await?;
                    }
                    Err(error) => {
                        tracing::warn!(
                            "failed to download NFO image for item {} from {}: {error:#}",
                            item_id,
                            image.path
                        );
                    }
                }
            }
            None => {}
        }
    }
    Ok(())
}

pub async fn extract_embedded_audio_image(
    db: &DatabaseConnection,
    ffmpeg_path: &str,
    media_path: &Path,
    item_id: &str,
    streams: &[ProbedStream],
) -> anyhow::Result<bool> {
    if ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq("Primary"))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    let Some(stream) = preferred_embedded_image_stream(streams) else {
        return Ok(false);
    };
    if !media_path.is_file() {
        return Ok(false);
    }

    let data_dir = std::env::var_os("JELLYFIN_RS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    let prefix = item_id.get(..1).unwrap_or(item_id);
    let output_path = data_dir
        .join("extracted-audio-images")
        .join(prefix)
        .join(format!("{item_id}.jpg"));
    if !output_path.is_file() {
        let parent = output_path
            .parent()
            .context("embedded audio image path has no parent")?;
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create audio image directory: {}",
                parent.display()
            )
        })?;
        let temp_path = output_path.with_extension("tmp.jpg");
        let ffmpeg_path = ffmpeg_path.to_string();
        let media_path = media_path.to_path_buf();
        let stream_index = stream.stream_index;
        let temp_for_task = temp_path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let output = std::process::Command::new(&ffmpeg_path)
                .args(["-v", "error", "-y", "-i"])
                .arg(&media_path)
                .args([
                    "-map",
                    &format!("0:{stream_index}"),
                    "-frames:v",
                    "1",
                    "-f",
                    "image2",
                ])
                .arg(&temp_for_task)
                .output()
                .with_context(|| {
                    format!("failed to start ffmpeg image extraction: {ffmpeg_path}")
                })?;
            if !output.status.success() || !temp_for_task.is_file() {
                let _ = std::fs::remove_file(&temp_for_task);
                anyhow::bail!(
                    "ffmpeg embedded audio image extraction failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        })
        .await
        .context("embedded audio image extraction task panicked")??;
        tokio::fs::rename(&temp_path, &output_path)
            .await
            .with_context(|| format!("failed to publish audio image: {}", output_path.display()))?;
    }

    let size_bytes = tokio::fs::metadata(&output_path)
        .await
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok());
    upsert_image_asset(
        db,
        item_id,
        "Primary",
        0,
        output_path.to_string_lossy().as_ref(),
        size_bytes,
    )
    .await?;
    Ok(true)
}

fn preferred_embedded_image_stream(streams: &[ProbedStream]) -> Option<&ProbedStream> {
    let images = streams
        .iter()
        .filter(|stream| stream.stream_type == "EmbeddedImage")
        .collect::<Vec<_>>();
    images
        .iter()
        .copied()
        .find(|stream| {
            stream
                .comment
                .as_deref()
                .is_some_and(|comment| comment.to_ascii_lowercase().contains("front"))
        })
        .or_else(|| {
            images.iter().copied().find(|stream| {
                stream
                    .comment
                    .as_deref()
                    .is_some_and(|comment| comment.to_ascii_lowercase().contains("cover"))
            })
        })
        .or_else(|| images.first().copied())
}

fn nfo_image_local_path(value: &str) -> Option<PathBuf> {
    if let Some(path) = value.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }
    Path::new(value).is_absolute().then(|| PathBuf::from(value))
}

fn is_remote_image_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

async fn download_nfo_remote_image(
    item_id: &str,
    image_type: &str,
    url: &str,
) -> anyhow::Result<(PathBuf, i64)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build NFO image HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request NFO image: {url}"))?
        .error_for_status()
        .with_context(|| format!("NFO image HTTP error: {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_NFO_REMOTE_IMAGE_BYTES)
    {
        anyhow::bail!("NFO image is larger than {MAX_NFO_REMOTE_IMAGE_BYTES} bytes");
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read NFO image: {url}"))?;
    if bytes.len() as u64 > MAX_NFO_REMOTE_IMAGE_BYTES {
        anyhow::bail!("NFO image is larger than {MAX_NFO_REMOTE_IMAGE_BYTES} bytes");
    }

    let directory = Path::new("data/images/nfo");
    tokio::fs::create_dir_all(directory)
        .await
        .context("failed to create NFO image directory")?;
    let extension = image_extension_from_url(url).unwrap_or("jpg");
    let filename = format!(
        "{}.{}",
        stable_text_id(&format!("nfo-image:{item_id}:{image_type}:{url}")),
        extension
    );
    let path = directory.join(filename);
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to save NFO image: {}", path.display()))?;
    let size_bytes = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    Ok((path, size_bytes))
}

fn image_extension_from_url(url: &str) -> Option<&'static str> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let extension = Path::new(without_query)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        _ => None,
    }
}

pub async fn upsert_image_asset(
    db: &DatabaseConnection,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    path: &str,
    size_bytes: Option<i64>,
) -> anyhow::Result<()> {
    let now = now_unix();
    let etag = stable_text_id(&format!(
        "image:{item_id}:{image_type}:{image_index}:{path}"
    ));
    ImageAssets::insert(image_assets::ActiveModel {
        id: Set(stable_text_id(&format!(
            "image-asset:{item_id}:{image_type}:{image_index}"
        ))),
        item_id: Set(item_id.to_string()),
        image_type: Set(image_type.to_string()),
        image_index: Set(image_index),
        path: Set(Some(path.to_string())),
        etag: Set(Some(etag)),
        size_bytes: Set(size_bytes),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::columns([
            image_assets::Column::ItemId,
            image_assets::Column::ImageType,
            image_assets::Column::ImageIndex,
        ])
        .update_columns([
            image_assets::Column::Path,
            image_assets::Column::Etag,
            image_assets::Column::SizeBytes,
            image_assets::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to upsert image asset for item: {item_id}"))?;
    Ok(())
}

fn sidecar_image_candidates(item_path: &Path) -> Vec<(&'static str, i64, PathBuf)> {
    let mut candidates = Vec::new();
    let directory = if item_path.is_dir() {
        item_path
    } else {
        item_path.parent().unwrap_or_else(|| Path::new(""))
    };

    for extension in ["jpg", "jpeg", "png", "webp"] {
        if !item_path.is_dir() {
            candidates.push(("Primary", 0, item_path.with_extension(extension)));
            if let Some(stem) = item_path.file_stem().and_then(|stem| stem.to_str()) {
                candidates.push((
                    "Backdrop",
                    0,
                    directory.join(format!("{stem}-fanart.{extension}")),
                ));
                candidates.push((
                    "Backdrop",
                    0,
                    directory.join(format!("{stem}-backdrop.{extension}")),
                ));
            }
        }

        for name in ["poster", "folder", "cover"] {
            candidates.push(("Primary", 0, directory.join(format!("{name}.{extension}"))));
        }
        for name in ["fanart", "backdrop", "background"] {
            candidates.push(("Backdrop", 0, directory.join(format!("{name}.{extension}"))));
        }
        for name in ["thumb", "landscape"] {
            candidates.push(("Thumb", 0, directory.join(format!("{name}.{extension}"))));
        }
    }

    candidates
}
