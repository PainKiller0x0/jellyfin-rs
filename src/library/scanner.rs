use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use chrono::Datelike;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use walkdir::{DirEntry, WalkDir};

use crate::{
    app::state::AppState,
    entities::{
        libraries::Entity as Libraries,
        library_paths::{self, Entity as LibraryPaths},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_scan_states::{self, Entity as MediaScanStates},
        media_studios::{self, Entity as MediaStudios},
        provider_ids::{self, Entity as ProviderIds},
    },
    library::{
        classify::{
            audiobook_file_for_directory, book_file_for_directory, classify_media_path,
            file_video_type, folder_video_type, is_music_multi_part_folder, is_video_stub,
            iso_type_for_path, parent_id_for_path, parent_id_for_scanned_file,
            should_skip_disc_structure_entry, tv_folder_type, video_extra_type,
        },
        images::{
            extract_embedded_audio_image, upsert_image_asset, upsert_nfo_images,
            upsert_sidecar_images,
        },
        metadata::{ParsedMetadata, ParsedPerson, parse_sidecar_metadata_for_item},
        naming::parse_media_name,
        path_utils,
        probe::{ProbedAudioMetadata, probe_video_media},
        storage::{
            CachedMediaProbe, ScannedMediaItem, cached_media_probe_if_current,
            refresh_external_lyric_stream, remove_missing_media_items,
            remove_missing_media_items_in_scopes, upsert_default_media_stream,
            upsert_failed_media_probe, upsert_media_item, upsert_media_metadata,
            upsert_probed_audio_metadata, upsert_probed_media_streams,
        },
        subtitles::{
            clear_sidecar_audio, clear_sidecar_subtitles, upsert_sidecar_audio,
            upsert_sidecar_subtitles,
        },
    },
    strm,
};

const MEDIA_PROBE_CACHE_VERSION_KEY: &str = "media_probe_cache_version";
const MEDIA_PROBE_CACHE_VERSION: &str = "6";
const MIN_ITEM_REFRESH_QUEUE_CAPACITY: usize = 512;
const MAX_ITEM_REFRESH_QUEUE_CAPACITY: usize = 4096;
const METADATA_DEPENDENCY_MAX_ATTEMPTS: usize = 30;
const METADATA_DEPENDENCY_RETRY_DELAY: Duration = Duration::from_secs(1);
const SCAN_FINGERPRINT_VERSION: u64 = 2;

#[derive(Debug)]
pub struct ScanCancelled;

impl std::fmt::Display for ScanCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("media library scan was cancelled")
    }
}

impl std::error::Error for ScanCancelled {}

pub fn is_scan_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ScanCancelled>().is_some()
}

struct ScanTaskGuard<'a>(&'a crate::app::state::ScanTaskControl);

impl Drop for ScanTaskGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

#[derive(Clone)]
struct ScanProgressTracker {
    reporter: crate::app::state::ScanProgressReporter,
    discovered_entries: Arc<AtomicU64>,
    refreshed_items: Arc<AtomicU64>,
}

impl ScanProgressTracker {
    fn new(reporter: crate::app::state::ScanProgressReporter) -> Self {
        Self {
            reporter,
            discovered_entries: Arc::new(AtomicU64::new(0)),
            refreshed_items: Arc::new(AtomicU64::new(0)),
        }
    }

    fn set(&self, percentage: f64) {
        self.reporter.set(percentage);
    }

    fn discovered_entry(&self) {
        let discovered = self.discovered_entries.fetch_add(1, Ordering::Relaxed) + 1;
        if discovered.is_multiple_of(64) {
            let fraction = discovered as f64 / (discovered as f64 + 2_048.0);
            self.set(2.0 + 28.0 * fraction);
        }
    }

    fn root_completed(&self, completed: usize, total: usize) {
        if total > 0 {
            self.set(2.0 + 38.0 * completed as f64 / total as f64);
        }
    }

    fn refresh_completed(&self, total: usize) {
        let completed = self.refreshed_items.fetch_add(1, Ordering::Relaxed) + 1;
        let fraction = completed.min(total.max(1) as u64) as f64 / total.max(1) as f64;
        self.set(45.0 + 45.0 * fraction);
    }
}

fn ensure_scan_active(cancellation: &CancellationToken) -> anyhow::Result<()> {
    if cancellation.is_cancelled() {
        return Err(ScanCancelled.into());
    }
    Ok(())
}

#[derive(Default)]
struct SidecarFingerprintCache {
    by_directory: HashMap<PathBuf, u64>,
}

impl SidecarFingerprintCache {
    fn for_media_path(&mut self, path: &std::path::Path) -> u64 {
        let directory = if path.is_dir() {
            Some(path)
        } else {
            path.parent()
        };
        let Some(directory) = directory else {
            return 0;
        };

        *self
            .by_directory
            .entry(directory.to_path_buf())
            .or_insert_with(|| sidecar_directory_fingerprint(directory))
    }
}

struct ScanRootResult {
    scanned: usize,
    unchanged: usize,
    seen_paths: Vec<String>,
    refresh_queued: usize,
}

struct RefreshJobStager {
    tx: mpsc::Sender<ItemRefreshJob>,
    handle: tokio::task::JoinHandle<anyhow::Result<usize>>,
}

#[derive(Serialize, Deserialize)]
struct ItemRefreshJob {
    item: ScannedMediaItem,
    source_path: PathBuf,
    parsed_metadata: ParsedMetadata,
    clear_folder_metadata: bool,
    media_probe: Option<PendingMediaProbe>,
    scan_fingerprint: i64,
}

#[derive(Serialize, Deserialize)]
struct PendingMediaProbe {
    media_path: PathBuf,
    probe_path: PathBuf,
    allow_size_mismatch: bool,
}

struct LocalRefreshResult {
    stored_item_id: String,
    probe_skipped: bool,
    scan_state_cacheable: bool,
}

#[derive(Default)]
struct ItemRefreshResult {
    metadata_completed: bool,
    metadata_deferred: bool,
    metadata_failed: bool,
    probe_completed: bool,
    probe_skipped: bool,
    probe_failed: bool,
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
    preserve_existing_metadata: bool,
}

#[derive(Default)]
struct ItemRefreshWorkerStats {
    completed: usize,
    metadata_completed: usize,
    metadata_deferred: usize,
    metadata_failed: usize,
    probe_completed: usize,
    probe_skipped: usize,
    probe_failed: usize,
    failed: usize,
}

pub async fn scan_media_library(state: &AppState) -> anyhow::Result<usize> {
    Ok(scan_media_library_if_idle(state).await?.unwrap_or_default())
}

pub async fn refresh_media_item(
    state: &AppState,
    item_id: &str,
    mut policy: crate::library::tmdb_metadata::MetadataRefreshPolicy,
) -> anyhow::Result<bool> {
    let Some(item) = MediaItems::find_by_id(item_id.to_string())
        .one(&state.db)
        .await?
    else {
        return Ok(false);
    };
    let path = PathBuf::from(&item.path);

    crate::library::images::validate_image_assets(&state.db, item_id).await?;
    crate::library::images::upsert_sidecar_images(&state.db, &path, item_id).await?;

    let parsed_metadata = parse_sidecar_metadata_for_item(&path, &item.item_type).await;
    if policy.refresh_metadata && parsed_metadata.has_nfo {
        crate::library::storage::apply_local_metadata_refresh(
            &state.db,
            &item,
            &parsed_metadata,
            policy.replace_metadata,
        )
        .await?;
        crate::library::images::upsert_nfo_images(&state.db, item_id, &parsed_metadata.images)
            .await?;
        // Jellyfin merges local providers before remote providers. Once the NFO
        // has populated a field, a remote provider must only fill what remains.
        policy.replace_metadata = false;
    }

    let mut refreshed_item = MediaItems::find_by_id(item_id.to_string())
        .one(&state.db)
        .await?
        .unwrap_or(item);
    if policy.refresh_metadata && policy.force_refresh {
        force_refresh_media_probe(state, &refreshed_item).await?;
        if let Some(updated) = MediaItems::find_by_id(item_id.to_string())
            .one(&state.db)
            .await?
        {
            refreshed_item = updated;
        }
    }
    if refreshed_item.lock_data != 0 {
        policy.refresh_metadata = false;
        if !policy.force_refresh {
            policy.refresh_images = false;
        }
    }
    if !policy.refresh_metadata && !policy.refresh_images {
        return Ok(true);
    }

    let Some(api_key) = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|key| !key.is_empty())
    else {
        return Ok(true);
    };
    let tmdb_base_url = state.tmdb_proxy_url.read().await.clone();
    let tmdb_client = state.tmdb_http_client().await;
    match refreshed_item.item_type.as_str() {
        "Movie" | "Series" => {
            crate::library::tmdb_metadata::fetch_and_apply_tmdb_metadata(
                &state.db,
                item_id,
                &refreshed_item.item_type,
                &path,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
                policy,
            )
            .await?;
        }
        "Season" => {
            crate::library::tmdb_metadata::fetch_and_apply_season_tmdb_metadata(
                &state.db,
                item_id,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
                policy,
            )
            .await?;
        }
        "Episode" => {
            crate::library::tmdb_metadata::fetch_and_apply_episode_tmdb_metadata(
                &state.db,
                item_id,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
                policy,
            )
            .await?;
        }
        _ => {}
    }
    Ok(true)
}

async fn force_refresh_media_probe(
    state: &AppState,
    item: &media_items::Model,
) -> anyhow::Result<()> {
    if !matches!(
        item.item_type.as_str(),
        "Audio" | "AudioBook" | "Movie" | "Episode" | "Video" | "Trailer" | "MusicVideo"
    ) {
        return Ok(());
    }
    let media_path = PathBuf::from(&item.path);
    if is_video_stub(&media_path) {
        return Ok(());
    }
    let probe_path = if strm::is_strm_path(&media_path) {
        strm::resolve_strm_path(&media_path)
            .with_context(|| format!("failed to resolve STRM {}", media_path.display()))?
    } else {
        media_path.clone()
    };
    let dummy_chapter_duration_seconds = configured_dummy_chapter_duration_seconds(&state.db).await;
    let chapter_image_settings = crate::chapters::chapter_image_scan_settings(&state.db).await;
    run_media_probe_job(
        state.db.clone(),
        MediaProbeJob {
            item: ScannedMediaItem::from_stored(item),
            media_path,
            probe_path,
        },
        dummy_chapter_duration_seconds,
        &state.sa_config.ffmpeg_path,
        &chapter_image_settings,
    )
    .await?;
    Ok(())
}

pub async fn scan_media_library_if_idle(state: &AppState) -> anyhow::Result<Option<usize>> {
    scan_media_libraries_if_idle(state, &[]).await
}

pub async fn scan_media_libraries_if_idle(
    state: &AppState,
    library_ids: &[String],
) -> anyhow::Result<Option<usize>> {
    scan_media_libraries_internal(state, library_ids, None).await
}

pub async fn scan_media_paths_if_idle(
    state: &AppState,
    changed_paths: &[PathBuf],
) -> anyhow::Result<Option<usize>> {
    scan_media_libraries_internal(state, &[], Some(changed_paths)).await
}

