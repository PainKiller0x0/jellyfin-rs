use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use sea_orm::ConnectionTrait;
use tokio::sync::{Mutex, mpsc};
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
            ScannedMediaItem, cached_media_probe_if_current, remove_missing_media_items,
            upsert_default_media_stream, upsert_media_item, upsert_media_metadata,
            upsert_probed_media_streams,
        },
        subtitles::{clear_sidecar_subtitles, upsert_sidecar_subtitles},
    },
    strm,
    util::now_unix,
};

const MEDIA_PROBE_CACHE_VERSION_KEY: &str = "media_probe_cache_version";
const MEDIA_PROBE_CACHE_VERSION: &str = "3";
const DEFAULT_MEDIA_PROBE_CONCURRENCY: usize = 4;
const DEFAULT_MEDIA_PROBE_QUEUE_CAPACITY: usize = 1024;
const DEFAULT_METADATA_FETCH_CONCURRENCY: usize = 2;

struct ScanRootResult {
    scanned: usize,
    seen_paths: Vec<String>,
    probe_queued: usize,
    probe_skipped: usize,
}

struct MediaProbeJob {
    item: ScannedMediaItem,
    media_path: PathBuf,
    probe_path: PathBuf,
}

struct MediaProbeJobResult {
    stream_probe_succeeded: bool,
}

struct MetadataFetchJob {
    item_id: String,
    item_type: String,
    path: PathBuf,
}

#[derive(Default)]
struct MediaProbeWorkerStats {
    completed: usize,
    stream_probe_succeeded: usize,
    failed: usize,
}

pub async fn scan_media_library(state: &AppState) -> anyhow::Result<usize> {
    Ok(scan_media_library_if_idle(state).await?.unwrap_or_default())
}

pub async fn scan_media_library_if_idle(state: &AppState) -> anyhow::Result<Option<usize>> {
    let roots = media_roots(state).await?;
    if roots.is_empty() {
        tracing::info!("media scan skipped because no media library paths are configured");
        return Ok(Some(0));
    }

    let _scan_guard = match state.scan_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            tracing::info!("media scan skipped because another scan is already running");
            return Ok(None);
        }
    };

    let force_probe = !media_probe_cache_is_current(&state.db).await?;
    if force_probe {
        tracing::info!("media probe cache version changed; forcing one-time media stream reprobe");
    }

    let probe_tx =
        start_media_probe_pipeline(state.db.clone(), force_probe.then(|| state.db.clone()));
    let mut tasks = tokio::task::JoinSet::new();
    let api_key = state.tmdb_api_key.read().await.clone().unwrap_or_default();
    let douban_cookie = state.douban_cookie.read().await.clone();
    let metadata_tx =
        start_metadata_fetch_pipeline(state.db.clone(), api_key.clone(), douban_cookie);
    for (root, library_id, collection_type) in roots {
        if !root.exists() {
            tracing::warn!("media directory does not exist: {}", root.display());
            continue;
        }
        let db = state.db.clone();
        let probe_tx = probe_tx.clone();
        let metadata_tx = metadata_tx.clone();
        tasks.spawn(async move {
            scan_root(
                db,
                root,
                library_id,
                collection_type,
                force_probe,
                probe_tx,
                metadata_tx,
            )
            .await
        });
    }

    let mut total = 0usize;
    let mut all_seen = Vec::new();
    let mut probe_queued = 0usize;
    let mut probe_skipped = 0usize;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Ok(result)) => {
                total += result.scanned;
                all_seen.extend(result.seen_paths);
                probe_queued += result.probe_queued;
                probe_skipped += result.probe_skipped;
            }
            Ok(Err(e)) => tracing::warn!("library scan failed: {e:#}"),
            Err(e) => tracing::warn!("scan task panicked: {e}"),
        }
    }
    drop(probe_tx);
    drop(metadata_tx);

    remove_missing_media_items(&state.db, &all_seen).await?;
    tracing::info!(
        "media scan indexed {total} item(s) across all libraries; streamed {probe_queued} media probe job(s)"
    );
    if probe_skipped > 0 {
        tracing::info!(
            "media scan reused cached probe data for {probe_skipped}/{total} file item(s)"
        );
    }

    Ok(Some(total))
}

