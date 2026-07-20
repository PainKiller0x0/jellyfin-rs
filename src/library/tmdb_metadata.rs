use std::{collections::HashMap, path::Path, sync::OnceLock};

use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection, Value};
use tokio::sync::Mutex;

use crate::{
    db::row_ext::QueryResultExt,
    jellyfin::providers,
    tmdb,
    util::{normalize_yyyy_mm_dd, now_unix, year_from_yyyy_mm_dd},
};

static EPISODE_TMDB_BATCH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const TMDB_API_KEY_QUERY: &str = "api_key=";
const TMDB_TITLE_BACKFILL_KEY: &str = "tmdb_title_backfill_v1_completed";
const MAX_METADATA_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Extract TMDb ID from `{tmdb-XXXXX}`, `{tmdbid-XXXXX}`, or `[tmdbid=XXXXX]` in the path
pub fn extract_tmdb_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        let Some((_, after_open)) = name.rsplit_once(open) else {
            continue;
        };
        let Some((tag, _)) = after_open.split_once(close) else {
            continue;
        };
        if let Some(id) = tmdb_id_from_tag(tag) {
            return Some(id);
        }
    }
    None
}

pub(crate) fn redact_tmdb_error(error: &anyhow::Error) -> String {
    redact_tmdb_api_key(&format!("{error:#}"))
}

fn redact_tmdb_api_key(message: &str) -> String {
    let mut redacted = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(index) = rest.find(TMDB_API_KEY_QUERY) {
        let value_start = index + TMDB_API_KEY_QUERY.len();
        redacted.push_str(&rest[..value_start]);
        redacted.push_str("<redacted>");
        let value = &rest[value_start..];
        let value_end = value
            .find(|ch: char| matches!(ch, '&' | ')' | ' ' | '\n' | '\r'))
            .unwrap_or(value.len());
        rest = &value[value_end..];
    }
    redacted.push_str(rest);
    redacted
}

