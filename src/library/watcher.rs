use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::app::state::AppState;

pub fn schedule_paths_scan(state: Arc<AppState>, paths: Vec<String>, reason: &'static str) {
    let changed_paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    tokio::spawn(async move {
        run_scheduled_scan_when_idle(&state, &changed_paths, reason).await;
    });
}

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
    let pending_scan = Arc::new(Mutex::new(PendingWatchScan::default()));
    let mut last_targets = Vec::<LibraryWatchTarget>::new();
    loop {
        let targets = library_watch_targets(&state).await;
        let paths = targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        if targets != last_targets {
            info!(
                "reconfiguring media file watcher old_paths={} new_paths={}",
                last_targets.len(),
                paths.len()
            );
            last_targets.clone_from(&targets);
        }
        if targets.is_empty() {
            tokio::time::sleep(watcher_reload_interval()).await;
            continue;
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Event, notify::Error>>(256);
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.blocking_send(event);
        })?;
        for target in &targets {
            if let Err(error) = watcher.watch(Path::new(&target.path), RecursiveMode::Recursive) {
                warn!("failed to watch {}: {error}", target.path);
            } else {
                info!(
                    "watching library path: {} library_id={}",
                    target.path, target.library_id
                );
            }
        }

        loop {
            match tokio::time::timeout(watcher_reload_interval(), rx.recv()).await {
                Ok(Some(Ok(event))) if is_relevant(&event) => {
                    let paths = changed_paths(&event, &targets);
                    if !paths.is_empty() {
                        schedule_watch_scan(state.clone(), pending_scan.clone(), paths).await;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(error))) => warn!("file watch error: {error}"),
                Ok(None) => break,
                Err(_) => {
                    if library_watch_targets(&state).await != targets {
                        break;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryWatchTarget {
    path: String,
    library_id: String,
}

#[derive(Default)]
struct PendingWatchScan {
    scheduled: bool,
    paths: HashSet<PathBuf>,
}

async fn schedule_watch_scan(
    state: Arc<AppState>,
    pending_scan: Arc<Mutex<PendingWatchScan>>,
    paths: HashSet<PathBuf>,
) {
    let mut pending = pending_scan.lock().await;
    pending.paths.extend(paths);
    if pending.scheduled {
        return;
    }
    pending.scheduled = true;
    drop(pending);

    tokio::spawn(async move {
        tokio::time::sleep(debounce_duration()).await;
        loop {
            let paths = {
                let mut pending = pending_scan.lock().await;
                std::mem::take(&mut pending.paths)
            };
            if !paths.is_empty() {
                let mut paths = paths.into_iter().collect::<Vec<_>>();
                paths.sort();
                info!(
                    "file change detected, refreshing {} changed path(s)",
                    paths.len()
                );
                run_scheduled_scan_when_idle(&state, &paths, "targeted file watcher trigger").await;
            }

            let mut pending = pending_scan.lock().await;
            if pending.paths.is_empty() {
                pending.scheduled = false;
                break;
            }
            drop(pending);
            tokio::time::sleep(debounce_duration()).await;
        }
    });
}

fn changed_paths(event: &Event, targets: &[LibraryWatchTarget]) -> HashSet<PathBuf> {
    event
        .paths
        .iter()
        .filter(|changed| {
            !crate::library::scanner::is_ignored_scan_path(changed)
                && targets
                    .iter()
                    .any(|target| changed.starts_with(Path::new(&target.path)))
        })
        .cloned()
        .collect()
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
        let paths = library_watch_targets(&state)
            .await
            .into_iter()
            .map(|target| target.path)
            .collect::<Vec<_>>();
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
                            let changed_paths = media_tree_changes(previous, &settled_snapshot);
                            if run_scheduled_scan_when_idle(
                                &state,
                                &changed_paths,
                                "file watcher polling fallback",
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

async fn run_scheduled_scan_when_idle(
    state: &AppState,
    changed_paths: &[PathBuf],
    reason: &str,
) -> bool {
    loop {
        match crate::library::scanner::scan_media_paths_if_idle(state, changed_paths).await {
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

fn is_relevant(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

async fn library_watch_targets(state: &AppState) -> Vec<LibraryWatchTarget> {
    use sea_orm::{EntityTrait, QueryOrder};

    let paths = crate::entities::library_paths::Entity::find()
        .order_by_asc(crate::entities::library_paths::Column::Path)
        .all(&state.db)
        .await
        .unwrap_or_default();
    let options = crate::library::options::load_runtime_options(&state.db)
        .await
        .unwrap_or_default();

    paths
        .into_iter()
        .filter(|model| {
            let options = options.get(&model.library_id).cloned().unwrap_or_default();
            options.enabled && options.enable_realtime_monitor
        })
        .map(|model| LibraryWatchTarget {
            path: model.path,
            library_id: model.library_id,
        })
        .collect()
}

fn debounce_duration() -> Duration {
    Duration::from_secs(env_duration_secs("JELLYFIN_RS_WATCH_DEBOUNCE_SECONDS", 10))
}

fn watcher_reload_interval() -> Duration {
    Duration::from_secs(10)
}

fn poll_interval() -> Option<Duration> {
    let seconds = env_duration_secs("JELLYFIN_RS_WATCH_POLL_SECONDS", 0);
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

fn media_tree_changes(previous: &MediaTreeSnapshot, current: &MediaTreeSnapshot) -> Vec<PathBuf> {
    let previous = previous
        .files
        .iter()
        .map(|file| (file.path.as_str(), (file.len, file.modified_at_nanos)))
        .collect::<HashMap<_, _>>();
    let current = current
        .files
        .iter()
        .map(|file| (file.path.as_str(), (file.len, file.modified_at_nanos)))
        .collect::<HashMap<_, _>>();
    let mut changed = previous
        .iter()
        .filter(|(path, fingerprint)| current.get(*path) != Some(*fingerprint))
        .map(|(path, _)| PathBuf::from(path))
        .chain(
            current
                .iter()
                .filter(|(path, fingerprint)| previous.get(*path) != Some(*fingerprint))
                .map(|(path, _)| PathBuf::from(path)),
        )
        .collect::<Vec<_>>();
    changed.sort();
    changed.dedup();
    changed
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
        for entry in WalkDir::new(root_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !crate::library::scanner::is_ignored_scan_path(entry.path()))
        {
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

    use notify::event::{CreateKind, EventAttributes};
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
    fn file_event_preserves_only_paths_inside_watched_roots() {
        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![Path::new("/media/tv/Show/episode.mkv").to_path_buf()],
            attrs: EventAttributes::new(),
        };
        let targets = vec![
            LibraryWatchTarget {
                path: "/media/movies".to_string(),
                library_id: "movies".to_string(),
            },
            LibraryWatchTarget {
                path: "/media/tv".to_string(),
                library_id: "tv".to_string(),
            },
        ];

        let selected = changed_paths(&event, &targets);
        assert_eq!(
            selected,
            HashSet::from([PathBuf::from("/media/tv/Show/episode.mkv")])
        );
    }

    #[test]
    fn snapshot_diff_returns_added_modified_and_removed_paths() {
        let previous = MediaTreeSnapshot {
            files: vec![
                FileSnapshot {
                    path: "/media/removed.mkv".to_string(),
                    len: 1,
                    modified_at_nanos: 1,
                },
                FileSnapshot {
                    path: "/media/modified.mkv".to_string(),
                    len: 1,
                    modified_at_nanos: 1,
                },
            ],
        };
        let current = MediaTreeSnapshot {
            files: vec![
                FileSnapshot {
                    path: "/media/added.mkv".to_string(),
                    len: 1,
                    modified_at_nanos: 1,
                },
                FileSnapshot {
                    path: "/media/modified.mkv".to_string(),
                    len: 2,
                    modified_at_nanos: 2,
                },
            ],
        };

        assert_eq!(
            media_tree_changes(&previous, &current),
            vec![
                PathBuf::from("/media/added.mkv"),
                PathBuf::from("/media/modified.mkv"),
                PathBuf::from("/media/removed.mkv"),
            ]
        );
    }
}
