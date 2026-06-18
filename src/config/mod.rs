use crate::db::row_ext::QueryResultExt;
use sea_orm::ConnectionTrait;

#[derive(Debug, Clone)]
pub struct StrmAssistantConfig {
    pub enabled: bool,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub fpcalc_path: String,
    pub max_concurrent_count: usize,
    pub tier2_max_concurrent_count: usize,
    pub cooldown_duration_secs: u64,

    // STRM
    pub strm_enabled: bool,

    // MediaInfo
    pub mediainfo_extract_enabled: bool,
    pub mediainfo_json_root_folder: Option<String>,

    // Intro Skip
    pub intro_skip_enabled: bool,
    pub max_intro_duration_secs: i64,
    pub max_credits_duration_secs: i64,
    pub min_opening_plot_duration_secs: i64,
    pub intro_skip_library_scope: Vec<String>,
    pub intro_skip_user_scope: Vec<String>,
    pub intro_skip_client_scope: Vec<String>,
    pub intro_skip_preference: IntroSkipPreference,

    // Fingerprint
    pub fingerprint_enabled: bool,
    pub intro_detection_fingerprint_minutes: i64,

    // Video Thumbnail
    pub thumbnail_enabled: bool,
    pub thumbnail_interval_secs: u64,
    pub thumbnail_width: u32,

    // TMDB Enhancements
    pub tmdb_rate_limit_ms: u64,
    pub tmdb_cache_size: usize,
    pub tmdb_original_language_posters: bool,

    // Chinese
    pub chinese_convert_enabled: bool,
    pub chinese_search_enhancement: bool,
    pub pinyin_sorting: bool,

    // Multi-Version Merge
    pub merge_enabled: bool,

    // Subtitle
    pub enhanced_subtitle_scan: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IntroSkipPreference {
    DetectOnly,
    ResetAndOverwrite,
    NoDetectionButReset,
}

impl Default for StrmAssistantConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ffmpeg_path: "ffmpeg".to_string(),
            ffprobe_path: "ffprobe".to_string(),
            fpcalc_path: "fpcalc".to_string(),
            max_concurrent_count: 1,
            tier2_max_concurrent_count: 1,
            cooldown_duration_secs: 0,
            strm_enabled: true,
            mediainfo_extract_enabled: false,
            mediainfo_json_root_folder: None,
            intro_skip_enabled: true,
            max_intro_duration_secs: 150,
            max_credits_duration_secs: 360,
            min_opening_plot_duration_secs: 60,
            intro_skip_library_scope: Vec::new(),
            intro_skip_user_scope: Vec::new(),
            intro_skip_client_scope: vec![
                "Emby".to_string(),
                "Infuse".to_string(),
                "SenPlayer".to_string(),
            ],
            intro_skip_preference: IntroSkipPreference::DetectOnly,
            fingerprint_enabled: false,
            intro_detection_fingerprint_minutes: 10,
            thumbnail_enabled: false,
            thumbnail_interval_secs: 10,
            thumbnail_width: 320,
            tmdb_rate_limit_ms: 100,
            tmdb_cache_size: 20,
            tmdb_original_language_posters: false,
            chinese_convert_enabled: true,
            chinese_search_enhancement: true,
            pinyin_sorting: false,
            merge_enabled: false,
            enhanced_subtitle_scan: true,
        }
    }
}