async fn scan_media_libraries_internal(
    state: &AppState,
    library_ids: &[String],
    changed_paths: Option<&[PathBuf]>,
) -> anyhow::Result<Option<usize>> {
    let selected_library_ids =
        (!library_ids.is_empty()).then(|| library_ids.iter().cloned().collect::<HashSet<_>>());
    let roots = media_roots(state, selected_library_ids.as_ref()).await?;
    let targeted = changed_paths.is_some();
    let roots = match changed_paths {
        Some(changed_paths) => targeted_scan_roots(roots, changed_paths),
        None => roots
            .into_iter()
            .map(|(root, library_id, collection_type, enable_photos)| {
                (
                    root.clone(),
                    root,
                    library_id,
                    collection_type,
                    enable_photos,
                )
            })
            .collect(),
    };
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
    let cancellation = state.scan_task.start();
    let _scan_task_guard = ScanTaskGuard(&state.scan_task);
    let progress = ScanProgressTracker::new(state.scan_task.progress());
    progress.set(1.0);

    let scan_database_url = crate::db::database_url_from_env();
    let scan_db = crate::db::background_connection(&scan_database_url).await?;
    let scanned_library_ids = roots
        .iter()
        .map(|(_, _, library_id, _, _)| library_id.clone())
        .collect::<HashSet<_>>();
    let scan_states = Arc::new(load_scan_states(&scan_db, &scanned_library_ids).await?);
    let tmdb_enabled = crate::library::tmdb_metadata::tmdb_provider_enabled(&scan_db).await;
    let api_key = if tmdb_enabled {
        state.tmdb_api_key.read().await.clone().unwrap_or_default()
    } else {
        tracing::info!("TMDb metadata disabled by provider setting for this scan");
        String::new()
    };
    let scan_contexts =
        scan_context_fingerprints(&scan_db, &scanned_library_ids, !api_key.is_empty()).await?;

    let force_probe = !media_probe_cache_is_current(&scan_db).await?;
    if force_probe {
        tracing::info!("media probe cache version changed; forcing one-time media stream reprobe");
    }

    let mut tasks = tokio::task::JoinSet::new();
    let root_concurrency = scan_root_concurrency();
    let douban_enabled = crate::library::douban_metadata::douban_provider_enabled(&scan_db).await;
    let douban_cookie = if douban_enabled {
        state.douban_cookie.read().await.clone()
    } else {
        tracing::info!("Douban metadata disabled by provider setting for this scan");
        None
    };
    let tmdb_library_options =
        crate::library::tmdb_metadata::load_tmdb_library_provider_options(&scan_db).await?;
    let scan_id = uuid::Uuid::new_v4().to_string();
    let refresh_stager = start_refresh_job_stager(scan_db.clone(), scan_id.clone());
    let mut completed_library_ids = Vec::new();
    let mut completed_scan_scopes = Vec::new();
    let mut total = 0usize;
    let mut unchanged = 0usize;
    let mut all_seen = Vec::new();
    let mut refresh_queued = 0usize;
    let root_count = roots.len();
    let mut completed_roots = 0usize;
    let mut pending_roots = roots.into_iter();
    loop {
        while !cancellation.is_cancelled() && tasks.len() < root_concurrency {
            let Some((root, scan_path, library_id, collection_type, enable_photos)) =
                pending_roots.next()
            else {
                break;
            };
            if !root.exists() {
                tracing::warn!("media directory does not exist: {}", root.display());
                continue;
            }
            completed_library_ids.push(library_id.clone());
            completed_scan_scopes.push((library_id.clone(), scan_path.to_string_lossy().into()));
            let refresh_tx = refresh_stager.tx.clone();
            let scan_states = scan_states.clone();
            let cancellation = cancellation.clone();
            let progress = progress.clone();
            let scan_context = scan_contexts.get(&library_id).copied().unwrap_or_default();
            tasks.spawn(async move {
                scan_root(
                    root,
                    scan_path,
                    library_id,
                    collection_type,
                    enable_photos,
                    scan_context,
                    scan_states,
                    refresh_tx,
                    cancellation,
                    progress,
                )
                .await
            });
        }

        let Some(result) = tasks.join_next().await else {
            break;
        };
        completed_roots += 1;
        progress.root_completed(completed_roots, root_count);
        match result {
            Ok(Ok(result)) => {
                total += result.scanned;
                unchanged += result.unchanged;
                all_seen.extend(result.seen_paths);
                refresh_queued += result.refresh_queued;
            }
            Ok(Err(e)) => tracing::warn!("library scan failed: {e:#}"),
            Err(e) => tracing::warn!("scan task panicked: {e}"),
        }
    }

    drop(refresh_stager.tx);
    let staged_jobs = refresh_stager
        .handle
        .await
        .context("refresh job stager task panicked")??;
    tracing::info!("staged {staged_jobs} item refresh job(s) in PostgreSQL");
    if cancellation.is_cancelled() {
        crate::library::refresh_queue::delete_scan(&scan_db, &scan_id).await?;
        return Err(ScanCancelled.into());
    }
    progress.set(40.0);

    completed_library_ids.sort();
    completed_library_ids.dedup();
    if targeted {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                crate::library::refresh_queue::delete_scan(&scan_db, &scan_id).await?;
                return Err(ScanCancelled.into());
            }
            result = remove_missing_media_items_in_scopes(
                &scan_db,
                &completed_scan_scopes,
                &all_seen,
            ) => result?,
        }
    } else {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                crate::library::refresh_queue::delete_scan(&scan_db, &scan_id).await?;
                return Err(ScanCancelled.into());
            }
            result = remove_missing_media_items(&scan_db, &completed_library_ids, &all_seen) => result?,
        }
    }
    progress.set(45.0);
    if staged_jobs == 0 {
        progress.set(90.0);
    }

    let refresh_stats = run_item_refresh_workers(
        scan_db.clone(),
        &scan_id,
        force_probe,
        state.sa_config.ffmpeg_path.clone(),
        api_key.clone(),
        state.tmdb_proxy_url.clone(),
        state.tmdb_http_client.clone(),
        tmdb_library_options,
        cancellation.clone(),
        progress.clone(),
        staged_jobs,
    )
    .await;
    if cancellation.is_cancelled() {
        crate::library::refresh_queue::delete_scan(&scan_db, &scan_id).await?;
        return Err(ScanCancelled.into());
    }
    progress.set(90.0);
    if force_probe && refresh_stats.probe_failed == 0 {
        if let Err(error) = set_media_probe_cache_current(&scan_db).await {
            tracing::warn!("failed to mark media probe cache current: {error:#}");
        }
    }
    if !targeted {
        let post_scan_metadata = run_post_scan_metadata_tasks(
            &scan_db,
            &api_key,
            state.tmdb_proxy_url.clone(),
            state.tmdb_http_client.clone(),
            douban_cookie.as_deref(),
        );
        progress.set(95.0);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                crate::library::refresh_queue::delete_scan(&scan_db, &scan_id).await?;
                return Err(ScanCancelled.into());
            }
            _ = post_scan_metadata => {}
        }
    }
    progress.set(99.0);

    let queue_stats = crate::library::refresh_queue::stats(&scan_db, &scan_id).await?;
    tracing::info!(
        "PostgreSQL refresh queue scan_id={scan_id} pending={} running={} completed={} failed={}",
        queue_stats.pending,
        queue_stats.running,
        queue_stats.completed,
        queue_stats.failed
    );
    crate::library::refresh_queue::delete_completed(&scan_db, &scan_id).await?;
    tracing::info!(
        "media scan discovered {total} file item(s) across all libraries; queued {refresh_queued} item refresh job(s)"
    );
    tracing::info!("media scan skipped {unchanged} unchanged item(s) through fingerprint cache");
    tracing::info!(
        "item refresh completed {} job(s); metadata_completed={} metadata_deferred={} metadata_failed={} probe_completed={} probe_skipped={} probe_failed={} failed={}",
        refresh_stats.completed,
        refresh_stats.metadata_completed,
        refresh_stats.metadata_deferred,
        refresh_stats.metadata_failed,
        refresh_stats.probe_completed,
        refresh_stats.probe_skipped,
        refresh_stats.probe_failed,
        refresh_stats.failed
    );
    if refresh_stats.probe_skipped > 0 {
        tracing::info!(
            "item refresh reused cached probe data for {}/{} file item(s)",
            refresh_stats.probe_skipped,
            total
        );
    }

    progress.set(100.0);

    Ok(Some(total))
}

