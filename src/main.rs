use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::Context;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::Response,
};
use serde_json::Value as JsonValue;
use tokio::sync::RwLock;
use tokio::{net::TcpListener, signal};
use tower_http::{
    compression::CompressionLayer,
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
mod intro_skip;
mod jellyfin;
mod library;
mod mediainfo;
mod playback;
mod queue;
mod strm;
mod tmdb;
mod util;
mod ws;

use app::state::{AdminHttpLogEntry, AppState, DEFAULT_USER_NAME, PlaybackDistribution};

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

    let database_url = db::database_url_from_env();
    db::ensure_database_exists(&database_url).await?;

    let db = db::connect_foreground_database(&database_url).await?;
    db::migrate(&db).await?;

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
    let tmdb_proxy_url = app::state::load_tmdb_proxy_url(&db).await;
    let tmdb_http_client = util::http_client().context("failed to build TMDb HTTP client")?;
    let douban_cookie = app::state::load_douban_cookie(&db).await;

    let state = Arc::new(AppState {
        user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, default_username.as_bytes()),
        access_token: Uuid::new_v4().simple().to_string(),
        db,
        media_dirs: app::state::media_dirs_from_env(),
        http_client,
        tmdb_api_key: RwLock::new(tmdb_api_key),
        tmdb_proxy_url: Arc::new(RwLock::new(tmdb_proxy_url)),
        tmdb_http_client: Arc::new(RwLock::new(tmdb_http_client)),
        douban_cookie: RwLock::new(douban_cookie),
        scan_lock: tokio::sync::Mutex::new(()),
        chapter_image_task_cancel: tokio::sync::Mutex::new(None),
        playback_sessions: RwLock::new(HashMap::new()),
        session_capabilities: RwLock::new(HashMap::new()),
        admin_http_log_seq: std::sync::atomic::AtomicU64::new(0),
        admin_http_logs: RwLock::new(std::collections::VecDeque::new()),
        playback_distribution: RwLock::new(PlaybackDistribution::default()),
        ws_event_tx,
        sa_config,
        intro_detector,
        queue_manager,
    });

    db::seed_default_data(&state).await?;
    jellyfin::system::start_scheduled_task_scheduler(state.clone()).await;
    playback::streaming::start_embedded_subtitle_cache_scheduler(state.clone());
    if app::state::should_scan_on_startup() {
        let scan_state = state.clone();
        tokio::spawn(async move {
            let _ = library::scanner::scan_media_library(&scan_state).await;
        });
    }
    library::watcher::start_watching(state.clone());

    // Backfill existing TMDb metadata in the background. New scan results queue
    // Season/Episode metadata from the scanner as each item is ingested.
    if let Some(api_key) = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|k| !k.is_empty())
    {
        let ep_state = state.clone();
        let metadata_database_url = database_url.clone();
        tokio::spawn(async move {
            let metadata_db = match crate::db::background_connection(&metadata_database_url).await {
                Ok(db) => db,
                Err(error) => {
                    tracing::warn!("background TMDb metadata task skipped: {error:#}");
                    return;
                }
            };
            // First: fill in missing TMDb IDs for movies/series without tags
            let tmdb_base_url = ep_state.tmdb_proxy_url.read().await.clone();
            let tmdb_client = ep_state.tmdb_http_client().await;
            match library::tmdb_metadata::fill_missing_tmdb(
                &metadata_db,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
            )
            .await
            {
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

            match library::tmdb_metadata::refresh_existing_tmdb_titles(
                &metadata_db,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
            )
            .await
            {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("Refreshed {n} existing TMDb item title(s)");
                }
                Err(e) => {
                    tracing::warn!("refresh_existing_tmdb_titles failed: {e:#}");
                }
            }

            // Fetch person biographies and images in background
            let tmdb_base_url = ep_state.tmdb_proxy_url.read().await.clone();
            let tmdb_client = ep_state.tmdb_http_client().await;
            match library::tmdb_metadata::batch_fetch_person_tmdb(
                &metadata_db,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
            )
            .await
            {
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

            // Backfill older or missed episode rows; scan-time items use the metadata pipeline.
            let tmdb_base_url = ep_state.tmdb_proxy_url.read().await.clone();
            let tmdb_client = ep_state.tmdb_http_client().await;
            match library::tmdb_metadata::batch_fetch_episode_tmdb(
                &metadata_db,
                &api_key,
                &tmdb_client,
                tmdb_base_url.as_deref(),
            )
            .await
            {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("episode TMDb batch fetched {n} titles");
                }
                Err(e) => {
                    tracing::warn!("episode TMDb batch failed: {e:#}");
                }
            }
        });
    }

    {
        let douban_state = state.clone();
        let douban_database_url = database_url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let douban_db = match crate::db::background_connection(&douban_database_url).await {
                Ok(db) => db,
                Err(error) => {
                    tracing::warn!("background Douban metadata task skipped: {error:#}");
                    return;
                }
            };
            let cookie = douban_state.douban_cookie.read().await.clone();
            match library::douban_metadata::fill_missing_douban(&douban_db, cookie.as_deref()).await
            {
                Ok(0) => tracing::info!("No missing Douban metadata to fill"),
                Ok(n) => tracing::info!("Filled Douban metadata for {n} items via name search"),
                Err(e) => tracing::warn!("fill_missing_douban failed: {e:#}"),
            }
        });
    }

    let api_routes = jellyfin::routes::api_routes().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), jellyfin::auth::require_auth),
    );
    let admin_service =
        ServeDir::new("admin/dist").fallback(ServeFile::new("admin/dist/index.html"));
    let app = Router::new()
        .nest_service("/admin", admin_service)
        .nest("/emby", api_routes.clone())
        .merge(api_routes)
        .fallback(jellyfin::routes::not_found)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn(openapi_contract_response))
        .layer(axum::middleware::from_fn(tagged_image_cache_response))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            log_http_request,
        ))
        .layer(CorsLayer::permissive())
        .layer(CompressionLayer::new().gzip(true).br(true))
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(addr)
        .await
        .context("failed to bind server")?;
    info!("listening on http://{addr}");
    info!("http api request logging enabled");
    info!("jellyfin api routes enabled");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server failed")
}

