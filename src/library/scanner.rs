use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use chrono::Datelike;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use tokio::sync::{Mutex, RwLock, mpsc};
use walkdir::WalkDir;

use crate::{
    app::state::AppState,
    entities::{
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
            refresh_external_lyric_stream, remove_missing_media_items, upsert_default_media_stream,
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

struct MediaProbePipeline {
    tx: mpsc::Sender<MediaProbeJob>,
    handle: tokio::task::JoinHandle<MediaProbeWorkerStats>,
}

struct MetadataFetchPipeline {
    tx: mpsc::UnboundedSender<MetadataFetchJob>,
    handle: tokio::task::JoinHandle<()>,
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
    allow_size_mismatch: bool,
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
    library_id: String,
    item_type: String,
    path: PathBuf,
    preserve_existing_metadata: bool,
    attempts: usize,
    retry_tx: mpsc::UnboundedSender<MetadataFetchJob>,
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

    let probe_pipeline = start_media_probe_pipeline(
        scan_db.clone(),
        force_probe.then(|| scan_db.clone()),
        state.sa_config.ffmpeg_path.clone(),
    );
    let mut tasks = tokio::task::JoinSet::new();
    let root_concurrency = scan_root_concurrency();
    let api_key = state.tmdb_api_key.read().await.clone().unwrap_or_default();
    let douban_cookie = state.douban_cookie.read().await.clone();
    let tmdb_library_options =
        crate::library::tmdb_metadata::load_tmdb_library_provider_options(&scan_db).await?;
    let metadata_pipeline = start_metadata_fetch_pipeline(
        scan_db.clone(),
        api_key.clone(),
        state.tmdb_proxy_url.clone(),
        state.tmdb_http_client.clone(),
        douban_cookie,
        tmdb_library_options,
    );
    let ingest_pipeline = start_ingest_pipeline(
        scan_db.clone(),
        force_probe,
        probe_pipeline.tx.clone(),
        metadata_pipeline.tx.clone(),
    );
    let mut scanned_library_ids = Vec::new();
    let mut total = 0usize;
    let mut all_seen = Vec::new();
    let mut ingest_queued = 0usize;
    let mut pending_roots = roots.into_iter();
    loop {
        while tasks.len() < root_concurrency {
            let Some((root, library_id, collection_type, enable_photos)) = pending_roots.next()
            else {
                break;
            };
            if !root.exists() {
                tracing::warn!("media directory does not exist: {}", root.display());
                continue;
            }
            scanned_library_ids.push(library_id.clone());
            let ingest_tx = ingest_pipeline.tx.clone();
            tasks.spawn(async move {
                scan_root(root, library_id, collection_type, enable_photos, ingest_tx).await
            });
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
    drop(probe_pipeline.tx);
    drop(metadata_pipeline.tx);
    let (probe_result, metadata_result) =
        tokio::join!(probe_pipeline.handle, metadata_pipeline.handle);
    match probe_result {
        Ok(stats) => tracing::info!(
            "media probe completed {} item(s); stream_probe_succeeded={} failed={}",
            stats.completed,
            stats.stream_probe_succeeded,
            stats.failed
        ),
        Err(error) => tracing::warn!("media probe pipeline task panicked: {error}"),
    }
    if let Err(error) = metadata_result {
        tracing::warn!("metadata fetch pipeline task panicked: {error}");
    }

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
    enable_photos: bool,
    ingest_tx: mpsc::Sender<IngestJob>,
) -> anyhow::Result<ScanRootResult> {
    let mut scanned = 0usize;
    let mut seen_paths = Vec::new();
    let mut ingest_queued = 0usize;
    let stack_part_owners = movie_stack_part_owners(&root, &collection_type);
    let episode_version_owners = episode_version_owners(&root, &library_id, &collection_type);

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
            let job = IngestJob {
                item,
                source_path: path.to_path_buf(),
                parsed_metadata,
                clear_folder_metadata: folder_type == "Folder",
                media_probe,
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
        let mut season_number = parsed_metadata
            .season_number
            .or(parsed_name.season_number)
            .or_else(|| episode_season_number_from_path(path, &root, &collection_type));
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
        let job = IngestJob {
            item,
            source_path: path.to_path_buf(),
            parsed_metadata,
            clear_folder_metadata: false,
            media_probe,
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

fn movie_stack_part_owners(
    root: &std::path::Path,
    collection_type: &str,
) -> HashMap<PathBuf, PathBuf> {
    if collection_type != "movies" {
        return HashMap::new();
    }

    let mut groups = HashMap::<(PathBuf, String), Vec<(i64, PathBuf)>>::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
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

fn episode_version_owners(
    root: &std::path::Path,
    library_id: &str,
    collection_type: &str,
) -> HashMap<PathBuf, PathBuf> {
    if !matches!(collection_type, "tvshows" | "tv") {
        return HashMap::new();
    }

    let mut groups =
        HashMap::<(String, Option<i64>, String), Vec<(PathBuf, Option<String>, Option<i64>)>>::new(
        );
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if classify_media_path(path, collection_type).as_deref() != Some("Episode")
            || video_extra_type(path, false).is_some()
        {
            continue;
        }
        let parsed = parse_media_name(path, collection_type);
        let season = parsed
            .season_number
            .or_else(|| episode_season_number_from_path(path, root, collection_type))
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
        let parent_id =
            parent_id_for_scanned_file(path, root, library_id, collection_type, "Episode", false);
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

    let stored_item_id = upsert_media_item(&db, &job.item).await?;
    job.item.id = stored_item_id;
    upsert_media_metadata(&db, &job.item.id, &job.parsed_metadata).await?;
    if job.clear_folder_metadata {
        clear_scraped_folder_metadata(&db, &job.item.id).await;
    }
    upsert_sidecar_images(&db, &job.source_path, &job.item.id).await?;
    if job.item.item_type == "Photo" {
        let size_bytes = tokio::fs::metadata(&job.source_path)
            .await
            .ok()
            .and_then(|metadata| i64::try_from(metadata.len()).ok());
        upsert_image_asset(
            &db,
            &job.item.id,
            "Primary",
            0,
            job.source_path.to_string_lossy().as_ref(),
            size_bytes,
        )
        .await?;
    }
    upsert_nfo_images(&db, &job.item.id, &job.parsed_metadata.images).await?;

    let mut result = IngestJobResult {
        metadata_queued: queue_metadata_fetch(
            metadata_tx,
            &job.item,
            &job.source_path,
            job.parsed_metadata.has_nfo,
        ),
        probe_queued: false,
        probe_skipped: false,
    };

    let Some(pending_probe) = job.media_probe else {
        return Ok(result);
    };

    if cached_probe.is_some() {
        result.probe_skipped = true;
        refresh_sidecar_streams(
            &db,
            &job.source_path,
            &job.item.id,
            !matches!(job.item.item_type.as_str(), "Audio" | "AudioBook"),
        )
        .await?;
        return Ok(result);
    }

    upsert_default_media_stream(&db, &job.item).await?;
    upsert_sidecar_subtitles(&db, &job.source_path, &job.item.id).await?;
    let probe_job = MediaProbeJob {
        item: job.item,
        media_path: pending_probe.media_path,
        probe_path: pending_probe.probe_path,
    };
    probe_tx.send(probe_job).await.map_err(|error| {
        anyhow::anyhow!(
            "media probe queue closed before job could be scheduled for {}",
            error.0.item.path
        )
    })?;
    result.probe_queued = true;

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

fn start_media_probe_pipeline(
    db: sea_orm::DatabaseConnection,
    cache_version_db: Option<sea_orm::DatabaseConnection>,
    ffmpeg_path: String,
) -> MediaProbePipeline {
    let concurrency = media_probe_concurrency();
    let queue_capacity = media_probe_queue_capacity();
    let (tx, rx) = mpsc::channel::<MediaProbeJob>(queue_capacity);
    let handle = tokio::spawn(async move {
        let dummy_chapter_duration_seconds = configured_dummy_chapter_duration_seconds(&db).await;
        let chapter_image_settings = crate::chapters::chapter_image_scan_settings(&db).await;
        let receiver = Arc::new(Mutex::new(rx));
        let mut workers = tokio::task::JoinSet::new();
        tracing::info!(
            "media probe pipeline started concurrency={concurrency} queue_capacity={queue_capacity}"
        );

        for _ in 0..concurrency {
            let db = db.clone();
            let receiver = receiver.clone();
            let ffmpeg_path = ffmpeg_path.clone();
            let chapter_image_settings = chapter_image_settings.clone();
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
                    match run_media_probe_job(
                        db.clone(),
                        job,
                        dummy_chapter_duration_seconds,
                        &ffmpeg_path,
                        &chapter_image_settings,
                    )
                    .await
                    {
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
        stats
    });
    MediaProbePipeline { tx, handle }
}

fn start_metadata_fetch_pipeline(
    db: sea_orm::DatabaseConnection,
    api_key: String,
    tmdb_proxy_url: Arc<RwLock<Option<String>>>,
    tmdb_http_client: Arc<RwLock<reqwest::Client>>,
    douban_cookie: Option<String>,
    library_options: HashMap<String, crate::library::tmdb_metadata::TmdbLibraryProviderOptions>,
) -> MetadataFetchPipeline {
    let (tx, mut rx) = mpsc::unbounded_channel::<MetadataFetchJob>();
    let handle = tokio::spawn(async move {
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
            let provider_options = library_options
                .get(&job.library_id)
                .cloned()
                .unwrap_or_default();
            pending.spawn(async move {
                let tmdb_base_url = tmdb_proxy_url.read().await.clone();
                let tmdb_client = tmdb_http_client.read().await.clone();
                run_metadata_fetch_job(
                    db,
                    job,
                    &api_key,
                    &tmdb_client,
                    tmdb_base_url.as_deref(),
                    &provider_options,
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
    MetadataFetchPipeline { tx, handle }
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
    provider_options: &crate::library::tmdb_metadata::TmdbLibraryProviderOptions,
) -> anyhow::Result<()> {
    if api_key.is_empty() {
        return Ok(());
    }
    if metadata_item_is_locked(&db, &job.item_id).await {
        return Ok(());
    }
    let Some(policy) =
        provider_options.automatic_policy(&job.item_type, job.preserve_existing_metadata)
    else {
        return Ok(());
    };
    if matches!(job.item_type.as_str(), "Movie" | "Series" | "Episode")
        && tmdb_metadata_is_current(&db, &job.item_id, policy).await
    {
        return Ok(());
    }

    let metadata_ready = match job.item_type.as_str() {
        "Movie" | "Series" => {
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
        schedule_metadata_fetch_retry(job);
    }
    Ok(())
}

fn schedule_metadata_fetch_retry(mut job: MetadataFetchJob) {
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
    let item_type = job.item_type.clone();
    let retry_tx = job.retry_tx.clone();

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

fn same_normalized_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    path_utils::normalize_path(&left.to_string_lossy())
        == path_utils::normalize_path(&right.to_string_lossy())
}

fn queue_metadata_fetch(
    metadata_tx: &mpsc::UnboundedSender<MetadataFetchJob>,
    item: &ScannedMediaItem,
    path: &std::path::Path,
    preserve_existing_metadata: bool,
) -> bool {
    match item.item_type.as_str() {
        "Movie" | "Series" | "Season" | "Episode" => {}
        _ => return false,
    }
    match metadata_tx.send(MetadataFetchJob {
        item_id: item.id.clone(),
        library_id: item.library_id.clone(),
        item_type: item.item_type.clone(),
        path: path.to_path_buf(),
        preserve_existing_metadata,
        attempts: 0,
        retry_tx: metadata_tx.clone(),
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

async fn media_roots(state: &AppState) -> anyhow::Result<Vec<(PathBuf, String, String, bool)>> {
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
    let library_options = crate::db::settings::find_by_prefix(&state.db, "LibraryOptions.")
        .await?
        .into_iter()
        .filter_map(|setting| {
            let library_id = setting.key.strip_prefix("LibraryOptions.")?.to_string();
            let enable_photos = serde_json::from_str::<serde_json::Value>(&setting.value)
                .ok()
                .and_then(|options| {
                    options
                        .get("EnablePhotos")
                        .or_else(|| options.get("enablePhotos"))
                        .and_then(serde_json::Value::as_bool)
                })
                .unwrap_or(true);
            Some((library_id, enable_photos))
        })
        .collect::<std::collections::HashMap<_, _>>();

    Ok(paths
        .into_iter()
        .filter_map(|path| {
            libraries.get(&path.library_id).map(|library| {
                (
                    PathBuf::from(path_utils::normalize_path(&path.path)),
                    path.library_id,
                    library.collection_type.clone(),
                    library_options.get(&library.id).copied().unwrap_or(true),
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
    crate::db::cpu_parallelism().clamp(1, 2)
}

fn metadata_fetch_concurrency() -> usize {
    crate::db::cpu_parallelism().clamp(1, 2)
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

        let owners = movie_stack_part_owners(&root, "movies");
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
            "tv".to_string(),
            "tvshows".to_string(),
            true,
            tx,
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
        let episode_item = episode_item.expect("episode ingest job should be queued");
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
            "tv".to_string(),
            "tvshows".to_string(),
            true,
            tx,
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
    async fn scan_root_applies_local_nfo_scalars_to_ingest_item() {
        let root = test_dir("scan_root_applies_local_nfo_scalars_to_ingest_item");
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
            "movies".to_string(),
            "movies".to_string(),
            true,
            tx,
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
        let movie_item = movie_item.expect("movie ingest job should be queued");
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
            "movies".to_string(),
            "movies".to_string(),
            true,
            tx,
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
        let disc_job = disc_job.expect("DVD ingest job should be queued");
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
        ingest_without_running_probe(&db, photo_job).await;
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
        ingest_without_running_probe(&db, book_job).await;
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
        ingest_without_running_probe(&db, audiobook_job).await;
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
        ingest_without_running_probe(&db, music_video_job).await;
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
    ) -> Vec<IngestJob> {
        scan_jobs_with_photos(root, library_id, collection, true).await
    }

    async fn scan_jobs_with_photos(
        root: &std::path::Path,
        library_id: &str,
        collection: &str,
        enable_photos: bool,
    ) -> Vec<IngestJob> {
        let (tx, mut rx) = mpsc::channel(64);
        scan_root(
            root.to_path_buf(),
            library_id.to_string(),
            collection.to_string(),
            enable_photos,
            tx,
        )
        .await
        .unwrap();
        let mut jobs = Vec::new();
        while let Some(job) = rx.recv().await {
            jobs.push(job);
        }
        jobs
    }

    async fn ingest_without_running_probe(
        db: &sea_orm::DatabaseConnection,
        job: IngestJob,
    ) -> IngestJobResult {
        let (probe_tx, mut probe_rx) = mpsc::channel(8);
        let (metadata_tx, _metadata_rx) = mpsc::unbounded_channel();
        let result = run_ingest_job(db.clone(), job, false, &probe_tx, &metadata_tx)
            .await
            .unwrap();
        if result.probe_queued {
            probe_rx.recv().await.expect("probe job should be queued");
        }
        result
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
