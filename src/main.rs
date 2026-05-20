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
mod db;
mod entities;
mod jellyfin;
mod library;
mod playback;
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
    opt.max_connections(5).sqlx_logging(false);
    let db = Database::connect(opt)
        .await
        .with_context(|| format!("failed to connect database: {database_url}"))?;
    db::migrate(&db, &database_url).await?;

    let default_username =
        std::env::var("JELLYFIN_RS_USER").unwrap_or_else(|_| DEFAULT_USER_NAME.to_string());
    let (ws_event_tx, _) = tokio::sync::broadcast::channel::<ws::WsEvent>(64);
    let state = Arc::new(AppState {
        user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, default_username.as_bytes()),
        access_token: Uuid::new_v4().simple().to_string(),
        db,
        media_dirs: app::state::media_dirs_from_env(),
        http_client: reqwest::Client::new(),
        tmdb_api_key: std::env::var("JELLYFIN_RS_TMDB_API_KEY").ok(),
        playback_sessions: RwLock::new(HashMap::new()),
        session_capabilities: RwLock::new(HashMap::new()),
        ws_event_tx,
    });

    db::seed_default_data(&state).await?;
    if app::state::should_scan_on_startup() {
        let scan_state = state.clone();
        tokio::spawn(async move {
            let _ = library::scanner::scan_media_library(&scan_state).await;
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
