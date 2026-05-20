use std::path::Path;

use anyhow::Context;
use sea_orm::{ConnectionTrait, Value};

use crate::{db::row_ext::QueryResultExt, jellyfin::providers};

/// Extract TMDb ID from `{tmdb-XXXXX}` or `[tmdbid=XXXXX]` in the path
pub fn extract_tmdb_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let Some(tag) = name.split(open).nth(1)?.split(close).next() {
            let tag = tag.to_ascii_lowercase();
            if let Some(id) = tag.strip_prefix("tmdb-") {
                return Some(id.to_string());
            }
            if let Some(id) = tag.strip_prefix("tmdbid=") {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Clean title by removing `{tmdb-XXXXX}` and similar provider ID tags
pub fn clean_provider_tags(title: &str) -> String {
    let mut result = title.to_string();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start < end {
                let tag = &result[start + 1..end].to_ascii_lowercase();
                if tag.starts_with("tmdb-") || tag.starts_with("tmdbid=") {
                    result.replace_range(start..=end, "");
                }
            }
        }
    }
    result.trim().to_string()
}

/// Fetch TMDb episode details using series TMDb ID + season/episode numbers
pub async fn fetch_episode_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    season_number: i64,
    episode_number: i64,
    series_tmdb_id: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    let client = build_client()?;
    let url = format!(
        "https://api.themoviedb.org/3/tv/{series_tmdb_id}/season/{season_number}/episode/{episode_number}"
    );
    #[derive(serde::Deserialize)]
    struct TmdbEpisode {
        name: Option<String>,
        overview: Option<String>,
        still_path: Option<String>,
    }
    let ep: TmdbEpisode = client
        .get(&url)
        .query(&[("api_key", api_key), ("language", "zh-CN")])
        .send().await?
        .error_for_status()?
        .json().await?;

    let backend = db.get_database_backend();
    if let Some(name) = ep.name.as_ref().filter(|n| !n.is_empty()) {
        let _ = db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET title = ? WHERE id = ?",
            vec![name.as_str().into(), item_id.into()],
        )).await;
    }
    if let Some(overview) = ep.overview.as_ref().filter(|o| !o.is_empty()) {
        let _ = db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE media_items SET overview = ? WHERE id = ?",
            vec![overview.as_str().into(), item_id.into()],
        )).await;
    }
    if let Some(still) = ep.still_path.as_ref() {
        let img_url = format!("https://image.tmdb.org/t/p/w500{still}");
        let _ = download_and_save_tmdb_image(db, &client, item_id, &img_url, "Primary").await;
    }
    Ok(())
}

fn build_client() -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    let proxy_url = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("ALL_PROXY"))
        .unwrap_or_default();
    if !proxy_url.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(&proxy_url)?);
    }
    Ok(builder.build()?)
}

