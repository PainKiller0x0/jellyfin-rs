use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::Context;
use axum::Router;
use sea_orm::{ConnectOptions, Database};
use tokio::sync::RwLock;
use tokio::{net::TcpListener, signal};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use uuid::Uuid;

mod app;
mod chapters;
mod chinese;
mod config;
mod db;
mod entities;
mod fingerprint;
mod intro_skip;
mod jellyfin;
mod library;
mod mediainfo;
mod merge;
mod playback;
mod queue;
mod scheduler;
mod strm;
mod thumbnails;
mod tmdb_ext;
mod util;
mod ws;

use app::state::{AppState, DEFAULT_USER_NAME};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let host = std::env::var("JELLYFIN_RS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("JELLYFIN_RS_PORT")
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(8096);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .context("invalid listen address")?;

    let database_url = std::env::var("JELLYFIN_RS_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://jellyfin-rs.db".to_string());
    db::ensure_database_exists(&database_url).await?;

    let mut opt = ConnectOptions::new(database_url.clone());
    opt.max_connections(20).sqlx_logging(false);
    let db = Database::connect(opt)
        .await
        .with_context(|| format!("failed to connect database: {database_url}"))?;
    db::migrate(&db, &database_url).await?;

    let default_username =
        std::env::var("JELLYFIN_RS_USER").unwrap_or_else(|_| DEFAULT_USER_NAME.to_string());
    let (ws_event_tx, _) = tokio::sync::broadcast::channel::<ws::WsEvent>(64);
    let http_client = {
        let mut builder = reqwest::Client::builder();
        let proxy_url = std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("ALL_PROXY"))
            .unwrap_or_default();
        if !proxy_url.is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
                builder = builder.proxy(proxy);
            }
        }
        builder.build().context("failed to build HTTP client")?
    };
    let sa_config = config::StrmAssistantConfig::load(&db).await;
    let intro_detector = Arc::new(intro_skip::detector::IntroDetector::new(
        sa_config.max_intro_duration_secs,
        sa_config.max_credits_duration_secs,
        sa_config.min_opening_plot_duration_secs,
    ));
    let queue_manager = Arc::new(queue::QueueManager::new(
        sa_config.max_concurrent_count,
        sa_config.tier2_max_concurrent_count,
    ));

    let tmdb_api_key = app::state::load_tmdb_api_key(&db).await;

    let state = Arc::new(AppState {
        user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, default_username.as_bytes()),
        access_token: Uuid::new_v4().simple().to_string(),
        db,
        media_dirs: app::state::media_dirs_from_env(),
        http_client,
        tmdb_api_key: tmdb_api_key.clone(),
        playback_sessions: RwLock::new(HashMap::new()),
        session_capabilities: RwLock::new(HashMap::new()),
        ws_event_tx,
        sa_config,
        intro_detector,
        queue_manager,
    });

    db::seed_default_data(&state).await?;
    if app::state::should_scan_on_startup() {
        let scan_state = state.clone();
        tokio::spawn(async move {
            let _ = library::scanner::scan_media_library(&scan_state).await;
        });
    }
    library::watcher::start_watching(state.clone());

    // Fetch episode TMDb metadata in background (retries until data is available)
    if let Some(api_key) = tmdb_api_key.filter(|k| !k.is_empty()) {
        let ep_state = state.clone();
        tokio::spawn(async move {
            // First: fill in missing TMDb IDs for movies/series without tags
            match library::tmdb_metadata::fill_missing_tmdb(&ep_state.db, &api_key).await {
                Ok(0) => { tracing::info!("No missing TMDb metadata to fill"); }
                Ok(n) => { tracing::info!("Filled TMDb metadata for {n} items via name search"); }
                Err(e) => { tracing::warn!("fill_missing_tmdb failed: {e:#}"); }
            }

            // Fetch person biographies and images in background
            match library::tmdb_metadata::batch_fetch_person_tmdb(&ep_state.db, &api_key).await {
                Ok(0) => { tracing::info!("No missing TMDb person data to fill"); }
                Ok(n) => { tracing::info!("Fetched TMDb data for {n} people"); }
                Err(e) => { tracing::warn!("batch_fetch_person_tmdb failed: {e:#}"); }
            }

            // Then: fetch episode details in loop
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                match library::tmdb_metadata::batch_fetch_episode_tmdb(&ep_state.db, &api_key).await {
                    Ok(0) => {} // no episodes ready yet, keep trying
                    Ok(n) => { tracing::info!("episode TMDb batch fetched {n} titles"); break; }
                    Err(e) => { tracing::warn!("episode TMDb batch failed: {e:#}"); break; }
                }
            }
        });
    }

    let app = Router::new()
        .nest("/emby", jellyfin::routes::api_routes())
        .merge(jellyfin::routes::api_routes())
        .fallback(jellyfin::routes::not_found)
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(addr)
        .await
        .context("failed to bind server")?;
    info!("listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed")
}

async fn shutdown_signal() {
    let _ = signal::ctrl_c().await;
}
