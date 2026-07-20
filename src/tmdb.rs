const DEFAULT_API_BASE_URL: &str = "https://api.themoviedb.org/3";
const DEFAULT_IMAGE_BASE_URL: &str = "https://image.tmdb.org/t/p";

pub fn normalize_base_url(base_url: &str) -> anyhow::Result<Option<String>> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Ok(None);
    }
    if base_url.len() > 2048 {
        anyhow::bail!("TMDb proxy URL is too long");
    }
    if base_url
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        anyhow::bail!("TMDb proxy URL contains invalid characters");
    }

    let normalized = if base_url.contains("://") {
        base_url.to_string()
    } else {
        format!("https://{base_url}")
    };
    let parsed = reqwest::Url::parse(&normalized)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("TMDb proxy URL must be an http(s) URL");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        anyhow::bail!("TMDb proxy URL must not contain query or fragment");
    }
    Ok(Some(normalized.trim_end_matches('/').to_string()))
}

pub fn api_url(base_url: Option<&str>, path: &str) -> String {
    format!(
        "{}/{}",
        api_base_url(base_url),
        path.trim_start_matches('/')
    )
}

pub fn image_url(base_url: Option<&str>, size: &str, file_path: &str) -> String {
    format!(
        "{}/{}/{}",
        image_base_url(base_url),
        size.trim_matches('/'),
        file_path.trim_start_matches('/')
    )
}

pub fn is_allowed_image_url(url: &reqwest::Url, base_url: Option<&str>) -> bool {
    if url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("image.tmdb.org"))
        && url.path().starts_with("/t/")
    {
        return true;
    }

    let Some(base_url) = base_url else {
        return false;
    };
    let Ok(image_base) = reqwest::Url::parse(&image_base_url(Some(base_url))) else {
        return false;
    };
    same_origin(url, &image_base) && path_is_under(url.path(), image_base.path())
}

fn api_base_url(base_url: Option<&str>) -> String {
    let Some(base_url) = normalized_base(base_url) else {
        return DEFAULT_API_BASE_URL.to_string();
    };
    if base_url.ends_with("/3") {
        base_url
    } else {
        format!("{base_url}/3")
    }
}

fn image_base_url(base_url: Option<&str>) -> String {
    let Some(base_url) = normalized_base(base_url) else {
        return DEFAULT_IMAGE_BASE_URL.to_string();
    };
    let base_url = base_url.strip_suffix("/3").unwrap_or(&base_url);
    format!("{base_url}/t/p")
}

fn normalized_base(base_url: Option<&str>) -> Option<String> {
    base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn path_is_under(path: &str, base_path: &str) -> bool {
    let base_path = base_path.trim_end_matches('/');
    path == base_path || path.starts_with(&format!("{base_path}/"))
}

#[cfg(test)]
mod tests {
    use super::{api_url, image_url, is_allowed_image_url, normalize_base_url};

    #[test]
    fn tmdb_base_url_normalization_accepts_mirror_hosts() {
        assert_eq!(
            normalize_base_url("tmdb.qb.edu.kg").unwrap().as_deref(),
            Some("https://tmdb.qb.edu.kg")
        );
        assert_eq!(
            normalize_base_url(" https://tmdb.qb.edu.kg/3/ ")
                .unwrap()
                .as_deref(),
            Some("https://tmdb.qb.edu.kg/3")
        );
        assert!(normalize_base_url("").unwrap().is_none());
    }

    #[test]
    fn tmdb_base_url_normalization_rejects_invalid_urls() {
        assert!(normalize_base_url("https://tmdb qb edu kg").is_err());
        assert!(normalize_base_url("ftp://tmdb.qb.edu.kg").is_err());
        assert!(normalize_base_url("https://tmdb.qb.edu.kg/?x=1").is_err());
    }

    #[test]
    fn tmdb_urls_use_configured_mirror_base() {
        assert_eq!(
            api_url(Some("https://tmdb.qb.edu.kg"), "movie/1"),
            "https://tmdb.qb.edu.kg/3/movie/1"
        );
        assert_eq!(
            api_url(Some("https://tmdb.qb.edu.kg/3"), "/movie/1"),
            "https://tmdb.qb.edu.kg/3/movie/1"
        );
        assert_eq!(
            image_url(Some("https://tmdb.qb.edu.kg"), "w500", "/poster.jpg"),
            "https://tmdb.qb.edu.kg/t/p/w500/poster.jpg"
        );
    }

    #[test]
    fn tmdb_image_url_validation_allows_configured_mirror_path() {
        let url = reqwest::Url::parse("https://tmdb.qb.edu.kg/t/p/w500/poster.jpg").unwrap();
        assert!(is_allowed_image_url(&url, Some("https://tmdb.qb.edu.kg")));
        let bad = reqwest::Url::parse("https://tmdb.qb.edu.kg/metadata").unwrap();
        assert!(!is_allowed_image_url(&bad, Some("https://tmdb.qb.edu.kg")));
    }
}