async fn scan_root(
    db: sea_orm::DatabaseConnection,
    root: PathBuf,
    library_id: String,
    collection_type: String,
    force_probe: bool,
    probe_tx: mpsc::Sender<MediaProbeJob>,
    metadata_tx: mpsc::UnboundedSender<MetadataFetchJob>,
) -> anyhow::Result<ScanRootResult> {
    let mut scanned = 0usize;
    let mut probe_skipped = 0usize;
    let mut seen_paths = Vec::new();
    let mut probe_queued = 0usize;

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
            let mut item = ScannedMediaItem::folder_with_type(
                resolved.id,
                library_id.clone(),
                parent_id,
                path_string,
                folder_title,
                folder_type,
                resolved.modified_at,
                year,
            );
            if folder_type == "Season" {
                item.season_number =
                    crate::library::tmdb_metadata::parse_season_number(&resolved.name);
            }
            let stored_item_id = match upsert_media_item(&db, &item).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!("failed to upsert folder {}: {e:#}", item.path);
                    continue;
                }
            };
            item.id = stored_item_id;
            if folder_type == "Folder" {
                clear_scraped_folder_metadata(&db, &item.id).await;
            }
            upsert_sidecar_images(&db, path, &item.id).await?;
            queue_metadata_fetch(&metadata_tx, &item, path);
            continue;
        }

        // Handle STRM files: classify based on the resolved target's extension
        let is_strm_file = strm::is_strm_path(path);
        let (classify_path, probe_path) = if is_strm_file {
            match strm::resolve_strm_path(path) {
                Ok(resolved_target) => {
                    let ext_path = strm::classification_path_for_target(&resolved_target, path);
                    (ext_path, Some(resolved_target))
                }
                Err(e) => {
                    tracing::warn!("failed to resolve STRM {}: {e}", path.display());
                    continue;
                }
            }
        } else {
            (path.to_path_buf(), None)
        };

        let Some(item_type) = classify_media_path(&classify_path, &collection_type) else {
            continue;
        };
        let item_type = normalize_scanned_file_type(
            &item_type,
            &collection_type,
            &parent_id,
            &library_id,
            is_strm_file,
        );
        let container = if is_strm_file {
            probe_path
                .as_ref()
                .and_then(|target| strm::target_extension(&target.to_string_lossy()))
        } else {
            classify_path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
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
        let media_path = probe_path.as_deref().unwrap_or(path);
        let cached_probe = if force_probe {
            None
        } else {
            match cached_media_probe_if_current(
                &db,
                &path_string,
                resolved.modified_at,
                resolved.size_bytes,
                is_strm_file,
            )
            .await
            {
                Ok(cache) => cache,
                Err(error) => {
                    tracing::debug!(
                        "media probe cache check failed for {}: {error:#}",
                        path.display()
                    );
                    None
                }
            }
        };
        if cached_probe.is_some() {
            probe_skipped += 1;
        }
        let mut item = ScannedMediaItem {
            id: resolved.id,
            title,
            path: path_string,
            library_id: library_id.clone(),
            parent_id,
            item_type,
            is_folder: false,
            container,
            overview: parsed_metadata.overview.clone(),
            official_rating: parsed_metadata.official_rating.clone(),
            extended_video_type: (!parsed_name.extended_video_types.is_empty())
                .then(|| parsed_name.extended_video_types.join(",")),
            production_year: parsed_metadata.production_year,
            runtime_ticks: cached_probe.as_ref().and_then(|probe| probe.runtime_ticks),
            size_bytes: cached_probe
                .as_ref()
                .and_then(|probe| probe.size_bytes)
                .or(resolved.size_bytes),
            season_number: parsed_name.season_number,
            episode_number: parsed_name.episode_number,
            modified_at: resolved.modified_at,
            created_at: now_unix(),
        };

        let stored_item_id = match upsert_media_item(&db, &item).await {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("failed to upsert media item {}: {e:#}", item.path);
                continue;
            }
        };
        item.id = stored_item_id;
        upsert_media_metadata(&db, &item.id, &parsed_metadata).await?;
        upsert_sidecar_images(&db, path, &item.id).await?;
        queue_metadata_fetch(&metadata_tx, &item, path);
        if cached_probe.is_some() {
            upsert_sidecar_subtitles(&db, path, &item.id).await?;
        } else {
            upsert_default_media_stream(&db, &item).await?;
            upsert_sidecar_subtitles(&db, path, &item.id).await?;
            let job = MediaProbeJob {
                item,
                media_path: path.to_path_buf(),
                probe_path: media_path.to_path_buf(),
            };
            if let Err(error) = probe_tx.send(job).await {
                tracing::warn!(
                    "media probe queue closed before job could be scheduled for {}",
                    error.0.item.path
                );
            } else {
                probe_queued += 1;
            }
        }

        scanned += 1;
    }

    Ok(ScanRootResult {
        scanned,
        seen_paths,
        probe_queued,
        probe_skipped,
    })
}

