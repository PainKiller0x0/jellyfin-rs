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
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
    if no_proxy_requested() {
        builder = builder.no_proxy();
    } else if let Some(proxy_url) = configured_proxy_url() {
        builder = builder.no_proxy().proxy(reqwest::Proxy::all(&proxy_url)?);
    }
    Ok(builder.build()?)
}

fn no_proxy_requested() -> bool {
    std::env::var("JELLYFIN_RS_NO_PROXY")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn configured_proxy_url() -> Option<String> {
    std::env::var("JELLYFIN_RS_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(system_proxy_url)
}

#[cfg(windows)]
fn system_proxy_url() -> Option<String> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    let settings = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled = settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    if !enabled {
        return None;
    }

    let server = settings
        .get_value::<String, _>("ProxyServer")
        .ok()?
        .trim()
        .to_string();
    parse_windows_proxy_server(&server)
}

#[cfg(not(windows))]
fn system_proxy_url() -> Option<String> {
    None
}

#[cfg(windows)]
fn parse_windows_proxy_server(value: &str) -> Option<String> {
    parse_windows_proxy_server_value(value)
}

#[cfg(any(windows, test))]
fn parse_windows_proxy_server_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let https_proxy = value
        .split(';')
        .find_map(|entry| proxy_entry_value(entry, "https"));
    let http_proxy = value
        .split(';')
        .find_map(|entry| proxy_entry_value(entry, "http"));
    let proxy = https_proxy.or(http_proxy).unwrap_or(value);

    if proxy.contains("://") {
        Some(proxy.to_string())
    } else {
        Some(format!("http://{proxy}"))
    }
}

#[cfg(any(windows, test))]
fn proxy_entry_value<'a>(entry: &'a str, expected_scheme: &str) -> Option<&'a str> {
    let entry = entry.trim();
    let (scheme, proxy) = entry.split_once('=')?;
    scheme
        .eq_ignore_ascii_case(expected_scheme)
        .then_some(proxy.trim())
}

#[cfg(test)]
fn parse_windows_proxy_server_for_test(value: &str) -> Option<String> {
    parse_windows_proxy_server_value(value)
}

#[cfg(test)]
mod tests {
    use super::parse_windows_proxy_server_for_test;

    #[test]
    fn windows_proxy_server_accepts_host_port() {
        assert_eq!(
            parse_windows_proxy_server_for_test("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn windows_proxy_server_prefers_https_entry() {
        assert_eq!(
            parse_windows_proxy_server_for_test("http=127.0.0.1:8080;https=127.0.0.1:7890")
                .as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }
}