fn tmdb_id_from_tag(tag: &str) -> Option<String> {
    let tag = tag.trim();
    let lower = tag.to_ascii_lowercase();
    for prefix in ["tmdb-", "tmdbid-", "tmdbid="] {
        if lower.starts_with(prefix) {
            let id = tag[prefix.len()..].trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Clean title by removing `{tmdb-XXXXX}`, `{tmdbid-XXXXX}`, `[tmdbid=XXXXX]`, and `(YYYY)` tags
pub fn clean_provider_tags(title: &str) -> String {
    let mut result = title.to_string();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start < end {
                let tag = &result[start + 1..end].to_ascii_lowercase();
                if tag.starts_with("tmdb-")
                    || tag.starts_with("tmdbid-")
                    || tag.starts_with("tmdbid=")
                {
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

    let cleaned = result
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    (cleaned, year)
}

/// Download metadata and poster images for all seasons that belong to a TMDb series.
async fn download_season_images(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT s.id as season_id,
                      s.title,
                      s.season_number,
                      p.provider_item_id as series_tmdb_id,
                      CASE WHEN EXISTS (
                          SELECT 1 FROM image_assets ia
                          WHERE ia.item_id = s.id AND ia.image_type = 'Primary'
                      ) THEN 1 ELSE 0 END AS has_primary
           FROM media_items s
           JOIN media_items series ON series.id = s.parent_id
           JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
           LEFT JOIN provider_ids sp ON sp.item_id = s.id AND sp.provider = 'Tmdb'
           WHERE s.item_type = 'Season'
             AND (
                 sp.provider_item_id IS NULL
                 OR s.season_number IS NULL
                 OR s.production_year IS NULL
                 OR s.premiere_date IS NULL
                 OR NOT EXISTS (
                     SELECT 1 FROM image_assets ia
                     WHERE ia.item_id = s.id AND ia.image_type = 'Primary'
                 )
             )"#,
            vec![],
        ))
        .await;
    let Ok(rows) = rows else { return };
    if rows.is_empty() {
        return;
    };

    let total = rows.len();
    tracing::info!("Refreshing missing TMDb metadata for {total} season(s)...");

    let mut downloaded = 0usize;

    for chunk in rows.chunks(10) {
        let futures: Vec<_> = chunk
            .iter()
            .map(|row| {
                let client = client;
                async move {
                    let Ok(season_id) = row.get_str("season_id") else {
                        return None;
                    };
                    let title: String = row.get_str("title").ok()?;
                    let stored_season_number = row.get_opt_i64("season_number").ok().flatten();
                    let Ok(series_tmdb) = row.get_str("series_tmdb_id") else {
                        return None;
                    };
                    let has_primary = row.get_i64("has_primary").unwrap_or(0) != 0;
                    let sn = stored_season_number
                        .or_else(|| parse_season_number(&title))
                        .unwrap_or(0);
                    let url = crate::tmdb::api_url(
                        tmdb_base_url,
                        &format!("tv/{series_tmdb}/season/{sn}"),
                    );
                    let resp = client
                        .get(&url)
                        .query(&[("api_key", api_key), ("language", "zh-CN")])
                        .send()
                        .await
                        .ok()?;
                    let resp = resp.error_for_status().ok()?;
                    let season: SeasonTmdbResponse = resp.json().await.ok()?;
                    Some((season_id, sn, has_primary, season))
                }
            })
            .collect();

        let results = futures_util::future::join_all(futures).await;
        for (season_id, season_number, has_primary, season) in results.into_iter().flatten() {
            if apply_season_tmdb_response(
                db,
                client,
                tmdb_base_url,
                &season_id,
                season_number,
                has_primary,
                &season,
            )
            .await
            .unwrap_or(false)
            {
                downloaded += 1;
            }
        }
    }
    tracing::info!("Season images downloaded: {downloaded}/{total}");
}

pub(crate) fn parse_season_number(title: &str) -> Option<i64> {
    // "Season 1", "第1季", "S01", "season_1"
    let title_lower = title.to_ascii_lowercase();
    if let Some(pos) = title_lower.find("season") {
        let rest = &title_lower[pos + 6..];
        if let Some(number) = first_ascii_number(rest) {
            return Some(number);
        }
    }
    if let Some(rest) = title.split_once('第').map(|(_, rest)| rest) {
        if let Some((number, _)) = rest.split_once('季') {
            if let Ok(number) = number.trim().parse::<i64>() {
                return Some(number);
            }
        }
    }
    if let Some(rest) = title_lower.strip_prefix('s') {
        if let Some(number) = first_ascii_number(rest) {
            return Some(number);
        }
    }
    None
}

fn first_ascii_number(value: &str) -> Option<i64> {
    let digits: String = value
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i64>().ok()
    }
}

#[derive(Clone, Debug)]
struct EpisodeTmdbTarget {
    episode_id: String,
    current_title: String,
    path: String,
    series_title: String,
    series_year: Option<i64>,
    episode_number: i64,
}

#[derive(Clone, Debug)]
struct EpisodeTmdbTask {
    tmdb_id: String,
    season_number: i64,
    episode_number: i64,
    targets: Vec<EpisodeTmdbTarget>,
}

#[derive(serde::Deserialize)]
struct EpisodeTmdbResponse {
    id: Option<i64>,
    name: Option<String>,
    overview: Option<String>,
    still_path: Option<String>,
    air_date: Option<String>,
    vote_average: Option<f64>,
}

#[derive(serde::Deserialize)]
struct SeasonTmdbResponse {
    id: Option<i64>,
    poster_path: Option<String>,
    name: Option<String>,
    overview: Option<String>,
    air_date: Option<String>,
}

async fn normalize_episode_years_from_premiere_dates(db: &sea_orm::DatabaseConnection) {
    match db
        .execute(crate::db::helpers::pg_statement(
            r#"UPDATE media_items
               SET production_year = CAST(SUBSTRING(premiere_date FROM 1 FOR 4) AS BIGINT),
                   updated_at = ?
               WHERE item_type = 'Episode'
                 AND premiere_date ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
                 AND (
                     production_year IS NULL
                     OR production_year <> CAST(SUBSTRING(premiere_date FROM 1 FOR 4) AS BIGINT)
                 )"#,
            vec![now_unix().into()],
        ))
        .await
    {
        Ok(result) if result.rows_affected() > 0 => {
            tracing::info!(
                count = result.rows_affected(),
                "normalized episode production years from premiere dates"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!("failed to normalize episode production years: {error:#}");
        }
    }
}

pub async fn fetch_and_apply_season_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT s.id AS season_id,
                      s.title,
                      s.path,
                      s.season_number,
                      s.production_year,
                      s.premiere_date,
                      p.provider_item_id AS series_tmdb_id,
                      sp.provider_item_id AS season_tmdb_id,
                      CASE WHEN EXISTS (
                          SELECT 1 FROM image_assets ia
                          WHERE ia.item_id = s.id AND ia.image_type = 'Primary'
                      ) THEN 1 ELSE 0 END AS has_primary
               FROM media_items s
               LEFT JOIN media_items series ON series.id = s.parent_id
               LEFT JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
               LEFT JOIN provider_ids sp ON sp.item_id = s.id AND sp.provider = 'Tmdb'
               WHERE s.id = ? AND s.item_type = 'Season'"#,
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(true);
    };

    let Some(series_tmdb_id) = row.get_opt_str("series_tmdb_id").ok().flatten() else {
        return Ok(false);
    };

    let title = row.get_str("title").unwrap_or_default();
    let path = row.get_str("path").unwrap_or_default();
    let stored_season_number = row.get_opt_i64("season_number").ok().flatten();
    let Some(season_number) = stored_season_number
        .or_else(|| parse_season_number(&title))
        .or_else(|| extract_season_number(Path::new(&path)))
    else {
        tracing::debug!("TMDb season fetch skipped because season number is unknown for {item_id}");
        return Ok(true);
    };

    let has_primary = row.get_i64("has_primary").unwrap_or(0) != 0;
    let season_is_current = row.get_opt_str("season_tmdb_id").ok().flatten().is_some()
        && stored_season_number.is_some()
        && row.get_opt_i64("production_year").ok().flatten().is_some()
        && row.get_opt_str("premiere_date").ok().flatten().is_some()
        && has_primary;
    if season_is_current {
        return Ok(true);
    }

    let url = tmdb::api_url(
        tmdb_base_url,
        &format!("tv/{series_tmdb_id}/season/{season_number}"),
    );
    let season: SeasonTmdbResponse = client
        .get(&url)
        .query(&[("api_key", api_key), ("language", "zh-CN")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    apply_season_tmdb_response(
        db,
        client,
        tmdb_base_url,
        item_id,
        season_number,
        has_primary,
        &season,
    )
    .await?;
    Ok(true)
}

async fn apply_season_tmdb_response(
    db: &sea_orm::DatabaseConnection,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    season_id: &str,
    season_number: i64,
    has_primary: bool,
    season: &SeasonTmdbResponse,
) -> anyhow::Result<bool> {
    let premiere_date = season.air_date.as_deref().and_then(normalize_yyyy_mm_dd);
    let production_year = premiere_date.as_deref().and_then(year_from_yyyy_mm_dd);
    let mut primary_downloaded = false;
    if let Some(tmdb_season_id) = season.id {
        crate::db::provider_ids::upsert(db, season_id, "Tmdb", &tmdb_season_id.to_string()).await?;
    }
    if let Some(name) = season
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET title = ?, season_number = ?, production_year = COALESCE(?, production_year), premiere_date = COALESCE(?, premiere_date), updated_at = ? WHERE id = ?",
            vec![
                name.into(),
                season_number.into(),
                production_year.into(),
                premiere_date.as_deref().into(),
                now_unix().into(),
                season_id.into(),
            ],
        ))
        .await?;
    } else {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET season_number = ?, production_year = COALESCE(?, production_year), premiere_date = COALESCE(?, premiere_date), updated_at = ? WHERE id = ?",
            vec![
                season_number.into(),
                production_year.into(),
                premiere_date.as_deref().into(),
                now_unix().into(),
                season_id.into(),
            ],
        ))
        .await?;
    }
    if let Some(overview) = season
        .overview
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET overview = ?, updated_at = ? WHERE id = ?",
            vec![overview.into(), now_unix().into(), season_id.into()],
        ))
        .await?;
    }
    if !has_primary {
        if let Some(poster_path) = season.poster_path.as_deref() {
            let img_url = tmdb::image_url(tmdb_base_url, "w500", poster_path);
            if let Err(error) =
                download_and_save_tmdb_image(db, client, season_id, &img_url, "Primary").await
            {
                tracing::warn!("failed to download season image for {season_id}: {error:#}");
            } else {
                primary_downloaded = true;
            }
        }
    }
    Ok(primary_downloaded)
}

