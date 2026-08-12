use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::app::state::AppState;

pub fn start_watching(state: Arc<AppState>) {
    let notify_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = watch_loop(notify_state).await {
            warn!("file watcher stopped: {e:#}");
        }
    });
    tokio::spawn(async move {
        poll_loop(state).await;
    });
}

async fn watch_loop(state: Arc<AppState>) -> anyhow::Result<()> {
    let paths = library_paths(&state).await;
    if paths.is_empty() {
        info!("no library paths to watch, file watcher disabled");
        return Ok(());
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Event, notify::Error>>(256);
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.blocking_send(event);
    })?;

    for path in &paths {
        if let Err(e) = watcher.watch(Path::new(path), RecursiveMode::Recursive) {
            warn!("failed to watch {path}: {e}");
        } else {
            info!("watching library path: {path}");
        }
    }

    let debounce = debounce_duration();
    let scan_triggered = Arc::new(Mutex::new(false));

    loop {
        let mut changed = false;
        let mut changed_paths = Vec::new();
        // Drain pending events
        while let Ok(Ok(event)) = rx.try_recv() {
            if is_relevant(&event) {
                changed = true;
                changed_paths.extend(event.paths);
            }
        }
        // If no immediate events, wait for the next one
        if !changed {
            match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
                Ok(Some(Ok(event))) if is_relevant(&event) => {
                    changed = true;
                    changed_paths.extend(event.paths);
                }
                Ok(Some(Ok(_))) => {} // non-relevant event, continue
                Ok(Some(Err(e))) => warn!("file watch error: {e}"),
                Ok(None) => break,
                Err(_) => {} // timeout, continue
            }
        }

        if changed {
            let mut triggered = scan_triggered.lock().await;
            if !*triggered {
                *triggered = true;
                drop(triggered);
                info!("file change detected, scheduling scan after debounce...");
                let state = state.clone();
                let flag = scan_triggered.clone();
                let changed_roots = library_roots_for_changed_paths(&paths, &changed_paths);
                tokio::spawn(async move {
                    tokio::time::sleep(debounce).await;
                    run_scheduled_scan_when_idle(
                        &state,
                        "file watcher trigger",
                        Some(&changed_roots),
                    )
                    .await;
                    let mut triggered = flag.lock().await;
                    *triggered = false;
                });
            }
        }
    }

    Ok(())
}

