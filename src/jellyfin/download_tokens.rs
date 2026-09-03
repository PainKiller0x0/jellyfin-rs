use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::util::now_unix;

type DownloadTokenMac = Hmac<Sha256>;

const DOWNLOAD_TOKEN_QUERY: &str = "download_token";
const DOWNLOAD_TOKEN_TTL_SECONDS: i64 = 24 * 60 * 60;

pub fn public_download_path_with_filename_with_base(
    item_id: &str,
    filename: &str,
    public_base: Option<&str>,
) -> String {
    let path = signed_download_path_with_filename(
        item_id,
        filename,
        configured_secret().as_deref(),
        now_unix(),
    );
    public_base
        .map(|base| format!("{}{}", base.trim_end_matches('/'), path))
        .unwrap_or(path)
}

pub fn valid_download_token(item_id: &str, query: &HashMap<String, String>) -> bool {
    let Some(secret) = configured_secret() else {
        return false;
    };
    let Some(token) = query.get(DOWNLOAD_TOKEN_QUERY) else {
        return false;
    };
    valid_download_token_with_secret(item_id, token, &secret, now_unix())
}

fn configured_secret() -> Option<String> {
    std::env::var("JELLYFIN_RS_DOWNLOAD_SIGNING_SECRET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn signed_download_path_with_filename(
    item_id: &str,
    filename: &str,
    secret: Option<&str>,
    now: i64,
) -> String {
    let path = format!(
        "/Items/{item_id}/Download/{}",
        percent_encode_path_segment(filename)
    );
    let Some(secret) = secret else {
        return path;
    };
    let expires_at = now.saturating_add(DOWNLOAD_TOKEN_TTL_SECONDS);
    let signature = sign_download_token(item_id, expires_at, secret);
    format!("{path}?{DOWNLOAD_TOKEN_QUERY}={expires_at}.{signature}")
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sign_download_token(item_id: &str, expires_at: i64, secret: &str) -> String {
    let mut mac = DownloadTokenMac::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts secrets of every length");
    mac.update(token_payload(item_id, expires_at).as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn valid_download_token_with_secret(item_id: &str, token: &str, secret: &str, now: i64) -> bool {
    let Some((expires_at, signature)) = token.split_once('.') else {
        return false;
    };
    let Ok(expires_at) = expires_at.parse::<i64>() else {
        return false;
    };
    if expires_at <= now {
        return false;
    }
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
        return false;
    };
    let Ok(mut mac) = DownloadTokenMac::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(token_payload(item_id, expires_at).as_bytes());
    mac.verify_slice(&signature).is_ok()
}

fn token_payload(item_id: &str, expires_at: i64) -> String {
    format!("jellyfin-rs:download:{item_id}:{expires_at}")
}

#[cfg(test)]
mod tests {
    use super::{
        DOWNLOAD_TOKEN_TTL_SECONDS, public_download_path_with_filename_with_base,
        signed_download_path_with_filename, valid_download_token_with_secret,
    };
    use std::collections::HashMap;

    #[test]
    fn signed_path_is_bound_to_item_and_expiry() {
        let path = signed_download_path_with_filename("movie", "movie.mp4", Some("secret"), 100);
        let token = path.split("download_token=").nth(1).unwrap();
        let query = HashMap::from([(String::from("download_token"), token.to_string())]);

        assert!(valid_download_token_with_secret(
            "movie", token, "secret", 100
        ));
        assert!(!valid_download_token_with_secret(
            "other", token, "secret", 100
        ));
        assert!(!valid_download_token_with_secret(
            "movie", token, "wrong", 100
        ));
        assert!(!valid_download_token_with_secret(
            "movie",
            token,
            "secret",
            100 + DOWNLOAD_TOKEN_TTL_SECONDS + 1
        ));
        assert_eq!(query.len(), 1);
    }

    #[test]
    fn public_base_is_added_without_losing_signed_query() {
        let path = public_download_path_with_filename_with_base(
            "movie",
            "movie.mp4",
            Some("https://example.test/"),
        );
        assert!(path.starts_with("https://example.test/Items/movie/Download/movie.mp4"));
    }

    #[test]
    fn filename_is_encoded_in_the_download_path() {
        let path = signed_download_path_with_filename(
            "movie",
            "极速车魂 - S01E01.mp4",
            Some("secret"),
            100,
        );
        assert!(path.starts_with(
            "/Items/movie/Download/%E6%9E%81%E9%80%9F%E8%BD%A6%E9%AD%82%20-%20S01E01.mp4?download_token="
        ));
    }
}
