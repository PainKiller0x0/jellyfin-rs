use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::common::{internal_error, ok_response, wants_json_response},
    jellyfin::providers,
    library::images::upsert_image_asset,
};

pub async fn remote_images_providers(State(state): State<Arc<AppState>>) -> Response {
    let mut providers = vec![json!({
        "Name": "Local",
        "SupportedImages": ["Primary", "Art", "Backdrop", "Banner", "Logo", "Thumb", "Disc", "Box", "Screenshot", "Menu", "Chapter"]
    })];

    if state
        .tmdb_api_key
        .read()
        .await
        .as_deref()
        .is_some_and(|key| !key.is_empty())
    {
        providers.push(json!({
            "Name": "TheMovieDb",
            "SupportedImages": ["Primary", "Backdrop", "Banner", "Logo", "Thumb"]
        }));
    }

    Json(providers).into_response()
}

pub async fn remote_images(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let image_type = query
        .get("Type")
        .or_else(|| query.get("type"))
        .map(String::as_str)
        .unwrap_or("Primary");
    let requested_type = remote_image_type(image_type);
    let include_all_languages = query
        .get("IncludeAllLanguages")
        .or_else(|| query.get("includeAllLanguages"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let provider_name = query
        .get("ProviderName")
        .or_else(|| query.get("providerName"))
        .filter(|value| !value.is_empty())
        .map(String::as_str);

    if provider_name.is_some_and(|name| name.eq_ignore_ascii_case("Local")) {
        return Json(json!({
            "Images": [],
            "TotalRecordCount": 0,
            "Providers": ["Local"]
        }))
        .into_response();
    }

    let mut all_images: Vec<Value> = Vec::new();

    let tmdb_id = if let Some(api_key) = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|key| !key.is_empty())
    {
        let tmdb_id = lookup_tmdb_id(&state.db, &item_id).await.ok().flatten();
        match tmdb_id {
            Some(ref tmdb_id) => {
                let images_result = fetch_remote_images_by_type(
                    &state,
                    &api_key,
                    &item_id,
                    tmdb_id,
                    requested_type,
                )
                .await;
                match images_result {
                    Ok(images) => all_images.extend(images),
                    Err(error) => {
                        tracing::warn!("TMDb images fetch failed for {item_id}: {error:#}")
                    }
                }
                Some(tmdb_id.clone())
            }
            None => {
                if let Ok(Some(found)) = search_tmdb_id_by_item(&state, &item_id).await {
                    match fetch_remote_images_by_type(
                        &state,
                        &api_key,
                        &item_id,
                        &found,
                        requested_type,
                    )
                    .await
                    {
                        Ok(images) => all_images.extend(images),
                        Err(error) => {
                            tracing::warn!("TMDb images search failed for {item_id}: {error:#}")
                        }
                    }
                    Some(found)
                } else {
                    None
                }
            }
        }
    } else {
        None
    };

    if !include_all_languages {
        let preferred_language = preferred_metadata_language(&state.db).await;
        all_images.retain(|image| {
            should_keep_remote_image_language(
                image.get("Language").and_then(Value::as_str),
                &preferred_language,
            )
        });
    }

    all_images.retain(|image| {
        image
            .get("Type")
            .and_then(Value::as_str)
            .is_some_and(|type_| type_.eq_ignore_ascii_case(requested_type))
    });

    let total = all_images.len();
    let providers: Vec<&str> = if tmdb_id.is_some() {
        vec!["TheMovieDb"]
    } else {
        vec!["Local"]
    };

    Json(json!({
        "Images": all_images,
        "TotalRecordCount": total,
        "Providers": providers
    }))
    .into_response()
}

fn remote_image_type(image_type: &str) -> &'static str {
    match image_type.trim().to_ascii_lowercase().as_str() {
        "backdrop" | "art" | "banner" | "thumb" => "Backdrop",
        "logo" => "Logo",
        _ => "Primary",
    }
}

async fn preferred_metadata_language(db: &DatabaseConnection) -> String {
    db.query_one(crate::db::helpers::pg_statement(
        "SELECT value FROM app_settings WHERE key = 'PreferredMetadataLanguage'",
        vec![],
    ))
    .await
    .ok()
    .flatten()
    .and_then(|row| row.get_opt_str("value").ok().flatten())
    .filter(|language| !language.trim().is_empty())
    .unwrap_or_else(|| "zh-CN".to_string())
}

