use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{HeaderMap, header},
    middleware::Next,
    response::Response,
};
use sea_orm::{ConnectOptions, Database};
use tokio::sync::RwLock;
use tokio::{net::TcpListener, signal};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
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
    let http_client = util::http_client().context("failed to build HTTP client")?;
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
        tmdb_api_key: RwLock::new(tmdb_api_key),
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
    if let Some(api_key) = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|k| !k.is_empty())
    {
        let ep_state = state.clone();
        tokio::spawn(async move {
            // First: fill in missing TMDb IDs for movies/series without tags
            match library::tmdb_metadata::fill_missing_tmdb(&ep_state.db, &api_key).await {
                Ok(0) => {
                    tracing::info!("No missing TMDb metadata to fill");
                }
                Ok(n) => {
                    tracing::info!("Filled TMDb metadata for {n} items via name search");
                }
                Err(e) => {
                    tracing::warn!("fill_missing_tmdb failed: {e:#}");
                }
            }

            // Fetch person biographies and images in background
            match library::tmdb_metadata::batch_fetch_person_tmdb(&ep_state.db, &api_key).await {
                Ok(0) => {
                    tracing::info!("No missing TMDb person data to fill");
                }
                Ok(n) => {
                    tracing::info!("Fetched TMDb data for {n} people");
                }
                Err(e) => {
                    tracing::warn!("batch_fetch_person_tmdb failed: {e:#}");
                }
            }

            // Then: fetch episode details once after startup scan has had time to populate rows.
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                match library::tmdb_metadata::batch_fetch_episode_tmdb(&ep_state.db, &api_key).await
                {
                    Ok(0) => break,
                    Ok(n) => {
                        tracing::info!("episode TMDb batch fetched {n} titles");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("episode TMDb batch failed: {e:#}");
                        break;
                    }
                }
            }
        });
    }

    let api_routes = jellyfin::routes::api_routes().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), jellyfin::auth::require_auth),
    );
    let admin_service =
        ServeDir::new("admin/dist").not_found_service(ServeFile::new("admin/dist/index.html"));
    let app = Router::new()
        .nest_service("/admin", admin_service)
        .nest("/emby", api_routes.clone())
        .merge(api_routes)
        .fallback(jellyfin::routes::not_found)
        .with_state(state)
        .layer(axum::middleware::from_fn(log_http_request))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(addr)
        .await
        .context("failed to bind server")?;
    info!("listening on http://{addr}");
    info!("http api request logging enabled");
    info!("jellyfin api routes enabled");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed")
}

async fn shutdown_signal() {
    let _ = signal::ctrl_c().await;
}

async fn log_http_request(request: Request<Body>, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().map(sanitize_query).unwrap_or_default();
    let user_agent = header_value(request.headers(), header::USER_AGENT.as_str());
    let jellyfin_client = jellyfin_auth_field(request.headers(), "Client");
    let jellyfin_device = jellyfin_auth_field(request.headers(), "Device");
    let jellyfin_device_id = jellyfin_auth_field(request.headers(), "DeviceId");
    let started = Instant::now();

    tracing::info!(
        method = %method,
        path = %path,
        query = %query,
        user_agent = %user_agent,
        jellyfin_client = %jellyfin_client,
        jellyfin_device = %jellyfin_device,
        jellyfin_device_id = %jellyfin_device_id,
        "http api request started"
    );

    let response = next.run(request).await;
    let status = response.status();
    let elapsed_ms = started.elapsed().as_millis();

    tracing::info!(
        method = %method,
        path = %path,
        query = %query,
        status = status.as_u16(),
        elapsed_ms,
        user_agent = %user_agent,
        jellyfin_client = %jellyfin_client,
        jellyfin_device = %jellyfin_device,
        jellyfin_device_id = %jellyfin_device_id,
        "http api request"
    );

    response
}

fn sanitize_query(query: &str) -> String {
    let sanitized = query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            if sensitive_query_key(key) {
                format!("{key}=<redacted>")
            } else if value.is_empty() {
                key.to_string()
            } else {
                format!("{key}={}", truncate_log_value(value, 160))
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    truncate_log_value(&sanitized, 1024)
}

fn sensitive_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("access_key")
        || key.contains("authorization")
        || key == "pw"
        || key.contains("password")
        || key.contains("secret")
}

fn header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| truncate_log_value(value.trim(), 240))
        .unwrap_or_default()
}

fn jellyfin_auth_field(headers: &HeaderMap, field: &str) -> String {
    jellyfin_auth_header_field(headers, field)
        .map(|value| truncate_log_value(&value, 160))
        .unwrap_or_default()
}

fn jellyfin_auth_header_field(headers: &HeaderMap, field: &str) -> Option<String> {
    [
        header::AUTHORIZATION.as_str(),
        "X-Emby-Authorization",
        "X-MediaBrowser-Authorization",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| auth_header_field(value, field))
    })
}

fn auth_header_field(value: &str, field: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let part = auth_header_part(part);
        let (key, value) = part.split_once('=')?;
        key.trim().eq_ignore_ascii_case(field).then(|| {
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
    })
}

fn auth_header_part(value: &str) -> &str {
    let value = value.trim();
    match value.split_once(' ') {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("MediaBrowser")
                || scheme.eq_ignore_ascii_case("Emby") =>
        {
            rest.trim()
        }
        _ => value,
    }
}

fn truncate_log_value(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    let mut end = max_len;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}