pub async fn fetch_and_apply_episode_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT e.id AS episode_id,
                      e.title AS episode_title,
                      e.path AS episode_path,
                      COALESCE(e.season_number, se.season_number) AS season_number,
                      e.episode_number,
                      e.production_year,
                      e.premiere_date,
                      s.title AS series_title,
                      s.production_year AS series_year,
                      p.provider_item_id AS tmdb_id,
                      ep.provider_item_id AS episode_tmdb_id
               FROM media_items e
               LEFT JOIN media_items se ON se.id = e.parent_id
               LEFT JOIN media_items s ON s.id = se.parent_id
               LEFT JOIN provider_ids p ON p.item_id = s.id AND p.provider = 'Tmdb'
               LEFT JOIN provider_ids ep ON ep.item_id = e.id AND ep.provider = 'Tmdb'
               WHERE e.id = ? AND e.item_type = 'Episode'"#,
            vec![item_id.into()],
        ))
        .await?
    else {
        return Ok(true);
    };

    let Some(tmdb_id) = row.get_opt_str("tmdb_id").ok().flatten() else {
        return Ok(false);
    };
    let Some(season_number) = row.get_opt_i64("season_number").ok().flatten() else {
        tracing::debug!(
            "TMDb episode fetch skipped because season number is unknown for {item_id}"
        );
        return Ok(true);
    };
    let Some(episode_number) = row.get_opt_i64("episode_number").ok().flatten() else {
        tracing::debug!(
            "TMDb episode fetch skipped because episode number is unknown for {item_id}"
        );
        return Ok(true);
    };

    let current_title = row.get_str("episode_title").unwrap_or_default();
    let series_title = row.get_str("series_title").unwrap_or_default();
    let series_year = row.get_opt_i64("series_year").ok().flatten();
    let episode_is_current = row.get_opt_str("episode_tmdb_id").ok().flatten().is_some()
        && row.get_opt_i64("production_year").ok().flatten().is_some()
        && row.get_opt_str("premiere_date").ok().flatten().is_some()
        && !is_generic_series_episode_title(&current_title, &series_title, series_year);
    if episode_is_current {
        return Ok(true);
    }

    let url = tmdb::api_url(
        tmdb_base_url,
        &format!("tv/{tmdb_id}/season/{season_number}/episode/{episode_number}"),
    );
    let episode: EpisodeTmdbResponse = client
        .get(&url)
        .query(&[("api_key", api_key), ("language", "zh-CN")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let target = EpisodeTmdbTarget {
        episode_id: row.get_str("episode_id")?,
        current_title,
        path: row.get_str("episode_path").unwrap_or_default(),
        series_title,
        series_year,
        episode_number,
    };
    apply_episode_tmdb_response(db, client, tmdb_base_url, &episode, &target).await?;
    Ok(true)
}

async fn apply_episode_tmdb_response(
    db: &sea_orm::DatabaseConnection,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    episode: &EpisodeTmdbResponse,
    target: &EpisodeTmdbTarget,
) -> anyhow::Result<()> {
    if let Some(tmdb_episode_id) = episode.id {
        crate::db::provider_ids::upsert(
            db,
            &target.episode_id,
            "Tmdb",
            &tmdb_episode_id.to_string(),
        )
        .await?;
    }

    if let Some(title) = episode_title_candidate(
        episode.name.as_deref(),
        &target.current_title,
        &target.series_title,
        target.series_year,
        target.episode_number,
        &target.path,
    ) {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET title = ?, updated_at = ? WHERE id = ?",
            vec![
                title.into(),
                now_unix().into(),
                target.episode_id.as_str().into(),
            ],
        ))
        .await?;
    }

    if let Some(overview) = episode
        .overview
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET overview = ?, updated_at = ? WHERE id = ?",
            vec![
                overview.into(),
                now_unix().into(),
                target.episode_id.as_str().into(),
            ],
        ))
        .await?;
    }

    let premiere_date = episode.air_date.as_deref().and_then(normalize_yyyy_mm_dd);
    let production_year = premiere_date.as_deref().and_then(year_from_yyyy_mm_dd);
    let community_rating = episode.vote_average.filter(|rating| *rating > 0.0);
    if production_year.is_some() || premiere_date.is_some() || community_rating.is_some() {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET production_year = COALESCE(?, production_year), premiere_date = COALESCE(?, premiere_date), community_rating = COALESCE(?, community_rating), updated_at = ? WHERE id = ?",
            vec![
                production_year.into(),
                premiere_date.as_deref().into(),
                community_rating.into(),
                now_unix().into(),
                target.episode_id.as_str().into(),
            ],
        ))
        .await?;
    }

    if let Some(still) = episode.still_path.as_deref() {
        let img = tmdb::image_url(tmdb_base_url, "w500", still);
        if let Err(error) =
            download_and_save_tmdb_image(db, client, &target.episode_id, &img, "Primary").await
        {
            tracing::warn!(
                "failed to download episode image for {}: {error:#}",
                target.episode_id
            );
        }
    }
    Ok(())
}