async fn scan_root(
    root: PathBuf,
    scan_path: PathBuf,
    library_id: String,
    collection_type: String,
    enable_photos: bool,
    scan_context: i64,
    scan_states: Arc<HashMap<String, i64>>,
    refresh_tx: mpsc::Sender<ItemRefreshJob>,
    cancellation: CancellationToken,
    progress: ScanProgressTracker,
) -> anyhow::Result<ScanRootResult> {
    let mut scanned = 0usize;
    let mut unchanged = 0usize;
    let mut visited_entries = 0usize;
    let mut seen_paths = Vec::new();
    let mut refresh_queued = 0usize;
    let mut sidecar_fingerprints = SidecarFingerprintCache::default();
    let inventory = scan_path_inventory(&scan_path, &cancellation, &progress).await?;
    let stack_part_owners = scan_path
        .is_dir()
        .then(|| {
            movie_stack_part_owners(
                inventory.iter().filter_map(inventory_file_path),
                &scan_path,
                &collection_type,
                &cancellation,
            )
        })
        .unwrap_or_default();
    let episode_version_owners = scan_path
        .is_dir()
        .then(|| {
            episode_version_owners(
                inventory.iter().filter_map(inventory_file_path),
                &root,
                &library_id,
                &collection_type,
                &cancellation,
            )
        })
        .unwrap_or_default();

    for entry in inventory {
        ensure_scan_active(&cancellation)?;
        visited_entries += 1;
        if visited_entries.is_multiple_of(64) {
            crate::library::resource_governor::cooperative_scan_yield().await;
        }
        let path = entry.path();
        if path == root {
            continue;
        }
        if should_skip_disc_structure_entry(path, &root) {
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
            // Jellyfin's BookResolver collapses a directory containing exactly one supported
            // book into that file-backed Book item. The file is handled below by WalkDir.
            if collection_type == "books"
                && (book_file_for_directory(path).is_some()
                    || audiobook_file_for_directory(path).is_some())
            {
                continue;
            }
            if collection_type == "music"
                && is_music_multi_part_folder(path)
                && path.parent().is_some_and(|parent| {
                    tv_folder_type(parent, &root, &collection_type) == "MusicAlbum"
                })
            {
                continue;
            }
            seen_paths.push(path_string.clone());
            let scan_fingerprint = media_scan_fingerprint(
                path,
                resolved.modified_at,
                resolved.size_bytes,
                scan_context,
                &mut sidecar_fingerprints,
            );
            if scan_states.get(&path_string) == Some(&scan_fingerprint) {
                unchanged += 1;
                continue;
            }
            let mut folder_type = tv_folder_type(path, &root, &collection_type);
            if collection_type == "homevideos" && !enable_photos && folder_type == "PhotoAlbum" {
                folder_type = "Folder";
            }
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
                resolved.created_at,
                year,
            );
            if folder_type == "Season" {
                item.season_number =
                    crate::library::tmdb_metadata::parse_season_number(&resolved.name);
            }
            item.video_type = folder_video_type(path).map(ToString::to_string);
            // Jellyfin resolves DVD and Blu-ray directory structures as Video
            // items whose Path happens to be a directory, not as folders.
            item.is_folder = item
                .video_type
                .as_deref()
                .is_none_or(|video_type| !matches!(video_type, "Dvd" | "BluRay"));
            item.iso_type = item
                .video_type
                .as_deref()
                .filter(|video_type| *video_type == "Iso")
                .and_then(|_| iso_type_for_path(path).map(ToString::to_string));
            item.video_3d_format = crate::library::naming::parse_video_3d_format(&item.path);
            let parsed_metadata = parse_sidecar_metadata_for_item(path, folder_type).await;
            if parsed_metadata.has_nfo {
                if let Some(title) = parsed_metadata.title.clone() {
                    item.title = title;
                }
                item.overview = parsed_metadata.overview.clone();
                item.official_rating = parsed_metadata.official_rating.clone();
                item.custom_rating = parsed_metadata.custom_rating.clone();
                item.video_3d_format = parsed_metadata
                    .video_3d_format
                    .clone()
                    .or(item.video_3d_format);
                item.original_title = parsed_metadata.original_title.clone();
                item.sort_name = parsed_metadata.sort_name.clone();
                item.forced_sort_name = parsed_metadata.forced_sort_name.clone();
                item.lock_data = parsed_metadata.lock_data;
                item.locked_fields = parsed_metadata.locked_fields.clone();
                item.tagline = parsed_metadata.tagline.clone();
                item.collection_name = parsed_metadata.collection_name.clone();
                item.original_language = parsed_metadata.original_language.clone();
                item.preferred_metadata_language =
                    parsed_metadata.preferred_metadata_language.clone();
                item.preferred_metadata_country_code =
                    parsed_metadata.preferred_metadata_country_code.clone();
                item.series_status = parsed_metadata.series_status.clone();
                item.air_days = parsed_metadata.air_days.clone();
                item.air_time = parsed_metadata.air_time.clone();
                item.home_page_url = parsed_metadata.home_page_url.clone();
                item.remote_trailers = parsed_metadata.remote_trailers.clone();
                item.production_locations = parsed_metadata.production_locations.clone();
                item.production_year = parsed_metadata.production_year.or(item.production_year);
                item.premiere_date = parsed_metadata.premiere_date.clone();
                item.end_date = parsed_metadata.end_date.clone();
                item.runtime_ticks = parsed_metadata.runtime_ticks;
                item.aspect_ratio = parsed_metadata.aspect_ratio.clone();
                item.width = parsed_metadata.width;
                item.height = parsed_metadata.height;
                item.has_subtitles = parsed_metadata.has_subtitles.unwrap_or(false);
                item.display_order = parsed_metadata.display_order.clone();
                item.community_rating = parsed_metadata.community_rating;
                item.critic_rating = parsed_metadata.critic_rating;
                item.created_at = parsed_metadata.created_at.unwrap_or(item.created_at);
                if folder_type == "Season" {
                    item.season_number = parsed_metadata.season_number.or(item.season_number);
                }
            }
            let media_probe = item
                .video_type
                .as_deref()
                .filter(|video_type| matches!(*video_type, "Dvd" | "BluRay"))
                .map(|_| PendingMediaProbe {
                    media_path: path.to_path_buf(),
                    probe_path: path.to_path_buf(),
                    allow_size_mismatch: true,
                });
            let job = ItemRefreshJob {
                item,
                source_path: path.to_path_buf(),
                parsed_metadata,
                clear_folder_metadata: folder_type == "Folder",
                media_probe,
                scan_fingerprint,
            };
            if queue_item_refresh(&refresh_tx, job, &cancellation).await {
                refresh_queued += 1;
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
        if collection_type == "homevideos" && !enable_photos && item_type == "Photo" {
            continue;
        }
        let extra_type =
            video_extra_type(path, matches!(item_type.as_str(), "Audio" | "AudioBook"))
                .map(|extra_type| extra_type.as_jellyfin_str().to_string());
        let video_type = file_video_type(path).map(ToString::to_string);
        let iso_type = video_type
            .as_deref()
            .filter(|video_type| *video_type == "Iso")
            .and_then(|_| iso_type_for_path(path).map(ToString::to_string));
        let mut item_type = normalize_scanned_file_type(
            &item_type,
            &collection_type,
            &parent_id,
            &library_id,
            path,
            &root,
            extra_type.is_some(),
        );
        parent_id = parent_id_for_scanned_file(
            path,
            &root,
            &library_id,
            &collection_type,
            &item_type,
            extra_type.is_some(),
        );
        if item_type == "Movie"
            && let Some(primary_path) = stack_part_owners.get(path)
        {
            item_type = "Video".to_string();
            parent_id = crate::util::stable_item_id(primary_path);
        } else if item_type == "Episode"
            && let Some(primary_path) = episode_version_owners.get(path)
        {
            item_type = "Video".to_string();
            parent_id = crate::util::stable_item_id(primary_path);
        }
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
        scanned += 1;
        let scan_fingerprint = media_scan_fingerprint(
            path,
            resolved.modified_at,
            resolved.size_bytes,
            scan_context,
            &mut sidecar_fingerprints,
        );
        if scan_states.get(&path_string) == Some(&scan_fingerprint) {
            unchanged += 1;
            continue;
        }
        let parsed_metadata = parse_sidecar_metadata_for_item(path, &item_type).await;
        let parsed_name = parse_media_name(path, &collection_type);
        let collapsed_book_directory = (item_type == "Book")
            .then(|| path.parent())
            .flatten()
            .filter(|parent| {
                book_file_for_directory(parent)
                    .as_deref()
                    .is_some_and(|book| same_normalized_path(book, path))
            });
        let collapsed_audiobook_directory = (item_type == "AudioBook")
            .then(|| path.parent())
            .flatten()
            .filter(|parent| {
                audiobook_file_for_directory(parent)
                    .as_deref()
                    .is_some_and(|audiobook| same_normalized_path(audiobook, path))
            });
        let parsed_book = (item_type == "Book").then(|| {
            let source = collapsed_book_directory
                .and_then(|parent| parent.file_name())
                .or_else(|| path.file_stem())
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            crate::library::naming::parse_book_name(source)
        });
        if let Some(parent) = collapsed_book_directory.or(collapsed_audiobook_directory) {
            parent_id = parent_id_for_path(parent, &root, &library_id);
        }
        let (audiobook_title, audiobook_year) = collapsed_audiobook_directory
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .map(crate::library::tmdb_metadata::clean_title_with_year)
            .unzip();
        let mut season_number = resolve_episode_season_number(
            path,
            &root,
            &collection_type,
            parsed_metadata.season_number,
            parsed_name.season_number,
        );
        let episode_number = parsed_metadata
            .episode_number
            .or(parsed_name.episode_number);
        if item_type == "Episode"
            && season_number.is_none()
            && (episode_number.is_some()
                || parsed_metadata.premiere_date.is_some()
                || parsed_name.premiere_date.is_some())
        {
            season_number = Some(1);
        }
        let episode_number_end = parsed_metadata
            .ending_episode_number
            .or(parsed_name.ending_episode_number);
        let title = if parsed_metadata.has_nfo {
            parsed_metadata
                .title
                .clone()
                .unwrap_or_else(|| resolved.name.clone())
        } else if let Some(book) = parsed_book.as_ref() {
            book.title.clone()
        } else if let Some(title) = audiobook_title {
            title
        } else if parsed_name.title.is_empty() {
            resolved.name.clone()
        } else {
            parsed_name.title.clone()
        };
        let probe_target_path = probe_path.as_deref().unwrap_or(path);
        let standalone_book_series_name = (item_type == "Book"
            && collapsed_book_directory.is_none())
        .then(|| {
            path.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .map(ToString::to_string)
        })
        .flatten();
        let item = ScannedMediaItem {
            id: resolved.id,
            title,
            path: path_string,
            library_id: library_id.clone(),
            parent_id,
            item_type,
            extra_type,
            video_type,
            iso_type,
            video_3d_format: parsed_metadata
                .video_3d_format
                .clone()
                .or(parsed_name.video_3d_format),
            is_folder: false,
            container,
            overview: parsed_metadata.overview.clone(),
            official_rating: parsed_metadata.official_rating.clone(),
            custom_rating: parsed_metadata.custom_rating.clone(),
            extended_video_type: (!parsed_name.extended_video_types.is_empty())
                .then(|| parsed_name.extended_video_types.join(",")),
            original_title: parsed_metadata.original_title.clone(),
            sort_name: parsed_metadata.sort_name.clone(),
            forced_sort_name: parsed_metadata.forced_sort_name.clone(),
            lock_data: parsed_metadata.lock_data,
            locked_fields: parsed_metadata.locked_fields.clone(),
            tagline: parsed_metadata.tagline.clone(),
            collection_name: parsed_metadata.collection_name.clone(),
            original_language: parsed_metadata.original_language.clone(),
            preferred_metadata_language: parsed_metadata.preferred_metadata_language.clone(),
            preferred_metadata_country_code: parsed_metadata
                .preferred_metadata_country_code
                .clone(),
            series_status: parsed_metadata.series_status.clone(),
            air_days: parsed_metadata.air_days.clone(),
            air_time: parsed_metadata.air_time.clone(),
            home_page_url: parsed_metadata.home_page_url.clone(),
            remote_trailers: parsed_metadata.remote_trailers.clone(),
            production_locations: parsed_metadata.production_locations.clone(),
            production_year: parsed_metadata
                .production_year
                .or_else(|| {
                    parsed_name
                        .premiere_date
                        .as_deref()
                        .and_then(crate::util::year_from_yyyy_mm_dd)
                })
                .or_else(|| parsed_book.as_ref().and_then(|book| book.production_year))
                .or(audiobook_year.flatten()),
            premiere_date: parsed_metadata
                .premiere_date
                .clone()
                .or_else(|| parsed_name.premiere_date.clone()),
            end_date: parsed_metadata.end_date.clone(),
            runtime_ticks: parsed_metadata.runtime_ticks,
            aspect_ratio: parsed_metadata.aspect_ratio.clone(),
            width: parsed_metadata.width,
            height: parsed_metadata.height,
            has_subtitles: parsed_metadata.has_subtitles.unwrap_or(false),
            photo_metadata: None,
            display_order: parsed_metadata.display_order.clone(),
            size_bytes: resolved.size_bytes,
            season_number: parsed_book
                .as_ref()
                .and_then(|book| book.parent_index_number)
                .or(season_number),
            episode_number: parsed_book
                .as_ref()
                .and_then(|book| book.index_number)
                .or(episode_number),
            episode_number_end,
            airs_before_episode_number: parsed_metadata.airs_before_episode_number,
            airs_after_season_number: parsed_metadata.airs_after_season_number,
            airs_before_season_number: parsed_metadata.airs_before_season_number,
            series_name: parsed_metadata
                .series_name
                .clone()
                .or_else(|| {
                    parsed_book
                        .as_ref()
                        .and_then(|book| book.series_name.clone())
                })
                .or(standalone_book_series_name),
            community_rating: parsed_metadata.community_rating,
            critic_rating: parsed_metadata.critic_rating,
            modified_at: resolved.modified_at,
            created_at: parsed_metadata.created_at.unwrap_or(resolved.created_at),
        };

        let media_probe = (!is_video_stub(path)
            && matches!(
                item.item_type.as_str(),
                "Audio" | "AudioBook" | "Movie" | "Episode" | "Video" | "Trailer" | "MusicVideo"
            ))
        .then(|| PendingMediaProbe {
            media_path: path.to_path_buf(),
            probe_path: probe_target_path.to_path_buf(),
            allow_size_mismatch: is_strm_file,
        });
        let job = ItemRefreshJob {
            item,
            source_path: path.to_path_buf(),
            parsed_metadata,
            clear_folder_metadata: false,
            media_probe,
            scan_fingerprint,
        };
        if queue_item_refresh(&refresh_tx, job, &cancellation).await {
            refresh_queued += 1;
        }
    }

    Ok(ScanRootResult {
        scanned,
        unchanged,
        seen_paths,
        refresh_queued,
    })
}

async fn scan_path_inventory(
    scan_path: &std::path::Path,
    cancellation: &CancellationToken,
    progress: &ScanProgressTracker,
) -> anyhow::Result<Vec<DirEntry>> {
    let mut inventory = Vec::new();
    for entry in WalkDir::new(scan_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_scan_path(entry.path()))
    {
        ensure_scan_active(cancellation)?;
        progress.discovered_entry();
        match entry {
            Ok(entry) => inventory.push(entry),
            Err(error) => tracing::warn!("failed to read media path: {error}"),
        }
        if inventory.len().is_multiple_of(64) {
            crate::library::resource_governor::cooperative_scan_yield().await;
        }
    }
    Ok(inventory)
}

fn inventory_file_path(entry: &DirEntry) -> Option<&std::path::Path> {
    entry.file_type().is_file().then(|| entry.path())
}

fn movie_stack_part_owners<'a>(
    paths: impl IntoIterator<Item = &'a std::path::Path>,
    root: &std::path::Path,
    collection_type: &str,
    cancellation: &CancellationToken,
) -> HashMap<PathBuf, PathBuf> {
    if collection_type != "movies" {
        return HashMap::new();
    }

    let mut groups = HashMap::<(PathBuf, String), Vec<(i64, PathBuf)>>::new();
    for path in paths {
        if cancellation.is_cancelled() {
            break;
        }
        if should_skip_disc_structure_entry(path, root)
            || classify_media_path(path, collection_type).as_deref() != Some("Video")
            || video_extra_type(path, false).is_some()
        {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent != root && tv_folder_type(parent, root, collection_type) == "Movie" {
            continue;
        }
        let parsed = parse_media_name(path, collection_type);
        let (Some(stack_key), Some(stack_part)) = (parsed.stack_key, parsed.stack_part) else {
            continue;
        };
        groups
            .entry((parent.to_path_buf(), stack_key))
            .or_default()
            .push((stack_part, path.to_path_buf()));
    }

    let mut owners = HashMap::new();
    for mut parts in groups.into_values() {
        if parts.len() < 2 {
            continue;
        }
        parts.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let primary = parts[0].1.clone();
        for (_, path) in parts.into_iter().skip(1) {
            owners.insert(path, primary.clone());
        }
    }
    owners
}

fn episode_version_owners<'a>(
    paths: impl IntoIterator<Item = &'a std::path::Path>,
    library_root: &std::path::Path,
    library_id: &str,
    collection_type: &str,
    cancellation: &CancellationToken,
) -> HashMap<PathBuf, PathBuf> {
    if !matches!(collection_type, "tvshows" | "tv") {
        return HashMap::new();
    }

    let mut groups =
        HashMap::<(String, Option<i64>, String), Vec<(PathBuf, Option<String>, Option<i64>)>>::new(
        );
    for path in paths {
        if cancellation.is_cancelled() {
            break;
        }
        if classify_media_path(path, collection_type).as_deref() != Some("Episode")
            || video_extra_type(path, false).is_some()
        {
            continue;
        }
        let parsed = parse_media_name(path, collection_type);
        let season = resolve_episode_season_number(
            path,
            library_root,
            collection_type,
            None,
            parsed.season_number,
        )
        .or_else(|| {
            (parsed.episode_number.is_some() || parsed.premiere_date.is_some()).then_some(1)
        });
        let episode_key = if let Some(episode) = parsed.episode_number {
            format!(
                "E{episode}:{}",
                parsed.ending_episode_number.unwrap_or(episode)
            )
        } else if let Some(date) = parsed.premiere_date.as_deref() {
            format!("D{date}")
        } else {
            continue;
        };
        let parent_id = parent_id_for_scanned_file(
            path,
            library_root,
            library_id,
            collection_type,
            "Episode",
            false,
        );
        groups
            .entry((parent_id, season, episode_key))
            .or_default()
            .push((path.to_path_buf(), parsed.version, parsed.stack_part));
    }

    let mut owners = HashMap::new();
    for mut versions in groups.into_values() {
        if versions.len() < 2 {
            continue;
        }
        let mut stack_counts = HashMap::<String, usize>::new();
        for (path, _, _) in &versions {
            if let Some(key) = parse_media_name(path, collection_type).stack_key {
                *stack_counts.entry(key).or_default() += 1;
            }
        }
        versions.sort_by(|left, right| {
            let left_parsed = parse_media_name(&left.0, collection_type);
            let right_parsed = parse_media_name(&right.0, collection_type);
            let left_stacked = left_parsed
                .stack_key
                .as_ref()
                .and_then(|key| stack_counts.get(key))
                .is_some_and(|count| *count > 1);
            let right_stacked = right_parsed
                .stack_key
                .as_ref()
                .and_then(|key| stack_counts.get(key))
                .is_some_and(|count| *count > 1);
            right_stacked
                .cmp(&left_stacked)
                .then_with(|| {
                    video_version_rank(right.1.as_deref())
                        .cmp(&video_version_rank(left.1.as_deref()))
                })
                .then_with(|| left.2.unwrap_or(i64::MAX).cmp(&right.2.unwrap_or(i64::MAX)))
                .then_with(|| left.0.cmp(&right.0))
        });
        let primary = versions[0].0.clone();
        for (path, _, _) in versions.into_iter().skip(1) {
            owners.insert(path, primary.clone());
        }
    }
    owners
}

fn video_version_rank(version: Option<&str>) -> i64 {
    let version = version.unwrap_or_default().to_ascii_lowercase();
    for (needle, rank) in [
        ("2160p", 2160),
        ("4k", 2160),
        ("1080p", 1080),
        ("720p", 720),
        ("480p", 480),
    ] {
        if version.contains(needle) {
            return rank;
        }
    }
    0
}

async fn queue_item_refresh(
    refresh_tx: &mpsc::Sender<ItemRefreshJob>,
    job: ItemRefreshJob,
    cancellation: &CancellationToken,
) -> bool {
    let sent = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return false,
        result = refresh_tx.send(job) => result,
    };
    match sent {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                "item refresh queue closed before job could be scheduled for {}",
                error.0.item.path
            );
            false
        }
    }
}

fn start_refresh_job_stager(db: sea_orm::DatabaseConnection, scan_id: String) -> RefreshJobStager {
    const STAGE_BATCH_SIZE: usize = 256;
    let (tx, mut rx) = mpsc::channel::<ItemRefreshJob>(item_refresh_queue_capacity());
    let handle = tokio::spawn(async move {
        let mut staged = 0usize;
        let mut batch = Vec::with_capacity(STAGE_BATCH_SIZE);
        while let Some(job) = rx.recv().await {
            batch.push(job);
            if batch.len() >= STAGE_BATCH_SIZE {
                staged += stage_refresh_job_batch(&db, &scan_id, &mut batch).await?;
            }
        }
        staged += stage_refresh_job_batch(&db, &scan_id, &mut batch).await?;
        Ok(staged)
    });
    RefreshJobStager { tx, handle }
}

