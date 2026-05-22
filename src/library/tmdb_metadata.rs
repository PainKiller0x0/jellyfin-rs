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

/// Clean title by removing `{tmdb-XXXXX}`, `[tmdbid=XXXXX]`, and `(YYYY)` tags
pub fn clean_provider_tags(title: &str) -> String {
    let mut result = title.to_string();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start < end {
                let tag = &result[start + 1..end].to_ascii_lowercase();
                if tag.starts_with("tmdb-") || tag.starts_with("tmdbid=") {
                    result.replace_range(start..=end, "");
                } else if tag.len() == 4 && tag.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(y) = tag.parse::<i64>() {
                        if (1880..=2100).contains(&y) {
                            result.replace_range(start..=end, "");
                        }
                    }
                }
            }
        }
    }
    result = result.split_whitespace().collect::<Vec<_>>().join(" ");
    result.trim().to_string()
}

/// Clean title AND extract year separately, returning (title, year).
/// e.g. "X战警 (2000) {tmdb-36657}" → ("X战警", Some(2000))
pub fn clean_title_with_year(title: &str) -> (String, Option<i64>) {
    let mut result = title.to_string();
    let mut year: Option<i64> = None;

    // First remove provider tags like {tmdb-XXXXX}
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start < end {
                result.replace_range(start..=end, "");
            }
        }
    }

    // Then extract year from (YYYY) at the end
    if let (Some(start), Some(end)) = (result.rfind('('), result.rfind(')')) {
        if start < end {
            let tag = result[start + 1..end].trim();
            if let Ok(y) = tag.parse::<i64>() {
                if (1880..=2100).contains(&y) {
                    year = Some(y);
                    result.replace_range(start..=end, "");
                }
            }
        }
    }

    let cleaned = result.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string();
    (cleaned, year)
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

/// Download poster images for all seasons that belong to a TMDb series
async fn download_season_images(db: &sea_orm::DatabaseConnection, api_key: &str) {
    let backend = db.get_database_backend();
    let rows = db.query_all(crate::db::helpers::portable_statement(
        backend,
        r#"SELECT s.id as season_id, s.title, p.provider_item_id as series_tmdb_id
           FROM media_items s
           JOIN media_items series ON series.id = s.parent_id
           JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
           WHERE s.item_type = 'Season'
           AND NOT EXISTS (SELECT 1 FROM image_assets ia WHERE ia.item_id = s.id)"#,
        vec![],
    )).await;
    let Ok(rows) = rows else { return };
    if rows.is_empty() { return };

    let total = rows.len();
    tracing::info!("Downloading {total} season poster images...");

    let client = match build_client() {
        Ok(c) => c,
        Err(e) => { tracing::warn!("Failed to build HTTP client: {e:#}"); return },
    };
    let mut downloaded = 0usize;

    for chunk in rows.chunks(10) {
        let futures: Vec<_> = chunk.iter().map(|row| {
            let client = &client;
            async move {
                let Ok(season_id) = row.get_str("season_id") else { return None };
                let title: String = row.get_str("title").ok()?;
                let Ok(series_tmdb) = row.get_str("series_tmdb_id") else { return None };
                // Parse season number from title like "Season 1" or "第1季"
                let sn = parse_season_number(&title).unwrap_or(0);
                let url = format!("https://api.themoviedb.org/3/tv/{series_tmdb}/season/{sn}?api_key={api_key}&language=zh-CN");
                let resp = client.get(&url).send().await.ok()?;
                let resp = resp.error_for_status().ok()?;
                #[derive(serde::Deserialize)]
                struct SeasonResp { poster_path: Option<String> }
                let season: SeasonResp = resp.json().await.ok()?;
                season.poster_path.map(|p| (season_id, p))
            }
        }).collect();

        let results = futures_util::future::join_all(futures).await;
        for (season_id, poster_path) in results.into_iter().flatten() {
            let img_url = format!("https://image.tmdb.org/t/p/w500{poster_path}");
            if download_and_save_tmdb_image(db, &client, &season_id, &img_url, "Primary").await.is_ok() {
                downloaded += 1;
            }
        }
    }
    tracing::info!("Season images downloaded: {downloaded}/{total}");
}

