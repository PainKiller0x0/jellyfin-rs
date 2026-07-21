use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use tokio::sync::{Mutex, RwLock, mpsc};
use walkdir::WalkDir;

use crate::{
    app::state::AppState,
    entities::{
        image_assets::{self, Entity as ImageAssets},
        libraries::Entity as Libraries,
        library_paths::{self, Entity as LibraryPaths},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_studios::{self, Entity as MediaStudios},
        provider_ids::{self, Entity as ProviderIds},
    },
    library::{
        classify::{
            classify_media_path, parent_id_for_path, parent_id_for_scanned_file, tv_folder_type,
        },
        images::upsert_sidecar_images,
        metadata::{ParsedMetadata, parse_sidecar_metadata, provider_ids_from_path},
        naming::parse_media_name,
        path_utils,
        probe::probe_media,
        storage::{
            CachedMediaProbe, ScannedMediaItem, cached_media_probe_if_current,
            remove_missing_media_items, upsert_default_media_stream, upsert_failed_media_probe,
            upsert_media_item, upsert_media_metadata, upsert_probed_media_streams,
        },
        subtitles::{clear_sidecar_subtitles, upsert_sidecar_subtitles},
    },
    strm,
    util::now_unix,
};

const MEDIA_PROBE_CACHE_VERSION_KEY: &str = "media_probe_cache_version";
const MEDIA_PROBE_CACHE_VERSION: &str = "3";
const MIN_INGEST_QUEUE_CAPACITY: usize = 512;
const MAX_INGEST_QUEUE_CAPACITY: usize = 4096;
const MIN_MEDIA_PROBE_QUEUE_CAPACITY: usize = 32;
const MAX_MEDIA_PROBE_QUEUE_CAPACITY: usize = 256;
const METADATA_FETCH_MAX_ATTEMPTS: usize = 30;
const METADATA_FETCH_RETRY_DELAY_SECS: u64 = 2;

struct ScanRootResult {
    scanned: usize,
    seen_paths: Vec<String>,
    ingest_queued: usize,
}

struct IngestPipeline {
    tx: mpsc::Sender<IngestJob>,
    handle: tokio::task::JoinHandle<IngestWorkerStats>,
}

struct IngestJob {
    item: ScannedMediaItem,
    source_path: PathBuf,
    parsed_metadata: ParsedMetadata,
    clear_folder_metadata: bool,
    media_probe: Option<PendingMediaProbe>,
}

struct PendingMediaProbe {
    media_path: PathBuf,
    probe_path: PathBuf,
    is_strm_file: bool,
}

struct IngestJobResult {
    metadata_queued: bool,
    probe_queued: bool,
    probe_skipped: bool,
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
    attempts: usize,
}

#[derive(Default)]
struct IngestWorkerStats {
    completed: usize,
    metadata_queued: usize,
    probe_queued: usize,
    probe_skipped: usize,
    failed: usize,
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

    let scan_database_url = crate::db::database_url_from_env();
    let scan_db = crate::db::background_connection(&scan_database_url).await?;

    let force_probe = !media_probe_cache_is_current(&scan_db).await?;
    if force_probe {
        tracing::info!("media probe cache version changed; forcing one-time media stream reprobe");
    }

    let probe_tx =
        start_media_probe_pipeline(scan_db.clone(), force_probe.then(|| scan_db.clone()));
    let mut tasks = tokio::task::JoinSet::new();
    let root_concurrency = scan_root_concurrency();
    let api_key = state.tmdb_api_key.read().await.clone().unwrap_or_default();
    let douban_cookie = state.douban_cookie.read().await.clone();
    let metadata_tx = start_metadata_fetch_pipeline(
        scan_db.clone(),
        api_key.clone(),
        state.tmdb_proxy_url.clone(),
        state.tmdb_http_client.clone(),
        douban_cookie,
    );
    let ingest_pipeline = start_ingest_pipeline(
        scan_db.clone(),
        force_probe,
        probe_tx.clone(),
        metadata_tx.clone(),
    );
    let mut scanned_library_ids = Vec::new();
    let mut total = 0usize;
    let mut all_seen = Vec::new();
    let mut ingest_queued = 0usize;
    let mut pending_roots = roots.into_iter();
    loop {
        while tasks.len() < root_concurrency {
            let Some((root, library_id, collection_type)) = pending_roots.next() else {
                break;
            };
            if !root.exists() {
                tracing::warn!("media directory does not exist: {}", root.display());
                continue;
            }
            scanned_library_ids.push(library_id.clone());
            let ingest_tx = ingest_pipeline.tx.clone();
            tasks.spawn(
                async move { scan_root(root, library_id, collection_type, ingest_tx).await },
            );
        }

        let Some(result) = tasks.join_next().await else {
            break;
        };
        match result {
            Ok(Ok(result)) => {
                total += result.scanned;
                all_seen.extend(result.seen_paths);
                ingest_queued += result.ingest_queued;
            }
            Ok(Err(e)) => tracing::warn!("library scan failed: {e:#}"),
            Err(e) => tracing::warn!("scan task panicked: {e}"),
        }
    }