fn should_keep_remote_image_language(language: Option<&str>, preferred_language: &str) -> bool {
    let Some(language) = language
        .map(str::trim)
        .filter(|language| !language.is_empty())
    else {
        return true;
    };
    let preferred_language = preferred_language.trim();
    let preferred_base = preferred_language
        .split(['-', '_'])
        .next()
        .unwrap_or(preferred_language);

    language.eq_ignore_ascii_case(preferred_language)
        || language.eq_ignore_ascii_case(preferred_base)
        || language.eq_ignore_ascii_case("en")
}

pub async fn download_remote_image(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let image_type = query
        .get("Type")
        .or_else(|| query.get("type"))
        .map(String::as_str)
        .unwrap_or("Primary");
    let Some(image_url) = query
        .get("ImageUrl")
        .or_else(|| query.get("imageUrl"))
        .filter(|value| !value.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "ImageUrl is required" })),
        )
            .into_response();
    };

    match download_and_cache_image(&state, &item_id, image_type, image_url).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if is_rejected_remote_image_url(&error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) if super::is_image_too_large(&error) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Image is too large" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn image_by_name_remote(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(image_url) = image_url_query(&query) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "ImageUrl is required" })),
        )
            .into_response();
    };
    if wants_json_response(&headers) {
        return ok_response();
    }

    match fetch_remote_image(&state, image_url).await {
        Ok((bytes, content_type)) => remote_image_response(bytes, content_type),
        Err(error) if is_rejected_remote_image_url(&error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) if super::is_image_too_large(&error) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "Error": "Image is too large" })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub(super) async fn download_and_cache_image(
    state: &AppState,
    item_id: &str,
    image_type: &str,
    image_url: &str,
) -> anyhow::Result<()> {
    let image_url = validate_remote_image_url(image_url)?;
    let response = remote_image_request(state, image_url.clone())
        .await
        .send()
        .await?
        .error_for_status()?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    if response
        .content_length()
        .is_some_and(|length| length > super::MAX_IMAGE_BYTES as u64)
    {
        bail!("image is too large");
    }
    let bytes = response.bytes().await?;
    if bytes.len() > super::MAX_IMAGE_BYTES {
        bail!("image is too large");
    }
    let extension = content_type
        .as_deref()
        .and_then(super::extension_from_content_type)
        .or_else(|| super::extension_from_url(image_url.as_str()))
        .unwrap_or("bin");

    let directory = PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&directory)
        .await
        .context("failed to create image cache directory")?;
    let path = directory.join(format!(
        "{}_{}_remote.{}",
        super::sanitize_file_part(item_id),
        super::sanitize_file_part(image_type),
        extension
    ));
    tokio::fs::write(&path, &bytes)
        .await
        .with_context(|| format!("failed to write remote image file: {}", path.display()))?;
    upsert_image_asset(
        &state.db,
        item_id,
        image_type,
        0,
        path.to_string_lossy().as_ref(),
        i64::try_from(bytes.len()).ok(),
    )
    .await
}

async fn fetch_remote_image(
    state: &AppState,
    image_url: &str,
) -> anyhow::Result<(Bytes, &'static str)> {
    let image_url = validate_remote_image_url(image_url)?;
    let response = remote_image_request(state, image_url)
        .await
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > super::MAX_IMAGE_BYTES as u64)
    {
        bail!("image is too large");
    }
    let content_type = remote_image_content_type(response.headers());
    let bytes = response.bytes().await?;
    if bytes.len() > super::MAX_IMAGE_BYTES {
        bail!("image is too large");
    }
    Ok((bytes, content_type))
}

fn remote_image_response(bytes: Bytes, content_type: &'static str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    (headers, Body::from(bytes)).into_response()
}

fn remote_image_content_type(headers: &HeaderMap) -> &'static str {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match content_type.as_str() {
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

fn validate_remote_image_url(image_url: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(image_url.trim()).context("invalid remote image url")?;
    let allowed = url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            (host.eq_ignore_ascii_case("image.tmdb.org") && url.path().starts_with("/t/"))
                || is_douban_image_host(host)
        });
    if !allowed {
        bail!("remote image url is not allowed");
    }
    Ok(url)
}