/// Batch fetch TMDb episode metadata for all episodes after scan completes
pub async fn batch_fetch_episode_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
) -> anyhow::Result<usize> {
    let backend = db.get_database_backend();
    // Find episodes with season/episode numbers whose parent series has a Tmdb ID
    let rows = db.query_all(crate::db::helpers::portable_statement(
        backend,
        r#"SELECT e.id as episode_id, e.season_number, e.episode_number, p.provider_item_id as tmdb_id
           FROM media_items e
           JOIN media_items se ON se.id = e.parent_id
           JOIN media_items s ON s.id = se.parent_id
           JOIN provider_ids p ON p.item_id = s.id AND p.provider = 'Tmdb'
           WHERE e.item_type = 'Episode' AND e.season_number IS NOT NULL AND e.episode_number IS NOT NULL"#,
        vec![],
    )).await?;

    let client = build_client()?;
    let mut count = 0usize;
    for row in &rows {
        let episode_id: String = row.get_str("episode_id")?;
        let sn: i64 = row.get_i64("season_number")?;
        let en: i64 = row.get_i64("episode_number")?;
        let tmdb_id: String = row.get_str("tmdb_id")?;

        let url = format!("https://api.themoviedb.org/3/tv/{tmdb_id}/season/{sn}/episode/{en}");
        #[derive(serde::Deserialize)]
        struct Ep { name: Option<String>, overview: Option<String>, still_path: Option<String> }
        let resp = match client.get(&url).query(&[("api_key", api_key), ("language", "zh-CN")]).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let Ok(resp) = resp.error_for_status() else { continue };
        let Ok(ep) = resp.json::<Ep>().await else { continue };

        if let Some(name) = ep.name.as_ref().filter(|n| !n.is_empty()) {
            let _ = db.execute(crate::db::helpers::portable_statement(
                backend, "UPDATE media_items SET title = ? WHERE id = ?",
                vec![name.as_str().into(), episode_id.clone().into()],
            )).await;
        }
        if let Some(overview) = ep.overview.as_ref().filter(|o| !o.is_empty()) {
            let _ = db.execute(crate::db::helpers::portable_statement(
                backend, "UPDATE media_items SET overview = ? WHERE id = ?",
                vec![overview.as_str().into(), episode_id.clone().into()],
            )).await;
        }
        if let Some(still) = ep.still_path.as_ref() {
            let img = format!("https://image.tmdb.org/t/p/w500{still}");
            let _ = download_and_save_tmdb_image(db, &client, &episode_id, &img, "Primary").await;
        }
        count += 1;
    }
    if count > 0 {
        tracing::info!("TMDb episode metadata fetched for {count} episodes");
    }
    Ok(count)
}

/// Look up the TMDb series ID for an episode by walking up the parent chain
pub async fn lookup_series_tmdb_id(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<(String, String)>> {
    // Walk up: Episode → Season → Series, check provider_ids at each level
    let backend = db.get_database_backend();
    let mut current_id = item_id.to_string();
    for _ in 0..3 {
        let row = db.query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT parent_id, item_type FROM media_items WHERE id = ?",
            vec![current_id.clone().into()],
        )).await?;
        let Some(row) = row else { break };
        let parent_id: String = row.get_str("parent_id")?;
        let item_type: String = row.get_str("item_type")?;
        // Check parent for TMDb ID
        if let Some(tmdb_id) = db.query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT provider_item_id FROM provider_ids WHERE item_id = ? AND provider = 'Tmdb'",
            vec![parent_id.clone().into()],
        )).await?.and_then(|r| r.get_opt_str("provider_item_id").ok().flatten())
        {
            return Ok(Some((parent_id, tmdb_id)));
        }
        if item_type == "Season" {
            current_id = parent_id;
        } else {
            break;
        }
    }
    Ok(None)
}