impl StrmAssistantConfig {
    /// Load config from database app_settings table, falling back to env vars, then defaults.
    pub async fn load(db: &sea_orm::DatabaseConnection) -> Self {
        let mut cfg = Self::default();

        // Load all sa.* settings from DB in one query
        let backend = db.get_database_backend();
        let db_settings: std::collections::HashMap<String, String> = match db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                "SELECT key, value FROM app_settings WHERE key LIKE 'sa.%'",
                vec![],
            ))
            .await
        {
            Ok(rows) => rows
                .iter()
                .filter_map(|row| {
                    let key = row.get_str("key").ok()?;
                    let val = row.get_str("value").ok()?;
                    Some((key, val))
                })
                .collect(),
            Err(_) => std::collections::HashMap::new(),
        };

        let get_db = |key: &str| -> Option<String> { db_settings.get(key).cloned() };

        // Load each setting: DB first, then env var, then default
        cfg.enabled = get_db("sa.enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| env_bool("JELLYFIN_RS_SA_ENABLED", cfg.enabled));

        cfg.ffmpeg_path = get_db("sa.ffmpeg_path")
            .unwrap_or_else(|| env_str("JELLYFIN_RS_SA_FFMPEG_PATH", &cfg.ffmpeg_path));

        cfg.ffprobe_path = get_db("sa.ffprobe_path")
            .unwrap_or_else(|| env_str("JELLYFIN_RS_SA_FFPROBE_PATH", &cfg.ffprobe_path));

        cfg.fpcalc_path = get_db("sa.fpcalc_path")
            .unwrap_or_else(|| env_str("JELLYFIN_RS_SA_FPCALC_PATH", &cfg.fpcalc_path));

        cfg.max_concurrent_count = get_db("sa.max_concurrent")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_usize("JELLYFIN_RS_SA_MAX_CONCURRENT", cfg.max_concurrent_count)
            });

        cfg.tier2_max_concurrent_count = get_db("sa.tier2_max_concurrent")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_usize(
                    "JELLYFIN_RS_SA_TIER2_MAX_CONCURRENT",
                    cfg.tier2_max_concurrent_count,
                )
            });

        cfg.cooldown_duration_secs = get_db("sa.cooldown_secs")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| env_u64("JELLYFIN_RS_SA_COOLDOWN_SECS", cfg.cooldown_duration_secs));

        cfg.strm_enabled = get_db("sa.strm_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| env_bool("JELLYFIN_RS_SA_STRM_ENABLED", cfg.strm_enabled));

        cfg.mediainfo_extract_enabled = get_db("sa.mediainfo_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_MEDIAINFO_ENABLED",
                    cfg.mediainfo_extract_enabled,
                )
            });

        cfg.mediainfo_json_root_folder = get_db("sa.mediainfo_json_root")
            .or_else(|| std::env::var("JELLYFIN_RS_SA_MEDIAINFO_JSON_ROOT").ok());

        cfg.intro_skip_enabled = get_db("sa.intro_skip_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool("JELLYFIN_RS_SA_INTRO_SKIP_ENABLED", cfg.intro_skip_enabled)
            });

        cfg.max_intro_duration_secs = get_db("sa.max_intro_duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_i64(
                    "JELLYFIN_RS_SA_MAX_INTRO_DURATION",
                    cfg.max_intro_duration_secs,
                )
            });

        cfg.max_credits_duration_secs = get_db("sa.max_credits_duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_i64(
                    "JELLYFIN_RS_SA_MAX_CREDITS_DURATION",
                    cfg.max_credits_duration_secs,
                )
            });

        cfg.min_opening_plot_duration_secs = get_db("sa.min_opening_plot_duration")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_i64(
                    "JELLYFIN_RS_SA_MIN_OPENING_PLOT_DURATION",
                    cfg.min_opening_plot_duration_secs,
                )
            });

        cfg.intro_skip_library_scope = get_db("sa.intro_library_scope")
            .map(|v| split_csv(&v))
            .unwrap_or_else(|| env_vec("JELLYFIN_RS_SA_INTRO_LIBRARY_SCOPE"));

        cfg.intro_skip_user_scope = get_db("sa.intro_user_scope")
            .map(|v| split_csv(&v))
            .unwrap_or_else(|| env_vec("JELLYFIN_RS_SA_INTRO_USER_SCOPE"));

        if let Some(val) = get_db("sa.intro_client_scope")
            .or_else(|| std::env::var("JELLYFIN_RS_SA_INTRO_CLIENT_SCOPE").ok())
        {
            if !val.is_empty() {
                cfg.intro_skip_client_scope = split_csv(&val);
            }
        }

        if let Some(val) = get_db("sa.intro_preference")
            .or_else(|| std::env::var("JELLYFIN_RS_SA_INTRO_PREFERENCE").ok())
        {
            cfg.intro_skip_preference = match val.to_ascii_lowercase().as_str() {
                "resetandoverwrite" | "reset_and_overwrite" => {
                    IntroSkipPreference::ResetAndOverwrite
                }
                "nodetectionbutreset" | "no_detection_but_reset" => {
                    IntroSkipPreference::NoDetectionButReset
                }
                _ => IntroSkipPreference::DetectOnly,
            };
        }

        cfg.fingerprint_enabled = get_db("sa.fingerprint_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_FINGERPRINT_ENABLED",
                    cfg.fingerprint_enabled,
                )
            });

        cfg.intro_detection_fingerprint_minutes = get_db("sa.fingerprint_minutes")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_i64(
                    "JELLYFIN_RS_SA_FINGERPRINT_MINUTES",
                    cfg.intro_detection_fingerprint_minutes,
                )
            });

        cfg.thumbnail_enabled = get_db("sa.thumbnail_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| env_bool("JELLYFIN_RS_SA_THUMBNAIL_ENABLED", cfg.thumbnail_enabled));

        cfg.thumbnail_interval_secs = get_db("sa.thumbnail_interval")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_u64(
                    "JELLYFIN_RS_SA_THUMBNAIL_INTERVAL",
                    cfg.thumbnail_interval_secs,
                )
            });

        cfg.thumbnail_width = get_db("sa.thumbnail_width")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| env_u32("JELLYFIN_RS_SA_THUMBNAIL_WIDTH", cfg.thumbnail_width));

        cfg.tmdb_rate_limit_ms = get_db("sa.tmdb_rate_limit_ms")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                env_u64("JELLYFIN_RS_SA_TMDB_RATE_LIMIT_MS", cfg.tmdb_rate_limit_ms)
            });

        cfg.tmdb_cache_size = get_db("sa.tmdb_cache_size")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| env_usize("JELLYFIN_RS_SA_TMDB_CACHE_SIZE", cfg.tmdb_cache_size));

        cfg.tmdb_original_language_posters = get_db("sa.tmdb_original_posters")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_TMDB_ORIGINAL_POSTERS",
                    cfg.tmdb_original_language_posters,
                )
            });

        cfg.chinese_convert_enabled = get_db("sa.chinese_convert")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_CHINESE_CONVERT",
                    cfg.chinese_convert_enabled,
                )
            });

        cfg.chinese_search_enhancement = get_db("sa.chinese_search")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_CHINESE_SEARCH",
                    cfg.chinese_search_enhancement,
                )
            });

        cfg.pinyin_sorting = get_db("sa.pinyin_sorting")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| env_bool("JELLYFIN_RS_SA_PINYIN_SORTING", cfg.pinyin_sorting));

        cfg.merge_enabled = get_db("sa.merge_enabled")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| env_bool("JELLYFIN_RS_SA_MERGE_ENABLED", cfg.merge_enabled));

        cfg.enhanced_subtitle_scan = get_db("sa.enhanced_subtitle_scan")
            .map(|v| parse_bool(&v))
            .unwrap_or_else(|| {
                env_bool(
                    "JELLYFIN_RS_SA_ENHANCED_SUBTITLE_SCAN",
                    cfg.enhanced_subtitle_scan,
                )
            });

        cfg
    }

    /// Load config from env vars only (for startup before DB is ready).
    #[allow(dead_code)]
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        cfg.enabled = env_bool("JELLYFIN_RS_SA_ENABLED", cfg.enabled);
        cfg.ffmpeg_path = env_str("JELLYFIN_RS_SA_FFMPEG_PATH", &cfg.ffmpeg_path);
        cfg.ffprobe_path = env_str("JELLYFIN_RS_SA_FFPROBE_PATH", &cfg.ffprobe_path);
        cfg.fpcalc_path = env_str("JELLYFIN_RS_SA_FPCALC_PATH", &cfg.fpcalc_path);
        cfg.max_concurrent_count =
            env_usize("JELLYFIN_RS_SA_MAX_CONCURRENT", cfg.max_concurrent_count);
        cfg.tier2_max_concurrent_count = env_usize(
            "JELLYFIN_RS_SA_TIER2_MAX_CONCURRENT",
            cfg.tier2_max_concurrent_count,
        );
        cfg.cooldown_duration_secs =
            env_u64("JELLYFIN_RS_SA_COOLDOWN_SECS", cfg.cooldown_duration_secs);
        cfg.strm_enabled = env_bool("JELLYFIN_RS_SA_STRM_ENABLED", cfg.strm_enabled);
        cfg.mediainfo_extract_enabled = env_bool(
            "JELLYFIN_RS_SA_MEDIAINFO_ENABLED",
            cfg.mediainfo_extract_enabled,
        );
        cfg.mediainfo_json_root_folder = std::env::var("JELLYFIN_RS_SA_MEDIAINFO_JSON_ROOT").ok();
        cfg.intro_skip_enabled =
            env_bool("JELLYFIN_RS_SA_INTRO_SKIP_ENABLED", cfg.intro_skip_enabled);
        cfg.max_intro_duration_secs = env_i64(
            "JELLYFIN_RS_SA_MAX_INTRO_DURATION",
            cfg.max_intro_duration_secs,
        );
        cfg.max_credits_duration_secs = env_i64(
            "JELLYFIN_RS_SA_MAX_CREDITS_DURATION",
            cfg.max_credits_duration_secs,
        );
        cfg.min_opening_plot_duration_secs = env_i64(
            "JELLYFIN_RS_SA_MIN_OPENING_PLOT_DURATION",
            cfg.min_opening_plot_duration_secs,
        );
        cfg.intro_skip_library_scope = env_vec("JELLYFIN_RS_SA_INTRO_LIBRARY_SCOPE");
        cfg.intro_skip_user_scope = env_vec("JELLYFIN_RS_SA_INTRO_USER_SCOPE");
        if let Ok(val) = std::env::var("JELLYFIN_RS_SA_INTRO_CLIENT_SCOPE") {
            if !val.is_empty() {
                cfg.intro_skip_client_scope = split_csv(&val);
            }
        }
        if let Ok(val) = std::env::var("JELLYFIN_RS_SA_INTRO_PREFERENCE") {
            cfg.intro_skip_preference = match val.to_ascii_lowercase().as_str() {
                "resetandoverwrite" | "reset_and_overwrite" => {
                    IntroSkipPreference::ResetAndOverwrite
                }
                "nodetectionbutreset" | "no_detection_but_reset" => {
                    IntroSkipPreference::NoDetectionButReset
                }
                _ => IntroSkipPreference::DetectOnly,
            };
        }
        cfg.fingerprint_enabled = env_bool(
            "JELLYFIN_RS_SA_FINGERPRINT_ENABLED",
            cfg.fingerprint_enabled,
        );
        cfg.intro_detection_fingerprint_minutes = env_i64(
            "JELLYFIN_RS_SA_FINGERPRINT_MINUTES",
            cfg.intro_detection_fingerprint_minutes,
        );
        cfg.thumbnail_enabled = env_bool("JELLYFIN_RS_SA_THUMBNAIL_ENABLED", cfg.thumbnail_enabled);
        cfg.thumbnail_interval_secs = env_u64(
            "JELLYFIN_RS_SA_THUMBNAIL_INTERVAL",
            cfg.thumbnail_interval_secs,
        );
        cfg.thumbnail_width = env_u32("JELLYFIN_RS_SA_THUMBNAIL_WIDTH", cfg.thumbnail_width);
        cfg.tmdb_rate_limit_ms =
            env_u64("JELLYFIN_RS_SA_TMDB_RATE_LIMIT_MS", cfg.tmdb_rate_limit_ms);
        cfg.tmdb_cache_size = env_usize("JELLYFIN_RS_SA_TMDB_CACHE_SIZE", cfg.tmdb_cache_size);
        cfg.tmdb_original_language_posters = env_bool(
            "JELLYFIN_RS_SA_TMDB_ORIGINAL_POSTERS",
            cfg.tmdb_original_language_posters,
        );
        cfg.chinese_convert_enabled = env_bool(
            "JELLYFIN_RS_SA_CHINESE_CONVERT",
            cfg.chinese_convert_enabled,
        );
        cfg.chinese_search_enhancement = env_bool(
            "JELLYFIN_RS_SA_CHINESE_SEARCH",
            cfg.chinese_search_enhancement,
        );
        cfg.pinyin_sorting = env_bool("JELLYFIN_RS_SA_PINYIN_SORTING", cfg.pinyin_sorting);
        cfg.merge_enabled = env_bool("JELLYFIN_RS_SA_MERGE_ENABLED", cfg.merge_enabled);
        cfg.enhanced_subtitle_scan = env_bool(
            "JELLYFIN_RS_SA_ENHANCED_SUBTITLE_SCAN",
            cfg.enhanced_subtitle_scan,
        );

        cfg
    }
}

fn parse_bool(s: &str) -> bool {
    !matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no")
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(default)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_vec(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