fn start_media_probe_pipeline(
    db: sea_orm::DatabaseConnection,
    cache_version_db: Option<sea_orm::DatabaseConnection>,
) -> mpsc::Sender<MediaProbeJob> {
    let concurrency = media_probe_concurrency();
    let queue_capacity = media_probe_queue_capacity();
    let (tx, rx) = mpsc::channel::<MediaProbeJob>(queue_capacity);
    tokio::spawn(async move {
        let receiver = Arc::new(Mutex::new(rx));
        let mut workers = tokio::task::JoinSet::new();
        tracing::info!(
            "media probe pipeline started concurrency={concurrency} queue_capacity={queue_capacity}"
        );

        for _ in 0..concurrency {
            let db = db.clone();
            let receiver = receiver.clone();
            workers.spawn(async move {
                let mut stats = MediaProbeWorkerStats::default();
                loop {
                    let job = {
                        let mut receiver = receiver.lock().await;
                        receiver.recv().await
                    };
                    let Some(job) = job else {
                        break;
                    };
                    stats.completed += 1;
                    match run_media_probe_job(db.clone(), job).await {
                        Ok(result) => {
                            if result.stream_probe_succeeded {
                                stats.stream_probe_succeeded += 1;
                            }
                        }
                        Err(error) => {
                            stats.failed += 1;
                            tracing::warn!("media probe job failed: {error:#}");
                        }
                    }
                }
                stats
            });
        }

        let mut stats = MediaProbeWorkerStats::default();
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(worker_stats) => {
                    stats.completed += worker_stats.completed;
                    stats.stream_probe_succeeded += worker_stats.stream_probe_succeeded;
                    stats.failed += worker_stats.failed;
                }
                Err(error) => {
                    stats.failed += 1;
                    tracing::warn!("media probe worker task panicked: {error}");
                }
            }
        }

        tracing::info!(
            "media probe pipeline completed {} item(s); stream_probe_succeeded={} failed={}",
            stats.completed,
            stats.stream_probe_succeeded,
            stats.failed
        );
        if let Some(db) = cache_version_db {
            if let Err(error) = set_media_probe_cache_current(&db).await {
                tracing::warn!("failed to mark media probe cache current: {error:#}");
            }
        }
    });
    tx
}

fn start_metadata_fetch_pipeline(
    db: sea_orm::DatabaseConnection,
    api_key: String,
    douban_cookie: Option<String>,
) -> mpsc::UnboundedSender<MetadataFetchJob> {
    let (tx, mut rx) = mpsc::unbounded_channel::<MetadataFetchJob>();
    tokio::spawn(async move {
        let concurrency = metadata_fetch_concurrency();
        let mut pending = tokio::task::JoinSet::new();
        let mut queued = 0usize;
        let mut completed = 0usize;
        let mut failed = 0usize;
        tracing::info!("metadata fetch pipeline started concurrency={concurrency}");

        while let Some(job) = rx.recv().await {
            queued += 1;
            while pending.len() >= concurrency {
                if metadata_job_finished(pending.join_next().await) {
                    completed += 1;
                } else {
                    failed += 1;
                }
            }
            let db = db.clone();
            let api_key = api_key.clone();
            pending.spawn(async move { run_metadata_fetch_job(db, job, &api_key).await });
        }

        while let Some(result) = pending.join_next().await {
            if metadata_job_finished(Some(result)) {
                completed += 1;
            } else {
                failed += 1;
            }
        }

        run_post_scan_metadata_tasks(&db, &api_key, douban_cookie.as_deref()).await;
        tracing::info!(
            "metadata fetch pipeline completed {completed}/{queued} item(s); failed={failed}"
        );
    });
    tx
}

fn metadata_job_finished(
    result: Option<Result<anyhow::Result<()>, tokio::task::JoinError>>,
) -> bool {
    match result {
        Some(Ok(Ok(()))) => true,
        Some(Ok(Err(error))) => {
            tracing::warn!("metadata fetch job failed: {error:#}");
            false
        }
        Some(Err(error)) => {
            tracing::warn!("metadata fetch worker task panicked: {error}");
            false
        }
        None => false,
    }
}