async fn stage_refresh_job_batch(
    db: &sea_orm::DatabaseConnection,
    scan_id: &str,
    batch: &mut Vec<ItemRefreshJob>,
) -> anyhow::Result<usize> {
    if batch.is_empty() {
        return Ok(0);
    }
    let jobs = batch
        .drain(..)
        .map(|job| {
            Ok(crate::library::refresh_queue::RefreshJobPayload {
                library_id: job.item.library_id.clone(),
                path: job.item.path.clone(),
                payload: serde_json::to_value(job).context("failed to encode item refresh job")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    crate::library::refresh_queue::enqueue_batch(db, scan_id, &jobs).await
}

async fn run_item_refresh_workers(
    db: sea_orm::DatabaseConnection,
    preferred_scan_id: &str,
    force_probe: bool,
    ffmpeg_path: String,
    api_key: String,
    tmdb_proxy_url: Arc<RwLock<Option<String>>>,
    tmdb_http_client: Arc<RwLock<reqwest::Client>>,
    library_options: HashMap<String, crate::library::tmdb_metadata::TmdbLibraryProviderOptions>,
    cancellation: CancellationToken,
    progress: ScanProgressTracker,
    refresh_total: usize,
) -> ItemRefreshWorkerStats {
    let concurrency = item_refresh_concurrency();
    let chapter_image_settings = Arc::new(crate::chapters::chapter_image_scan_settings(&db).await);
    let dummy_chapter_duration_seconds = configured_dummy_chapter_duration_seconds(&db).await;
    let library_options = Arc::new(library_options);
    let preferred_scan_id = Arc::<str>::from(preferred_scan_id);
    let mut workers = tokio::task::JoinSet::new();
    tracing::info!(
        "Jellyfin-style item refresh workers started concurrency={concurrency} source=PostgreSQL"
    );

    for worker_index in 0..concurrency {
        let db = db.clone();
        let ffmpeg_path = ffmpeg_path.clone();
        let api_key = api_key.clone();
        let tmdb_proxy_url = tmdb_proxy_url.clone();
        let tmdb_http_client = tmdb_http_client.clone();
        let library_options = library_options.clone();
        let chapter_image_settings = chapter_image_settings.clone();
        let preferred_scan_id = preferred_scan_id.clone();
        let cancellation = cancellation.clone();
        let progress = progress.clone();
        workers.spawn(async move {
            let mut stats = ItemRefreshWorkerStats::default();
            let mut idle_rounds = 0usize;
            loop {
                if cancellation.is_cancelled() {
                    break;
                }
                let claim = crate::library::refresh_queue::claim_next(&db, &preferred_scan_id);
                let claimed = match tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break,
                    claimed = claim => claimed,
                } {
                    Ok(claimed) => claimed,
                    Err(error) => {
                        stats.failed += 1;
                        tracing::warn!("failed to claim PostgreSQL refresh job: {error:#}");
                        break;
                    }
                };
                let Some(claimed) = claimed else {
                    idle_rounds += 1;
                    if idle_rounds >= 3 {
                        break;
                    }
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                    continue;
                };
                idle_rounds = 0;
                let job = match serde_json::from_str::<ItemRefreshJob>(&claimed.payload) {
                    Ok(job) => job,
                    Err(error) => {
                        stats.failed += 1;
                        let _ = crate::library::refresh_queue::fail(
                            &db,
                            claimed.id,
                            &format!("invalid refresh payload: {error}"),
                        )
                        .await;
                        progress.refresh_completed(refresh_total);
                        continue;
                    }
                };
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break,
                    _ = crate::library::resource_governor::wait_for_background_worker(worker_index, 1) => {}
                }
                let refresh = run_item_refresh_job(
                    db.clone(),
                    job,
                    force_probe,
                    &ffmpeg_path,
                    dummy_chapter_duration_seconds,
                    &chapter_image_settings,
                    &api_key,
                    tmdb_proxy_url.clone(),
                    tmdb_http_client.clone(),
                    &library_options,
                );
                let refresh_result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => break,
                    result = refresh => result,
                };
                match refresh_result {
                    Ok(result) => {
                        stats.completed += 1;
                        if result.metadata_completed {
                            stats.metadata_completed += 1;
                        }
                        if result.metadata_deferred {
                            stats.metadata_deferred += 1;
                        }
                        if result.metadata_failed {
                            stats.metadata_failed += 1;
                        }
                        if result.probe_completed {
                            stats.probe_completed += 1;
                        }
                        if result.probe_skipped {
                            stats.probe_skipped += 1;
                        }
                        if result.probe_failed {
                            stats.probe_failed += 1;
                        }
                        if let Err(error) =
                            crate::library::refresh_queue::complete(&db, claimed.id).await
                        {
                            stats.failed += 1;
                            tracing::warn!("failed to complete PostgreSQL refresh job: {error:#}");
                        }
                    }
                    Err(error) => {
                        if let Err(queue_error) = crate::library::refresh_queue::fail(
                            &db,
                            claimed.id,
                            &format!("{error:#}"),
                        )
                        .await
                        {
                            stats.failed += 1;
                            tracing::warn!(
                                "failed to record PostgreSQL refresh error: {queue_error:#}"
                            );
                        }
                        tracing::warn!("item refresh failed: {error:#}");
                    }
                }
                progress.refresh_completed(refresh_total);
            }
            stats
        });
    }

    let mut stats = ItemRefreshWorkerStats::default();
    while let Some(result) = workers.join_next().await {
        match result {
            Ok(worker_stats) => {
                stats.completed += worker_stats.completed;
                stats.metadata_completed += worker_stats.metadata_completed;
                stats.metadata_deferred += worker_stats.metadata_deferred;
                stats.metadata_failed += worker_stats.metadata_failed;
                stats.probe_completed += worker_stats.probe_completed;
                stats.probe_skipped += worker_stats.probe_skipped;
                stats.probe_failed += worker_stats.probe_failed;
                stats.failed += worker_stats.failed;
            }
            Err(error) => {
                stats.failed += 1;
                tracing::warn!("item refresh worker task panicked: {error}");
            }
        }
    }

    tracing::info!(
        "Jellyfin-style item refresh completed {} job(s); metadata_completed={} metadata_deferred={} metadata_failed={} probe_completed={} probe_skipped={} probe_failed={} failed={}",
        stats.completed,
        stats.metadata_completed,
        stats.metadata_deferred,
        stats.metadata_failed,
        stats.probe_completed,
        stats.probe_skipped,
        stats.probe_failed,
        stats.failed
    );
    stats
}

#[allow(clippy::too_many_arguments)]
async fn run_item_refresh_job(
    db: sea_orm::DatabaseConnection,
    mut job: ItemRefreshJob,
    force_probe: bool,
    ffmpeg_path: &str,
    dummy_chapter_duration_seconds: i64,
    chapter_image_settings: &crate::chapters::ChapterImageScanSettings,
    api_key: &str,
    tmdb_proxy_url: Arc<RwLock<Option<String>>>,
    tmdb_http_client: Arc<RwLock<reqwest::Client>>,
    library_options: &HashMap<String, crate::library::tmdb_metadata::TmdbLibraryProviderOptions>,
) -> anyhow::Result<ItemRefreshResult> {
    let mut result = ItemRefreshResult::default();
    let local = run_local_refresh_job(&db, &mut job, force_probe).await?;
    job.item.id = local.stored_item_id;

    if let Some(metadata_job) =
        metadata_fetch_job(&job.item, &job.source_path, job.parsed_metadata.has_nfo)
    {
        let provider_options = library_options
            .get(&job.item.library_id)
            .cloned()
            .unwrap_or_default();
        let tmdb_base_url = tmdb_proxy_url.read().await.clone();
        let tmdb_client = tmdb_http_client.read().await.clone();
        for attempt in 0..=METADATA_DEPENDENCY_MAX_ATTEMPTS {
            match run_metadata_fetch_job(
                db.clone(),
                &metadata_job,
                api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
                &provider_options,
            )
            .await
            {
                Ok(true) => {
                    result.metadata_completed = true;
                    break;
                }
                Ok(false) if attempt < METADATA_DEPENDENCY_MAX_ATTEMPTS => {
                    tokio::time::sleep(METADATA_DEPENDENCY_RETRY_DELAY).await;
                }
                Ok(false) => {
                    result.metadata_deferred = true;
                    break;
                }
                Err(error) => {
                    result.metadata_failed = true;
                    tracing::warn!(
                        "metadata provider failed during item refresh: {}",
                        crate::library::tmdb_metadata::redact_tmdb_error(&error)
                    );
                    break;
                }
            }
        }

        if let Some(stored) = MediaItems::find_by_id(job.item.id.clone()).one(&db).await? {
            job.item = ScannedMediaItem::from_stored(&stored);
        }
    }

    let scan_state = (
        job.item.path.clone(),
        job.item.id.clone(),
        job.item.library_id.clone(),
        job.scan_fingerprint,
    );
    let Some(pending_probe) = job.media_probe.take() else {
        persist_completed_scan_state(&db, &scan_state, local.scan_state_cacheable).await;
        return Ok(result);
    };

    if local.probe_skipped {
        result.probe_skipped = true;
        persist_completed_scan_state(&db, &scan_state, local.scan_state_cacheable).await;
        return Ok(result);
    }

    match run_media_probe_job(
        db.clone(),
        MediaProbeJob {
            item: job.item,
            media_path: pending_probe.media_path,
            probe_path: pending_probe.probe_path,
        },
        dummy_chapter_duration_seconds,
        ffmpeg_path,
        chapter_image_settings,
    )
    .await
    {
        Ok(probe) if probe.stream_probe_succeeded => {
            result.probe_completed = true;
            persist_completed_scan_state(&db, &scan_state, true).await;
        }
        Ok(_) => result.probe_failed = true,
        Err(error) => {
            result.probe_failed = true;
            tracing::warn!("ProbeProvider failed during item refresh: {error:#}");
        }
    }

    Ok(result)
}

async fn persist_completed_scan_state(
    db: &sea_orm::DatabaseConnection,
    scan_state: &(String, String, String, i64),
    cacheable: bool,
) {
    if cacheable
        && let Err(error) = upsert_scan_state(
            db,
            &scan_state.0,
            &scan_state.1,
            &scan_state.2,
            scan_state.3,
        )
        .await
    {
        tracing::warn!(
            "failed to persist scan fingerprint for {}: {error:#}",
            scan_state.0
        );
    }
}

async fn run_local_refresh_job(
    db: &sea_orm::DatabaseConnection,
    job: &mut ItemRefreshJob,
    force_probe: bool,
) -> anyhow::Result<LocalRefreshResult> {
    let cached_probe = match job.media_probe.as_ref() {
        Some(pending) => cached_media_probe_for_job(db, &job.item, pending, force_probe).await,
        None => None,
    };
    if let Some(cached_probe) = cached_probe.as_ref() {
        apply_cached_probe(&mut job.item, cached_probe, job.parsed_metadata.has_nfo);
    }
    if job.item.item_type == "Photo" {
        let photo_path = job.source_path.clone();
        if let Ok((dimensions, metadata)) = tokio::task::spawn_blocking(move || {
            (
                image::image_dimensions(&photo_path),
                crate::library::photo::read_photo_metadata(&photo_path),
            )
        })
        .await
        {
            if let Ok((width, height)) = dimensions {
                job.item.width = Some(i64::from(width));
                job.item.height = Some(i64::from(height));
            }
            if let Some(date_taken) = metadata.date_taken_unix
                && let Some(date) = chrono::DateTime::from_timestamp(date_taken, 0)
            {
                job.item.created_at = date_taken;
                job.item.premiere_date = Some(date.format("%Y-%m-%d").to_string());
                job.item.production_year = Some(i64::from(date.year()));
            }
            if job.item.overview.is_none() {
                job.item.overview = metadata.overview.clone();
            }
            job.item.photo_metadata = metadata.to_storage();
        }
    }

    let stored_item_id = upsert_media_item(db, &job.item).await?;
    job.item.id = stored_item_id.clone();
    upsert_media_metadata(db, &job.item.id, &job.parsed_metadata).await?;
    if job.clear_folder_metadata {
        clear_scraped_folder_metadata(db, &job.item.id).await;
    }
    upsert_sidecar_images(db, &job.source_path, &job.item.id).await?;
    if job.item.item_type == "Photo" {
        let size_bytes = tokio::fs::metadata(&job.source_path)
            .await
            .ok()
            .and_then(|metadata| i64::try_from(metadata.len()).ok());
        upsert_image_asset(
            db,
            &job.item.id,
            "Primary",
            0,
            job.source_path.to_string_lossy().as_ref(),
            size_bytes,
        )
        .await?;
    }
    upsert_nfo_images(db, &job.item.id, &job.parsed_metadata.images).await?;

    let mut result = LocalRefreshResult {
        stored_item_id,
        probe_skipped: false,
        scan_state_cacheable: cached_probe
            .as_ref()
            .is_none_or(|probe| !probe.probe_failed),
    };

    let Some(_pending_probe) = job.media_probe.as_ref() else {
        return Ok(result);
    };

    if cached_probe.is_some() {
        result.probe_skipped = true;
        refresh_sidecar_streams(
            db,
            &job.source_path,
            &job.item.id,
            !matches!(job.item.item_type.as_str(), "Audio" | "AudioBook"),
        )
        .await?;
        return Ok(result);
    }

    upsert_default_media_stream(db, &job.item).await?;
    upsert_sidecar_subtitles(db, &job.source_path, &job.item.id).await?;

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
        pending_probe.allow_size_mismatch,
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

fn apply_cached_probe(item: &mut ScannedMediaItem, cached_probe: &CachedMediaProbe, has_nfo: bool) {
    item.runtime_ticks = cached_probe.runtime_ticks;
    item.size_bytes = cached_probe.size_bytes.or(item.size_bytes);
    if !matches!(item.item_type.as_str(), "Audio" | "AudioBook") {
        return;
    }
    if !has_nfo && !metadata_field_locked(item, "Name") {
        item.title.clone_from(&cached_probe.title);
    }
    if item.overview.is_none() {
        item.overview.clone_from(&cached_probe.overview);
    }
    if item.forced_sort_name.is_none() {
        item.forced_sort_name
            .clone_from(&cached_probe.forced_sort_name);
    }
    if item.collection_name.is_none() {
        item.collection_name
            .clone_from(&cached_probe.collection_name);
    }
    if item.production_year.is_none() {
        item.production_year = cached_probe.production_year;
    }
    if item.premiere_date.is_none() {
        item.premiere_date.clone_from(&cached_probe.premiere_date);
    }
    if item.episode_number.is_none() {
        item.episode_number = cached_probe.index_number;
    }
    if item.season_number.is_none() {
        item.season_number = cached_probe.parent_index_number;
    }
    if item.series_name.is_none() {
        item.series_name.clone_from(&cached_probe.series_name);
    }
}

async fn run_metadata_fetch_job(
    db: sea_orm::DatabaseConnection,
    job: &MetadataFetchJob,
    api_key: &str,
    tmdb_client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    provider_options: &crate::library::tmdb_metadata::TmdbLibraryProviderOptions,
) -> anyhow::Result<bool> {
    if api_key.is_empty() {
        return Ok(true);
    }
    if metadata_item_is_locked(&db, &job.item_id).await {
        return Ok(true);
    }
    let Some(policy) =
        provider_options.automatic_policy(&job.item_type, job.preserve_existing_metadata)
    else {
        return Ok(true);
    };

    let metadata_ready = match job.item_type.as_str() {
        "Movie" | "Series" => {
            if tmdb_metadata_is_current(&db, &job.item_id, policy).await {
                return Ok(true);
            }
            crate::library::tmdb_metadata::fetch_and_apply_tmdb_metadata(
                &db,
                &job.item_id,
                &job.item_type,
                &job.path,
                api_key,
                tmdb_client,
                tmdb_base_url,
                policy,
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
                policy,
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
                policy,
            )
            .await?
        }
        _ => true,
    };

    if !metadata_ready && provider_options.metadata_enabled("Series") {
        tracing::debug!(
            "metadata dependency is not ready for {} {}; deferring to the post-scan hierarchy refresh",
            job.item_type,
            job.item_id
        );
    }
    Ok(metadata_ready)
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

    run_provider_reconciliation(db).await;
}

pub(crate) fn is_ignored_scan_path(path: &std::path::Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == std::ffi::OsStr::new(".oani-download-tmp"))
}

async fn run_provider_reconciliation(db: &sea_orm::DatabaseConnection) {
    match crate::library::reconcile::reconcile_provider_duplicates(db).await {
        Ok(stats) if stats.changed() => tracing::info!(
            "provider reconciliation merged {} series and {} movie version(s); normalized {} series date range(s), {} dimension year(s), and {} episode metadata row(s)",
            stats.merged_series,
            stats.merged_versions,
            stats.normalized_series_dates,
            stats.normalized_dimension_years,
            stats.normalized_episode_metadata,
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!("provider reconciliation failed: {error:#}"),
    }
}

async fn run_media_probe_job(
    db: sea_orm::DatabaseConnection,
    mut job: MediaProbeJob,
    dummy_chapter_duration_seconds: i64,
    ffmpeg_path: &str,
    chapter_image_settings: &crate::chapters::ChapterImageScanSettings,
) -> anyhow::Result<MediaProbeJobResult> {
    let probe_path = job.probe_path.clone();
    let video_type = job.item.video_type.clone();
    let iso_type = job.item.iso_type.clone();
    let probe = tokio::task::spawn_blocking(move || {
        probe_video_media(&probe_path, video_type.as_deref(), iso_type.as_deref())
    })
    .await
    .context("media probe task join failed")?;
    let mut stream_probe_succeeded = false;

    if let Some(mut probe) = probe {
        apply_jellyfin_chapter_policy(&mut probe, dummy_chapter_duration_seconds)?;
        job.item.runtime_ticks = probe.runtime_ticks;
        job.item.size_bytes = probe.size_bytes.or(job.item.size_bytes);
        job.item.container = job.item.container.or(probe.container.clone());
        job.item.video_3d_format = job.item.video_3d_format.or(probe.video_3d_format.clone());
        if let Some(video) = probe
            .streams
            .iter()
            .find(|stream| stream.stream_type == "Video")
        {
            job.item.aspect_ratio = video.aspect_ratio.clone().or(job.item.aspect_ratio);
            job.item.width = video.width.or(job.item.width);
            job.item.height = video.height.or(job.item.height);
        }
        job.item.has_subtitles = probe
            .streams
            .iter()
            .any(|stream| stream.stream_type == "Subtitle");
        let probed_metadata = apply_probed_audio_metadata(&mut job.item, &probe.audio_metadata);
        if matches!(job.item.item_type.as_str(), "Audio" | "AudioBook")
            && job.item.lock_data != Some(true)
            && let Err(error) =
                save_embedded_audio_lyrics(&job.media_path, &probe.audio_metadata).await
        {
            tracing::warn!(
                "failed to save embedded audio lyrics for {}: {error:#}",
                job.item.path
            );
        }
        upsert_media_item(&db, &job.item).await?;
        upsert_probed_audio_metadata(&db, &job.item.id, &probed_metadata).await?;
        clear_sidecar_subtitles(&db, &job.item.id).await?;
        clear_sidecar_audio(&db, &job.item.id).await?;
        stream_probe_succeeded = match upsert_probed_media_streams(&db, &job.item, &probe).await {
            Ok(succeeded) => succeeded,
            Err(error) => {
                let _ = refresh_sidecar_streams(
                    &db,
                    &job.media_path,
                    &job.item.id,
                    !matches!(job.item.item_type.as_str(), "Audio" | "AudioBook"),
                )
                .await;
                return Err(error);
            }
        };
        if matches!(job.item.item_type.as_str(), "Audio" | "AudioBook")
            && let Err(error) = extract_embedded_audio_image(
                &db,
                ffmpeg_path,
                &job.media_path,
                &job.item.id,
                &probe.streams,
            )
            .await
        {
            tracing::warn!(
                "failed to extract embedded audio image for {}: {error:#}",
                job.item.path
            );
        }
        let chapters = probe
            .chapters
            .iter()
            .map(|chapter| crate::chapters::ChapterInfo {
                id: String::new(),
                item_id: job.item.id.clone(),
                start_position_ticks: chapter.start_position_ticks,
                name: chapter.name.clone(),
                marker_type: None,
                source: "ffprobe".to_string(),
                image_path: None,
                image_date_modified: None,
            })
            .collect::<Vec<_>>();
        crate::chapters::save_source_chapters(&db, &job.item.id, "ffprobe", &chapters).await?;
        let has_video_stream = probe
            .streams
            .iter()
            .any(|stream| stream.stream_type == "Video");
        let is_shortcut = std::path::Path::new(&job.item.path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"));
        if let Err(error) = crate::chapters::refresh_chapter_images(
            &db,
            ffmpeg_path,
            &job.item.id,
            &job.item.library_id,
            &job.media_path,
            job.item.video_type.as_deref(),
            job.item.iso_type.as_deref(),
            job.item.modified_at,
            job.item.runtime_ticks.unwrap_or_default(),
            has_video_stream,
            is_shortcut,
            true,
            chapter_image_settings,
        )
        .await
        {
            tracing::warn!(
                "failed to refresh chapter images for {}: {error:#}",
                job.item.path
            );
        }
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

    refresh_sidecar_streams(
        &db,
        &job.media_path,
        &job.item.id,
        !matches!(job.item.item_type.as_str(), "Audio" | "AudioBook"),
    )
    .await?;
    Ok(MediaProbeJobResult {
        stream_probe_succeeded,
    })
}

fn apply_probed_audio_metadata(
    item: &mut ScannedMediaItem,
    audio: &ProbedAudioMetadata,
) -> ParsedMetadata {
    if !matches!(item.item_type.as_str(), "Audio" | "AudioBook") || item.lock_data == Some(true) {
        return ParsedMetadata::default();
    }

    if !metadata_field_locked(item, "Name")
        && let Some(title) = audio.title.as_ref()
    {
        item.title.clone_from(title);
    }
    if item.forced_sort_name.is_none() {
        item.forced_sort_name.clone_from(&audio.forced_sort_name);
    }
    if item.collection_name.is_none() {
        item.collection_name.clone_from(&audio.album);
    }
    if item.production_year.is_none() {
        item.production_year = audio.production_year;
    }
    if item.premiere_date.is_none() {
        item.premiere_date.clone_from(&audio.premiere_date);
    }
    if item.episode_number.is_none() {
        item.episode_number = audio.index_number;
    }
    if item.season_number.is_none() {
        item.season_number = audio.parent_index_number;
    }
    if item.item_type == "AudioBook" {
        if !metadata_field_locked(item, "Overview") && item.overview.is_none() {
            item.overview.clone_from(&audio.overview);
        }
        if item.series_name.is_none() {
            item.series_name.clone_from(&audio.series_name);
        }
    }

    let mut metadata = ParsedMetadata {
        provider_ids: audio.provider_ids.clone(),
        genres: (!metadata_field_locked(item, "Genres"))
            .then(|| audio.genres.clone())
            .unwrap_or_default(),
        studios: (!metadata_field_locked(item, "Studios"))
            .then(|| audio.studios.clone())
            .unwrap_or_default(),
        ..Default::default()
    };
    if !metadata_field_locked(item, "Cast") {
        metadata.people = probed_audio_people(audio, item.item_type == "AudioBook");
    }
    metadata
}

async fn save_embedded_audio_lyrics(
    media_path: &std::path::Path,
    audio: &ProbedAudioMetadata,
) -> anyhow::Result<bool> {
    let Some(lyrics) = audio
        .lyrics
        .as_deref()
        .map(str::trim)
        .filter(|lyrics| !lyrics.is_empty())
    else {
        return Ok(false);
    };
    if !media_path.is_file()
        || media_path.with_extension("lrc").is_file()
        || media_path.with_extension("txt").is_file()
    {
        return Ok(false);
    }
    let lyric_path = media_path.with_extension("lrc");
    tokio::fs::write(&lyric_path, lyrics.as_bytes())
        .await
        .with_context(|| format!("failed to write embedded lyrics: {}", lyric_path.display()))?;
    Ok(true)
}

fn probed_audio_people(audio: &ProbedAudioMetadata, is_audiobook: bool) -> Vec<ParsedPerson> {
    let mut people = Vec::new();
    if is_audiobook {
        let authors = if audio.album_artists.is_empty() {
            &audio.artists
        } else {
            &audio.album_artists
        };
        for name in authors {
            push_probed_person(&mut people, name, "Author", None);
        }
        for name in audio.narrators.iter().chain(audio.composers.iter()) {
            push_probed_person(&mut people, name, "Narrator", None);
        }
        for name in &audio.illustrators {
            push_probed_person(&mut people, name, "Illustrator", None);
        }
        for name in &audio.artists {
            if !authors
                .iter()
                .any(|author| author.eq_ignore_ascii_case(name))
            {
                push_probed_person(&mut people, name, "Actor", None);
            }
        }
        return people;
    }

    for name in &audio.album_artists {
        push_probed_person(&mut people, name, "AlbumArtist", None);
    }
    for name in &audio.artists {
        push_probed_person(&mut people, name, "Artist", None);
    }
    for (names, person_type) in [
        (&audio.composers, "Composer"),
        (&audio.conductors, "Conductor"),
        (&audio.lyricists, "Lyricist"),
        (&audio.writers, "Writer"),
        (&audio.arrangers, "Arranger"),
        (&audio.engineers, "Engineer"),
        (&audio.mixers, "Mixer"),
        (&audio.remixers, "Remixer"),
    ] {
        for name in names {
            push_probed_person(&mut people, name, person_type, None);
        }
    }
    people
}

fn push_probed_person(
    people: &mut Vec<ParsedPerson>,
    name: &str,
    person_type: &str,
    role: Option<&str>,
) {
    let name = name.trim();
    if name.is_empty()
        || people.iter().any(|person| {
            person.name.eq_ignore_ascii_case(name)
                && person.person_type.eq_ignore_ascii_case(person_type)
        })
    {
        return;
    }
    people.push(ParsedPerson {
        name: name.to_string(),
        role: role.map(ToString::to_string),
        person_type: person_type.to_string(),
    });
}

fn metadata_field_locked(item: &ScannedMediaItem, field: &str) -> bool {
    item.locked_fields
        .iter()
        .any(|locked| locked.eq_ignore_ascii_case(field))
}

async fn refresh_sidecar_streams(
    db: &sea_orm::DatabaseConnection,
    media_path: &std::path::Path,
    item_id: &str,
    include_external_audio: bool,
) -> anyhow::Result<()> {
    clear_sidecar_subtitles(db, item_id).await?;
    if include_external_audio {
        upsert_sidecar_audio(db, media_path, item_id).await?;
    } else {
        clear_sidecar_audio(db, item_id).await?;
    }
    upsert_sidecar_subtitles(db, media_path, item_id).await?;
    refresh_external_lyric_stream(db, media_path, item_id).await
}

async fn configured_dummy_chapter_duration_seconds(db: &sea_orm::DatabaseConnection) -> i64 {
    crate::db::settings::get(db, "server_config")
        .await
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .and_then(|value| value.get("DummyChapterDuration")?.as_i64())
        .filter(|seconds| *seconds > 0 && *seconds <= 12 * 60 * 60)
        .unwrap_or_default()
}

fn apply_jellyfin_chapter_policy(
    probe: &mut crate::library::probe::MediaProbe,
    dummy_chapter_duration_seconds: i64,
) -> anyhow::Result<()> {
    if dummy_chapter_duration_seconds > 0
        && probe.chapters.len() <= 1
        && probe
            .streams
            .iter()
            .any(|stream| stream.stream_type == "Video")
    {
        let runtime_ticks = probe.runtime_ticks.unwrap_or_default();
        if !(0..=12 * 60 * 60 * 10_000_000).contains(&runtime_ticks) {
            anyhow::bail!("media has an invalid runtime for dummy chapter generation");
        }
        if runtime_ticks > 0 {
            let chapter_duration_ticks = dummy_chapter_duration_seconds.saturating_mul(10_000_000);
            let chapter_count = (runtime_ticks / chapter_duration_ticks).max(1);
            probe.chapters = (0..chapter_count)
                .map(|index| crate::library::probe::ProbedChapter {
                    start_position_ticks: index.saturating_mul(chapter_duration_ticks),
                    name: String::new(),
                })
                .collect();
        } else {
            probe.chapters.clear();
        }
    }

    for (index, chapter) in probe.chapters.iter_mut().enumerate() {
        if chapter.name.trim().is_empty() || chapter_name_is_time(&chapter.name) {
            chapter.name = format!("Chapter {}", index + 1);
        }
    }
    Ok(())
}

fn chapter_name_is_time(name: &str) -> bool {
    let name = name.trim();
    let mut parts = name.split(':');
    let Some(hours) = parts.next() else {
        return false;
    };
    let Some(minutes) = parts.next() else {
        return false;
    };
    let Some(seconds) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || hours.parse::<u64>().is_err() || minutes.parse::<u64>().is_err() {
        return false;
    }
    let seconds = seconds.split_once('.').map_or(seconds, |(whole, _)| whole);
    seconds.parse::<u64>().is_ok()
}

fn normalize_scanned_file_type(
    item_type: &str,
    collection_type: &str,
    parent_id: &str,
    library_id: &str,
    path: &std::path::Path,
    root: &std::path::Path,
    is_extra: bool,
) -> String {
    if is_extra {
        return item_type.to_string();
    }
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

fn resolve_episode_season_number(
    path: &std::path::Path,
    root: &std::path::Path,
    collection_type: &str,
    metadata_season: Option<i64>,
    filename_season: Option<i64>,
) -> Option<i64> {
    let archive_season = episode_season_number_from_path(path, root, collection_type);
    if is_jojo_steel_ball_run_archive(path, root) {
        metadata_season.or(archive_season).or(filename_season)
    } else {
        metadata_season.or(filename_season).or(archive_season)
    }
}

fn is_jojo_steel_ball_run_archive(path: &std::path::Path, root: &std::path::Path) -> bool {
    const SERIES_FOLDER: &str = "JOJO的奇妙冒险 (2012)";

    if !path.starts_with(root) {
        return false;
    }
    let mut current = path.parent();
    while let Some(season_folder) = current {
        let is_target_season = season_folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("Season 6"));
        let is_target_series = season_folder
            .parent()
            .and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == SERIES_FOLDER);
        if is_target_season && is_target_series {
            return true;
        }
        if same_normalized_path(season_folder, root) {
            break;
        }
        current = season_folder.parent();
    }
    false
}

fn same_normalized_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    path_utils::normalize_path(&left.to_string_lossy())
        == path_utils::normalize_path(&right.to_string_lossy())
}

fn metadata_fetch_job(
    item: &ScannedMediaItem,
    path: &std::path::Path,
    preserve_existing_metadata: bool,
) -> Option<MetadataFetchJob> {
    match item.item_type.as_str() {
        "Movie" | "Series" | "Season" | "Episode" => {}
        _ => return None,
    }
    Some(MetadataFetchJob {
        item_id: item.id.clone(),
        item_type: item.item_type.clone(),
        path: path.to_path_buf(),
        preserve_existing_metadata,
    })
}

async fn metadata_item_is_locked(db: &sea_orm::DatabaseConnection, item_id: &str) -> bool {
    MediaItems::find_by_id(item_id.to_string())
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some_and(|item| item.lock_data != 0)
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
            active.custom_rating = Set(None);
            active.original_title = Set(None);
            active.sort_name = Set(None);
            active.forced_sort_name = Set(None);
            active.lock_data = Set(0);
            active.locked_fields = Set(None);
            active.tagline = Set(None);
            active.collection_name = Set(None);
            active.original_language = Set(None);
            active.preferred_metadata_language = Set(None);
            active.preferred_metadata_country_code = Set(None);
            active.series_status = Set(None);
            active.air_days = Set(None);
            active.air_time = Set(None);
            active.home_page_url = Set(None);
            active.remote_trailers = Set(None);
            active.production_locations = Set(None);
            active.production_year = Set(None);
            active.premiere_date = Set(None);
            active.end_date = Set(None);
            active.community_rating = Set(None);
            active.critic_rating = Set(None);
            active.runtime_ticks = Set(None);
            active.aspect_ratio = Set(None);
            active.width = Set(None);
            active.height = Set(None);
            active.has_subtitles = Set(0);
            active.display_order = Set(None);
            active.airs_before_episode_number = Set(None);
            active.airs_after_season_number = Set(None);
            active.airs_before_season_number = Set(None);
            active.series_name = Set(None);
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

async fn tmdb_metadata_is_current(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    policy: crate::library::tmdb_metadata::MetadataRefreshPolicy,
) -> bool {
    let item = match MediaItems::find_by_id(item_id.to_string()).one(db).await {
        Ok(Some(item)) => item,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!("failed to read item for TMDb metadata state {item_id}: {error:#}");
            return false;
        }
    };
    if policy.refresh_metadata
        && item.tmdb_metadata_version != crate::library::tmdb_metadata::TMDB_METADATA_VERSION
    {
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
    if !policy.refresh_images {
        return true;
    }
    crate::entities::image_assets::Entity::find()
        .filter(crate::entities::image_assets::Column::ItemId.eq(item_id))
        .filter(crate::entities::image_assets::Column::ImageType.eq("Primary"))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn media_roots(
    state: &AppState,
    selected_library_ids: Option<&HashSet<String>>,
) -> anyhow::Result<Vec<(PathBuf, String, String, bool)>> {
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
    let library_options = crate::library::options::load_runtime_options(&state.db).await?;

    Ok(paths
        .into_iter()
        .filter_map(|path| {
            if selected_library_ids.is_some_and(|selected| !selected.contains(&path.library_id)) {
                return None;
            }
            libraries.get(&path.library_id).map(|library| {
                let options = library_options
                    .get(&library.id)
                    .cloned()
                    .unwrap_or_default();
                (
                    PathBuf::from(path_utils::normalize_path(&path.path)),
                    path.library_id,
                    library.collection_type.clone(),
                    options,
                )
            })
        })
        .filter(|(_, _, _, options)| options.enabled)
        .map(|(path, library_id, collection_type, options)| {
            (path, library_id, collection_type, options.enable_photos)
        })
        .collect())
}

type MediaRoot = (PathBuf, String, String, bool);
type MediaScanRoot = (PathBuf, PathBuf, String, String, bool);

fn targeted_scan_roots(roots: Vec<MediaRoot>, changed_paths: &[PathBuf]) -> Vec<MediaScanRoot> {
    let mut targets = Vec::<MediaScanRoot>::new();
    for changed_path in changed_paths {
        let changed_path =
            PathBuf::from(path_utils::normalize_path(&changed_path.to_string_lossy()));
        let Some((root, library_id, collection_type, enable_photos)) = roots
            .iter()
            .filter(|(root, _, _, _)| changed_path.starts_with(root))
            .max_by_key(|(root, _, _, _)| root.components().count())
        else {
            continue;
        };
        let Some(scan_path) = targeted_scan_path(&changed_path, root, collection_type) else {
            continue;
        };

        if targets
            .iter()
            .any(|(existing_root, existing_path, existing_library, _, _)| {
                existing_root == root
                    && existing_library == library_id
                    && scan_path.starts_with(existing_path)
            })
        {
            continue;
        }
        targets.retain(|(existing_root, existing_path, existing_library, _, _)| {
            existing_root != root
                || existing_library != library_id
                || !existing_path.starts_with(&scan_path)
        });
        targets.push((
            root.clone(),
            scan_path,
            library_id.clone(),
            collection_type.clone(),
            *enable_photos,
        ));
    }
    targets.sort_by(|left, right| left.1.cmp(&right.1));
    targets
}

fn targeted_scan_path(
    changed_path: &std::path::Path,
    library_root: &std::path::Path,
    collection_type: &str,
) -> Option<PathBuf> {
    if changed_path.is_dir()
        || classify_media_path(changed_path, collection_type).is_some()
        || strm::is_strm_path(changed_path)
    {
        return Some(changed_path.to_path_buf());
    }
    if is_metadata_sidecar_path(changed_path) {
        return changed_path
            .parent()
            .filter(|parent| parent.starts_with(library_root))
            .map(std::path::Path::to_path_buf);
    }
    if !changed_path.exists() {
        return Some(changed_path.to_path_buf());
    }
    None
}

fn is_metadata_sidecar_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "nfo"
                    | "xml"
                    | "jpg"
                    | "jpeg"
                    | "png"
                    | "webp"
                    | "srt"
                    | "ass"
                    | "ssa"
                    | "sub"
                    | "vtt"
                    | "lrc"
                    | "cue"
            )
        })
        .unwrap_or(false)
}

