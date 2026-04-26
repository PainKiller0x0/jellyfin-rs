use std::path::{Path, PathBuf};

use anyhow::Context;
use sqlx::AnyPool;

use crate::util::{now_unix, stable_text_id};

pub async fn upsert_sidecar_images(
    db: &AnyPool,
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

pub async fn upsert_image_asset(
    db: &AnyPool,
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
    sqlx::query(r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, size_bytes, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET path = excluded.path, etag = excluded.etag, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#)
        .bind(stable_text_id(&format!("image-asset:{item_id}:{image_type}:{image_index}")))
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .bind(path)
        .bind(etag)
        .bind(size_bytes)
        .bind(now)
        .bind(now)
        .execute(db)
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
