use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{common::internal_error, providers},
    library::images::upsert_image_asset,
};

pub async fn remote_images_providers(
    State(state): State<Arc<AppState>>,
) -> Response {
    let mut providers = vec![json!({
        "Name": "Local",
        "SupportedImages": ["Primary", "Art", "Backdrop", "Banner", "Logo", "Thumb", "Disc", "Box", "Screenshot", "Menu", "Chapter"]
    })];

    if state
        .tmdb_api_key
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

    let tmdb_id = if let Some(api_key) = state.tmdb_api_key.as_deref().filter(|key| !key.is_empty())
    {
        let tmdb_id = lookup_tmdb_id(&state.db, &item_id).await.ok().flatten();
        match tmdb_id {
            Some(ref tmdb_id) => {
                let images_result =
                    fetch_remote_images_by_type(&state, api_key, &item_id, tmdb_id).await;
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
                    match providers::tmdb_movie_images(&state.http_client, api_key, &found).await {
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
        all_images.retain(|image| {
            image
                .get("Language")
                .and_then(Value::as_str)
                .is_none_or(|lang| lang.is_empty() || lang.eq_ignore_ascii_case("en"))
        });
    }

    if image_type.eq_ignore_ascii_case("Backdrop") {
        all_images.retain(|image| {
            image
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|type_| type_.eq_ignore_ascii_case("Backdrop"))
        });
    } else {
        all_images.retain(|image| {
            image
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|type_| type_.eq_ignore_ascii_case("Primary"))
        });
    }

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
        Err(error) => internal_error(error),
    }
}

pub(super) async fn download_and_cache_image(
    state: &AppState,
    item_id: &str,
    image_type: &str,
    image_url: &str,
) -> anyhow::Result<()> {
    let response = state
        .http_client
        .get(image_url)
        .send()
        .await?
        .error_for_status()?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let bytes = response.bytes().await?;
    let extension = content_type
        .as_deref()
        .and_then(super::extension_from_content_type)
        .or_else(|| super::extension_from_url(image_url))
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

async fn lookup_tmdb_id(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Option<String>> {
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
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
        .as_deref()
        .filter(|key| !key.is_empty())
        .context("no TMDb API key configured")?;
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT title, production_year, item_type FROM media_items WHERE id = ?",
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

    let results = if item_type.eq_ignore_ascii_case("Series") {
        providers::tmdb_tv_search(&state.http_client, api_key, &name, year).await?
    } else {
        providers::tmdb_movie_search(&state.http_client, api_key, &name, year).await?
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
) -> anyhow::Result<Vec<Value>> {
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT item_type FROM media_items WHERE id = ?",
            vec![item_id.into()],
        ))
        .await
        .context("failed to fetch item type for images")?;

    let is_series = row
        .and_then(|row| row.get_str("item_type").ok())
        .is_some_and(|t| t.eq_ignore_ascii_case("Series"));

    if is_series {
        let response: TmdbTvImageResponse = state
            .http_client
            .get(format!("https://api.themoviedb.org/3/tv/{tmdb_id}/images"))
            .query(&[("api_key", api_key)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut images = Vec::new();
        for poster in &response.posters {
            images.push(build_remote_image(
                "Primary",
                &poster.file_path,
                poster.width,
                poster.height,
                poster.vote_average,
                poster.vote_count,
                poster.iso_639_1.as_deref(),
            ));
        }
        for backdrop in &response.backdrops {
            images.push(build_remote_image(
                "Backdrop",
                &backdrop.file_path,
                backdrop.width,
                backdrop.height,
                backdrop.vote_average,
                backdrop.vote_count,
                backdrop.iso_639_1.as_deref(),
            ));
        }
        Ok(images)
    } else {
        providers::tmdb_movie_images(&state.http_client, api_key, tmdb_id).await
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
    let thumbnail_url = if image_type == "Primary" {
        format!("https://image.tmdb.org/t/p/w342{file_path}")
    } else {
        format!("https://image.tmdb.org/t/p/w780{file_path}")
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