async fn remote_image_request(
    state: &AppState,
    image_url: reqwest::Url,
) -> reqwest::RequestBuilder {
    let request = state.http_client.get(image_url.clone());
    let Some(host) = image_url
        .host_str()
        .filter(|host| is_douban_image_host(host))
    else {
        return request;
    };

    let mut request = request
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        )
        .header(header::ACCEPT, "image/avif,image/webp,image/apng,image/*,*/*;q=0.8")
        .header(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.6")
        .header(header::REFERER, "https://movie.douban.com/");
    if let Some(cookie) = state.douban_cookie.read().await.as_deref() {
        request = request.header(header::COOKIE, cookie);
    }
    request.header(
        HeaderName::from_static("sec-fetch-site"),
        if host.ends_with("doubanio.com") {
            HeaderValue::from_static("cross-site")
        } else {
            HeaderValue::from_static("same-origin")
        },
    )
}

fn is_douban_image_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "img1.doubanio.com"
        || host == "img2.doubanio.com"
        || host == "img3.doubanio.com"
        || host == "img9.doubanio.com"
        || host == "qnmob3.doubanio.com"
        || host == "img1.douban.com"
        || host == "img2.douban.com"
        || host == "img3.douban.com"
        || host == "img9.douban.com"
}

pub(super) fn is_rejected_remote_image_url(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("remote image url is not allowed")
        || message.contains("invalid remote image url")
}

fn image_url_query(query: &HashMap<String, String>) -> Option<&str> {
    query
        .get("ImageUrl")
        .or_else(|| query.get("imageUrl"))
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str)
}

async fn lookup_tmdb_id(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Option<String>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT provider_item_id FROM provider_ids WHERE item_id = ? AND provider = 'Tmdb'",
            vec![item_id.into()],
        ))
        .await
        .context("failed to look up TMDb id")?;
    Ok(row.and_then(|row| row.get_opt_str("provider_item_id").ok().flatten()))
}

async fn search_tmdb_id_by_item(state: &AppState, item_id: &str) -> anyhow::Result<Option<String>> {
    let api_key = state
        .tmdb_api_key
        .read()
        .await
        .clone()
        .filter(|key| !key.is_empty())
        .context("no TMDb API key configured")?;
    let row = state
        .db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT title, production_year, item_type, parent_id FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await
        .context("failed to fetch item for TMDb search")?;
    let Some(row) = row else {
        return Ok(None);
    };
    let name: String = row.get_str("title")?;
    let year: Option<i64> = row.get_opt_i64("production_year")?;
    let item_type: String = row.get_str("item_type")?;
    let parent_id: Option<String> = row.get_opt_str("parent_id")?;

    if item_type.eq_ignore_ascii_case("Season") {
        return match parent_id
            .as_deref()
            .filter(|parent_id| !parent_id.is_empty())
        {
            Some(parent_id) => lookup_tmdb_id(&state.db, parent_id).await,
            None => Ok(None),
        };
    }

    if item_type.eq_ignore_ascii_case("Episode") {
        return lookup_episode_series_tmdb_id(&state.db, item_id).await;
    }

    let results = if item_type.eq_ignore_ascii_case("Series") {
        providers::tmdb_tv_search(&state.http_client, &api_key, &name, year).await?
    } else {
        providers::tmdb_movie_search(&state.http_client, &api_key, &name, year).await?
    };

    Ok(results
        .first()
        .and_then(|result| result.get("ProviderIds"))
        .and_then(|providers| providers.get("Tmdb"))
        .and_then(Value::as_str)
        .map(ToString::to_string))
}