fn parse_season_number(title: &str) -> Option<i64> {
    // "Season 1", "第1季", "S01", "season_1"
    let title_lower = title.to_ascii_lowercase();
    // Try "season 1"
    if let Some(pos) = title_lower.find("season") {
        let rest = &title_lower[pos + 6..];
        return rest.trim().split_whitespace().next()
            .and_then(|s| s.parse::<i64>().ok());
    }
    // Try "第1季"
    if let Some(pos) = title.find("第") {
        if let Some(end) = title[pos..].find("季") {
            let num = &title[pos + 3..pos + end];
            return num.parse::<i64>().ok();
        }
    }
    // Try "S01" at start
    if title_lower.starts_with('s') {
        let rest = &title_lower[1..];
        if let Some(end) = rest.find(|c: char| !c.is_ascii_digit()) {
            return rest[..end].parse::<i64>().ok();
        }
    }
    None
}

/// Batch fetch TMDb episode metadata for all episodes after scan completes
pub async fn batch_fetch_episode_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
) -> anyhow::Result<usize> {
    let backend = db.get_database_backend();
    tracing::info!("Starting episode TMDb batch fetch...");

    // First: download season images for all seasons
    download_season_images(db, api_key).await;

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

    // Deduplicate: only fetch each (tmdb_id, season, episode) once
    let mut seen = std::collections::HashSet::new();
    let mut tasks = Vec::new();

    for row in &rows {
        let Ok(episode_id) = row.get_str("episode_id") else { continue };
        let Ok(sn) = row.get_i64("season_number") else { continue };
        let Ok(en) = row.get_i64("episode_number") else { continue };
        let Ok(tmdb_id) = row.get_str("tmdb_id") else { continue };

        let key = format!("{tmdb_id}:{sn}:{en}");
        if !seen.insert(key) {
            continue;
        }
        tasks.push((episode_id, tmdb_id, sn, en));
    }

    let total = tasks.len();
    tracing::info!("Episode TMDb batch: {total} unique episodes to fetch");

    let client = build_client()?;
    let api_key = api_key.to_string();

    // Process in concurrent batches (10 at a time)
    let mut count = 0usize;
    for chunk in tasks.chunks(10) {
        let futures: Vec<_> = chunk.iter().map(|(ep_id, tmdb, sn, en)| {
            let api_key = api_key.clone();
            let ep_id = ep_id.clone();
            let tmdb = tmdb.clone();
            let sn = *sn;
            let en = *en;
            let client = client.clone();
            async move {
                let url = format!("https://api.themoviedb.org/3/tv/{tmdb}/season/{sn}/episode/{en}");
                #[derive(serde::Deserialize)]
                struct Ep { name: Option<String>, overview: Option<String>, still_path: Option<String> }
                let resp = client.get(&url).query(&[("api_key", api_key.as_str()), ("language", "zh-CN")]).send().await.ok()?;
                let resp = resp.error_for_status().ok()?;
                let ep: Ep = resp.json().await.ok()?;
                Some((ep, ep_id))
            }
        }).collect();

        let results: Vec<_> = futures_util::future::join_all(futures).await;
        for result in results.into_iter().flatten() {
            let (ep, episode_id) = result;
            let backend = db.get_database_backend();
            if let Some(ref name) = ep.name {
                if !name.is_empty() {
                    let _ = db.execute(crate::db::helpers::portable_statement(
                        backend, "UPDATE media_items SET title = ? WHERE id = ?",
                        vec![name.as_str().into(), episode_id.clone().into()],
                    )).await;
                }
            }
            if let Some(ref overview) = ep.overview {
                if !overview.is_empty() {
                    let _ = db.execute(crate::db::helpers::portable_statement(
                        backend, "UPDATE media_items SET overview = ? WHERE id = ?",
                        vec![overview.as_str().into(), episode_id.clone().into()],
                    )).await;
                }
            }
            if let Some(ref still) = ep.still_path {
                let img = format!("https://image.tmdb.org/t/p/w500{still}");
                let _ = download_and_save_tmdb_image(db, &client, &episode_id, &img, "Primary").await;
            }
            count += 1;
        }
        if count % 100 == 0 || (count > 0 && count < 100) {
            tracing::info!("Episode TMDb progress: {count}/{total}");
        }
    }

    tracing::info!("TMDb episode metadata fetched for {count} episodes");
    Ok(count)
}