async fn run_metadata_fetch_job(
    db: sea_orm::DatabaseConnection,
    job: MetadataFetchJob,
    api_key: &str,
) -> anyhow::Result<()> {
    if api_key.is_empty() {
        return Ok(());
    }
    if job.item_type != "Movie" && job.item_type != "Series" {
        return Ok(());
    }
    if tmdb_metadata_is_current(&db, &job.item_id).await {
        return Ok(());
    }
    crate::library::tmdb_metadata::fetch_and_apply_tmdb_metadata(
        &db,
        &job.item_id,
        &job.item_type,
        &job.path,
        api_key,
    )
    .await
}

async fn run_post_scan_metadata_tasks(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    douban_cookie: Option<&str>,
) {
    if !api_key.is_empty() {
        if let Err(error) =
            crate::library::tmdb_metadata::batch_fetch_episode_tmdb(db, api_key).await
        {
            tracing::warn!("episode TMDb fetch failed: {error:#}");
        }
    }
    if let Err(error) =
        crate::library::douban_metadata::fill_missing_douban(db, douban_cookie).await
    {
        tracing::warn!("Douban metadata fetch failed: {error:#}");
    }
}

async fn run_media_probe_job(
    db: sea_orm::DatabaseConnection,
    mut job: MediaProbeJob,
) -> anyhow::Result<MediaProbeJobResult> {
    let probe_path = job.probe_path.clone();
    let probe = tokio::task::spawn_blocking(move || probe_media(&probe_path))
        .await
        .context("media probe task join failed")?;
    let mut stream_probe_succeeded = false;

    if let Some(probe) = probe {
        job.item.runtime_ticks = probe.runtime_ticks;
        job.item.size_bytes = probe.size_bytes.or(job.item.size_bytes);
        upsert_media_item(&db, &job.item).await?;
        clear_sidecar_subtitles(&db, &job.item.id).await?;
        stream_probe_succeeded = match upsert_probed_media_streams(&db, &job.item, &probe).await {
            Ok(succeeded) => succeeded,
            Err(error) => {
                let _ = upsert_sidecar_subtitles(&db, &job.media_path, &job.item.id).await;
                return Err(error);
            }
        };
        if !stream_probe_succeeded {
            upsert_default_media_stream(&db, &job.item).await?;
        }
    } else {
        match crate::mediainfo::deserialize_mediainfo(&db, &job.item.id, &job.probe_path, None)
            .await
        {
            Ok(true) => {
                tracing::debug!("restored mediainfo from sidecar for {}", job.item.path);
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    "failed to restore mediainfo sidecar for {}: {error:#}",
                    job.item.path
                );
            }
        }
    }

    upsert_sidecar_subtitles(&db, &job.media_path, &job.item.id).await?;
    Ok(MediaProbeJobResult {
        stream_probe_succeeded,
    })
}

fn normalize_scanned_file_type(
    item_type: &str,
    collection_type: &str,
    parent_id: &str,
    library_id: &str,
    is_strm_file: bool,
) -> String {
    if is_strm_file
        && collection_type == "movies"
        && item_type == "Video"
        && parent_id == library_id
    {
        "Movie".to_string()
    } else {
        item_type.to_string()
    }
}

fn queue_metadata_fetch(
    metadata_tx: &mpsc::UnboundedSender<MetadataFetchJob>,
    item: &ScannedMediaItem,
    path: &std::path::Path,
) {
    if item.item_type != "Movie" && item.item_type != "Series" {
        return;
    }
    if let Err(error) = metadata_tx.send(MetadataFetchJob {
        item_id: item.id.clone(),
        item_type: item.item_type.clone(),
        path: path.to_path_buf(),
    }) {
        tracing::warn!(
            "metadata fetch queue closed before job could be scheduled for {}",
            error.0.path.display()
        );
    }
}

