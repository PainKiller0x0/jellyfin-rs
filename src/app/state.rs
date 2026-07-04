use std::{collections::HashMap, path::PathBuf, sync::Arc};

use sea_orm::DatabaseConnection;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

pub const SERVER_NAME: &str = "jellyfin-rs";
pub const VERSION: &str = "0.1.0";
pub const DEFAULT_USER_NAME: &str = "tsukimi";

pub struct AppState {
    pub user_id: Uuid,
    pub access_token: String,
    pub db: DatabaseConnection,
    pub media_dirs: Vec<PathBuf>,
    pub http_client: reqwest::Client,
    pub tmdb_api_key: RwLock<Option<String>>,
    pub playback_sessions: RwLock<HashMap<String, PlaybackSession>>,
    pub session_capabilities: RwLock<HashMap<String, SessionCapabilities>>,
    pub ws_event_tx: broadcast::Sender<crate::ws::WsEvent>,
    // StrmAssistant integration
    pub sa_config: crate::config::StrmAssistantConfig,
    pub intro_detector: Arc<crate::intro_skip::detector::IntroDetector>,
    #[allow(dead_code)]
    pub queue_manager: Arc<crate::queue::QueueManager>,
}

#[derive(Clone, Serialize)]
pub struct PlaybackSession {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "UserId")]
    pub user_id: String,
    #[serde(rename = "PlaySessionId")]
    pub play_session_id: String,
    #[serde(rename = "NowPlayingItemId")]
    pub item_id: String,
    #[serde(rename = "NowPlayingItemName")]
    pub item_name: Option<String>,
    #[serde(rename = "NowPlayingQueue")]
    pub now_playing_queue: Vec<Value>,
    #[serde(rename = "AdditionalUsers")]
    pub additional_users: Vec<SessionUserInfo>,
    #[serde(rename = "Client")]
    pub client: String,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "DeviceId")]
    pub device_id: String,
    #[serde(rename = "ApplicationVersion")]
    pub application_version: String,
    #[serde(rename = "IsActive")]
    pub is_active: bool,
    #[serde(rename = "LastActivityDate")]
    pub last_activity_date: String,
    #[serde(rename = "LastPlaybackCheckIn")]
    pub last_playback_check_in: String,
    #[serde(skip_serializing)]
    pub last_activity_unix: i64,
    #[serde(rename = "PlayState")]
    pub play_state: PlaybackState,
    #[serde(rename = "PlayableMediaTypes")]
    pub playable_media_types: Vec<String>,
    #[serde(rename = "SupportsMediaControlCommands")]
    pub supports_media_control_commands: Vec<String>,
    #[serde(rename = "SupportedCommands")]
    pub supported_commands: Vec<String>,
    #[serde(rename = "SupportsMediaControl")]
    pub supports_media_control: bool,
    #[serde(rename = "SupportsRemoteControl")]
    pub supports_remote_control: bool,
    #[serde(rename = "SupportsPersistentIdentifier")]
    pub supports_persistent_identifier: bool,
    #[serde(rename = "Capabilities")]
    pub capabilities: SessionCapabilities,
}

#[derive(Clone, Serialize)]
pub struct SessionUserInfo {
    #[serde(rename = "UserId")]
    pub user_id: String,
    #[serde(rename = "UserName")]
    pub user_name: String,
}

#[derive(Clone, Serialize)]
pub struct PlaybackState {
    #[serde(rename = "PositionTicks")]
    pub position_ticks: i64,
    #[serde(rename = "IsPaused")]
    pub is_paused: bool,
    #[serde(rename = "CanSeek")]
    pub can_seek: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct SessionCapabilities {
    #[serde(rename = "UserId")]
    pub user_id: String,
    #[serde(rename = "Client")]
    pub client: String,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "DeviceId")]
    pub device_id: String,
    #[serde(rename = "ApplicationVersion")]
    pub application_version: String,
    #[serde(rename = "PlayableMediaTypes")]
    pub playable_media_types: Vec<String>,
    #[serde(rename = "SupportedCommands")]
    pub supported_commands: Vec<String>,
    #[serde(rename = "SupportsMediaControl")]
    pub supports_media_control: bool,
    #[serde(rename = "SupportsPersistentIdentifier")]
    pub supports_persistent_identifier: bool,
}

pub fn media_dirs_from_env() -> Vec<PathBuf> {
    std::env::var("JELLYFIN_RS_MEDIA_DIRS")
        .unwrap_or_default()
        .split(';')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub fn should_scan_on_startup() -> bool {
    std::env::var("JELLYFIN_RS_SCAN_ON_STARTUP")
        .map(|value| !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

pub fn session_timeout_seconds() -> i64 {
    std::env::var("JELLYFIN_RS_SESSION_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(120)
}

/// Load TMDB API key from database app_settings.
pub async fn load_tmdb_api_key(db: &sea_orm::DatabaseConnection) -> Option<String> {
    use crate::db::row_ext::QueryResultExt;
    use sea_orm::ConnectionTrait;

    let backend = db.get_database_backend();
    if let Ok(Some(row)) = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT value FROM app_settings WHERE key = 'tmdb_api_key'",
            vec![],
        ))
        .await
    {
        if let Ok(val) = row.get_str("value") {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    std::env::var("JELLYFIN_RS_TMDB_API_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

impl AppState {
    /// Update the TMDB API key in both database and runtime state.
    pub async fn set_tmdb_api_key(&self, key: &str) -> anyhow::Result<()> {
        use sea_orm::ConnectionTrait;
        let backend = self.db.get_database_backend();
        let now = crate::util::now_unix();
        self.db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO app_settings (key, value, updated_at) VALUES ('tmdb_api_key', ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            vec![key.to_string().into(), now.into()],
        )).await?;
        *self.tmdb_api_key.write().await = if key.is_empty() {
            None
        } else {
            Some(key.to_string())
        };
        Ok(())
    }
}