    drop(ingest_pipeline.tx);
    let ingest_stats = match ingest_pipeline.handle.await {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!("ingest pipeline task panicked: {error}");
            IngestWorkerStats::default()
        }
    };
    drop(probe_tx);
    drop(metadata_tx);

    scanned_library_ids.sort();
    scanned_library_ids.dedup();
    remove_missing_media_items(&scan_db, &scanned_library_ids, &all_seen).await?;
    tracing::info!(
        "media scan discovered {total} file item(s) across all libraries; queued {ingest_queued} ingest job(s)"
    );
    tracing::info!(
        "media ingest completed {} job(s); metadata_queued={} probe_queued={} probe_skipped={} failed={}",
        ingest_stats.completed,
        ingest_stats.metadata_queued,
        ingest_stats.probe_queued,
        ingest_stats.probe_skipped,
        ingest_stats.failed
    );
    if ingest_stats.probe_skipped > 0 {
        tracing::info!(
            "media ingest reused cached probe data for {}/{} file item(s)",
            ingest_stats.probe_skipped,
            total
        );
    }

    Ok(Some(total))
}

async fn scan_root(
    root: PathBuf,
    library_id: String,
    collection_type: String,
    ingest_tx: mpsc::Sender<IngestJob>,
) -> anyhow::Result<ScanRootResult> {
    let mut scanned = 0usize;
    let mut seen_paths = Vec::new();
    let mut ingest_queued = 0usize;

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
        let mut parent_id = parent_id_for_path(path, &root, &library_id);

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
            let provider_ids = provider_ids_from_path(path);
            let job = IngestJob {
                item,
                source_path: path.to_path_buf(),
                parsed_metadata: ParsedMetadata {
                    provider_ids,
                    ..Default::default()
                },
                clear_folder_metadata: folder_type == "Folder",
                media_probe: None,
            };
            if queue_ingest(&ingest_tx, job).await {
                ingest_queued += 1;
            }
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
            path,
            &root,
        );
        parent_id =
            parent_id_for_scanned_file(path, &root, &library_id, &collection_type, &item_type);
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
        let season_number = parsed_name
            .season_number
            .or_else(|| episode_season_number_from_path(path, &root, &collection_type));
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
        let probe_target_path = probe_path.as_deref().unwrap_or(path);
        let item = ScannedMediaItem {
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
            runtime_ticks: None,
            size_bytes: resolved.size_bytes,
            season_number,
            episode_number: parsed_name.episode_number,
            modified_at: resolved.modified_at,
            created_at: now_unix(),
        };

        let job = IngestJob {
            item,
            source_path: path.to_path_buf(),
            parsed_metadata,
            clear_folder_metadata: false,
            media_probe: Some(PendingMediaProbe {
                media_path: path.to_path_buf(),
                probe_path: probe_target_path.to_path_buf(),
                is_strm_file,
            }),
        };
        if queue_ingest(&ingest_tx, job).await {
            ingest_queued += 1;
            scanned += 1;
        }
    }

    Ok(ScanRootResult {
        scanned,
        seen_paths,
        ingest_queued,
    })
}

async fn queue_ingest(ingest_tx: &mpsc::Sender<IngestJob>, job: IngestJob) -> bool {
    match ingest_tx.send(job).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                "ingest queue closed before job could be scheduled for {}",
                error.0.item.path
            );
            false
        }
    }
}