async fn load_scan_states(
    db: &sea_orm::DatabaseConnection,
    library_ids: &HashSet<String>,
) -> anyhow::Result<HashMap<String, i64>> {
    if library_ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(MediaScanStates::find()
        .filter(media_scan_states::Column::LibraryId.is_in(library_ids.iter().cloned()))
        .all(db)
        .await?
        .into_iter()
        .map(|state| (state.path, state.fingerprint))
        .collect())
}

async fn scan_context_fingerprints(
    db: &sea_orm::DatabaseConnection,
    library_ids: &HashSet<String>,
    tmdb_configured: bool,
) -> anyhow::Result<HashMap<String, i64>> {
    let options = crate::library::options::raw_options(db).await?;
    let preferred_language = crate::db::settings::get(db, "PreferredMetadataLanguage").await?;
    let metadata_country = crate::db::settings::get(db, "MetadataCountryCode").await?;
    Ok(library_ids
        .iter()
        .map(|library_id| {
            let mut hasher = DefaultHasher::new();
            SCAN_FINGERPRINT_VERSION.hash(&mut hasher);
            MEDIA_PROBE_CACHE_VERSION.hash(&mut hasher);
            tmdb_configured.hash(&mut hasher);
            preferred_language.hash(&mut hasher);
            metadata_country.hash(&mut hasher);
            options
                .get(library_id)
                .map(ToString::to_string)
                .hash(&mut hasher);
            (library_id.clone(), hasher.finish() as i64)
        })
        .collect())
}