/// Batch fetch missing TMDb episode metadata for existing rows.
pub async fn batch_fetch_episode_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<usize> {
    let lock = EPISODE_TMDB_BATCH_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.try_lock() else {
        tracing::info!("Episode TMDb batch already running; skipping overlapping request");
        return Ok(0);
    };

    tracing::info!("Starting episode TMDb batch fetch...");

    normalize_episode_years_from_premiere_dates(db).await;

    // First: download season images for all seasons
    download_season_images(db, api_key, client, tmdb_base_url).await;

    let rows = db.query_all(crate::db::helpers::pg_statement(
        r#"SELECT e.id as episode_id,
                  e.title as episode_title,
                  e.path as episode_path,
                  e.season_number,
                  e.episode_number,
                  s.title as series_title,
                  s.production_year as series_year,
                  p.provider_item_id as tmdb_id
           FROM media_items e
           JOIN media_items se ON se.id = e.parent_id
           JOIN media_items s ON s.id = se.parent_id
           JOIN provider_ids p ON p.item_id = s.id AND p.provider = 'Tmdb'
           LEFT JOIN provider_ids ep ON ep.item_id = e.id AND ep.provider = 'Tmdb'
           WHERE e.item_type = 'Episode'
             AND e.season_number IS NOT NULL
             AND e.episode_number IS NOT NULL
             AND (
                 ep.provider_item_id IS NULL
                 OR e.production_year IS NULL
                 OR e.premiere_date IS NULL
                 OR (
                     e.production_year <> CAST(SUBSTRING(e.premiere_date FROM 1 FOR 4) AS BIGINT)
                 )
                 OR e.title = s.title
                 OR (s.production_year IS NOT NULL AND e.title = CONCAT(s.title, ' ', s.production_year))
                 OR (e.production_year IS NOT NULL AND e.title = CONCAT(s.title, ' ', e.production_year))
             )"#,
        vec![],
    )).await?;

    // Fetch each (series TMDb, season, episode) once, then apply the result to
    // every local file/version for that episode.
    let mut tasks_by_key: HashMap<String, EpisodeTmdbTask> = HashMap::new();

    for row in &rows {
        let Ok(episode_id) = row.get_str("episode_id") else {
            continue;
        };
        let episode_title: String = row.get_str("episode_title").unwrap_or_default();
        let episode_path: String = row.get_str("episode_path").unwrap_or_default();
        let Ok(sn) = row.get_i64("season_number") else {
            continue;
        };
        let Ok(en) = row.get_i64("episode_number") else {
            continue;
        };
        let series_title: String = row.get_str("series_title").unwrap_or_default();
        let series_year = row.get_opt_i64("series_year").ok().flatten();
        let Ok(tmdb_id) = row.get_str("tmdb_id") else {
            continue;
        };

        let key = format!("{tmdb_id}:{sn}:{en}");
        let target = EpisodeTmdbTarget {
            episode_id,
            current_title: episode_title,
            path: episode_path,
            series_title,
            series_year,
            episode_number: en,
        };
        tasks_by_key
            .entry(key)
            .and_modify(|task| task.targets.push(target.clone()))
            .or_insert_with(|| EpisodeTmdbTask {
                tmdb_id,
                season_number: sn,
                episode_number: en,
                targets: vec![target],
            });
    }

    let tasks = tasks_by_key.into_values().collect::<Vec<_>>();
    let total = tasks.len();
    tracing::info!("Episode TMDb batch: {total} missing unique episodes to fetch");

    let api_key = api_key.to_string();

    // Process in concurrent batches (10 at a time)
    let mut count = 0usize;
    for chunk in tasks.chunks(10) {
        let futures: Vec<_> = chunk
            .iter()
            .map(|task| {
                let api_key = api_key.clone();
                let task = task.clone();
                let client = (*client).clone();
                async move {
                    let url = crate::tmdb::api_url(
                        tmdb_base_url,
                        &format!(
                            "tv/{}/season/{}/episode/{}",
                            task.tmdb_id, task.season_number, task.episode_number
                        ),
                    );
                    let resp = client
                        .get(&url)
                        .query(&[("api_key", api_key.as_str()), ("language", "zh-CN")])
                        .send()
                        .await
                        .ok()?;
                    let resp = resp.error_for_status().ok()?;
                    let ep: EpisodeTmdbResponse = resp.json().await.ok()?;
                    Some((ep, task))
                }
            })
            .collect();

        let results: Vec<_> = futures_util::future::join_all(futures).await;
        for result in results.into_iter().flatten() {
            let (ep, task) = result;
            for target in &task.targets {
                if let Err(error) =
                    apply_episode_tmdb_response(db, client, tmdb_base_url, &ep, target).await
                {
                    tracing::warn!(
                        "failed to apply episode TMDb metadata for {}: {error:#}",
                        target.episode_id
                    );
                }
            }
            count += 1;
        }
        if count % 10 == 0 || count == total {
            tracing::info!("Episode TMDb progress: {count}/{total}");
        }
    }

    tracing::info!("TMDb episode metadata fetched for {count} episodes");
    Ok(count)
}

fn episode_title_candidate(
    tmdb_name: Option<&str>,
    current_title: &str,
    series_title: &str,
    series_year: Option<i64>,
    episode_number: i64,
    path: &str,
) -> Option<String> {
    if let Some(name) = tmdb_name.map(str::trim).filter(|name| !name.is_empty()) {
        if !is_generic_series_episode_title(name, series_title, series_year) {
            return Some(name.to_string());
        }
    }

    if is_generic_series_episode_title(current_title, series_title, series_year) {
        return local_episode_title_from_path(path)
            .filter(|title| !is_generic_series_episode_title(title, series_title, series_year))
            .or_else(|| Some(format!("Episode {episode_number}")));
    }

    None
}

fn is_generic_series_episode_title(
    title: &str,
    series_title: &str,
    series_year: Option<i64>,
) -> bool {
    let title = compact_title_for_compare(title);
    let series_title = compact_title_for_compare(series_title);
    if title.is_empty() || series_title.is_empty() {
        return false;
    }
    if title == series_title {
        return true;
    }
    series_year
        .map(|year| title == format!("{series_title}{year}"))
        .unwrap_or(false)
}

fn compact_title_for_compare(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn local_episode_title_from_path(path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_str()?;
    let marker = find_episode_marker_end(stem)?;
    let tail = stem[marker..].trim_matches(|c: char| c.is_whitespace() || ".-_[]()".contains(c));
    let mut parts = Vec::new();
    for part in tail.split(['.', '_', '-']) {
        let part = part.trim_matches(|c: char| c.is_whitespace() || "[]()".contains(c));
        if part.is_empty() {
            continue;
        }
        if looks_like_technical_video_token(part) {
            break;
        }
        parts.push(part);
    }
    let title = parts.join(" ").trim().to_string();
    (!title.is_empty()).then_some(title)
}

fn find_episode_marker_end(stem: &str) -> Option<usize> {
    let bytes = stem.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index].eq_ignore_ascii_case(&b's') {
            let mut cursor = index + 1;
            let season_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor == season_start
                || cursor >= bytes.len()
                || !bytes[cursor].eq_ignore_ascii_case(&b'e')
            {
                index += 1;
                continue;
            }
            cursor += 1;
            let episode_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor > episode_start {
                return Some(cursor);
            }
        }
        index += 1;
    }
    None
}

fn looks_like_technical_video_token(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|c: char| "[]()".contains(c))
        .to_ascii_lowercase();
    if value.is_empty() {
        return true;
    }
    if value.ends_with('p') && value[..value.len() - 1].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if value.ends_with("fps") {
        return true;
    }
    matches!(
        value.as_str(),
        "web"
            | "webdl"
            | "web-dl"
            | "webrip"
            | "bluray"
            | "bdrip"
            | "hdr"
            | "hdr10"
            | "hdr10+"
            | "sdr"
            | "dv"
            | "hevc"
            | "h264"
            | "h265"
            | "x264"
            | "x265"
            | "avc"
            | "aac"
            | "hiveweb"
            | "pure@hiveweb"
    ) || value.contains("bit")
        || value.starts_with("h.")
}