fn start_ingest_pipeline(
    db: sea_orm::DatabaseConnection,
    force_probe: bool,
    probe_tx: mpsc::Sender<MediaProbeJob>,
    metadata_tx: mpsc::UnboundedSender<MetadataFetchJob>,
) -> IngestPipeline {
    let concurrency = ingest_concurrency();
    let queue_capacity = ingest_queue_capacity();
    let (tx, rx) = mpsc::channel::<IngestJob>(queue_capacity);
    let handle = tokio::spawn(async move {
        let receiver = Arc::new(Mutex::new(rx));
        let mut workers = tokio::task::JoinSet::new();
        tracing::info!(
            "ingest pipeline started concurrency={concurrency} queue_capacity={queue_capacity}"
        );

        for _ in 0..concurrency {
            let db = db.clone();
            let receiver = receiver.clone();
            let probe_tx = probe_tx.clone();
            let metadata_tx = metadata_tx.clone();
            workers.spawn(async move {
                let mut stats = IngestWorkerStats::default();
                loop {
                    let job = {
                        let mut receiver = receiver.lock().await;
                        receiver.recv().await
                    };
                    let Some(job) = job else {
                        break;
                    };
                    stats.completed += 1;
                    match run_ingest_job(db.clone(), job, force_probe, &probe_tx, &metadata_tx)
                        .await
                    {
                        Ok(result) => {
                            if result.metadata_queued {
                                stats.metadata_queued += 1;
                            }
                            if result.probe_queued {
                                stats.probe_queued += 1;
                            }
                            if result.probe_skipped {
                                stats.probe_skipped += 1;
                            }
                        }
                        Err(error) => {
                            stats.failed += 1;
                            tracing::warn!("ingest job failed: {error:#}");
                        }
                    }
                }
                stats
            });
        }

        let mut stats = IngestWorkerStats::default();
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(worker_stats) => {
                    stats.completed += worker_stats.completed;
                    stats.metadata_queued += worker_stats.metadata_queued;
                    stats.probe_queued += worker_stats.probe_queued;
                    stats.probe_skipped += worker_stats.probe_skipped;
                    stats.failed += worker_stats.failed;
                }
                Err(error) => {
                    stats.failed += 1;
                    tracing::warn!("ingest worker task panicked: {error}");
                }
            }
        }

        tracing::info!(
            "ingest pipeline completed {} job(s); metadata_queued={} probe_queued={} probe_skipped={} failed={}",
            stats.completed,
            stats.metadata_queued,
            stats.probe_queued,
            stats.probe_skipped,
            stats.failed
        );
        stats
    });
    IngestPipeline { tx, handle }
}

async fn run_ingest_job(
    db: sea_orm::DatabaseConnection,
    mut job: IngestJob,
    force_probe: bool,
    probe_tx: &mpsc::Sender<MediaProbeJob>,
    metadata_tx: &mpsc::UnboundedSender<MetadataFetchJob>,
) -> anyhow::Result<IngestJobResult> {
    let cached_probe = match job.media_probe.as_ref() {
        Some(pending) => cached_media_probe_for_job(&db, &job.item, pending, force_probe).await,
        None => None,
    };
    if let Some(cached_probe) = cached_probe.as_ref() {
        apply_cached_probe(&mut job.item, cached_probe);
    }

    let stored_item_id = upsert_media_item(&db, &job.item).await?;
    job.item.id = stored_item_id;
    upsert_media_metadata(&db, &job.item.id, &job.parsed_metadata).await?;
    if job.clear_folder_metadata {
        clear_scraped_folder_metadata(&db, &job.item.id).await;
    }
    upsert_sidecar_images(&db, &job.source_path, &job.item.id).await?;

    let mut result = IngestJobResult {
        metadata_queued: queue_metadata_fetch(metadata_tx, &job.item, &job.source_path),
        probe_queued: false,
        probe_skipped: false,
    };

    let Some(pending_probe) = job.media_probe else {
        return Ok(result);
    };

    if cached_probe.is_some() {
        result.probe_skipped = true;
        upsert_sidecar_subtitles(&db, &job.source_path, &job.item.id).await?;
        return Ok(result);
    }

    upsert_default_media_stream(&db, &job.item).await?;
    upsert_sidecar_subtitles(&db, &job.source_path, &job.item.id).await?;
    let probe_job = MediaProbeJob {
        item: job.item,
        media_path: pending_probe.media_path,
        probe_path: pending_probe.probe_path,
    };
    match probe_tx.try_send(probe_job) {
        Ok(()) => {
            result.probe_queued = true;
        }
        Err(mpsc::error::TrySendError::Full(job)) => {
            result.probe_skipped = true;
            tracing::debug!(
                "media probe queue is full; skipping probe for {}",
                job.item.path
            );
        }
        Err(mpsc::error::TrySendError::Closed(job)) => {
            tracing::warn!(
                "media probe queue closed before job could be scheduled for {}",
                job.item.path
            );
        }
    }

    Ok(result)
}