fn media_scan_fingerprint(
    path: &std::path::Path,
    modified_at: i64,
    size_bytes: Option<i64>,
    scan_context: i64,
    sidecar_fingerprints: &mut SidecarFingerprintCache,
) -> i64 {
    let mut hasher = DefaultHasher::new();
    SCAN_FINGERPRINT_VERSION.hash(&mut hasher);
    scan_context.hash(&mut hasher);
    modified_at.hash(&mut hasher);
    size_bytes.hash(&mut hasher);
    sidecar_fingerprints.for_media_path(path).hash(&mut hasher);
    hasher.finish() as i64
}

fn sidecar_directory_fingerprint(directory: &std::path::Path) -> u64 {
    let mut sidecars = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || !is_scan_sidecar(&path) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            Some((entry.file_name(), metadata.len(), modified))
        })
        .collect::<Vec<_>>();
    sidecars.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = DefaultHasher::new();
    sidecars.hash(&mut hasher);
    hasher.finish()
}

fn is_scan_sidecar(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "nfo"
                    | "jpg"
                    | "jpeg"
                    | "png"
                    | "webp"
                    | "srt"
                    | "ass"
                    | "ssa"
                    | "vtt"
                    | "sub"
                    | "smi"
                    | "sami"
                    | "mpl"
                    | "lrc"
                    | "mka"
            )
        })
}