async fn poll_loop(state: Arc<AppState>) {
    let Some(poll_interval) = poll_interval() else {
        info!("file watcher polling fallback disabled");
        return;
    };
    let debounce = debounce_duration();
    let mut previous_snapshot: Option<MediaTreeSnapshot> = None;
    info!(
        "file watcher polling fallback enabled interval_secs={}",
        poll_interval.as_secs()
    );

    loop {
        let paths = library_paths(&state).await;
        if paths.is_empty() {
            if previous_snapshot.take().is_some() {
                info!(
                    "media watch polling snapshot cleared because no library paths are configured"
                );
            }
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        let path_count = paths.len();
        let current_snapshot = match media_tree_snapshot(paths.clone()).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!("media watch polling failed: {error:#}");
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };

        if let Some(previous) = &previous_snapshot {
            if *previous != current_snapshot {
                info!("media directory change detected by polling; waiting for changes to settle");
                tokio::time::sleep(debounce).await;
                match media_tree_snapshot(paths).await {
                    Ok(settled_snapshot) => {
                        if settled_snapshot == current_snapshot {
                            if run_scheduled_scan_when_idle(
                                &state,
                                "file watcher polling fallback",
                                None,
                            )
                            .await
                            {
                                previous_snapshot = Some(settled_snapshot);
                            }
                        } else {
                            info!("media directory is still changing; polling scan delayed");
                        }
                    }
                    Err(error) => {
                        warn!("media watch polling settle check failed: {error:#}");
                        previous_snapshot = Some(current_snapshot);
                    }
                }
            }
        } else {
            info!("media watch polling snapshot initialized for {path_count} library path(s)");
            previous_snapshot = Some(current_snapshot);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

pub fn schedule_paths_scan(state: Arc<AppState>, paths: Vec<String>, reason: &'static str) {
    tokio::spawn(async move {
        run_scheduled_scan_when_idle(&state, reason, Some(&paths)).await;
    });
}

async fn run_scheduled_scan_when_idle(
    state: &AppState,
    reason: &str,
    paths: Option<&[String]>,
) -> bool {
    loop {
        let result = match paths {
            Some(paths) if !paths.is_empty() => {
                crate::library::scanner::scan_media_library_paths_if_idle(state, paths).await
            }
            _ => crate::library::scanner::scan_media_library_if_idle(state).await,
        };
        match result {
            Ok(Some(_)) => {
                info!("scheduled scan completed ({reason})");
                return true;
            }
            Ok(None) => {
                info!("scheduled scan delayed because another scan is running ({reason})");
                tokio::time::sleep(debounce_duration()).await;
            }
            Err(error) => {
                warn!("scheduled media scan failed ({reason}): {error:#}");
                return false;
            }
        }
    }
}

fn library_roots_for_changed_paths(
    library_paths: &[String],
    changed_paths: &[PathBuf],
) -> Vec<String> {
    let mut roots = HashSet::new();
    for changed_path in changed_paths {
        for library_path in library_paths {
            if changed_path.starts_with(Path::new(library_path)) {
                roots.insert(library_path.clone());
            }
        }
    }
    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort();
    roots
}

fn is_relevant(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

async fn library_paths(state: &AppState) -> Vec<String> {
    use sea_orm::{EntityTrait, QueryOrder};

    crate::entities::library_paths::Entity::find()
        .order_by_asc(crate::entities::library_paths::Column::Path)
        .all(&state.db)
        .await
        .map(|models| models.into_iter().map(|m| m.path).collect())
        .unwrap_or_default()
}

fn debounce_duration() -> Duration {
    Duration::from_secs(env_duration_secs("JELLYFIN_RS_WATCH_DEBOUNCE_SECONDS", 10))
}

fn poll_interval() -> Option<Duration> {
    let seconds = env_duration_secs("JELLYFIN_RS_WATCH_POLL_SECONDS", 60);
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

fn env_duration_secs(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MediaTreeSnapshot {
    files: Vec<FileSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FileSnapshot {
    path: String,
    len: u64,
    modified_at_nanos: u128,
}

async fn media_tree_snapshot(paths: Vec<String>) -> anyhow::Result<MediaTreeSnapshot> {
    tokio::task::spawn_blocking(move || media_tree_snapshot_blocking(&paths)).await?
}

fn media_tree_snapshot_blocking(paths: &[String]) -> anyhow::Result<MediaTreeSnapshot> {
    let mut files = Vec::new();
    for root in paths {
        let root_path = Path::new(root);
        if !root_path.exists() {
            continue;
        }
        for entry in WalkDir::new(root_path).follow_links(false).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warn!("failed to read media watch path: {error}");
                    continue;
                }
            };
            if !entry.file_type().is_file() || !is_snapshot_relevant(entry.path()) {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    warn!(
                        "failed to stat media watch path {}: {error}",
                        entry.path().display()
                    );
                    continue;
                }
            };
            files.push(FileSnapshot {
                path: entry.path().to_string_lossy().to_string(),
                len: metadata.len(),
                modified_at_nanos: modified_at_nanos(metadata.modified().ok()),
            });
        }
    }
    files.sort();
    Ok(MediaTreeSnapshot { files })
}

fn modified_at_nanos(modified: Option<SystemTime>) -> u128 {
    modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn is_snapshot_relevant(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv"
                    | "mp4"
                    | "m4v"
                    | "mov"
                    | "avi"
                    | "wmv"
                    | "webm"
                    | "ts"
                    | "m2ts"
                    | "flv"
                    | "mp3"
                    | "flac"
                    | "m4a"
                    | "aac"
                    | "ogg"
                    | "opus"
                    | "wav"
                    | "ape"
                    | "alac"
                    | "strm"
                    | "nfo"
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
            )
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::*;

    #[test]
    fn media_tree_snapshot_detects_added_files() {
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-watch-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        let paths = vec![dir.to_string_lossy().to_string()];

        let before = media_tree_snapshot_blocking(&paths).unwrap();
        fs::write(dir.join("movie.mkv"), b"video").unwrap();
        let after = media_tree_snapshot_blocking(&paths).unwrap();

        assert_ne!(before, after);
        assert_eq!(after.files.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn media_tree_snapshot_ignores_unrelated_files() {
        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-watch-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), b"ignore").unwrap();

        let paths = vec![dir.to_string_lossy().to_string()];
        let snapshot = media_tree_snapshot_blocking(&paths).unwrap();

        assert!(snapshot.files.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_file_is_mapped_to_its_library_root() {
        let library_paths = vec![
            "/media/tv-domestic".to_string(),
            "/media/tv-xunlei".to_string(),
        ];
        let changed_paths = vec![PathBuf::from(
            "/media/tv-xunlei/Home Temptation/episode-01.strm",
        )];

        assert_eq!(
            library_roots_for_changed_paths(&library_paths, &changed_paths),
            vec!["/media/tv-xunlei".to_string()]
        );
    }
}