/// Look up a stored TMDb ID from the provider_ids table for the given item
pub async fn lookup_stored_tmdb_id(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<String>> {
    let backend = db.get_database_backend();
    let row = db.query_one(crate::db::helpers::portable_statement(
        backend,
        "SELECT provider_item_id FROM provider_ids WHERE item_id = ? AND provider = 'Tmdb'",
        vec![item_id.into()],
    )).await?;
    Ok(row.and_then(|r| r.get_opt_str("provider_item_id").ok().flatten()))
}

/// Parse a folder name into (title, optional year) by cleaning provider tags
/// e.g. "X战警 (2000) {tmdb-36657}" → ("X战警", Some(2000))
fn parse_folder_name(path: &Path) -> Option<(String, Option<i64>)> {
    let name = path.file_name()?.to_str()?;
    let cleaned = clean_provider_tags(name);
    // Extract year from "(YYYY)" at the end or embedded
    let mut title = cleaned.clone();
    let mut year = None;
    // Try to find a 4-digit year in parentheses or brackets
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let Some(start) = cleaned.rfind(open) {
            if let Some(end) = cleaned[start..].find(close) {
                let inner = &cleaned[start + 1..start + end];
                // Check if it's purely a 4-digit year
                if let Ok(y) = inner.trim().parse::<i64>() {
                    if (1880..=2100).contains(&y) {
                        year = Some(y);
                        title = format!("{}{}", &cleaned[..start], &cleaned[start + end + 1..]);
                        break;
                    }
                }
            }
        }
    }
    // If no year found in parens, try to find any 4-digit year in the string
    if year.is_none() {
        for window in title.as_bytes().windows(4) {
            if let Ok(digits) = std::str::from_utf8(window) {
                if let Ok(y) = digits.parse::<i64>() {
                    if (1880..=2100).contains(&y) {
                        year = Some(y);
                        break;
                    }
                }
            }
        }
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some((title, year))
}

/// Search TMDb by name+year and return the first match's TMDb ID
async fn lookup_tmdb_id_by_name(
    client: &reqwest::Client,
    api_key: &str,
    name: &str,
    year: Option<i64>,
    is_tv: bool,
) -> anyhow::Result<Option<String>> {
    let url = if is_tv {
        "https://api.themoviedb.org/3/search/tv"
    } else {
        "https://api.themoviedb.org/3/search/movie"
    };
    let mut request = client.get(url).query(&[("api_key", api_key), ("query", name), ("language", "zh-CN")]);
    let year_param: String;
    if let Some(year) = year {
        year_param = year.to_string();
        let key = if is_tv { "first_air_date_year" } else { "year" };
        request = request.query(&[(key, year_param.as_str())]);
    }
    let response = request.send().await?.error_for_status()?.json::<serde_json::Value>().await?;
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        if let Some(first) = results.first() {
            return Ok(first.get("id").and_then(|v| v.as_i64()).map(|id| id.to_string()));
        }
    }
    Ok(None)
}

/// Fill in missing TMDb metadata for existing Movie/Series items by searching by name
pub async fn fill_missing_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
) -> anyhow::Result<usize> {
    let backend = db.get_database_backend();
    let rows = db.query_all(crate::db::helpers::portable_statement(
        backend,
        r#"SELECT mi.id, mi.title, mi.path, mi.item_type FROM media_items mi WHERE mi.is_folder = 1 AND mi.item_type IN ('Movie', 'Series') AND NOT EXISTS (SELECT 1 FROM provider_ids p WHERE p.item_id = mi.id AND p.provider = 'Tmdb')"#,
        vec![],
    )).await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let total = rows.len();
    tracing::info!("fill_missing_tmdb: {total} items need name-based TMDb lookup");

    let client = build_client()?;
    let mut count = 0usize;

    for row in &rows {
        let Ok(item_id) = row.get_str("id") else { continue };
        let title: String = row.get_str("title").unwrap_or_default();
        let item_type: String = row.get_str("item_type").unwrap_or_default();
        let path_str: String = row.get_str("path").unwrap_or_default();
        let is_tv = item_type == "Series";

        // Try to parse name from path first, fall back to title
        let (name, year) = parse_folder_name(Path::new(&path_str))
            .unwrap_or_else(|| (title.clone(), None));

        match lookup_tmdb_id_by_name(&client, api_key, &name, year, is_tv).await {
            Ok(Some(tmdb_id)) => {
                let _ = db.execute(crate::db::helpers::portable_statement(
                    backend,
                    "INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, 'Tmdb', ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id",
                    vec![item_id.clone().into(), tmdb_id.clone().into()],
                )).await;
                tracing::info!("fill_missing_tmdb: matched '{name}' → tmdb-{tmdb_id}");

                // Now fetch full metadata
                let _ = fetch_and_apply_tmdb_metadata(db, &item_id, &item_type, Path::new(&path_str), api_key).await;
                count += 1;
            }
            Ok(None) => {
                tracing::warn!("fill_missing_tmdb: no match for '{name}' (type: {item_type})");
            }
            Err(e) => {
                tracing::warn!("fill_missing_tmdb: search failed for '{name}': {e:#}");
            }
        }
    }

    tracing::info!("fill_missing_tmdb: filled {count}/{total} items");
    Ok(count)
}