async fn upsert_scan_state(
    db: &sea_orm::DatabaseConnection,
    path: &str,
    item_id: &str,
    library_id: &str,
    fingerprint: i64,
) -> anyhow::Result<()> {
    MediaScanStates::insert(media_scan_states::ActiveModel {
        path: Set(path.to_string()),
        item_id: Set(item_id.to_string()),
        library_id: Set(library_id.to_string()),
        fingerprint: Set(fingerprint),
        updated_at: Set(crate::util::now_unix()),
    })
    .on_conflict(
        OnConflict::column(media_scan_states::Column::Path)
            .update_columns([
                media_scan_states::Column::ItemId,
                media_scan_states::Column::LibraryId,
                media_scan_states::Column::Fingerprint,
                media_scan_states::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec(db)
    .await?;
    Ok(())
}

fn scan_root_concurrency() -> usize {
    configured_concurrency(
        "JELLYFIN_RS_SCAN_ROOT_CONCURRENCY",
        crate::db::cpu_parallelism().clamp(1, 2),
    )
}

fn item_refresh_concurrency() -> usize {
    configured_concurrency(
        "JELLYFIN_RS_INGEST_CONCURRENCY",
        crate::db::cpu_parallelism().clamp(1, 2),
    )
}

fn configured_concurrency(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 4))
        .unwrap_or(default)
}

fn item_refresh_queue_capacity() -> usize {
    crate::db::cpu_parallelism().saturating_mul(512).clamp(
        MIN_ITEM_REFRESH_QUEUE_CAPACITY,
        MAX_ITEM_REFRESH_QUEUE_CAPACITY,
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
    use crate::entities::{
        image_assets::{self, Entity as ImageAssets},
        libraries,
        media_streams::Entity as MediaStreams,
    };
    use sea_orm::{PaginatorTrait, sea_query::OnConflict};
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
    fn mixed_movie_stack_parts_are_owned_by_the_primary_file() {
        let root = test_dir("mixed_movie_stack_parts_are_owned_by_the_primary_file");
        let cd1 = root.join("Movie (2024) CD1.mkv");
        let cd2 = root.join("Movie (2024) CD2.mkv");
        fs::write(&cd1, []).unwrap();
        fs::write(&cd2, []).unwrap();

        let owners = movie_stack_part_owners(
            [cd1.as_path(), cd2.as_path()],
            &root,
            "movies",
            &CancellationToken::new(),
        );
        assert_eq!(owners.get(&cd2), Some(&cd1));
        assert!(!owners.contains_key(&cd1));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn mixed_movie_stack_scan_creates_one_movie_and_hidden_parts() {
        let root = test_dir("mixed_movie_stack_scan_creates_one_movie_and_hidden_parts");
        let cd1 = root.join("Movie (2024) CD1.mkv");
        let cd2 = root.join("Movie (2024) CD2.mkv");
        fs::write(&cd1, []).unwrap();
        fs::write(&cd2, []).unwrap();

        let jobs = scan_jobs(&root, "movies", "movies").await;
        let movie = jobs
            .iter()
            .find(|job| job.item.path == cd1.to_string_lossy())
            .unwrap();
        let part = jobs
            .iter()
            .find(|job| job.item.path == cd2.to_string_lossy())
            .unwrap();
        assert_eq!(movie.item.item_type, "Movie");
        assert_eq!(part.item.item_type, "Video");
        assert_eq!(part.item.parent_id, movie.item.id);

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn episode_versions_scan_as_one_episode_with_hidden_video_sources() {
        let root = test_dir("episode_versions_scan_as_one_episode_with_hidden_video_sources");
        let season = root.join("Show/Season 1");
        fs::create_dir_all(&season).unwrap();
        let high = season.join("Show S01E01 2160p.mkv");
        let low = season.join("Show S01E01 1080p.mkv");
        fs::write(&high, []).unwrap();
        fs::write(&low, []).unwrap();

        let jobs = scan_jobs(&root, "tv", "tvshows").await;
        let primary = jobs
            .iter()
            .find(|job| job.item.path == high.to_string_lossy())
            .unwrap();
        let alternate = jobs
            .iter()
            .find(|job| job.item.path == low.to_string_lossy())
            .unwrap();
        assert_eq!(primary.item.item_type, "Episode");
        assert_eq!(alternate.item.item_type, "Video");
        assert_eq!(alternate.item.parent_id, primary.item.id);

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn dummy_chapter_duration_reads_jellyfin_server_configuration() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        crate::db::settings::set(&db, "server_config", r#"{"DummyChapterDuration":300}"#)
            .await
            .unwrap();

        assert_eq!(configured_dummy_chapter_duration_seconds(&db).await, 300);
    }

    #[test]
    fn dummy_chapters_follow_jellyfin_runtime_and_naming_rules() {
        let mut probe = crate::library::probe::MediaProbe {
            runtime_ticks: Some(16 * 60 * 10_000_000),
            streams: vec![crate::library::probe::ProbedStream {
                stream_type: "Video".to_string(),
                ..Default::default()
            }],
            chapters: vec![crate::library::probe::ProbedChapter {
                start_position_ticks: 0,
                name: "00:00:00.000".to_string(),
            }],
            ..Default::default()
        };

        apply_jellyfin_chapter_policy(&mut probe, 5 * 60).unwrap();

        assert_eq!(probe.chapters.len(), 3);
        assert_eq!(probe.chapters[0].start_position_ticks, 0);
        assert_eq!(probe.chapters[1].start_position_ticks, 3_000_000_000);
        assert_eq!(probe.chapters[2].start_position_ticks, 6_000_000_000);
        assert_eq!(probe.chapters[0].name, "Chapter 1");
        assert_eq!(probe.chapters[2].name, "Chapter 3");
    }

    #[test]
    fn probed_music_tags_update_audio_item_and_people() {
        let mut item = ScannedMediaItem {
            title: "Filename Title".to_string(),
            item_type: "Audio".to_string(),
            ..Default::default()
        };
        let metadata = apply_probed_audio_metadata(
            &mut item,
            &ProbedAudioMetadata {
                title: Some("Tagged Title".to_string()),
                album: Some("Tagged Album".to_string()),
                index_number: Some(4),
                parent_index_number: Some(2),
                artists: vec!["Track Artist".to_string()],
                album_artists: vec!["Album Artist".to_string()],
                composers: vec!["Composer".to_string()],
                ..Default::default()
            },
        );

        assert_eq!(item.title, "Tagged Title");
        assert_eq!(item.collection_name.as_deref(), Some("Tagged Album"));
        assert_eq!(item.episode_number, Some(4));
        assert_eq!(item.season_number, Some(2));
        assert_eq!(metadata.people.len(), 3);
        assert_eq!(metadata.people[0].person_type, "AlbumArtist");
        assert_eq!(metadata.people[1].person_type, "Artist");
        assert_eq!(metadata.people[2].person_type, "Composer");
    }

    #[test]
    fn probed_audiobook_tags_follow_official_person_roles_and_locks() {
        let mut item = ScannedMediaItem {
            title: "NFO Title".to_string(),
            item_type: "AudioBook".to_string(),
            locked_fields: vec!["Name".to_string()],
            ..Default::default()
        };
        let metadata = apply_probed_audio_metadata(
            &mut item,
            &ProbedAudioMetadata {
                title: Some("Tagged Title".to_string()),
                album_artists: vec!["Author One".to_string()],
                artists: vec!["Author One".to_string(), "Cast One".to_string()],
                composers: vec!["Fallback Narrator".to_string()],
                narrators: vec!["Narrator One".to_string()],
                illustrators: vec!["Illustrator One".to_string()],
                ..Default::default()
            },
        );

        assert_eq!(item.title, "NFO Title");
        assert_eq!(
            metadata
                .people
                .iter()
                .map(|person| (person.name.as_str(), person.person_type.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Author One", "Author"),
                ("Narrator One", "Narrator"),
                ("Fallback Narrator", "Narrator"),
                ("Illustrator One", "Illustrator"),
                ("Cast One", "Actor"),
            ]
        );
    }

    #[test]
    fn existing_chapter_time_names_are_normalized_when_dummy_chapters_are_disabled() {
        let mut probe = crate::library::probe::MediaProbe {
            chapters: vec![
                crate::library::probe::ProbedChapter {
                    start_position_ticks: 0,
                    name: "01:02:03".to_string(),
                },
                crate::library::probe::ProbedChapter {
                    start_position_ticks: 10_000_000,
                    name: "Opening".to_string(),
                },
            ],
            ..Default::default()
        };

        apply_jellyfin_chapter_policy(&mut probe, 0).unwrap();

        assert_eq!(probe.chapters[0].name, "Chapter 1");
        assert_eq!(probe.chapters[1].name, "Opening");
    }

    #[test]
    fn scan_fingerprint_changes_when_sidecar_changes() {
        let root = test_dir("scan_fingerprint_changes_when_sidecar_changes");
        let media = root.join("Movie.mkv");
        let nfo = root.join("Movie.nfo");
        fs::write(&media, b"video").unwrap();
        fs::write(&nfo, b"<movie />").unwrap();

        let before = media_scan_fingerprint(
            &media,
            10,
            Some(5),
            20,
            &mut SidecarFingerprintCache::default(),
        );
        fs::write(&nfo, b"<movie><title>Changed</title></movie>").unwrap();
        let after = media_scan_fingerprint(
            &media,
            10,
            Some(5),
            20,
            &mut SidecarFingerprintCache::default(),
        );

        assert_ne!(before, after);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scan_fingerprint_reuses_sidecar_signature_within_a_directory() {
        let root = test_dir("scan_fingerprint_reuses_sidecar_signature_within_a_directory");
        let first = root.join("First.mkv");
        let second = root.join("Second.mkv");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::write(root.join("First.nfo"), b"<movie />").unwrap();
        let mut cache = SidecarFingerprintCache::default();

        media_scan_fingerprint(&first, 10, Some(5), 20, &mut cache);
        media_scan_fingerprint(&second, 11, Some(6), 20, &mut cache);

        assert_eq!(cache.by_directory.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_root_skips_matching_fingerprint_before_refresh() {
        let root = test_dir("scan_root_skips_matching_fingerprint_before_refresh");
        let media = root.join("Movie.mkv");
        fs::write(&media, b"video").unwrap();
        let resolved = path_utils::resolve_path_info(&media).unwrap();
        let fingerprint = media_scan_fingerprint(
            &media,
            resolved.modified_at,
            resolved.size_bytes,
            42,
            &mut SidecarFingerprintCache::default(),
        );
        let states = Arc::new(HashMap::from([(resolved.path, fingerprint)]));
        let (tx, mut rx) = mpsc::channel(8);

        let result = scan_root(
            root.clone(),
            root.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
            42,
            states,
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        assert_eq!(result.scanned, 1);
        assert_eq!(result.unchanged, 1);
        assert!(rx.recv().await.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_root_stops_before_traversal_when_cancelled() {
        let root = test_dir("scan_root_stops_before_traversal_when_cancelled");
        fs::write(root.join("Movie.mkv"), b"video").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = scan_root(
            root.clone(),
            root.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            cancellation,
            test_progress(),
        )
        .await
        .err()
        .expect("cancelled scan should return an error");

        assert!(is_scan_cancelled(&error));
        assert!(rx.recv().await.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn targeted_file_scan_does_not_queue_sibling_media() {
        let root = test_dir("targeted_file_scan_does_not_queue_sibling_media");
        let changed = root.join("Changed.mkv");
        let sibling = root.join("Sibling.mkv");
        fs::write(&changed, b"changed").unwrap();
        fs::write(&sibling, b"sibling").unwrap();
        let (tx, mut rx) = mpsc::channel(8);

        scan_root(
            root.clone(),
            changed.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        let mut paths = Vec::new();
        while let Some(job) = rx.recv().await {
            paths.push(job.item.path);
        }
        assert_eq!(
            paths,
            vec![path_utils::normalize_path(&changed.to_string_lossy())]
        );
        assert!(!paths.contains(&path_utils::normalize_path(&sibling.to_string_lossy())));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn targeted_roots_keep_media_files_narrow_and_expand_sidecars() {
        let root = test_dir("targeted_roots_keep_media_files_narrow_and_expand_sidecars");
        let media = root.join("Movie.mkv");
        let sidecar = root.join("Movie.nfo");
        let unrelated = root.join("notes.txt");
        fs::write(&media, b"video").unwrap();
        fs::write(&sidecar, b"<movie />").unwrap();
        fs::write(&unrelated, b"notes").unwrap();
        let roots = vec![(
            root.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
        )];

        let media_target = targeted_scan_roots(roots.clone(), std::slice::from_ref(&media));
        assert_eq!(media_target[0].1, media);
        let sidecar_target = targeted_scan_roots(roots.clone(), std::slice::from_ref(&sidecar));
        assert_eq!(sidecar_target[0].1, root);
        assert!(targeted_scan_roots(roots, &[unrelated]).is_empty());

        let _ = fs::remove_dir_all(sidecar_target[0].0.clone());
    }

    #[test]
    fn item_refresh_concurrency_defaults_to_positive_value() {
        assert!(item_refresh_concurrency() > 0);
    }

    #[test]
    fn scan_root_concurrency_defaults_to_positive_value() {
        assert!(scan_root_concurrency() > 0);
    }

    #[test]
    fn temporary_download_tree_is_ignored() {
        assert!(is_ignored_scan_path(
            PathBuf::from("/media/tv/.oani-download-tmp/job/movie.mkv").as_path()
        ));
        assert!(!is_ignored_scan_path(
            PathBuf::from("/media/tv/Show/Season 1/movie.strm").as_path()
        ));
    }

    #[test]
    fn item_refresh_queue_capacity_defaults_to_positive_value() {
        assert!(item_refresh_queue_capacity() > 0);
    }

    #[test]
    fn movie_library_root_video_file_is_movie() {
        let root = test_dir("movie_library_root_video_file_is_movie");
        let path = root.join("Movie One.mkv");
        fs::write(&path, []).unwrap();

        assert_eq!(
            normalize_scanned_file_type(
                "Video", "movies", "library", "library", &path, &root, false,
            ),
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
            normalize_scanned_file_type("Video", "movies", "group", "library", &path, &root, false),
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
            normalize_scanned_file_type("Video", "movies", "movie", "library", &path, &root, false),
            "Video"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn movie_library_extra_video_file_stays_video() {
        let root = test_dir("movie_library_extra_video_file_stays_video");
        let movie = root.join("Movie One");
        let extras = movie.join("extras");
        fs::create_dir_all(&extras).unwrap();
        let path = extras.join("Behind the Scenes.mkv");
        fs::write(&path, []).unwrap();

        assert_eq!(
            normalize_scanned_file_type("Video", "movies", "extras", "library", &path, &root, true),
            "Video"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_root_preserves_multi_episode_end_number() {
        let root = test_dir("scan_root_preserves_multi_episode_end_number");
        let season = root.join("Show").join("Season 01");
        fs::create_dir_all(&season).unwrap();
        fs::write(season.join("Show.S01E01-E03.mkv"), []).unwrap();
        let (tx, mut rx) = mpsc::channel(16);

        let result = scan_root(
            root.clone(),
            root.clone(),
            "tv".to_string(),
            "tvshows".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        assert_eq!(result.scanned, 1);
        let mut episode_item = None;
        while let Some(job) = rx.recv().await {
            if job.item.item_type == "Episode" {
                episode_item = Some(job.item);
                break;
            }
        }
        let episode_item = episode_item.expect("episode refresh job should be queued");
        assert_eq!(episode_item.season_number, Some(1));
        assert_eq!(episode_item.episode_number, Some(1));
        assert_eq!(episode_item.episode_number_end, Some(3));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn flat_tv_episode_without_season_defaults_to_season_one() {
        let root = test_dir("flat_tv_episode_without_season_defaults_to_season_one");
        let series = root.join("Show");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Show E01.mkv"), []).unwrap();
        let (tx, mut rx) = mpsc::channel(16);

        scan_root(
            root.clone(),
            root.clone(),
            "tv".to_string(),
            "tvshows".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        let mut episode = None;
        while let Some(job) = rx.recv().await {
            if job.item.item_type == "Episode" {
                episode = Some(job.item);
                break;
            }
        }
        let episode = episode.expect("flat episode should be queued");
        assert_eq!(episode.episode_number, Some(1));
        assert_eq!(episode.season_number, Some(1));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_root_applies_local_nfo_scalars_to_refresh_item() {
        let root = test_dir("scan_root_applies_local_nfo_scalars_to_refresh_item");
        let path = root.join("Movie One.mkv");
        fs::write(&path, []).unwrap();
        fs::write(
            root.join("Movie One.nfo"),
            r#"
            <movie>
                <title>NFO Movie One</title>
                <originaltitle>Original Movie One</originaltitle>
                <sortname>Movie One Sort</sortname>
                <sorttitle>Forced Movie One</sorttitle>
                <lockdata>true</lockdata>
                <lockedfields>Name|Overview</lockedfields>
                <mpaa>PG-13</mpaa>
                <customrating>TV-MA</customrating>
                <tagline>Trust no one.</tagline>
                <country>United States / Japan</country>
                <rating>8.4</rating>
                <criticrating>91</criticrating>
                <premiered>2024-07-02</premiered>
                <enddate>2024-08-03</enddate>
                <dateadded>2024-07-03 04:05:06</dateadded>
                <displayorder>dvd</displayorder>
                <runtime>90 min</runtime>
            </movie>
            "#,
        )
        .unwrap();
        let (tx, mut rx) = mpsc::channel(16);

        let result = scan_root(
            root.clone(),
            root.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        assert_eq!(result.scanned, 1);
        let mut movie_item = None;
        while let Some(job) = rx.recv().await {
            if job.item.item_type == "Movie" {
                movie_item = Some(job.item);
                break;
            }
        }
        let movie_item = movie_item.expect("movie refresh job should be queued");
        assert_eq!(movie_item.title, "NFO Movie One");
        assert_eq!(
            movie_item.original_title.as_deref(),
            Some("Original Movie One")
        );
        assert_eq!(movie_item.sort_name.as_deref(), Some("Movie One Sort"));
        assert_eq!(
            movie_item.forced_sort_name.as_deref(),
            Some("Forced Movie One")
        );
        assert_eq!(movie_item.lock_data, Some(true));
        assert_eq!(movie_item.locked_fields, ["Name", "Overview"]);
        assert_eq!(movie_item.official_rating.as_deref(), Some("PG-13"));
        assert_eq!(movie_item.custom_rating.as_deref(), Some("TV-MA"));
        assert_eq!(movie_item.tagline.as_deref(), Some("Trust no one."));
        assert_eq!(movie_item.production_locations, ["United States", "Japan"]);
        assert_eq!(movie_item.community_rating, Some(8.4));
        assert_eq!(movie_item.critic_rating, Some(91.0));
        assert_eq!(movie_item.premiere_date.as_deref(), Some("2024-07-02"));
        assert_eq!(movie_item.end_date.as_deref(), Some("2024-08-03"));
        assert_eq!(movie_item.production_year, Some(2024));
        assert_eq!(movie_item.display_order.as_deref(), Some("dvd"));
        assert_eq!(
            movie_item.created_at,
            chrono::NaiveDate::from_ymd_opt(2024, 7, 3)
                .unwrap()
                .and_hms_opt(4, 5, 6)
                .unwrap()
                .and_utc()
                .timestamp()
        );
        assert_eq!(movie_item.runtime_ticks, Some(54_000_000_000));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_root_resolves_dvd_directory_as_playable_video() {
        let root = test_dir("scan_root_resolves_dvd_directory_as_playable_video");
        let movie = root.join("Movie One");
        let video_ts = movie.join("VIDEO_TS");
        fs::create_dir_all(&video_ts).unwrap();
        fs::write(video_ts.join("VIDEO_TS.IFO"), []).unwrap();
        fs::write(video_ts.join("VTS_01_1.VOB"), []).unwrap();
        let (tx, mut rx) = mpsc::channel(16);

        scan_root(
            root.clone(),
            root.clone(),
            "movies".to_string(),
            "movies".to_string(),
            true,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();

        let mut disc_job = None;
        while let Some(job) = rx.recv().await {
            if job.item.path == movie.to_string_lossy() {
                disc_job = Some(job);
                break;
            }
        }
        let disc_job = disc_job.expect("DVD refresh job should be queued");
        assert_eq!(disc_job.item.item_type, "Movie");
        assert_eq!(disc_job.item.video_type.as_deref(), Some("Dvd"));
        assert!(!disc_job.item.is_folder);
        let media_probe = disc_job
            .media_probe
            .expect("DVD media probe should be queued");
        assert_eq!(media_probe.media_path, movie);
        assert!(media_probe.allow_size_mismatch);

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn specialized_library_items_persist_with_jellyfin_stream_and_image_semantics() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        let photo_root = test_dir("specialized_library_photo");
        let photo_path = photo_root.join("holiday.jpg");
        crate::library::photo::tests::write_test_photo(&photo_path);
        let mut photo_jobs = scan_jobs(&photo_root, "photos", "photos").await;
        let photo_job = photo_jobs
            .drain(..)
            .find(|job| job.item.item_type == "Photo")
            .expect("photo should be scanned");
        let photo_id = photo_job.item.id.clone();
        refresh_without_running_probe(&db, photo_job).await;
        let photo = MediaItems::find_by_id(&photo_id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((photo.width, photo.height), (Some(3), Some(2)));
        let photo_metadata =
            crate::library::photo::PhotoMetadata::from_storage(photo.photo_metadata.as_deref());
        assert_eq!(photo_metadata.camera_make.as_deref(), Some("OpenAI Camera"));
        assert_eq!(
            photo_metadata.image_orientation.as_deref(),
            Some("RightTop")
        );
        assert_eq!(photo_metadata.iso_speed_rating, Some(200));
        assert_eq!(photo.production_year, Some(2024));
        assert_eq!(photo.overview.as_deref(), Some("EXIF description"));
        assert_eq!(
            ImageAssets::find()
                .filter(image_assets::Column::ItemId.eq(&photo_id))
                .filter(image_assets::Column::ImageType.eq("Primary"))
                .count(&db)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            media_stream_types(&db, &photo_id).await,
            Vec::<String>::new()
        );
        Libraries::insert(libraries::ActiveModel {
            id: Set("photos".to_string()),
            name: Set("Photos".to_string()),
            collection_type: Set("photos".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .on_conflict(
            OnConflict::column(libraries::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(&db)
        .await
        .unwrap();
        let photo_dto = crate::jellyfin::routes::find_media_item_for_admin(&db, "", &photo_id)
            .await
            .unwrap()
            .unwrap()
            .to_jellyfin_json();
        assert_eq!(photo_dto["CameraMake"], "OpenAI Camera");
        assert_eq!(photo_dto["ImageOrientation"], "RightTop");
        assert_eq!(photo_dto["IsoSpeedRating"], 200);
        assert_eq!(photo_dto["PrimaryImageAspectRatio"], 2.0 / 3.0);

        let book_root = test_dir("specialized_library_book");
        let book_dir = book_root.join("2 - The Sign of the Four (1890)");
        let book_path = book_dir.join("content.epub");
        fs::create_dir_all(&book_dir).unwrap();
        fs::write(&book_path, []).unwrap();
        let book_jobs = scan_jobs(&book_root, "books", "books").await;
        assert!(
            !book_jobs
                .iter()
                .any(|job| job.item.path == book_dir.to_string_lossy())
        );
        let book_job = book_jobs
            .into_iter()
            .find(|job| job.item.item_type == "Book")
            .expect("single-file book directory should collapse to a Book");
        assert_eq!(book_job.item.path, book_path.to_string_lossy());
        assert_eq!(book_job.item.title, "The Sign of the Four");
        assert_eq!(book_job.item.episode_number, Some(2));
        assert_eq!(book_job.item.production_year, Some(1890));
        assert_eq!(book_job.item.parent_id, "books");
        let book_id = book_job.item.id.clone();
        refresh_without_running_probe(&db, book_job).await;
        assert_eq!(
            media_stream_types(&db, &book_id).await,
            Vec::<String>::new()
        );

        let audiobook_root = test_dir("specialized_library_audiobook");
        let audiobook_dir = audiobook_root.join("The Spoken Book (2024)");
        let audiobook_path = audiobook_dir.join("book.m4b");
        fs::create_dir_all(&audiobook_dir).unwrap();
        fs::write(&audiobook_path, []).unwrap();
        let audiobook_jobs = scan_jobs(&audiobook_root, "books-audio", "books").await;
        assert!(
            !audiobook_jobs
                .iter()
                .any(|job| job.item.path == audiobook_dir.to_string_lossy())
        );
        let audiobook_job = audiobook_jobs
            .into_iter()
            .find(|job| job.item.item_type == "AudioBook")
            .expect("single-file audiobook directory should collapse to an AudioBook");
        assert_eq!(audiobook_job.item.title, "The Spoken Book");
        assert_eq!(audiobook_job.item.production_year, Some(2024));
        let audiobook_id = audiobook_job.item.id.clone();
        refresh_without_running_probe(&db, audiobook_job).await;
        assert_eq!(media_stream_types(&db, &audiobook_id).await, ["Audio"]);

        let music_video_root = test_dir("specialized_library_music_video");
        let music_video_path = music_video_root.join("Song.mkv");
        fs::write(&music_video_path, []).unwrap();
        let music_video_job = scan_jobs(&music_video_root, "music-videos", "musicvideos")
            .await
            .into_iter()
            .find(|job| job.item.item_type == "MusicVideo")
            .expect("music video should be scanned");
        let music_video_id = music_video_job.item.id.clone();
        refresh_without_running_probe(&db, music_video_job).await;
        assert_eq!(media_stream_types(&db, &music_video_id).await, ["Video"]);

        for root in [photo_root, book_root, audiobook_root, music_video_root] {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn music_artist_album_and_multi_disc_tracks_have_jellyfin_parents() {
        let root = test_dir("music_artist_album_and_multi_disc_tracks_have_jellyfin_parents");
        let artist = root.join("Artist");
        let album = artist.join("Album");
        let disc = album.join("CD1");
        let track = disc.join("01 Song.flac");
        fs::create_dir_all(&disc).unwrap();
        fs::write(&track, []).unwrap();

        let jobs = scan_jobs(&root, "music", "music").await;
        let artist_job = jobs
            .iter()
            .find(|job| job.item.path == artist.to_string_lossy())
            .unwrap();
        let album_job = jobs
            .iter()
            .find(|job| job.item.path == album.to_string_lossy())
            .unwrap();
        let track_job = jobs
            .iter()
            .find(|job| job.item.path == track.to_string_lossy())
            .unwrap();

        assert_eq!(artist_job.item.item_type, "MusicArtist");
        assert_eq!(album_job.item.item_type, "MusicAlbum");
        assert_eq!(album_job.item.parent_id, artist_job.item.id);
        assert_eq!(track_job.item.item_type, "Audio");
        assert_eq!(track_job.item.parent_id, album_job.item.id);
        assert!(
            !jobs
                .iter()
                .any(|job| job.item.path == disc.to_string_lossy())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn home_video_photo_resolver_honors_enable_photos() {
        let root = test_dir("home_video_photo_resolver_honors_enable_photos");
        let album = root.join("Album");
        fs::create_dir_all(&album).unwrap();
        image::RgbImage::new(2, 2)
            .save(album.join("holiday.jpg"))
            .unwrap();
        fs::write(album.join("clip.mp4"), []).unwrap();

        let enabled = scan_jobs_with_photos(&root, "home", "homevideos", true).await;
        assert!(enabled.iter().any(|job| job.item.item_type == "Photo"));
        assert!(enabled.iter().any(|job| job.item.item_type == "PhotoAlbum"));

        let disabled = scan_jobs_with_photos(&root, "home", "homevideos", false).await;
        assert!(!disabled.iter().any(|job| job.item.item_type == "Photo"));
        assert!(
            !disabled
                .iter()
                .any(|job| job.item.item_type == "PhotoAlbum")
        );
        assert!(disabled.iter().any(|job| job.item.item_type == "Video"));

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

    #[test]
    fn archive_folder_overrides_filename_only_for_jojo_steel_ball_run() {
        let root = PathBuf::from("/media/迅雷-番");
        let path = root.join("JOJO的奇妙冒险 (2012)/Season 6/套马的汉子 - S01E04.(mp4).strm");

        assert_eq!(
            resolve_episode_season_number(&path, &root, "tvshows", None, Some(1)),
            Some(6)
        );
        assert_eq!(
            resolve_episode_season_number(&path, &root, "tvshows", Some(7), Some(1)),
            Some(7)
        );
        assert_eq!(
            resolve_episode_season_number(
                &root.join("JOJO/套马的汉子 - S01E04.strm"),
                &root,
                "tvshows",
                None,
                Some(1),
            ),
            Some(1)
        );
        assert_eq!(
            resolve_episode_season_number(
                &root.join("Other Show/Season 6/episode - S01E04.strm"),
                &root,
                "tvshows",
                None,
                Some(1),
            ),
            Some(1)
        );
    }

    fn test_progress() -> ScanProgressTracker {
        ScanProgressTracker::new(crate::app::state::ScanTaskControl::default().progress())
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

    async fn scan_jobs(
        root: &std::path::Path,
        library_id: &str,
        collection: &str,
    ) -> Vec<ItemRefreshJob> {
        scan_jobs_with_photos(root, library_id, collection, true).await
    }

    async fn scan_jobs_with_photos(
        root: &std::path::Path,
        library_id: &str,
        collection: &str,
        enable_photos: bool,
    ) -> Vec<ItemRefreshJob> {
        let (tx, mut rx) = mpsc::channel(64);
        scan_root(
            root.to_path_buf(),
            root.to_path_buf(),
            library_id.to_string(),
            collection.to_string(),
            enable_photos,
            0,
            Arc::new(HashMap::new()),
            tx,
            CancellationToken::new(),
            test_progress(),
        )
        .await
        .unwrap();
        let mut jobs = Vec::new();
        while let Some(job) = rx.recv().await {
            jobs.push(job);
        }
        jobs
    }

    async fn refresh_without_running_probe(
        db: &sea_orm::DatabaseConnection,
        mut job: ItemRefreshJob,
    ) -> LocalRefreshResult {
        run_local_refresh_job(db, &mut job, false).await.unwrap()
    }

    async fn media_stream_types(db: &sea_orm::DatabaseConnection, item_id: &str) -> Vec<String> {
        MediaStreams::find()
            .filter(crate::entities::media_streams::Column::ItemId.eq(item_id))
            .all(db)
            .await
            .unwrap()
            .into_iter()
            .map(|stream| stream.stream_type)
            .collect()
    }
}