async fn cached_media_probe_for_job(
    db: &sea_orm::DatabaseConnection,
    item: &ScannedMediaItem,
    pending_probe: &PendingMediaProbe,
    force_probe: bool,
) -> Option<CachedMediaProbe> {
    if force_probe {
        return None;
    }
    match cached_media_probe_if_current(
        db,
        &item.path,
        item.modified_at,
        item.size_bytes,
        pending_probe.is_strm_file,
    )
    .await
    {
        Ok(cache) => cache,
        Err(error) => {
            tracing::debug!(
                "media probe cache check failed for {}: {error:#}",
                item.path
            );
            None
        }
    }
}

fn apply_cached_probe(item: &mut ScannedMediaItem, cached_probe: &CachedMediaProbe) {
    item.runtime_ticks = cached_probe.runtime_ticks;
    item.size_bytes = cached_probe.size_bytes.or(item.size_bytes);
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
    tmdb_proxy_url: Arc<RwLock<Option<String>>>,
    tmdb_http_client: Arc<RwLock<reqwest::Client>>,
    douban_cookie: Option<String>,
) -> mpsc::UnboundedSender<MetadataFetchJob> {
    let (tx, mut rx) = mpsc::unbounded_channel::<MetadataFetchJob>();
    let retry_tx = tx.downgrade();
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
            let tmdb_proxy_url = tmdb_proxy_url.clone();
            let tmdb_http_client = tmdb_http_client.clone();
            let retry_tx = retry_tx.clone();
            pending.spawn(async move {
                let tmdb_base_url = tmdb_proxy_url.read().await.clone();
                let tmdb_client = tmdb_http_client.read().await.clone();
                run_metadata_fetch_job(
                    db,
                    job,
                    &api_key,
                    &tmdb_client,
                    tmdb_base_url.as_deref(),
                    retry_tx,
                )
                .await
            });
        }

        while let Some(result) = pending.join_next().await {
            if metadata_job_finished(Some(result)) {
                completed += 1;
            } else {
                failed += 1;
            }
        }

        run_post_scan_metadata_tasks(
            &db,
            &api_key,
            tmdb_proxy_url,
            tmdb_http_client,
            douban_cookie.as_deref(),
        )
        .await;
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
            tracing::warn!(
                "metadata fetch job failed: {}",
                crate::library::tmdb_metadata::redact_tmdb_error(&error)
            );
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
    tmdb_client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    retry_tx: mpsc::WeakUnboundedSender<MetadataFetchJob>,
) -> anyhow::Result<()> {
    if api_key.is_empty() {
        return Ok(());
    }

    let metadata_ready = match job.item_type.as_str() {
        "Movie" | "Series" => {
            if tmdb_metadata_is_current(&db, &job.item_id).await {
                return Ok(());
            }
            crate::library::tmdb_metadata::fetch_and_apply_tmdb_metadata(
                &db,
                &job.item_id,
                &job.item_type,
                &job.path,
                api_key,
                tmdb_client,
                tmdb_base_url,
            )
            .await?;
            true
        }
        "Season" => {
            crate::library::tmdb_metadata::fetch_and_apply_season_tmdb_metadata(
                &db,
                &job.item_id,
                api_key,
                tmdb_client,
                tmdb_base_url,
            )
            .await?
        }
        "Episode" => {
            crate::library::tmdb_metadata::fetch_and_apply_episode_tmdb_metadata(
                &db,
                &job.item_id,
                api_key,
                tmdb_client,
                tmdb_base_url,
            )
            .await?
        }
        _ => true,
    };

    if !metadata_ready {
        schedule_metadata_fetch_retry(job, retry_tx);
    }
    Ok(())
}