/// Fetch TMDb metadata for a Series or Movie and store it in the database
pub async fn fetch_and_apply_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    item_type: &str,
    path: &Path,
    api_key: &str,
) -> anyhow::Result<()> {
    let Some(tmdb_id) = extract_tmdb_id(path) else {
        return Ok(());
    };

    let client = build_client()?;
    let metadata = if item_type == "Series" || item_type == "Season" || item_type == "Episode" {
        providers::tmdb_tv_details(&client, api_key, &tmdb_id).await
    } else {
        providers::tmdb_movie_details(&client, api_key, &tmdb_id).await
    };

    let metadata = match metadata {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("TMDb API call failed for {tmdb_id} (type: {item_type}): {e:#}");
            return Ok(());
        }
    };
    tracing::info!("TMDb metadata fetched for {item_type} {tmdb_id}");

    let backend = db.get_database_backend();
    let overview = metadata
        .get("Overview")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let year = metadata
        .get("ProductionYear")
        .and_then(|v| v.as_i64());
    let genres: Vec<String> = metadata
        .get("Genres")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let studios: Vec<String> = metadata
        .get("Studios")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let people: Vec<(String, String, String)> = metadata
        .get("People")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some((
                        p.get("Name")?.as_str()?.to_string(),
                        p.get("Role")?.as_str()?.to_string(),
                        p.get("Type")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    // Update media item
    if let Some(overview) = overview {
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET overview = ? WHERE id = ?",
                vec![overview.into(), item_id.into()],
            ))
            .await;
    }
    if let Some(year) = year {
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET production_year = ? WHERE id = ?",
                vec![year.into(), item_id.into()],
            ))
            .await;
    }

    // Store provider IDs
    if let Some(provider_ids) = metadata.get("ProviderIds") {
        if let Some(obj) = provider_ids.as_object() {
            for (provider, id) in obj {
                if let Some(id_str) = id.as_str().filter(|s| !s.is_empty()) {
                    let _ = db
                        .execute(crate::db::helpers::portable_statement(
                            backend,
                            "INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id",
                            vec![item_id.into(), provider.as_str().into(), id_str.into()],
                        ))
                        .await;
                }
            }
        }
    }

    // Upsert genres
    let now = crate::util::now_unix();
    for genre_name in &genres {
        let genre_id = crate::util::stable_text_id(&format!("genre:{genre_name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO genres (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![genre_id.clone().into(), genre_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_genres (item_id, genre_id) VALUES (?, ?) ON CONFLICT(item_id, genre_id) DO NOTHING",
                vec![item_id.into(), genre_id.into()],
            ))
            .await;
    }

    // Upsert studios
    for studio_name in &studios {
        let studio_id = crate::util::stable_text_id(&format!("studio:{studio_name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO studios (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![studio_id.clone().into(), studio_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_studios (item_id, studio_id) VALUES (?, ?) ON CONFLICT(item_id, studio_id) DO NOTHING",
                vec![item_id.into(), studio_id.into()],
            ))
            .await;
    }

    // Upsert people
    for (name, role, person_type) in &people {
        let person_id = crate::util::stable_text_id(&format!("person:{name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![person_id.clone().into(), name.as_str().into(), now.into()],
            ))
            .await;
        let sort_order = people.iter().position(|(n, _, _)| n == name).unwrap_or(0) as i64;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO NOTHING",
                vec![
                    item_id.into(),
                    person_id.into(),
                    role.as_str().into(),
                    Value::from(person_type.as_str()),
                    sort_order.into(),
                ],
            ))
            .await;
    }

    // Download poster and backdrop images
    if let Some(image_url) = metadata
        .get("ImageUrl")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let _ = download_and_save_tmdb_image(db, &client, item_id, image_url, "Primary").await;
    }
    // Also try to get backdrop from the original TMDb API response
    if let Some(backdrop) = metadata.get("BackdropUrl").and_then(|v| v.as_str()) {
        let _ = download_and_save_tmdb_image(db, &client, item_id, backdrop, "Backdrop").await;
    }

    Ok(())
}

async fn download_and_save_tmdb_image(
    db: &sea_orm::DatabaseConnection,
    client: &reqwest::Client,
    item_id: &str,
    url: &str,
    image_type: &str,
) -> anyhow::Result<()> {
    let response = client.get(url).send().await?.error_for_status()?;
    let bytes = response.bytes().await?;
    let ext = url
        .rsplit('.')
        .next()
        .and_then(|e| if e.len() <= 5 { Some(e) } else { None })
        .unwrap_or("jpg");
    let dir = std::path::PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&dir).await.ok();
    let path = dir.join(format!("{}_{}_tmdb.{}", crate::util::stable_text_id(item_id), image_type.to_ascii_lowercase(), ext));
    tokio::fs::write(&path, &bytes).await?;
    let now = crate::util::now_unix();
    let backend = db.get_database_backend();
    let _ = db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, width, height, size_bytes, created_at, updated_at) VALUES (?, ?, ?, 0, ?, ?, NULL, NULL, ?, ?, ?) ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET path = excluded.path, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#,
            vec![
                crate::util::stable_text_id(&format!("image-asset:{item_id}:{image_type}:0")).into(),
                item_id.into(),
                image_type.into(),
                path.to_string_lossy().to_string().into(),
                crate::util::stable_text_id(&format!("tmdb:{item_id}:{image_type}")).into(),
                i64::try_from(bytes.len()).unwrap_or(i64::MAX).into(),
                now.into(),
                now.into(),
            ],
        ))
        .await;
    Ok(())
}
