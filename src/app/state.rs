use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use sea_orm::DatabaseConnection;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use crate::util::{now_unix, unix_to_jellyfin_date};

pub const SERVER_NAME: &str = "jellyfin-rs";
pub const VERSION: &str = "0.1.0";
pub const DEFAULT_USER_NAME: &str = "tsukimi";
const MAX_ADMIN_HTTP_LOGS: usize = 1_000;
const MAX_PLAYBACK_RECENT_EVENTS: usize = 200;

pub struct AppState {
    pub user_id: Uuid,
    pub access_token: String,
    pub db: DatabaseConnection,
    #[allow(dead_code)]
    pub media_dirs: Vec<PathBuf>,
    pub http_client: reqwest::Client,
    pub tmdb_api_key: RwLock<Option<String>>,
    pub douban_cookie: RwLock<Option<String>>,
    pub scan_lock: Mutex<()>,
    pub playback_sessions: RwLock<HashMap<String, PlaybackSession>>,
    pub session_capabilities: RwLock<HashMap<String, SessionCapabilities>>,
    pub admin_http_log_seq: AtomicU64,
    pub admin_http_logs: RwLock<VecDeque<AdminHttpLogEntry>>,
    pub playback_distribution: RwLock<PlaybackDistribution>,
    pub ws_event_tx: broadcast::Sender<crate::ws::WsEvent>,
    // StrmAssistant integration
    pub sa_config: crate::config::StrmAssistantConfig,
    pub intro_detector: Arc<crate::intro_skip::detector::IntroDetector>,
    #[allow(dead_code)]
    pub queue_manager: Arc<crate::queue::QueueManager>,
}

#[derive(Clone, Serialize)]
pub struct AdminHttpLogEntry {
    #[serde(rename = "Id")]
    pub id: u64,
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "UnixTime")]
    pub unix_time: i64,
    #[serde(rename = "Method")]
    pub method: String,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Query")]
    pub query: String,
    #[serde(rename = "StatusCode")]
    pub status_code: u16,
    #[serde(rename = "ElapsedMs")]
    pub elapsed_ms: u64,
    #[serde(rename = "RemoteAddress")]
    pub remote_address: String,
    #[serde(rename = "Host")]
    pub host: String,
    #[serde(rename = "UserAgent")]
    pub user_agent: String,
    #[serde(rename = "Client")]
    pub client: String,
    #[serde(rename = "Device")]
    pub device: String,
    #[serde(rename = "DeviceId")]
    pub device_id: String,
}

#[derive(Default)]
pub struct PlaybackDistribution {
    total_play_count: i64,
    regions: HashMap<String, PlaybackRegionStats>,
    recent_events: VecDeque<PlaybackRecentEvent>,
}

#[derive(Clone, Default)]
struct PlaybackRegionStats {
    region: String,
    region_code: String,
    is_private: bool,
    x: u8,
    y: u8,
    play_count: i64,
    users: HashSet<String>,
    ips: HashSet<String>,
    last_seen_unix: i64,
}