fn schedule_metadata_fetch_retry(
    mut job: MetadataFetchJob,
    retry_tx: mpsc::WeakUnboundedSender<MetadataFetchJob>,
) {
    if job.attempts >= METADATA_FETCH_MAX_ATTEMPTS {
        tracing::debug!(
            "TMDb metadata dependency was not ready after {} attempt(s) for {} {} ({}); post-scan TMDb batch will retry it",
            METADATA_FETCH_MAX_ATTEMPTS,
            job.item_type,
            job.item_id,
            job.path.display()
        );
        return;
    }

    job.attempts += 1;
    let retry_number = job.attempts;
    let path = job.path.clone();
    let item_type = job.item_type.clone();
    let Some(retry_tx) = retry_tx.upgrade() else {
        tracing::debug!(
            "metadata fetch retry skipped because pipeline closed for {} {} ({})",
            item_type,
            job.item_id,
            path.display()
        );
        return;
    };

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(METADATA_FETCH_RETRY_DELAY_SECS)).await;
        match retry_tx.send(job) {
            Ok(()) => {
                tracing::debug!(
                    "metadata fetch retry queued for {item_type} after dependency wait attempt {retry_number}"
                );
            }
            Err(error) => {
                tracing::debug!(
                    "metadata fetch queue closed before retry could be scheduled for {}",
                    error.0.path.display()
                );
            }
        }
    });
}

async fn run_post_scan_metadata_tasks(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    tmdb_proxy_url: Arc<RwLock<Option<String>>>,
    tmdb_http_client: Arc<RwLock<reqwest::Client>>,
    douban_cookie: Option<&str>,
) {
    if !api_key.is_empty() {
        let tmdb_base_url = tmdb_proxy_url.read().await.clone();
        let tmdb_client = tmdb_http_client.read().await.clone();
        match crate::library::tmdb_metadata::batch_fetch_episode_tmdb(
            db,
            api_key,
            &tmdb_client,
            tmdb_base_url.as_deref(),
        )
        .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!("post-scan episode TMDb batch fetched {n} title(s)"),
            Err(error) => tracing::warn!(
                "post-scan episode TMDb batch failed: {}",
                crate::library::tmdb_metadata::redact_tmdb_error(&error)
            ),
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
        let restored_mediainfo =
            match crate::mediainfo::deserialize_mediainfo(&db, &job.item.id, &job.probe_path, None)
                .await
            {
                Ok(true) => {
                    tracing::debug!("restored mediainfo from sidecar for {}", job.item.path);
                    true
                }
                Ok(false) => false,
                Err(error) => {
                    tracing::debug!(
                        "failed to restore mediainfo sidecar for {}: {error:#}",
                        job.item.path
                    );
                    false
                }
            };
        if !restored_mediainfo
            && crate::strm::is_remote_url(&job.probe_path.to_string_lossy())
            && let Err(error) = upsert_failed_media_probe(&db, &job.item).await
        {
            tracing::debug!(
                "failed to cache remote media probe failure for {}: {error:#}",
                job.item.path
            );
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
    path: &std::path::Path,
    root: &std::path::Path,
) -> String {
    if collection_type == "movies" && item_type == "Video" {
        if parent_id == library_id {
            return "Movie".to_string();
        }
        let parent_is_movie_folder = path
            .parent()
            .is_some_and(|parent| tv_folder_type(parent, root, collection_type) == "Movie");
        if parent_is_movie_folder {
            "Video".to_string()
        } else {
            "Movie".to_string()
        }
    } else {
        item_type.to_string()
    }
}

fn episode_season_number_from_path(
    path: &std::path::Path,
    root: &std::path::Path,
    collection_type: &str,
) -> Option<i64> {
    if !matches!(collection_type, "tvshows" | "tv") {
        return None;
    }

    let mut current = path.parent();
    while let Some(parent) = current {
        if same_normalized_path(parent, root) {
            break;
        }
        if let Some(number) = parent
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::library::tmdb_metadata::parse_season_number)
        {
            return Some(number);
        }
        current = parent.parent();
    }
    None
}

fn same_normalized_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    path_utils::normalize_path(&left.to_string_lossy())
        == path_utils::normalize_path(&right.to_string_lossy())
}