/// Fetch TMDb metadata for a Series or Movie and store it in the database
pub async fn fetch_and_apply_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    item_type: &str,
    path: &Path,
    api_key: &str,
) -> anyhow::Result<()> {
    let is_tv = item_type == "Series" || item_type == "Season" || item_type == "Episode";

    // Step 1: check for existing TMDb ID already stored (e.g. from NFO parsing)
    let mut tmdb_id = lookup_stored_tmdb_id(db, item_id).await
        .ok().flatten()
        .or_else(|| extract_tmdb_id(path));

    // Step 2: name-based search fallback — only for Movie and Series (Season names like "Season 1" are not searchable)
    if tmdb_id.is_none() && (item_type == "Movie" || item_type == "Series") {
        if let Some((name, year)) = parse_folder_name(path) {
            let client = build_client()?;
            match lookup_tmdb_id_by_name(&client, api_key, &name, year, is_tv).await {
                Ok(Some(id)) => {
                    tmdb_id = Some(id);
                    // Store the found TMDb ID so next time we don't need to search
                    let backend = db.get_database_backend();
                    let _ = db.execute(crate::db::helpers::portable_statement(
                        backend,
                        "INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, 'Tmdb', ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id",
                        vec![item_id.into(), tmdb_id.as_ref().unwrap().into()],
                    )).await;
                    tracing::info!("TMDb name search matched '{name}' → tmdb-{}", tmdb_id.as_ref().unwrap());
                }
                Ok(None) => {
                    tracing::info!("TMDb name search found no results for '{name}'");
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("TMDb name search failed for '{name}': {e:#}");
                    return Ok(());
                }
            }
        }
    }
    let Some(tmdb_id) = tmdb_id else {
        return Ok(());
    };

    let client = build_client()?;
    let metadata = if is_tv {
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
    let people: Vec<(String, String, String, Option<String>)> = metadata
        .get("People")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some((
                        p.get("Name")?.as_str()?.to_string(),
                        p.get("Role")?.as_str()?.to_string(),
                        p.get("Type")?.as_str()?.to_string(),
                        p.get("ImageUrl").and_then(|v| v.as_str().filter(|s| !s.is_empty()).map(ToString::to_string)),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let people_with_images = people.iter().filter(|(_, _, _, img)| img.is_some()).count();
    if people_with_images > 0 {
        tracing::info!("Found {people_with_images} cast members with profile images for {item_type} {tmdb_id}");
    }

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
    // Update community_rating (TMDb vote_average is 0-10, store as-is)
    if let Some(rating) = metadata.get("CommunityRating").and_then(|v| v.as_f64()).filter(|r| *r > 0.0) {
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET community_rating = ? WHERE id = ?",
                vec![rating.into(), item_id.into()],
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
    for (name, role, person_type, image_url) in &people {
        let person_id = crate::util::stable_text_id(&format!("person:{name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![person_id.clone().into(), name.as_str().into(), now.into()],
            ))
            .await;
        let sort_order = people.iter().position(|(n, _, _, _)| n == name).unwrap_or(0) as i64;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = excluded.role WHERE excluded.role IS NOT NULL AND excluded.role <> ''",
                vec![
                    item_id.into(),
                    person_id.clone().into(),
                    role.as_str().into(),
                    Value::from(person_type.as_str()),
                    sort_order.into(),
                ],
            ))
            .await;
        // Download person profile image
        if let Some(img_url) = image_url {
            if let Err(e) = download_and_save_tmdb_image(db, &client, person_id.as_str(), img_url, "Primary").await {
                tracing::warn!("Failed to download person image for {name}: {e:#}");
            }
        }
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

/// Search TMDb for a person by name and return the first match's TMDb ID
pub async fn search_person_tmdb(name: &str, api_key: &str) -> anyhow::Result<Option<String>> {
    let client = build_client()?;
    let url = "https://api.themoviedb.org/3/search/person";
    #[derive(serde::Deserialize)]
    struct TmdbSearchResults {
        results: Vec<TmdbSearchPerson>,
    }
    #[derive(serde::Deserialize)]
    struct TmdbSearchPerson {
        id: i64,
    }
    let resp: TmdbSearchResults = client
        .get(url)
        .query(&[("api_key", api_key), ("query", name)])
        .send().await?
        .error_for_status()?
        .json().await?;
    Ok(resp.results.into_iter().next().map(|p| p.id.to_string()))
}

/// Fetch person details from TMDb and update the database
pub async fn fetch_person_tmdb(
    db: &sea_orm::DatabaseConnection,
    person_id: &str,
    tmdb_id: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    let client = build_client()?;
    let url = format!("https://api.themoviedb.org/3/person/{tmdb_id}");
    #[derive(serde::Deserialize)]
    struct TmdbPerson {
        biography: Option<String>,
    }
    let resp: TmdbPerson = client
        .get(&url)
        .query(&[("api_key", api_key)])
        .send().await?
        .error_for_status()?
        .json().await?;
    let backend = db.get_database_backend();
    if let Some(biography) = resp.biography.filter(|b| !b.is_empty()) {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE people SET overview = ?, tmdb_id = ? WHERE id = ?",
            vec![biography.into(), tmdb_id.into(), person_id.into()],
        )).await?;
    } else {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE people SET tmdb_id = ? WHERE id = ?",
            vec![tmdb_id.into(), person_id.into()],
        )).await?;
    }
    // Also try to fetch TMDb image
    let img_url = format!("https://api.themoviedb.org/3/person/{tmdb_id}/images?api_key={api_key}");
    #[derive(serde::Deserialize)]
    struct TmdbPersonImages {
        profiles: Vec<TmdbPersonProfile>,
    }
    #[derive(serde::Deserialize)]
    struct TmdbPersonProfile {
        file_path: String,
    }
    if let Ok(resp) = client.get(&img_url).send().await.and_then(|r| r.error_for_status()) {
        if let Ok(images) = resp.json::<TmdbPersonImages>().await {
            if let Some(img) = images.profiles.first() {
                let img_url = format!("https://image.tmdb.org/t/p/w780{}", img.file_path);
                let _ = download_and_save_tmdb_image(db, &client, person_id, &img_url, "Primary").await;
            }
        }
    }
    Ok(())
}

/// Try to find and fetch TMDb metadata for a person by name
pub async fn try_fetch_person_tmdb(
    db: &sea_orm::DatabaseConnection,
    person_id: &str,
    person_name: &str,
    api_key: &str,
) {
    if api_key.is_empty() {
        return;
    }
    match search_person_tmdb(person_name, api_key).await {
        Ok(Some(tmdb_id)) => {
            let _ = fetch_person_tmdb(db, person_id, &tmdb_id, api_key).await;
        }
        Ok(None) => {}
        Err(e) => tracing::debug!("failed to search TMDb for person {person_name}: {e:#}"),
    }
}

/// Batch fetch TMDb metadata for people without biography.
/// Returns the number of people updated.
pub async fn batch_fetch_person_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
) -> anyhow::Result<usize> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, name FROM people WHERE (overview IS NULL OR tmdb_id IS NULL) AND name IS NOT NULL AND name <> '' LIMIT 50",
            vec![],
        ))
        .await
        .context("failed to list people without biography")?;

    let mut count = 0;
    for row in &rows {
        let id: String = row.get_str("id")?;
        let name: String = row.get_str("name")?;
        let _ = try_fetch_person_tmdb(db, &id, &name, api_key).await;
        count += 1;
    }
    Ok(count)
}
