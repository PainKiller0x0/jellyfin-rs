use std::path::PathBuf;

use anyhow::Context;
use sea_orm::ConnectionTrait;
use walkdir::WalkDir;

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
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

    let mut tasks = tokio::task::JoinSet::new();
    for (root, library_id, collection_type) in roots {
        if !root.exists() {
            tracing::warn!("media directory does not exist: {}", root.display());
            continue;
        }
        let db = state.db.clone();
        tasks.spawn(async move {
            scan_root(db, root, library_id, collection_type).await
        });
    }

    let mut total = 0usize;
    let mut all_seen = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok((count, paths))) => {
                total += count;
                all_seen.extend(paths);
            }
            Ok(Err(e)) => tracing::warn!("library scan failed: {e:#}"),
            Err(e) => tracing::warn!("scan task panicked: {e}"),
        }
    }

    remove_missing_media_items(&state.db, &all_seen).await?;
    tracing::info!("media scan indexed {total} item(s) across all libraries");

    // Post-scan: fetch TMDb episode metadata (series provider_ids are ready now)
    if let Some(api_key) = std::env::var("JELLYFIN_RS_TMDB_API_KEY").ok().filter(|k| !k.is_empty()) {
        if let Err(e) = crate::library::tmdb_metadata::batch_fetch_episode_tmdb(&state.db, &api_key).await {
            tracing::warn!("episode TMDb fetch failed: {e:#}");
        }
    }

    Ok(total)
}

async fn scan_root(
    db: sea_orm::DatabaseConnection,
    root: PathBuf,
    library_id: String,
    collection_type: String,
) -> anyhow::Result<(usize, Vec<String>)> {
    let mut scanned = 0usize;
    let mut seen_paths = Vec::new();

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
            let folder_type = tv_folder_type(path, &root, &collection_type);
            let (folder_title, year) =
                crate::library::tmdb_metadata::clean_title_with_year(&resolved.name);
            let item = ScannedMediaItem::folder_with_type(
                resolved.id,
                library_id.clone(),
                parent_id,
                path_string,
                folder_title,
                folder_type,
                resolved.modified_at,
                year,
            );
            if let Err(e) = upsert_media_item(&db, &item).await {
                tracing::warn!("failed to upsert folder {}: {e:#}", item.path);
                continue;
            }
            upsert_sidecar_images(&db, path, &item.id).await?;
            try_fetch_tmdb(&db, &item, path).await;
            continue;
        }

        let Some(item_type) = classify_media_path(path, &collection_type) else {
            continue;
        };

        seen_paths.push(path_string.clone());
        let parsed_metadata = parse_sidecar_metadata(path).await;
        let parsed_name = parse_media_name(path, &collection_type);
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
            season_number: parsed_name.season_number,
            episode_number: parsed_name.episode_number,
            modified_at: resolved.modified_at,
            created_at: now_unix(),
        };

        if let Err(e) = upsert_media_item(&db, &item).await {
            tracing::warn!("failed to upsert media item {}: {e:#}", item.path);
            continue;
        }
        upsert_media_metadata(&db, &item.id, &parsed_metadata).await?;
        upsert_sidecar_images(&db, path, &item.id).await?;
        let probed = if let Some(probe) = &probe {
            upsert_probed_media_streams(&db, &item, probe).await?
        } else {
            false
        };
        if !probed {
            upsert_default_media_stream(&db, &item).await?;
        }
        upsert_sidecar_subtitles(&db, path, &item.id).await?;
        scanned += 1;
    }

    Ok((scanned, seen_paths))
}

async fn try_fetch_tmdb(db: &sea_orm::DatabaseConnection, item: &ScannedMediaItem, path: &std::path::Path) {
    let Some(api_key) = std::env::var("JELLYFIN_RS_TMDB_API_KEY")
        .ok()
        .filter(|k| !k.is_empty()) else { return };
    let check_path = if item.is_folder { path.to_path_buf() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| path.to_path_buf()) };
    let _ = crate::library::tmdb_metadata::fetch_and_apply_tmdb_metadata(db, &item.id, &item.item_type, &check_path, &api_key).await;
}

fn has_sidecar_nfo(path: &std::path::Path) -> bool {
    path.with_extension("nfo").exists()
        || path
            .parent()
            .map(|parent| parent.join("movie.nfo").exists())
            .unwrap_or_default()
}

async fn media_roots(state: &AppState) -> anyhow::Result<Vec<(PathBuf, String, String)>> {
    let backend = state.db.get_database_backend();
    let rows = state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT lp.path, lp.library_id, l.collection_type FROM library_paths lp JOIN libraries l ON l.id = lp.library_id ORDER BY lp.path ASC",
            vec![],
        ))
        .await
        .context("failed to list library paths for scan")?;
    if rows.is_empty() {
        return Ok(state
            .media_dirs
            .iter()
            .map(|path| {
                let path_str = path.to_string_lossy();
                let id = infer_library_id_from_path(&path_str).to_string();
                let ct = "movies".to_string();
                (PathBuf::from(path_utils::normalize_path(&path_str)), id, ct)
            })
            .collect());
    }

    rows.iter()
        .map(|row| -> anyhow::Result<(PathBuf, String, String)> {
            Ok((
                PathBuf::from(path_utils::normalize_path(&row.get_str("path")?)),
                row.get_str("library_id")?,
                row.get_str("collection_type")?,
            ))
        })
        .collect()
}