async fn fetch_remote_images_by_type(
    state: &AppState,
    api_key: &str,
    item_id: &str,
    tmdb_id: &str,
    requested_type: &str,
) -> anyhow::Result<Vec<Value>> {
    let row = state
        .db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT item_type, title, parent_id, season_number FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await
        .context("failed to fetch item type for images")?;

    let Some(row) = row else {
        return Ok(Vec::new());
    };

    let item_type: String = row.get_str("item_type").unwrap_or_default();
    if item_type.eq_ignore_ascii_case("Series") {
        return fetch_tmdb_tv_images(&state.http_client, api_key, tmdb_id).await;
    }

    if item_type.eq_ignore_ascii_case("Season") {
        let title: String = row.get_str("title").unwrap_or_default();
        let parent_id: Option<String> = row.get_opt_str("parent_id").ok().flatten();
        let series_tmdb_id = match parent_id
            .as_deref()
            .filter(|parent_id| !parent_id.is_empty())
        {
            Some(parent_id) => lookup_tmdb_id(&state.db, parent_id).await.ok().flatten(),
            None => None,
        };
        let season_number = row
            .get_opt_i64("season_number")
            .ok()
            .flatten()
            .or_else(|| crate::library::tmdb_metadata::parse_season_number(&title));

        let mut images = Vec::new();
        if requested_type.eq_ignore_ascii_case("Primary") {
            if let (Some(series_tmdb_id), Some(season_number)) =
                (series_tmdb_id.as_deref(), season_number)
            {
                match fetch_tmdb_tv_season_images(
                    &state.http_client,
                    api_key,
                    series_tmdb_id,
                    season_number,
                )
                .await
                {
                    Ok(season_images) => images.extend(season_images),
                    Err(error) => {
                        tracing::warn!("TMDb season images fetch failed for {item_id}: {error:#}")
                    }
                }
            }
        }

        if !requested_type.eq_ignore_ascii_case("Primary") || images.is_empty() {
            if let Some(series_tmdb_id) = series_tmdb_id.as_deref() {
                match fetch_tmdb_tv_images(&state.http_client, api_key, series_tmdb_id).await {
                    Ok(series_images) => images.extend(series_images),
                    Err(error) => {
                        tracing::warn!(
                            "TMDb parent series images fetch failed for {item_id}: {error:#}"
                        )
                    }
                }
            }
        }

        return Ok(images);
    }

    if item_type.eq_ignore_ascii_case("Episode") {
        if let Some(series_tmdb_id) = lookup_episode_series_tmdb_id(&state.db, item_id).await? {
            return fetch_tmdb_tv_images(&state.http_client, api_key, &series_tmdb_id).await;
        }
    }

    providers::tmdb_movie_images(&state.http_client, api_key, tmdb_id).await
}

async fn lookup_episode_series_tmdb_id(
    db: &DatabaseConnection,
    episode_id: &str,
) -> anyhow::Result<Option<String>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT p.provider_item_id
               FROM media_items episode
               JOIN media_items season ON season.id = episode.parent_id
               JOIN media_items series ON series.id = season.parent_id
               JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
               WHERE episode.id = ?"#,
            vec![episode_id.into()],
        ))
        .await
        .context("failed to look up episode series TMDb id")?;
    Ok(row.and_then(|row| row.get_opt_str("provider_item_id").ok().flatten()))
}

async fn fetch_tmdb_tv_images(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let response: TmdbTvImageResponse = client
        .get(format!("https://api.themoviedb.org/3/tv/{tmdb_id}/images"))
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut images = Vec::new();
    append_remote_images(&mut images, "Primary", &response.posters);
    append_remote_images(&mut images, "Backdrop", &response.backdrops);
    append_remote_images(&mut images, "Logo", &response.logos);
    Ok(images)
}

async fn fetch_tmdb_tv_season_images(
    client: &reqwest::Client,
    api_key: &str,
    series_tmdb_id: &str,
    season_number: i64,
) -> anyhow::Result<Vec<Value>> {
    let response: TmdbTvSeasonImageResponse = client
        .get(format!(
            "https://api.themoviedb.org/3/tv/{series_tmdb_id}/season/{season_number}/images"
        ))
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let mut images = Vec::new();
    append_remote_images(&mut images, "Primary", &response.posters);
    Ok(images)
}

fn append_remote_images(images: &mut Vec<Value>, image_type: &str, entries: &[TmdbTvImage]) {
    for entry in entries {
        images.push(build_remote_image(
            image_type,
            &entry.file_path,
            entry.width,
            entry.height,
            entry.vote_average,
            entry.vote_count,
            entry.iso_639_1.as_deref(),
        ));
    }
}

