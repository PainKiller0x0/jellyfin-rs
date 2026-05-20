use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::app::state::AppState;

pub fn start_watching(state: Arc<AppState>) {
    tokio::spawn(async move {
        if let Err(e) = watch_loop(state).await {
            warn!("file watcher stopped: {e:#}");
        }
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

    let debounce = Duration::from_secs(10);
    let scan_triggered = Arc::new(Mutex::new(false));

    loop {
        let mut changed = false;
        // Drain pending events
        while let Ok(Ok(event)) = rx.try_recv() {
            if is_relevant(&event) {
                changed = true;
            }
        }
        // If no immediate events, wait for the next one
        if !changed {
            match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
                Ok(Some(Ok(event))) if is_relevant(&event) => changed = true,
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
                tokio::spawn(async move {
                    tokio::time::sleep(debounce).await;
                    info!("running scheduled scan (file watcher trigger)");
                    let _ = crate::library::scanner::scan_media_library(&state).await;
                    let mut triggered = flag.lock().await;
                    *triggered = false;
                });
            }
        }
    }

    Ok(())
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