#[derive(Clone, Serialize)]
pub struct PlaybackRecentEvent {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "UnixTime")]
    pub unix_time: i64,
    #[serde(rename = "UserId")]
    pub user_id: String,
    #[serde(rename = "Ip")]
    pub ip: String,
    #[serde(rename = "Region")]
    pub region: String,
    #[serde(rename = "Client")]
    pub client: String,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "ItemId")]
    pub item_id: String,
    #[serde(rename = "ItemName")]
    pub item_name: Option<String>,
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
    if let Ok(Some(row)) = db
        .query_one(crate::db::helpers::pg_statement(
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

/// Load Douban cookie from database app_settings.
pub async fn load_douban_cookie(db: &sea_orm::DatabaseConnection) -> Option<String> {
    use crate::db::row_ext::QueryResultExt;
    use sea_orm::ConnectionTrait;
    if let Ok(Some(row)) = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT value FROM app_settings WHERE key = 'douban_cookie'",
            vec![],
        ))
        .await
    {
        if let Ok(val) = row.get_str("value") {
            let val = normalize_douban_cookie_value(&val);
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

pub fn normalize_douban_cookie_value(cookie: &str) -> String {
    let cookie = cookie.trim();
    cookie
        .split_once(':')
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("cookie"))
        .map(|(_, value)| value)
        .unwrap_or(cookie)
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string()
}

impl AppState {
    /// Update the TMDB API key in both database and runtime state.
    pub async fn set_tmdb_api_key(&self, key: &str) -> anyhow::Result<()> {
        use sea_orm::ConnectionTrait;
        let now = crate::util::now_unix();
        self.db.execute(crate::db::helpers::pg_statement(
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

    /// Update the Douban cookie in both database and runtime state.
    pub async fn set_douban_cookie(&self, cookie: &str) -> anyhow::Result<()> {
        use sea_orm::ConnectionTrait;
        let cookie = normalize_douban_cookie_value(cookie);
        let now = crate::util::now_unix();
        self.db
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO app_settings (key, value, updated_at) VALUES ('douban_cookie', ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![cookie.clone().into(), now.into()],
            ))
            .await?;
        *self.douban_cookie.write().await = if cookie.is_empty() {
            None
        } else {
            Some(cookie)
        };
        Ok(())
    }

    pub fn next_admin_http_log_id(&self) -> u64 {
        self.admin_http_log_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub async fn push_admin_http_log(&self, entry: AdminHttpLogEntry) {
        let mut logs = self.admin_http_logs.write().await;
        logs.push_back(entry);
        while logs.len() > MAX_ADMIN_HTTP_LOGS {
            logs.pop_front();
        }
    }

    pub async fn admin_http_logs(&self, after_id: u64, limit: usize) -> Vec<AdminHttpLogEntry> {
        let limit = limit.clamp(1, MAX_ADMIN_HTTP_LOGS);
        let logs = self.admin_http_logs.read().await;
        let mut entries = logs
            .iter()
            .filter(|entry| entry.id > after_id)
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        entries.reverse();
        entries
    }

    pub async fn record_playback_start(
        &self,
        remote_address: Option<&str>,
        user_id: &str,
        client: &str,
        device_name: &str,
        item_id: &str,
        item_name: Option<String>,
    ) {
        let ip = remote_address
            .and_then(normalize_remote_ip)
            .unwrap_or_else(|| "unknown".to_string());
        let region = playback_region_for_ip(&ip);
        let now = now_unix();
        let event = PlaybackRecentEvent {
            date: unix_to_jellyfin_date(now),
            unix_time: now,
            user_id: user_id.to_string(),
            ip: ip.clone(),
            region: region.region.clone(),
            client: client.to_string(),
            device_name: device_name.to_string(),
            item_id: item_id.to_string(),
            item_name,
        };

        let mut distribution = self.playback_distribution.write().await;
        distribution.total_play_count += 1;
        let stats = distribution
            .regions
            .entry(region.region_code.clone())
            .or_insert_with(|| PlaybackRegionStats {
                region: region.region,
                region_code: region.region_code,
                is_private: region.is_private,
                x: region.x,
                y: region.y,
                ..Default::default()
            });
        stats.play_count += 1;
        stats.users.insert(user_id.to_string());
        stats.ips.insert(ip);
        stats.last_seen_unix = now;
        distribution.recent_events.push_front(event);
        while distribution.recent_events.len() > MAX_PLAYBACK_RECENT_EVENTS {
            distribution.recent_events.pop_back();
        }
    }

    pub async fn playback_distribution_json(&self) -> Value {
        let distribution = self.playback_distribution.read().await;
        let mut regions = distribution
            .regions
            .values()
            .map(|stats| {
                let mut sample_ips = stats.ips.iter().cloned().collect::<Vec<_>>();
                sample_ips.sort();
                sample_ips.truncate(5);
                json!({
                    "Region": stats.region,
                    "RegionCode": stats.region_code,
                    "IsPrivate": stats.is_private,
                    "PlayCount": stats.play_count,
                    "UserCount": stats.users.len() as i64,
                    "IpCount": stats.ips.len() as i64,
                    "SampleIps": sample_ips,
                    "LastSeenDate": unix_to_jellyfin_date(stats.last_seen_unix),
                    "X": stats.x,
                    "Y": stats.y,
                })
            })
            .collect::<Vec<_>>();
        regions.sort_by(|a, b| {
            b.get("PlayCount")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                .cmp(
                    &a.get("PlayCount")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                )
        });
        json!({
            "TotalPlayCount": distribution.total_play_count,
            "RegionCount": regions.len() as i64,
            "Regions": regions,
            "RecentEvents": distribution.recent_events.iter().cloned().collect::<Vec<_>>(),
        })
    }
}

struct PlaybackRegion {
    region: String,
    region_code: String,
    is_private: bool,
    x: u8,
    y: u8,
}

fn normalize_remote_ip(value: &str) -> Option<String> {
    let value = value.split(',').next().unwrap_or(value).trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Some(addr.ip().to_string());
    }
    if let Ok(addr) = value.parse::<IpAddr>() {
        return Some(addr.to_string());
    }
    if let Some((ip, _)) = value.rsplit_once(':') {
        if ip.parse::<IpAddr>().is_ok() {
            return Some(ip.to_string());
        }
    }
    if value.starts_with('[') {
        if let Some((ip, _)) = value[1..].split_once(']') {
            if ip.parse::<IpAddr>().is_ok() {
                return Some(ip.to_string());
            }
        }
    }
    Some(value.to_string())
}

fn playback_region_for_ip(ip: &str) -> PlaybackRegion {
    if ip == "unknown" {
        return PlaybackRegion {
            region: "未知来源".to_string(),
            region_code: "unknown".to_string(),
            is_private: false,
            x: 50,
            y: 50,
        };
    }

    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) if addr.is_loopback() => PlaybackRegion {
            region: "本机".to_string(),
            region_code: "loopback".to_string(),
            is_private: true,
            x: 50,
            y: 58,
        },
        Ok(IpAddr::V4(addr)) if addr.is_private() => {
            let octets = addr.octets();
            if octets[0] == 192 && octets[1] == 168 {
                PlaybackRegion {
                    region: format!("局域网 {}.{}.{}.0/24", octets[0], octets[1], octets[2]),
                    region_code: format!("lan-{}-{}-{}", octets[0], octets[1], octets[2]),
                    is_private: true,
                    x: 64,
                    y: 58,
                }
            } else if octets[0] == 10 {
                PlaybackRegion {
                    region: "局域网 10.0.0.0/8".to_string(),
                    region_code: "lan-10".to_string(),
                    is_private: true,
                    x: 46,
                    y: 56,
                }
            } else {
                PlaybackRegion {
                    region: "局域网 172.16.0.0/12".to_string(),
                    region_code: "lan-172".to_string(),
                    is_private: true,
                    x: 56,
                    y: 66,
                }
            }
        }
        Ok(IpAddr::V4(addr)) => {
            let octets = addr.octets();
            let (x, y) = deterministic_map_point(ip);
            PlaybackRegion {
                region: format!("公网 {}.{}.0.0/16", octets[0], octets[1]),
                region_code: format!("public-{}-{}", octets[0], octets[1]),
                is_private: false,
                x,
                y,
            }
        }
        Ok(IpAddr::V6(addr)) if addr.is_loopback() => PlaybackRegion {
            region: "本机".to_string(),
            region_code: "loopback".to_string(),
            is_private: true,
            x: 50,
            y: 58,
        },
        Ok(IpAddr::V6(addr)) if addr.is_unique_local() => PlaybackRegion {
            region: "局域网 IPv6".to_string(),
            region_code: "lan-ipv6".to_string(),
            is_private: true,
            x: 58,
            y: 52,
        },
        Ok(IpAddr::V6(_)) => {
            let (x, y) = deterministic_map_point(ip);
            PlaybackRegion {
                region: "公网 IPv6".to_string(),
                region_code: "public-ipv6".to_string(),
                is_private: false,
                x,
                y,
            }
        }
        Err(_) => {
            let (x, y) = deterministic_map_point(ip);
            PlaybackRegion {
                region: ip.to_string(),
                region_code: format!("raw-{}", stable_hash(ip)),
                is_private: false,
                x,
                y,
            }
        }
    }
}

fn deterministic_map_point(value: &str) -> (u8, u8) {
    let hash = stable_hash(value);
    let x = 14 + (hash % 72) as u8;
    let y = 18 + ((hash / 73) % 58) as u8;
    (x, y)
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{normalize_douban_cookie_value, normalize_remote_ip, playback_region_for_ip};

    #[test]
    fn remote_ip_normalization_accepts_socket_addresses_and_forwarded_for() {
        assert_eq!(
            normalize_remote_ip("192.168.1.16:45264").as_deref(),
            Some("192.168.1.16")
        );
        assert_eq!(
            normalize_remote_ip("203.0.113.10, 10.0.0.2").as_deref(),
            Some("203.0.113.10")
        );
    }

    #[test]
    fn playback_region_groups_private_lan_ips() {
        let region = playback_region_for_ip("192.168.1.16");
        assert_eq!(region.region, "局域网 192.168.1.0/24");
        assert!(region.is_private);
    }

    #[test]
    fn douban_cookie_normalization_accepts_pasted_header_value() {
        assert_eq!(
            normalize_douban_cookie_value(" Cookie: bid=abc; ll=\"108288\"; "),
            "bid=abc; ll=\"108288\""
        );
        assert_eq!(normalize_douban_cookie_value("COOKIE: bid=abc;"), "bid=abc");
    }
}