fn build_remote_image(
    image_type: &str,
    file_path: &str,
    width: Option<i64>,
    height: Option<i64>,
    vote_average: Option<f64>,
    vote_count: Option<i64>,
    iso_639_1: Option<&str>,
) -> Value {
    let thumbnail_url = match image_type {
        "Primary" => format!("https://image.tmdb.org/t/p/w342{file_path}"),
        "Logo" => format!("https://image.tmdb.org/t/p/w500{file_path}"),
        _ => format!("https://image.tmdb.org/t/p/w780{file_path}"),
    };
    let full_url = format!("https://image.tmdb.org/t/p/original{file_path}");
    let mut image = json!({
        "ProviderName": "TheMovieDb",
        "Url": full_url,
        "ThumbnailUrl": thumbnail_url,
        "Height": height,
        "Width": width,
        "CommunityRating": vote_average,
        "VoteCount": vote_count,
        "Type": image_type,
    });
    if let Some(lang) = iso_639_1 {
        if !lang.is_empty() {
            image["Language"] = json!(lang);
        }
    }
    image
}

#[derive(Deserialize)]
struct TmdbTvImageResponse {
    #[serde(default)]
    posters: Vec<TmdbTvImage>,
    #[serde(default)]
    backdrops: Vec<TmdbTvImage>,
    #[serde(default)]
    logos: Vec<TmdbTvImage>,
}

#[derive(Deserialize)]
struct TmdbTvSeasonImageResponse {
    #[serde(default)]
    posters: Vec<TmdbTvImage>,
}

#[derive(Deserialize)]
struct TmdbTvImage {
    file_path: String,
    width: Option<i64>,
    height: Option<i64>,
    vote_average: Option<f64>,
    vote_count: Option<i64>,
    iso_639_1: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        image_url_query, remote_image_content_type, remote_image_type,
        should_keep_remote_image_language, validate_remote_image_url,
    };
    use axum::http::{HeaderMap, HeaderValue, header};
    use std::collections::HashMap;

    #[test]
    fn remote_image_url_allows_tmdb_cdn() {
        assert!(validate_remote_image_url("https://image.tmdb.org/t/p/w500/poster.jpg").is_ok());
    }

    #[test]
    fn remote_image_url_allows_douban_image_cdn() {
        assert!(
            validate_remote_image_url(
                "https://img9.doubanio.com/view/photo/s_ratio_poster/public/p2916595576.jpg"
            )
            .is_ok()
        );
        assert!(
            validate_remote_image_url(
                "https://qnmob3.doubanio.com/view/photo/large/public/p2916595576.jpg"
            )
            .is_ok()
        );
    }

    #[test]
    fn remote_image_url_rejects_private_or_unexpected_hosts() {
        assert!(validate_remote_image_url("http://127.0.0.1/admin.png").is_err());
        assert!(validate_remote_image_url("https://example.com/poster.jpg").is_err());
        assert!(validate_remote_image_url("https://image.tmdb.org/metadata").is_err());
        assert!(validate_remote_image_url("https://evil-doubanio.com/poster.jpg").is_err());
    }

    #[test]
    fn remote_image_query_accepts_jellyfin_and_emby_casing() {
        let mut query = HashMap::new();
        query.insert(
            "imageUrl".to_string(),
            " https://image.tmdb.org/t/p/w500/a.jpg ".to_string(),
        );
        assert_eq!(
            image_url_query(&query),
            Some(" https://image.tmdb.org/t/p/w500/a.jpg ")
        );

        query.clear();
        query.insert("ImageUrl".to_string(), "".to_string());
        assert_eq!(image_url_query(&query), None);
    }

    #[test]
    fn remote_image_content_type_is_image_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("image/webp; charset=binary"),
        );
        assert_eq!(remote_image_content_type(&headers), "image/webp");

        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        assert_eq!(
            remote_image_content_type(&headers),
            "application/octet-stream"
        );
    }

    #[test]
    fn remote_image_type_keeps_logo_requests() {
        assert_eq!(remote_image_type("Logo"), "Logo");
        assert_eq!(remote_image_type("logo"), "Logo");
        assert_eq!(remote_image_type("Backdrop"), "Backdrop");
        assert_eq!(remote_image_type("Art"), "Backdrop");
        assert_eq!(remote_image_type("Primary"), "Primary");
    }

    #[test]
    fn remote_image_language_allows_preferred_language_and_fallbacks() {
        assert!(should_keep_remote_image_language(Some("zh"), "zh-CN"));
        assert!(should_keep_remote_image_language(Some("zh-CN"), "zh-CN"));
        assert!(should_keep_remote_image_language(Some("en"), "zh-CN"));
        assert!(should_keep_remote_image_language(None, "zh-CN"));
        assert!(!should_keep_remote_image_language(Some("ja"), "zh-CN"));
    }
}