fn queue_metadata_fetch(
    metadata_tx: &mpsc::UnboundedSender<MetadataFetchJob>,
    item: &ScannedMediaItem,
    path: &std::path::Path,
) -> bool {
    match item.item_type.as_str() {
        "Movie" | "Series" | "Season" | "Episode" => {}
        _ => return false,
    }
    match metadata_tx.send(MetadataFetchJob {
        item_id: item.id.clone(),
        item_type: item.item_type.clone(),
        path: path.to_path_buf(),
        attempts: 0,
    }) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                "metadata fetch queue closed before job could be scheduled for {}",
                error.0.path.display()
            );
            false
        }
    }
}

async fn clear_scraped_folder_metadata(db: &sea_orm::DatabaseConnection, item_id: &str) {
    if let Err(error) = ProviderIds::delete_many()
        .filter(provider_ids::Column::ItemId.eq(item_id))
        .exec(db)
        .await
    {
        tracing::debug!("failed to clear provider_ids for folder {item_id}: {error:#}");
    }
    if let Err(error) = MediaGenres::delete_many()
        .filter(media_genres::Column::ItemId.eq(item_id))
        .exec(db)
        .await
    {
        tracing::debug!("failed to clear media_genres for folder {item_id}: {error:#}");
    }
    if let Err(error) = MediaStudios::delete_many()
        .filter(media_studios::Column::ItemId.eq(item_id))
        .exec(db)
        .await
    {
        tracing::debug!("failed to clear media_studios for folder {item_id}: {error:#}");
    }
    if let Err(error) = MediaPeople::delete_many()
        .filter(media_people::Column::ItemId.eq(item_id))
        .exec(db)
        .await
    {
        tracing::debug!("failed to clear media_people for folder {item_id}: {error:#}");
    }

    match MediaItems::find_by_id(item_id.to_string()).one(db).await {
        Ok(Some(item)) if item.item_type == "Folder" => {
            let mut active: media_items::ActiveModel = item.into();
            active.overview = Set(None);
            active.official_rating = Set(None);
            active.production_year = Set(None);
            active.premiere_date = Set(None);
            active.community_rating = Set(None);
            active.critic_rating = Set(None);
            active.runtime_ticks = Set(None);
            if let Err(error) = active.update(db).await {
                tracing::debug!(
                    "failed to clear scraped scalar metadata for folder {item_id}: {error:#}"
                );
            }
        }
        Ok(_) => {}
        Err(error) => {
            tracing::debug!("failed to load folder for metadata clearing {item_id}: {error:#}");
        }
    }
}

async fn tmdb_metadata_is_current(db: &sea_orm::DatabaseConnection, item_id: &str) -> bool {
    let item = match MediaItems::find_by_id(item_id.to_string()).one(db).await {
        Ok(Some(item)) => item,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!("failed to read item for TMDb metadata state {item_id}: {error:#}");
            return false;
        }
    };
    if item.overview.is_none() || item.production_year.is_none() || item.premiere_date.is_none() {
        return false;
    }

    let has_tmdb_id = ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(item_id))
        .filter(provider_ids::Column::Provider.eq("Tmdb"))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some();
    if !has_tmdb_id {
        return false;
    }

    ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq("Primary"))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

fn has_sidecar_nfo(path: &std::path::Path) -> bool {
    path.with_extension("nfo").exists()
        || path
            .parent()
            .map(|parent| parent.join("movie.nfo").exists())
            .unwrap_or_default()
}

async fn media_roots(state: &AppState) -> anyhow::Result<Vec<(PathBuf, String, String)>> {
    let paths = LibraryPaths::find()
        .order_by_asc(library_paths::Column::Path)
        .all(&state.db)
        .await
        .context("failed to list library paths for scan")?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let libraries = Libraries::find()
        .all(&state.db)
        .await
        .context("failed to list libraries for scan")?
        .into_iter()
        .map(|library| (library.id.clone(), library))
        .collect::<std::collections::HashMap<_, _>>();

    Ok(paths
        .into_iter()
        .filter_map(|path| {
            libraries.get(&path.library_id).map(|library| {
                (
                    PathBuf::from(path_utils::normalize_path(&path.path)),
                    path.library_id,
                    library.collection_type.clone(),
                )
            })
        })
        .collect())
}