async fn clear_scraped_folder_metadata(db: &sea_orm::DatabaseConnection, item_id: &str) {
    for table in [
        "provider_ids",
        "media_genres",
        "media_studios",
        "media_people",
    ] {
        if let Err(error) = db
            .execute(crate::db::helpers::pg_statement(
                &format!("DELETE FROM {table} WHERE item_id = ?"),
                vec![item_id.into()],
            ))
            .await
        {
            tracing::debug!("failed to clear {table} for folder {item_id}: {error:#}");
        }
    }
    if let Err(error) = db
        .execute(crate::db::helpers::pg_statement(
            r#"UPDATE media_items
               SET overview = NULL,
                   official_rating = NULL,
                   production_year = NULL,
                   premiere_date = NULL,
                   community_rating = NULL,
                   critic_rating = NULL,
                   runtime_ticks = NULL
               WHERE id = ? AND item_type = 'Folder'"#,
            vec![item_id.into()],
        ))
        .await
    {
        tracing::debug!("failed to clear scraped scalar metadata for folder {item_id}: {error:#}");
    }
}

async fn tmdb_metadata_is_current(db: &sea_orm::DatabaseConnection, item_id: &str) -> bool {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT CASE WHEN EXISTS (
                    SELECT 1 FROM provider_ids p
                    WHERE p.item_id = mi.id AND p.provider = 'Tmdb'
                )
                AND mi.overview IS NOT NULL
                AND mi.production_year IS NOT NULL
                AND mi.premiere_date IS NOT NULL
                AND EXISTS (
                    SELECT 1 FROM image_assets ia
                    WHERE ia.item_id = mi.id AND ia.image_type = 'Primary'
                )
                THEN 1::BIGINT ELSE 0::BIGINT END AS is_current
               FROM media_items mi
               WHERE mi.id = ?"#,
            vec![item_id.into()],
        ))
        .await;

    match row {
        Ok(Some(row)) => row.get_bool_from_i64("is_current").unwrap_or(false),
        Ok(None) => false,
        Err(error) => {
            tracing::debug!("failed to check TMDb metadata state for {item_id}: {error:#}");
            false
        }
    }
}

fn has_sidecar_nfo(path: &std::path::Path) -> bool {
    path.with_extension("nfo").exists()
        || path
            .parent()
            .map(|parent| parent.join("movie.nfo").exists())
            .unwrap_or_default()
}

async fn media_roots(state: &AppState) -> anyhow::Result<Vec<(PathBuf, String, String)>> {
    let rows = state
        .db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT lp.path, lp.library_id, l.collection_type FROM library_paths lp JOIN libraries l ON l.id = lp.library_id ORDER BY lp.path ASC",
            vec![],
        ))
        .await
        .context("failed to list library paths for scan")?;
    if rows.is_empty() {
        return Ok(Vec::new());
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

fn media_probe_concurrency() -> usize {
    std::env::var("JELLYFIN_RS_MEDIA_PROBE_CONCURRENCY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MEDIA_PROBE_CONCURRENCY)
}

fn metadata_fetch_concurrency() -> usize {
    std::env::var("JELLYFIN_RS_METADATA_FETCH_CONCURRENCY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_METADATA_FETCH_CONCURRENCY)
}

fn media_probe_queue_capacity() -> usize {
    std::env::var("JELLYFIN_RS_MEDIA_PROBE_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MEDIA_PROBE_QUEUE_CAPACITY)
}

async fn media_probe_cache_is_current(db: &sea_orm::DatabaseConnection) -> anyhow::Result<bool> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT value FROM app_settings WHERE key = ?",
            vec![MEDIA_PROBE_CACHE_VERSION_KEY.into()],
        ))
        .await
        .context("failed to read media probe cache version")?;
    Ok(row
        .as_ref()
        .map(|row| row.get_str("value"))
        .transpose()?
        .is_some_and(|value| value == MEDIA_PROBE_CACHE_VERSION))
}

async fn set_media_probe_cache_current(db: &sea_orm::DatabaseConnection) -> anyhow::Result<()> {
    db.execute(crate::db::helpers::pg_statement(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![
            MEDIA_PROBE_CACHE_VERSION_KEY.into(),
            MEDIA_PROBE_CACHE_VERSION.into(),
            now_unix().into(),
        ],
    ))
    .await
    .context("failed to update media probe cache version")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_probe_cache_version_is_persisted() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        assert!(!media_probe_cache_is_current(&db).await.unwrap());
        set_media_probe_cache_current(&db).await.unwrap();
        assert!(media_probe_cache_is_current(&db).await.unwrap());
    }

    #[test]
    fn media_probe_concurrency_defaults_to_positive_value() {
        assert!(media_probe_concurrency() > 0);
    }

    #[test]
    fn media_probe_queue_capacity_defaults_to_positive_value() {
        assert!(media_probe_queue_capacity() > 0);
    }
}
