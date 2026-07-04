use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use uuid::Uuid;

pub fn stable_item_id(path: &Path) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, path.to_string_lossy().as_bytes()).to_string()
}

pub fn stable_text_id(value: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, value.as_bytes()).to_string()
}

pub fn media_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .replace(['.', '_'], " ")
}

pub fn system_time_to_unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

pub fn now_unix() -> i64 {
    system_time_to_unix(SystemTime::now())
}

pub fn unix_to_jellyfin_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp.max(0), 0)
        .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

pub fn infer_library_id_from_path(path: &str) -> &'static str {
    let path = path.to_ascii_lowercase();
    if path.contains("music") || path.contains("audio") || path.contains("song") {
        "music"
    } else if path.contains("show") || path.contains("series") || path.contains("tv") {
        "tvshows"
    } else {
        "movies"
    }
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| anyhow::anyhow!("failed to hash password: {err}"))?
        .to_string())
}

pub fn verify_password(password: &str, password_hash: &str) -> bool {
    PasswordHash::new(password_hash)
        .ok()
        .and_then(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .ok()
        })
        .is_some()
}

pub fn http_client() -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy();
    let proxy_url = std::env::var("JELLYFIN_RS_PROXY").unwrap_or_default();
    if !proxy_url.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(&proxy_url)?);
    }
    Ok(builder.build()?)
}