async fn tagged_image_cache_response(request: Request<Body>, next: Next) -> Response {
    let tagged_image = is_tagged_image_request(request.uri());
    let mut response = next.run(request).await;
    let is_image = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("image/"));
    if tagged_image && (is_image || response.status() == StatusCode::NOT_MODIFIED) {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
    }
    response
}

fn is_tagged_image_request(uri: &axum::http::Uri) -> bool {
    let is_image_path = uri
        .path()
        .split('/')
        .any(|segment| segment.eq_ignore_ascii_case("images"));
    let has_tag = uri.query().is_some_and(|query| {
        query.split('&').any(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            key.eq_ignore_ascii_case("tag") && !value.is_empty()
        })
    });
    is_image_path && has_tag
}

#[cfg(test)]
mod http_optimization_tests {
    use super::*;

    #[test]
    fn tagged_image_detection_requires_an_image_path_and_nonempty_tag() {
        assert!(is_tagged_image_request(
            &"/Items/item/Images/Backdrop/0?tag=etag-1".parse().unwrap()
        ));
        assert!(is_tagged_image_request(
            &"/Users/user/Images/Primary?Tag=etag-2".parse().unwrap()
        ));
        assert!(!is_tagged_image_request(
            &"/Items/item/Images/Backdrop/0?tag=".parse().unwrap()
        ));
        assert!(!is_tagged_image_request(
            &"/Items/item?tag=etag-1".parse().unwrap()
        ));
    }
}

async fn shutdown_signal() {
    let _ = signal::ctrl_c().await;
}

async fn openapi_contract_response(request: Request<Body>, next: Next) -> Response {
    let contract_client = jellyfin_auth_header_field(request.headers(), "Client")
        .is_some_and(|client| client == "CodexOpenApiContract");
    let response = next.run(request).await;
    if !contract_client || !response.status().is_success() || !response_is_json(&response) {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, 10 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return jellyfin::routes::internal_error(error.into()),
    };
    if bytes.is_empty() {
        return Response::from_parts(parts, Body::empty());
    }
    let mut value = match serde_json::from_slice::<JsonValue>(&bytes) {
        Ok(value) => value,
        Err(_) => return Response::from_parts(parts, Body::from(bytes)),
    };

    prune_openapi_contract_json(&mut value);
    let bytes = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes,
        Err(error) => return jellyfin::routes::internal_error(error.into()),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    if let Ok(value) = HeaderValue::from_str(&bytes.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, value);
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn response_is_json(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
}

fn prune_openapi_contract_json(value: &mut JsonValue) {
    match value {
        JsonValue::Array(values) => {
            for value in values {
                prune_openapi_contract_json(value);
            }
        }
        JsonValue::Object(object) => {
            for field in OPENAPI_CONTRACT_EXTRA_FIELDS {
                object.remove(*field);
            }
            for value in object.values_mut() {
                prune_openapi_contract_json(value);
            }
            normalize_openapi_contract_pairs(value);
        }
        _ => {}
    }
}

fn normalize_openapi_contract_pairs(value: &mut JsonValue) {
    let JsonValue::Object(object) = value else {
        return;
    };
    for key in ["GenreItems", "Studios"] {
        let Some(JsonValue::Array(items)) = object.get_mut(key) else {
            continue;
        };
        for item in items {
            let JsonValue::Object(pair) = item else {
                continue;
            };
            let Some(id) = pair
                .get("Id")
                .and_then(JsonValue::as_str)
                .map(stable_long_id)
            else {
                continue;
            };
            pair.insert("Id".to_string(), JsonValue::Number(id.into()));
        }
    }
}

fn stable_long_id(value: &str) -> i64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    (hash & 0x7fff_ffff_ffff_ffff) as i64
}