/// Look up a stored TMDb ID from the provider_ids table for the given item
pub async fn lookup_stored_tmdb_id(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<String>> {
    crate::db::provider_ids::get(db, item_id, "Tmdb").await
}

async fn fetch_tmdb_display_name(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    tmdb_id: &str,
    item_type: &str,
) -> anyhow::Result<Option<String>> {
    let (endpoint, name_key) = match item_type {
        "Series" => (format!("tv/{tmdb_id}"), "name"),
        "Movie" => (format!("movie/{tmdb_id}"), "title"),
        _ => return Ok(None),
    };
    let response = client
        .get(tmdb::api_url(tmdb_base_url, &endpoint))
        .query(&[("api_key", api_key), ("language", "zh-CN")])
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    Ok(response
        .get(name_key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string))
}

async fn app_setting_is_true(db: &DatabaseConnection, key: &str) -> anyhow::Result<bool> {
    crate::db::settings::is_true(db, key).await
}

async fn set_app_setting(db: &DatabaseConnection, key: &str, value: &str) -> anyhow::Result<()> {
    crate::db::settings::set(db, key, value).await
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
    let title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    Some((title, year))
}

/// Search TMDb by name+year and return the first match's TMDb ID
async fn lookup_tmdb_id_by_name(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    name: &str,
    year: Option<i64>,
    is_tv: bool,
) -> anyhow::Result<Option<String>> {
    let url = tmdb::api_url(
        tmdb_base_url,
        if is_tv { "search/tv" } else { "search/movie" },
    );
    let mut request =
        client
            .get(&url)
            .query(&[("api_key", api_key), ("query", name), ("language", "zh-CN")]);
    let year_param: String;
    if let Some(year) = year {
        year_param = year.to_string();
        let key = if is_tv { "first_air_date_year" } else { "year" };
        request = request.query(&[(key, year_param.as_str())]);
    }
    let response = request
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    if let Some(results) = response.get("results").and_then(|v| v.as_array()) {
        if let Some(first) = results.first() {
            return Ok(first
                .get("id")
                .and_then(|v| v.as_i64())
                .map(|id| id.to_string()));
        }
    }
    Ok(None)
}

/// Fill in missing TMDb metadata for existing Movie/Series items by searching by name
pub async fn fill_missing_tmdb(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<usize> {
    let rows = db.query_all(crate::db::helpers::pg_statement(
        r#"SELECT mi.id, mi.title, mi.path, mi.item_type FROM media_items mi WHERE mi.is_folder = 1 AND mi.item_type IN ('Movie', 'Series') AND NOT EXISTS (SELECT 1 FROM provider_ids p WHERE p.item_id = mi.id AND p.provider = 'Tmdb')"#,
        vec![],
    )).await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let total = rows.len();
    tracing::info!("fill_missing_tmdb: {total} items need name-based TMDb lookup");

    let mut count = 0usize;

    for row in &rows {
        let Ok(item_id) = row.get_str("id") else {
            continue;
        };
        let title: String = row.get_str("title").unwrap_or_default();
        let item_type: String = row.get_str("item_type").unwrap_or_default();
        let path_str: String = row.get_str("path").unwrap_or_default();
        let is_tv = item_type == "Series";

        // Try to parse name from path first, fall back to title
        let (name, year) =
            parse_folder_name(Path::new(&path_str)).unwrap_or_else(|| (title.clone(), None));
        if should_skip_name_based_tmdb_lookup(&name) {
            tracing::debug!("fill_missing_tmdb: skipped generic folder name '{name}'");
            continue;
        }

        match lookup_tmdb_id_by_name(client, api_key, tmdb_base_url, &name, year, is_tv).await {
            Ok(Some(tmdb_id)) => {
                let _ = crate::db::provider_ids::upsert(db, &item_id, "Tmdb", &tmdb_id).await;
                tracing::info!("fill_missing_tmdb: matched '{name}' → tmdb-{tmdb_id}");

                // Now fetch full metadata
                let _ = fetch_and_apply_tmdb_metadata(
                    db,
                    &item_id,
                    &item_type,
                    Path::new(&path_str),
                    api_key,
                    client,
                    tmdb_base_url,
                )
                .await;
                count += 1;
            }
            Ok(None) => {
                tracing::warn!("fill_missing_tmdb: no match for '{name}' (type: {item_type})");
            }
            Err(e) => {
                tracing::warn!(
                    "fill_missing_tmdb: search failed for '{name}': {}",
                    redact_tmdb_error(&e)
                );
            }
        }
    }

    tracing::info!("fill_missing_tmdb: filled {count}/{total} items");
    Ok(count)
}

/// One-time backfill for libraries created before TMDb scraping updated item titles.
pub async fn refresh_existing_tmdb_titles(
    db: &sea_orm::DatabaseConnection,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<usize> {
    if app_setting_is_true(db, TMDB_TITLE_BACKFILL_KEY).await? {
        return Ok(0);
    }

    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT mi.id, mi.title, mi.item_type, p.provider_item_id AS tmdb_id
               FROM media_items mi
               JOIN provider_ids p ON p.item_id = mi.id AND p.provider = 'Tmdb'
               WHERE mi.item_type IN ('Movie', 'Series')"#,
            vec![],
        ))
        .await?;

    let total = rows.len();
    if total == 0 {
        set_app_setting(db, TMDB_TITLE_BACKFILL_KEY, "true").await?;
        return Ok(0);
    }

    tracing::info!("refresh_existing_tmdb_titles: checking {total} TMDb item title(s)");
    let mut updated = 0usize;
    let mut failed = 0usize;
    for row in &rows {
        let item_id = match row.get_str("id") {
            Ok(value) => value,
            Err(_) => continue,
        };
        let current_title = row.get_str("title").unwrap_or_default();
        let item_type = row.get_str("item_type").unwrap_or_default();
        let tmdb_id = match row.get_str("tmdb_id") {
            Ok(value) => value,
            Err(_) => continue,
        };

        match fetch_tmdb_display_name(client, api_key, tmdb_base_url, &tmdb_id, &item_type).await {
            Ok(Some(title)) if title != current_title => {
                db.execute(crate::db::helpers::pg_statement(
                    "UPDATE media_items SET title = ?, updated_at = ? WHERE id = ?",
                    vec![title.as_str().into(), now_unix().into(), item_id.into()],
                ))
                .await?;
                updated += 1;
            }
            Ok(_) => {}
            Err(error) => {
                failed += 1;
                tracing::warn!(
                    "refresh_existing_tmdb_titles: failed for {item_id} tmdb-{tmdb_id}: {}",
                    redact_tmdb_error(&error)
                );
            }
        }
    }

    if failed == 0 {
        set_app_setting(db, TMDB_TITLE_BACKFILL_KEY, "true").await?;
    } else {
        tracing::warn!(
            "refresh_existing_tmdb_titles: {failed} title(s) failed; will retry on next startup"
        );
    }
    tracing::info!("refresh_existing_tmdb_titles: updated {updated}/{total} title(s)");
    Ok(updated)
}

/// Fetch TMDb metadata for a Series or Movie and store it in the database
pub async fn fetch_and_apply_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    item_type: &str,
    path: &Path,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<()> {
    let is_tv = item_type == "Series" || item_type == "Season" || item_type == "Episode";

    // Step 1: check for existing TMDb ID already stored (e.g. from NFO parsing)
    let mut tmdb_id = lookup_stored_tmdb_id(db, item_id)
        .await
        .ok()
        .flatten()
        .or_else(|| extract_tmdb_id(path));

    // Step 2: name-based search fallback — only for Movie and Series (Season names like "Season 1" are not searchable)
    if tmdb_id.is_none() && (item_type == "Movie" || item_type == "Series") {
        if let Some((name, year)) = parse_folder_name(path) {
            if should_skip_name_based_tmdb_lookup(&name) {
                tracing::debug!("TMDb name search skipped generic folder name '{name}'");
                return Ok(());
            }
            match lookup_tmdb_id_by_name(client, api_key, tmdb_base_url, &name, year, is_tv).await {
                Ok(Some(id)) => {
                    tmdb_id = Some(id);
                    // Store the found TMDb ID so next time we don't need to search
                    let _ = crate::db::provider_ids::upsert(
                        db,
                        item_id,
                        "Tmdb",
                        tmdb_id.as_ref().unwrap(),
                    )
                    .await;
                    tracing::info!(
                        "TMDb name search matched '{name}' → tmdb-{}",
                        tmdb_id.as_ref().unwrap()
                    );
                }
                Ok(None) => {
                    tracing::info!("TMDb name search found no results for '{name}'");
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        "TMDb name search failed for '{name}': {}",
                        redact_tmdb_error(&e)
                    );
                    return Ok(());
                }
            }
        }
    }
    let Some(tmdb_id) = tmdb_id else {
        return Ok(());
    };
    let _ = crate::db::provider_ids::upsert(db, item_id, "Tmdb", &tmdb_id).await;

    let metadata = if is_tv {
        providers::tmdb_tv_details(client, api_key, &tmdb_id, tmdb_base_url).await
    } else {
        providers::tmdb_movie_details(client, api_key, &tmdb_id, tmdb_base_url).await
    };

    let metadata = match metadata {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "TMDb API call failed for {tmdb_id} (type: {item_type}): {}",
                redact_tmdb_error(&e)
            );
            return Ok(());
        }
    };
    tracing::info!("TMDb metadata fetched for {item_type} {tmdb_id}");
    let title = metadata
        .get("Name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let overview = metadata
        .get("Overview")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let year = metadata.get("ProductionYear").and_then(|v| v.as_i64());
    let premiere_date = metadata
        .get("PremiereDate")
        .and_then(|v| v.as_str())
        .and_then(normalize_yyyy_mm_dd);
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
                        p.get("ImageUrl").and_then(|v| {
                            v.as_str()
                                .filter(|s| !s.is_empty())
                                .map(ToString::to_string)
                        }),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let people_with_images = people.iter().filter(|(_, _, _, img)| img.is_some()).count();
    if people_with_images > 0 {
        tracing::info!(
            "Found {people_with_images} cast members with profile images for {item_type} {tmdb_id}"
        );
    }

    // Update media item
    let community_rating = metadata
        .get("CommunityRating")
        .and_then(|v| v.as_f64())
        .filter(|r| *r > 0.0);
    let official_rating = metadata
        .get("OfficialRating")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let runtime_ticks = metadata
        .get("RuntimeTicks")
        .and_then(|v| v.as_i64())
        .filter(|t| *t > 0);
    db.execute(crate::db::helpers::pg_statement(
        r#"UPDATE media_items
           SET title = COALESCE(?, title),
               overview = COALESCE(?, overview),
               production_year = COALESCE(?, production_year),
               premiere_date = COALESCE(?, premiere_date),
               community_rating = COALESCE(?, community_rating),
               official_rating = COALESCE(?, official_rating),
               runtime_ticks = COALESCE(runtime_ticks, ?),
               updated_at = ?
           WHERE id = ?"#,
        vec![
            title.into(),
            overview.into(),
            year.into(),
            premiere_date.as_deref().into(),
            community_rating.into(),
            official_rating.into(),
            runtime_ticks.into(),
            now_unix().into(),
            item_id.into(),
        ],
    ))
    .await?;

    // Store provider IDs
    if let Some(provider_ids) = metadata.get("ProviderIds") {
        if let Some(obj) = provider_ids.as_object() {
            for (provider, id) in obj {
                if let Some(id_str) = id.as_str().filter(|s| !s.is_empty()) {
                    let _ = crate::db::provider_ids::upsert(db, item_id, provider.as_str(), id_str)
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
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO genres (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![genre_id.clone().into(), genre_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_genres (item_id, genre_id) VALUES (?, ?) ON CONFLICT(item_id, genre_id) DO NOTHING",
                vec![item_id.into(), genre_id.into()],
            ))
            .await;
    }

    // Upsert studios
    for studio_name in &studios {
        let studio_id = crate::util::stable_text_id(&format!("studio:{studio_name}"));
        let _ = db
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO studios (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![studio_id.clone().into(), studio_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_studios (item_id, studio_id) VALUES (?, ?) ON CONFLICT(item_id, studio_id) DO NOTHING",
                vec![item_id.into(), studio_id.into()],
            ))
            .await;
    }

    // Upsert people
    for (name, role, person_type, image_url) in &people {
        let person_id = crate::util::stable_text_id(&format!("person:{name}"));
        let _ = db
            .execute(crate::db::helpers::pg_statement(
                "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![person_id.clone().into(), name.as_str().into(), now.into()],
            ))
            .await;
        let sort_order = people
            .iter()
            .position(|(n, _, _, _)| n == name)
            .unwrap_or(0) as i64;
        let _ = db
            .execute(crate::db::helpers::pg_statement(
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
            if let Err(e) =
                download_and_save_tmdb_image(db, client, person_id.as_str(), img_url, "Primary")
                    .await
            {
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
        let _ = download_and_save_tmdb_image(db, client, item_id, image_url, "Primary").await;
    }
    // Also try to get backdrop from the original TMDb API response
    if let Some(backdrop) = metadata.get("BackdropUrl").and_then(|v| v.as_str()) {
        let _ = download_and_save_tmdb_image(db, client, item_id, backdrop, "Backdrop").await;
    }

    // Fetch additional images (logo, banner, art) from TMDb /images endpoint
    let images_url = if is_tv {
        tmdb::api_url(tmdb_base_url, &format!("tv/{tmdb_id}/images"))
    } else {
        tmdb::api_url(tmdb_base_url, &format!("movie/{tmdb_id}/images"))
    };
    if let Ok(resp) = client
        .get(&images_url)
        .query(&[("api_key", api_key)])
        .send()
        .await
    {
        if let Ok(images) = resp.json::<TmdbImagesResponse>().await {
            // Download logo (clearlogo)
            if let Some(logo) = images.preferred_logo() {
                let url = tmdb::image_url(tmdb_base_url, "w500", &logo.file_path);
                let _ = download_and_save_tmdb_image(db, client, item_id, &url, "Logo").await;
            }
            // Download banner
            if let Some(banner) = images.backdrops.first() {
                // Use first backdrop as banner fallback
                let url = tmdb::image_url(tmdb_base_url, "w1280", &banner.file_path);
                let _ = download_and_save_tmdb_image(db, client, item_id, &url, "Banner").await;
                let _ = download_and_save_tmdb_image(db, client, item_id, &url, "Art").await;
            }
        }
    }

    // For Season items, fetch season-specific poster from TMDb
    if item_type == "Season" {
        if let Some(season_number) = extract_season_number(path) {
            // Get the parent series TMDb ID
            if let Some(series_tmdb_id) = get_parent_series_tmdb_id(db, item_id).await {
                let season_url = tmdb::api_url(
                    tmdb_base_url,
                    &format!("tv/{series_tmdb_id}/season/{season_number}"),
                );
                #[derive(serde::Deserialize)]
                struct TmdbSeason {
                    poster_path: Option<String>,
                }
                if let Ok(resp) = client
                    .get(&season_url)
                    .query(&[("api_key", api_key)])
                    .send()
                    .await
                {
                    if let Ok(season) = resp.json::<TmdbSeason>().await {
                        if let Some(poster) = season.poster_path {
                            let img_url = tmdb::image_url(tmdb_base_url, "w500", &poster);
                            let _ = download_and_save_tmdb_image(
                                db, client, item_id, &img_url, "Primary",
                            )
                            .await;
                        }
                    }
                }
            }
        }
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
    if response
        .content_length()
        .is_some_and(|length| length > MAX_METADATA_IMAGE_BYTES as u64)
    {
        anyhow::bail!("image is too large");
    }
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_METADATA_IMAGE_BYTES {
        anyhow::bail!("image is too large");
    }
    let ext = crate::library::image_processing::detect_image_extension(&bytes)
        .ok_or_else(|| anyhow::anyhow!("TMDb image response was not a supported image"))?;
    let dir = std::path::PathBuf::from("data").join("images");
    tokio::fs::create_dir_all(&dir).await.ok();
    let path = dir.join(format!(
        "{}_{}_tmdb.{}",
        crate::util::stable_text_id(item_id),
        image_type.to_ascii_lowercase(),
        ext
    ));
    tokio::fs::write(&path, &bytes).await?;
    let now = crate::util::now_unix();
    let _ = db
        .execute(crate::db::helpers::pg_statement(
            r#"INSERT INTO image_assets (id, item_id, image_type, image_index, path, etag, width, height, size_bytes, created_at, updated_at) VALUES (?, ?, ?, 0, ?, ?, NULL, NULL, ?, ?, ?) ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET path = excluded.path, etag = excluded.etag, size_bytes = excluded.size_bytes, updated_at = excluded.updated_at"#,
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

fn extract_season_number(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    // Look for "Season X" or "Season X" pattern
    let lower = name.to_ascii_lowercase();
    if let Some(pos) = lower.find("season ") {
        let rest = &name[pos + 7..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse().ok();
    }
    // Look for "S01" pattern
    if let Some(pos) = lower.find('s') {
        let rest = &name[pos + 1..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse().ok();
    }
    None
}

#[derive(serde::Deserialize)]
struct TmdbImagesResponse {
    #[serde(default)]
    logos: Vec<TmdbImageEntry>,
    #[serde(default)]
    backdrops: Vec<TmdbImageEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    posters: Vec<TmdbImageEntry>,
}

impl TmdbImagesResponse {
    fn preferred_logo(&self) -> Option<&TmdbImageEntry> {
        self.logos
            .iter()
            .find(|entry| entry.iso_639_1.as_deref() == Some("zh"))
            .or_else(|| {
                self.logos
                    .iter()
                    .find(|entry| entry.iso_639_1.as_deref() == Some("en"))
            })
            .or_else(|| self.logos.iter().find(|entry| entry.iso_639_1.is_none()))
            .or_else(|| self.logos.first())
    }
}

#[derive(serde::Deserialize)]
struct TmdbImageEntry {
    file_path: String,
    #[serde(default)]
    iso_639_1: Option<String>,
}

async fn get_parent_series_tmdb_id(
    db: &DatabaseConnection,
    season_item_id: &str,
) -> Option<String> {
    // Get the parent_id of the season, then get its TMDb ID
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT parent_id FROM media_items WHERE id = ?",
            vec![season_item_id.into()],
        ))
        .await
        .ok()??;
    let parent_id: String = row.get_str("parent_id").ok()?;
    // Get the TMDb ID from provider_ids
    crate::db::provider_ids::get(db, &parent_id, "Tmdb")
        .await
        .ok()?
}

fn should_skip_name_based_tmdb_lookup(name: &str) -> bool {
    let folded = fold_lookup_name(name);
    folded.is_empty() || is_season_lookup_name(name, &folded) || is_generic_container_name(&folded)
}

fn fold_lookup_name(name: &str) -> String {
    name.trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .chars()
        .map(|c| {
            if matches!(c, '.' | '_' | '-') {
                ' '
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_season_lookup_name(name: &str, folded: &str) -> bool {
    if folded == "season" || folded == "seasons" {
        return true;
    }
    if let Some(rest) = folded.strip_prefix("season ") {
        return rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = folded.strip_prefix('s') {
        return !rest.is_empty() && rest.len() <= 3 && rest.chars().all(|c| c.is_ascii_digit());
    }

    let trimmed = name.trim();
    if let Some(number) = trimmed
        .strip_prefix('第')
        .and_then(|value| value.strip_suffix('季'))
    {
        return !number.is_empty()
            && number
                .chars()
                .all(|c| c.is_ascii_digit() || "零一二三四五六七八九十百".contains(c));
    }

    false
}

fn is_generic_container_name(name: &str) -> bool {
    matches!(
        name,
        "media"
            | "movie"
            | "movies"
            | "film"
            | "films"
            | "video"
            | "videos"
            | "tv"
            | "tv show"
            | "tv shows"
            | "show"
            | "shows"
            | "series"
            | "music"
            | "audio"
            | "extra"
            | "extras"
            | "special"
            | "specials"
            | "sample"
            | "samples"
            | "subtitle"
            | "subtitles"
            | "subs"
            | "trailer"
            | "trailers"
            | "behind the scenes"
            | "featurette"
            | "featurettes"
            | "clouddrive"
            | "cloud drive"
            | "儿童"
    )
}

/// Search TMDb for a person by name and return the first match's TMDb ID
pub async fn search_person_tmdb(
    name: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let url = tmdb::api_url(tmdb_base_url, "search/person");
    #[derive(serde::Deserialize)]
    struct TmdbSearchResults {
        results: Vec<TmdbSearchPerson>,
    }
    #[derive(serde::Deserialize)]
    struct TmdbSearchPerson {
        id: i64,
    }
    let resp: TmdbSearchResults = client
        .get(&url)
        .query(&[("api_key", api_key), ("query", name)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.results.into_iter().next().map(|p| p.id.to_string()))
}

/// Fetch person details from TMDb and update the database
pub async fn fetch_person_tmdb(
    db: &sea_orm::DatabaseConnection,
    person_id: &str,
    tmdb_id: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<()> {
    let url = tmdb::api_url(tmdb_base_url, &format!("person/{tmdb_id}"));
    #[derive(serde::Deserialize)]
    struct TmdbPerson {
        biography: Option<String>,
    }
    let resp: TmdbPerson = client
        .get(&url)
        .query(&[("api_key", api_key)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(biography) = resp.biography.filter(|b| !b.is_empty()) {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE people SET overview = ?, tmdb_id = ? WHERE id = ?",
            vec![biography.into(), tmdb_id.into(), person_id.into()],
        ))
        .await?;
    } else {
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE people SET tmdb_id = ? WHERE id = ?",
            vec![tmdb_id.into(), person_id.into()],
        ))
        .await?;
    }
    // Also try to fetch TMDb image
    let img_url = tmdb::api_url(tmdb_base_url, &format!("person/{tmdb_id}/images"));
    #[derive(serde::Deserialize)]
    struct TmdbPersonImages {
        profiles: Vec<TmdbPersonProfile>,
    }
    #[derive(serde::Deserialize)]
    struct TmdbPersonProfile {
        file_path: String,
    }
    if let Ok(resp) = client
        .get(&img_url)
        .query(&[("api_key", api_key)])
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        if let Ok(images) = resp.json::<TmdbPersonImages>().await {
            if let Some(img) = images.profiles.first() {
                let img_url = tmdb::image_url(tmdb_base_url, "w780", &img.file_path);
                let _ =
                    download_and_save_tmdb_image(db, client, person_id, &img_url, "Primary").await;
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
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) {
    if api_key.is_empty() {
        return;
    }
    match search_person_tmdb(person_name, api_key, client, tmdb_base_url).await {
        Ok(Some(tmdb_id)) => {
            let _ =
                fetch_person_tmdb(db, person_id, &tmdb_id, api_key, client, tmdb_base_url).await;
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
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<usize> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT id, name FROM people WHERE (overview IS NULL OR tmdb_id IS NULL) AND name IS NOT NULL AND name <> '' LIMIT 50",
            vec![],
        ))
        .await
        .context("failed to list people without biography")?;

    let mut count = 0;
    for row in &rows {
        let id: String = row.get_str("id")?;
        let name: String = row.get_str("name")?;
        try_fetch_person_tmdb(db, &id, &name, api_key, client, tmdb_base_url).await;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::{
        clean_title_with_year, episode_title_candidate, extract_tmdb_id,
        local_episode_title_from_path, parse_season_number, redact_tmdb_api_key,
        should_skip_name_based_tmdb_lookup,
    };
    use std::path::Path;

    #[test]
    fn tmdb_error_redaction_hides_api_key_query_values() {
        let message = "error sending request for url (https://api.themoviedb.org/3/movie/1?api_key=secret123&language=zh-CN)";
        let redacted = redact_tmdb_api_key(message);
        assert!(!redacted.contains("secret123"));
        assert!(redacted.contains("api_key=<redacted>&language=zh-CN"));
    }

    #[test]
    fn clean_title_with_year_keeps_title_and_year() {
        assert_eq!(
            clean_title_with_year("Movie Name (2024) {tmdb-123}"),
            ("Movie Name".to_string(), Some(2024))
        );
    }

    #[test]
    fn tmdb_id_parser_accepts_common_folder_tags() {
        assert_eq!(
            extract_tmdb_id(Path::new("4 in love (2012) {tmdbid-42026}")).as_deref(),
            Some("42026")
        );
        assert_eq!(
            extract_tmdb_id(Path::new("Movie Name [tmdbid=123]")).as_deref(),
            Some("123")
        );
        assert_eq!(
            extract_tmdb_id(Path::new("Movie Name {edition} {tmdb-456}")).as_deref(),
            Some("456")
        );
    }

    #[test]
    fn name_based_tmdb_lookup_skips_generic_folder_names() {
        for name in ["Season 1", "S01", "第1季", "CloudDrive", "儿童"] {
            assert!(should_skip_name_based_tmdb_lookup(name), "{name}");
        }
        assert!(!should_skip_name_based_tmdb_lookup("Movie Name"));
    }

    #[test]
    fn season_number_parser_handles_common_folder_names() {
        assert_eq!(parse_season_number("Season 1"), Some(1));
        assert_eq!(parse_season_number("season_2"), Some(2));
        assert_eq!(parse_season_number("第3季"), Some(3));
        assert_eq!(parse_season_number("S04"), Some(4));
        assert_eq!(parse_season_number("Specials"), None);
    }

    #[test]
    fn episode_title_candidate_rejects_series_title_year_from_tmdb() {
        let path = "/media/镖人 (2023) {tmdb-107463}/Season 1/镖人.2023.S01E01.第1集.2160p.WEB-DL.H265.mp4";
        assert_eq!(
            episode_title_candidate(Some("镖人 2023"), "镖人 2023", "镖人", Some(2023), 1, path)
                .as_deref(),
            Some("第1集")
        );
    }

    #[test]
    fn episode_title_candidate_accepts_specific_tmdb_episode_name() {
        let path = "/media/镖人/Season 1/镖人.2023.S01E02.第2集.1080p.mkv";
        assert_eq!(
            episode_title_candidate(Some("双头蛇"), "镖人 2023", "镖人", Some(2023), 2, path)
                .as_deref(),
            Some("双头蛇")
        );
    }

    #[test]
    fn local_episode_title_from_path_uses_text_after_episode_marker() {
        assert_eq!(
            local_episode_title_from_path(
                "/media/show/Season 1/镖人.2023.S01E13.真相.2160p.WEB-DL.mkv"
            )
            .as_deref(),
            Some("真相")
        );
    }
}