fn media_probe_concurrency() -> usize {
    (crate::db::cpu_parallelism() / 4).clamp(1, 3)
}

fn scan_root_concurrency() -> usize {
    (crate::db::cpu_parallelism() / 4).clamp(1, 2)
}

fn ingest_concurrency() -> usize {
    (crate::db::cpu_parallelism() / 4).clamp(1, 2)
}

fn metadata_fetch_concurrency() -> usize {
    (crate::db::cpu_parallelism() / 4).clamp(1, 2)
}

fn ingest_queue_capacity() -> usize {
    crate::db::cpu_parallelism()
        .saturating_mul(512)
        .clamp(MIN_INGEST_QUEUE_CAPACITY, MAX_INGEST_QUEUE_CAPACITY)
}

fn media_probe_queue_capacity() -> usize {
    crate::db::cpu_parallelism().saturating_mul(32).clamp(
        MIN_MEDIA_PROBE_QUEUE_CAPACITY,
        MAX_MEDIA_PROBE_QUEUE_CAPACITY,
    )
}

async fn media_probe_cache_is_current(db: &sea_orm::DatabaseConnection) -> anyhow::Result<bool> {
    let value = crate::db::settings::get(db, MEDIA_PROBE_CACHE_VERSION_KEY)
        .await
        .context("failed to read media probe cache version")?;
    Ok(value.is_some_and(|value| value == MEDIA_PROBE_CACHE_VERSION))
}

async fn set_media_probe_cache_current(db: &sea_orm::DatabaseConnection) -> anyhow::Result<()> {
    crate::db::settings::set(db, MEDIA_PROBE_CACHE_VERSION_KEY, MEDIA_PROBE_CACHE_VERSION)
        .await
        .context("failed to update media probe cache version")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

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
    fn scan_root_concurrency_defaults_to_positive_value() {
        assert!(scan_root_concurrency() > 0);
    }

    #[test]
    fn media_probe_queue_capacity_defaults_to_positive_value() {
        assert!(media_probe_queue_capacity() > 0);
    }

    #[test]
    fn ingest_concurrency_defaults_to_positive_value() {
        assert!(ingest_concurrency() > 0);
    }

    #[test]
    fn ingest_queue_capacity_defaults_to_positive_value() {
        assert!(ingest_queue_capacity() > 0);
    }

    #[test]
    fn movie_library_root_video_file_is_movie() {
        let root = test_dir("movie_library_root_video_file_is_movie");
        let path = root.join("Movie One.mkv");
        fs::write(&path, []).unwrap();

        assert_eq!(
            normalize_scanned_file_type("Video", "movies", "library", "library", &path, &root),
            "Movie"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn movie_library_mixed_folder_video_file_is_movie() {
        let root = test_dir("movie_library_mixed_folder_video_file_is_movie");
        let group = root.join("动作");
        fs::create_dir_all(&group).unwrap();
        let path = group.join("Movie One.mkv");
        fs::write(&path, []).unwrap();
        fs::write(group.join("Movie Two.mkv"), []).unwrap();

        assert_eq!(
            normalize_scanned_file_type("Video", "movies", "group", "library", &path, &root),
            "Movie"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn movie_library_movie_folder_video_file_stays_video() {
        let root = test_dir("movie_library_movie_folder_video_file_stays_video");
        let movie = root.join("Movie One");
        fs::create_dir_all(&movie).unwrap();
        let path = movie.join("Movie One.mkv");
        fs::write(&path, []).unwrap();

        assert_eq!(
            normalize_scanned_file_type("Video", "movies", "movie", "library", &path, &root),
            "Video"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn episode_season_number_uses_decorated_chinese_season_ancestor() {
        let root = PathBuf::from("/media/动漫/国漫");
        let path = root.join("L/灵笼{tmdb-91097}/灵笼 第一季（2019）/4K 高码率/第1集.strm");

        assert_eq!(
            episode_season_number_from_path(&path, &root, "tvshows"),
            Some(1)
        );
    }

    fn test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jellyfin-rs-scanner-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