const OPENAPI_CONTRACT_EXTRA_FIELDS: &[&str] = &[
    "AllowedTags",
    "ApplicationVersion",
    "AudioSpatialFormat",
    "CastReceiverId",
    "Client",
    "DateLastMediaAdded",
    "DeviceId",
    "DeviceName",
    "DirectStreamUrl",
    "ETag",
    "EnableCollectionManagement",
    "EnableLyricManagement",
    "ExtendedVideoType",
    "Filename",
    "ForceRemoteSourceTranscoding",
    "GenPtsInput",
    "HasSegments",
    "IgnoreDts",
    "IgnoreIndex",
    "ImageBlurHashes",
    "IsActive",
    "IsHearingImpaired",
    "IsSupportedAsIdentifier",
    "LastPlaybackCheckIn",
    "LibraryId",
    "LoginAttemptsBeforeLockout",
    "MaxActiveSessions",
    "MaxConcurrentStreams",
    "MaxParentalSubRating",
    "MediaAttachments",
    "NowPlayingItemId",
    "NowPlayingItemName",
    "NowPlayingQueue",
    "PasswordResetProviderId",
    "PlaySessionId",
    "PrimaryImageTag",
    "ScreenshotImageTags",
    "ServerId",
    "Size",
    "SplashscreenEnabled",
    "StartIndex",
    "StartupWizardCompleted",
    "SupportsMediaControl",
    "SupportsMediaControlCommands",
    "SupportsPersistentIdentifier",
    "SupportsSegmentSeeking",
    "SyncPlayAccess",
    "TagItems",
    "Trickplay",
    "UseMostCompatibleTranscodingProfile",
    "UserData",
    "UserId",
    "VideoType",
    "Website",
];

async fn log_http_request(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().map(sanitize_query).unwrap_or_default();
    let request_id = Uuid::new_v4().simple().to_string();
    let remote_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.to_string())
        .unwrap_or_default();
    let host = header_value(request.headers(), header::HOST.as_str());
    let forwarded_for = header_value(request.headers(), "X-Forwarded-For");
    let user_agent = header_value(request.headers(), header::USER_AGENT.as_str());
    let jellyfin_client = jellyfin_auth_field(request.headers(), "Client");
    let jellyfin_device = jellyfin_auth_field(request.headers(), "Device");
    let jellyfin_device_id = jellyfin_auth_field(request.headers(), "DeviceId");
    let started = Instant::now();

    tracing::info!(
        request_id = %request_id,
        remote_addr = %remote_addr,
        method = %method,
        path = %path,
        query = %query,
        host = %host,
        forwarded_for = %forwarded_for,
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
        request_id = %request_id,
        remote_addr = %remote_addr,
        method = %method,
        path = %path,
        query = %query,
        status = status.as_u16(),
        elapsed_ms,
        host = %host,
        forwarded_for = %forwarded_for,
        user_agent = %user_agent,
        jellyfin_client = %jellyfin_client,
        jellyfin_device = %jellyfin_device,
        jellyfin_device_id = %jellyfin_device_id,
        "http api request"
    );

    if should_record_admin_http_log(&path) {
        let now = util::now_unix();
        state
            .push_admin_http_log(AdminHttpLogEntry {
                id: state.next_admin_http_log_id(),
                date: util::unix_to_jellyfin_date(now),
                unix_time: now,
                method: method.to_string(),
                path,
                query,
                status_code: status.as_u16(),
                elapsed_ms: elapsed_ms.min(u64::MAX as u128) as u64,
                remote_address: remote_addr,
                host,
                user_agent,
                client: jellyfin_client,
                device: jellyfin_device,
                device_id: jellyfin_device_id,
            })
            .await;
    }

    response
}

fn should_record_admin_http_log(path: &str) -> bool {
    !path.starts_with("/admin") && !path.ends_with("/Admin/Logs")
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
