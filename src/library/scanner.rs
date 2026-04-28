use std::path::PathBuf;

use anyhow::Context;
use sqlx::Row;
use walkdir::WalkDir;

use crate::{
    app::state::AppState,
    library::{
        classify::{classify_media_path, parent_id_for_path, tv_folder_type},
        images::upsert_sidecar_images,
        metadata::parse_sidecar_metadata,
        naming::parse_media_name,
        path_utils,
        probe::probe_media,
        storage::{
            ScannedMediaItem, remove_missing_media_items, upsert_default_media_stream,
            upsert_media_item, upsert_media_metadata, upsert_probed_media_streams,
        },
        subtitles::upsert_sidecar_subtitles,
    },
    util::{infer_library_id_from_path, now_unix},
};

pub async fn scan_media_library(state: &AppState) -> anyhow::Result<usize> {
    let roots = media_roots(state).await?;
    if roots.is_empty() {
        tracing::info!("media scan skipped because no media library paths are configured");
        return Ok(0);
    }

    let mut scanned = 0usize;
    let mut seen_paths = Vec::new();
    for (root, library_id) in roots {
        if !root.exists() {
            tracing::warn!("media directory does not exist: {}", root.display());
            continue;
        }

        for entry in WalkDir::new(&root).follow_links(false).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!("failed to read media path: {error}");
                    continue;
                }
            };
            let path = entry.path();
            if path == root {
                continue;
            }

            let resolved = match path_utils::resolve_path_info(path) {
                Ok(resolved) => resolved,
                Err(error) => {
                    tracing::warn!("failed to resolve media path {}: {error:#}", path.display());
                    continue;
                }
            };

            let path_string = resolved.path.clone();
            let parent_id = parent_id_for_path(path, &root, &library_id);

            if resolved.is_directory {
                seen_paths.push(path_string.clone());
                let item = ScannedMediaItem::folder_with_type(
                    resolved.id,
                    library_id.clone(),
                    parent_id,
                    path_string,
                    resolved.name,
                    tv_folder_type(path, &root, &library_id),
                    resolved.modified_at,
                );
                upsert_media_item(&state.db, &item).await?;
                upsert_sidecar_images(&state.db, path, &item.id).await?;
                continue;
            }

            if resolved.is_directory {
                continue;
            }
            let Some(item_type) = classify_media_path(path, &library_id) else {
                continue;
            };

            seen_paths.push(path_string.clone());
            let parsed_metadata = parse_sidecar_metadata(path).await;
            let parsed_name = parse_media_name(path, &library_id);
            let title = if has_sidecar_nfo(path) {
                parsed_metadata
                    .title
                    .clone()
                    .unwrap_or_else(|| resolved.name.clone())
            } else if parsed_name.title.is_empty() {
                resolved.name.clone()
            } else {
                parsed_name.title.clone()
            };
            let probe = probe_media(path);
            let item = ScannedMediaItem {
                id: resolved.id,
                title,
                path: path_string,
                library_id: library_id.clone(),
                parent_id,
                item_type,
                is_folder: false,
                container: path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_ascii_lowercase()),
                overview: parsed_metadata.overview.clone(),
                official_rating: parsed_metadata.official_rating.clone(),
                extended_video_type: (!parsed_name.extended_video_types.is_empty())
                    .then(|| parsed_name.extended_video_types.join(",")),
                production_year: parsed_metadata.production_year,
                runtime_ticks: probe.as_ref().and_then(|probe| probe.runtime_ticks),
                size_bytes: resolved.size_bytes,
                modified_at: resolved.modified_at,
                created_at: now_unix(),
            };

            upsert_media_item(&state.db, &item).await?;
            upsert_media_metadata(&state.db, &item.id, &parsed_metadata).await?;
            upsert_sidecar_images(&state.db, path, &item.id).await?;
            let probed = if let Some(probe) = &probe {
                upsert_probed_media_streams(&state.db, &item, probe).await?
            } else {
                false
            };
            if !probed {
                upsert_default_media_stream(&state.db, &item).await?;
            }
            upsert_sidecar_subtitles(&state.db, path, &item.id).await?;
            scanned += 1;
        }
    }

    remove_missing_media_items(&state.db, &seen_paths).await?;
    tracing::info!("media scan indexed {scanned} item(s)");
    Ok(scanned)
}

fn has_sidecar_nfo(path: &std::path::Path) -> bool {
    path.with_extension("nfo").exists()
        || path
            .parent()
            .map(|parent| parent.join("movie.nfo").exists())
            .unwrap_or_default()
}

async fn media_roots(state: &AppState) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let rows = sqlx::query("SELECT path, library_id FROM library_paths ORDER BY path ASC")
        .fetch_all(&state.db)
        .await
        .context("failed to list library paths for scan")?;
    if rows.is_empty() {
        return Ok(state
            .media_dirs
            .iter()
            .map(|path| {
                (
                    PathBuf::from(path_utils::normalize_path(&path.to_string_lossy())),
                    infer_library_id_from_path(&path.to_string_lossy()).to_string(),
                )
            })
            .collect());
    }

    rows.into_iter()
        .map(|row| -> anyhow::Result<(PathBuf, String)> {
            Ok((
                PathBuf::from(path_utils::normalize_path(
                    &row.try_get::<String, _>("path")?,
                )),
                row.try_get("library_id")?,
            ))
        })
        .collect()
}
