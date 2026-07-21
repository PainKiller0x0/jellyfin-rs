use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use anyhow::Context;
use futures_util::StreamExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, sea_query::OnConflict,
};
use tokio::sync::{Mutex, OnceCell};

use crate::{
    db::row_ext::QueryResultExt,
    entities::{
        image_assets::{self, Entity as ImageAssets},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        provider_ids::{self, Entity as ProviderIds},
    },
    jellyfin::providers,
    tmdb,
    util::{normalize_yyyy_mm_dd, now_unix, year_from_yyyy_mm_dd},
};

static EPISODE_TMDB_BATCH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
type EpisodeGroupCacheCell = Arc<OnceCell<Option<Arc<TmdbEpisodeGroupCollection>>>>;
static EPISODE_GROUP_CACHE: OnceLock<Mutex<HashMap<String, EpisodeGroupCacheCell>>> =
    OnceLock::new();
const MAX_EPISODE_GROUP_CACHE_ENTRIES: usize = 512;
const TMDB_API_KEY_QUERY: &str = "api_key=";
const TMDB_TITLE_BACKFILL_KEY: &str = "tmdb_title_backfill_v1_completed";
const MAX_METADATA_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const TMDB_DEFAULT_MAX_CAST_MEMBERS: usize = 15;
const TMDB_DEFAULT_MAX_CREW_MEMBERS: usize = 15;
pub(crate) const TMDB_METADATA_VERSION: i64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataRefreshPolicy {
    pub refresh_metadata: bool,
    pub replace_metadata: bool,
    pub refresh_images: bool,
    pub replace_images: bool,
    pub force_refresh: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TmdbLibraryProviderOptions {
    type_options: HashMap<String, TmdbTypeProviderOptions>,
}

#[derive(Clone, Copy, Debug)]
struct TmdbTypeProviderOptions {
    metadata: bool,
    images: bool,
}

impl TmdbLibraryProviderOptions {
    pub fn from_json(value: &serde_json::Value) -> Self {
        let mut options = Self::default();
        let Some(type_options) = value
            .get("TypeOptions")
            .or_else(|| value.get("typeOptions"))
            .and_then(serde_json::Value::as_array)
        else {
            return options;
        };

        for type_option in type_options {
            let Some(item_type) = type_option
                .get("Type")
                .or_else(|| type_option.get("type"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item_type| !item_type.is_empty())
            else {
                continue;
            };
            let metadata = provider_list_contains(
                type_option
                    .get("MetadataFetchers")
                    .or_else(|| type_option.get("metadataFetchers")),
                "TheMovieDb",
            );
            let images = provider_list_contains(
                type_option
                    .get("ImageFetchers")
                    .or_else(|| type_option.get("imageFetchers")),
                "TheMovieDb",
            );
            options.type_options.insert(
                item_type.to_ascii_lowercase(),
                TmdbTypeProviderOptions { metadata, images },
            );
        }
        options
    }

    pub fn automatic_policy(
        &self,
        item_type: &str,
        preserve_existing_metadata: bool,
    ) -> Option<MetadataRefreshPolicy> {
        let configured = self.options_for_type(item_type);
        (configured.metadata || configured.images).then_some(MetadataRefreshPolicy {
            refresh_metadata: configured.metadata,
            replace_metadata: configured.metadata && !preserve_existing_metadata,
            refresh_images: configured.images,
            replace_images: false,
            force_refresh: false,
        })
    }

    pub fn metadata_enabled(&self, item_type: &str) -> bool {
        self.options_for_type(item_type).metadata
    }

    fn options_for_type(&self, item_type: &str) -> TmdbTypeProviderOptions {
        self.type_options
            .get(&item_type.to_ascii_lowercase())
            .copied()
            .unwrap_or(TmdbTypeProviderOptions {
                metadata: true,
                images: true,
            })
    }
}

fn provider_list_contains(value: Option<&serde_json::Value>, provider: &str) -> bool {
    value
        .and_then(serde_json::Value::as_array)
        .is_some_and(|providers| {
            providers.iter().any(|candidate| {
                candidate
                    .as_str()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(provider))
            })
        })
}

pub async fn load_tmdb_library_provider_options(
    db: &DatabaseConnection,
) -> anyhow::Result<HashMap<String, TmdbLibraryProviderOptions>> {
    Ok(crate::db::settings::find_by_prefix(db, "LibraryOptions.")
        .await?
        .into_iter()
        .filter_map(|setting| {
            let library_id = setting.key.strip_prefix("LibraryOptions.")?.to_string();
            let value = serde_json::from_str::<serde_json::Value>(&setting.value).ok()?;
            Some((library_id, TmdbLibraryProviderOptions::from_json(&value)))
        })
        .collect())
}

impl MetadataRefreshPolicy {
    pub const fn automatic(preserve_existing_metadata: bool) -> Self {
        Self {
            refresh_metadata: true,
            replace_metadata: !preserve_existing_metadata,
            refresh_images: true,
            replace_images: false,
            force_refresh: false,
        }
    }

    pub const fn preserves_metadata(self) -> bool {
        !self.replace_metadata
    }
}

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
    for prefix in ["tmdb-", "tmdb=", "tmdbid-", "tmdbid="] {
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
                if is_provider_id_tag(tag) {
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

fn is_provider_id_tag(tag: &str) -> bool {
    let tag = tag.trim().to_ascii_lowercase();
    [
        "tmdb",
        "tmdbid",
        "douban",
        "doubanid",
        "imdb",
        "imdbid",
        "tvdb",
        "tvdbid",
        "tvmaze",
        "tvmazeid",
        "tvrage",
        "tvrageid",
        "anidb",
        "anidbid",
        "anilist",
        "anilistid",
        "anisearch",
        "anisearchid",
    ]
    .into_iter()
    .any(|prefix| {
        tag.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('='))
    })
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
    library_options: &HashMap<String, TmdbLibraryProviderOptions>,
) {
    let default_metadata_language =
        crate::db::settings::get_non_empty_or_default(db, "PreferredMetadataLanguage", "zh-CN")
            .await;
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT s.id as season_id,
                      s.library_id,
                      s.title,
                      s.season_number,
                      COALESCE(
                          NULLIF(s.preferred_metadata_language, ''),
                          NULLIF(series.preferred_metadata_language, '')
                      ) AS preferred_metadata_language,
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
             AND s.lock_data = 0
             AND (
                 sp.provider_item_id IS NULL
                 OR s.tmdb_metadata_version < ?
                 OR s.season_number IS NULL
                 OR s.production_year IS NULL
                 OR s.premiere_date IS NULL
                 OR NOT EXISTS (
                     SELECT 1 FROM image_assets ia
                     WHERE ia.item_id = s.id AND ia.image_type = 'Primary'
                 )
             )"#,
            vec![TMDB_METADATA_VERSION.into()],
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
                let default_metadata_language = default_metadata_language.clone();
                async move {
                    let library_id = row.get_str("library_id").ok()?;
                    let provider_options = library_options
                        .get(&library_id)
                        .cloned()
                        .unwrap_or_default();
                    let policy = provider_options.automatic_policy("Season", false)?;
                    let Ok(season_id) = row.get_str("season_id") else {
                        return None;
                    };
                    let title: String = row.get_str("title").ok()?;
                    let stored_season_number = row.get_opt_i64("season_number").ok().flatten();
                    let Ok(series_tmdb) = row.get_str("series_tmdb_id") else {
                        return None;
                    };
                    let has_primary = row.get_i64("has_primary").unwrap_or(0) != 0;
                    let metadata_language = row
                        .get_opt_str("preferred_metadata_language")
                        .ok()
                        .flatten()
                        .unwrap_or(default_metadata_language);
                    let sn = stored_season_number
                        .or_else(|| parse_season_number(&title))
                        .unwrap_or(0);
                    let url = crate::tmdb::api_url(
                        tmdb_base_url,
                        &format!("tv/{series_tmdb}/season/{sn}"),
                    );
                    let resp = client
                        .get(&url)
                        .query(&[
                            ("api_key", api_key),
                            ("language", metadata_language.as_str()),
                            ("append_to_response", "credits,external_ids"),
                        ])
                        .send()
                        .await
                        .ok()?;
                    let resp = resp.error_for_status().ok()?;
                    let season: SeasonTmdbResponse = resp.json().await.ok()?;
                    Some((season_id, sn, has_primary, season, policy))
                }
            })
            .collect();

        let results = futures_util::future::join_all(futures).await;
        for (season_id, season_number, has_primary, season, policy) in results.into_iter().flatten()
        {
            if apply_season_tmdb_response(
                db,
                client,
                tmdb_base_url,
                &season_id,
                season_number,
                has_primary,
                &season,
                policy,
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
    let title_lower = title.to_ascii_lowercase();
    let compact_title = title_lower
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .to_string();
    let clean_title = clean_season_name(&compact_title);
    if matches!(clean_title.as_str(), "specials" | "extras") {
        return Some(0);
    }
    if let Some(number) = season_prefix_regex()
        .captures(&compact_title)
        .and_then(|captures| captures.name("number"))
        .and_then(|number| number.as_str().parse::<i64>().ok())
    {
        return Some(number);
    }
    if let Ok(number) = clean_title.parse::<i64>() {
        return Some(number);
    }
    if let Some(number) = parse_keyword_season_number(&clean_title) {
        return Some(number);
    }
    if let Some(rest) = title.split_once('第').map(|(_, rest)| rest) {
        if let Some((number, _)) = rest.split_once('季') {
            if let Some(number) = crate::util::parse_chinese_number(number) {
                return Some(number);
            }
        }
    }
    None
}

fn clean_season_name(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '.' | '_' | '-' | '[' | ']'))
        .collect::<String>()
        .to_ascii_lowercase()
}

fn season_prefix_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(r#"(?i)(^|[ ._\-\[\]])s(?P<number>\d{1,4})($|[ ._\-\[\]].*)"#)
            .expect("season prefix regex must compile")
    })
}

fn parse_keyword_season_number(clean_name: &str) -> Option<i64> {
    if let Some((number, rest)) = take_ascii_number(clean_name) {
        let rest = rest
            .strip_prefix("st")
            .or_else(|| rest.strip_prefix("nd"))
            .or_else(|| rest.strip_prefix("rd"))
            .or_else(|| rest.strip_prefix("th"))
            .unwrap_or(rest);
        if season_keywords()
            .iter()
            .any(|keyword| rest.starts_with(keyword))
        {
            return Some(number);
        }
    }

    for keyword in season_keywords() {
        let Some(rest) = clean_name.strip_prefix(keyword) else {
            continue;
        };
        let Some((number_text, after_number)) = split_ascii_number_for_postfix_season(rest) else {
            continue;
        };
        if after_number
            .strip_prefix('e')
            .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return None;
        }
        return number_text.parse::<i64>().ok();
    }

    None
}

fn take_ascii_number(value: &str) -> Option<(i64, &str)> {
    let digit_len = value
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if digit_len == 0 {
        return None;
    }
    Some((value[..digit_len].parse::<i64>().ok()?, &value[digit_len..]))
}

fn split_ascii_number_for_postfix_season(value: &str) -> Option<(&str, &str)> {
    let digit_len = value
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if digit_len == 0 {
        return None;
    }
    let mut number = &value[..digit_len];
    let mut rest = &value[digit_len..];
    for quality in ["2160p", "1080p", "720p", "480p"] {
        let quality_digits = &quality[..quality.len() - 1];
        if number.len() > quality_digits.len() && number.ends_with(quality_digits) {
            let split = number.len() - quality_digits.len();
            rest = &value[split..];
            number = &number[..split];
            break;
        }
    }
    Some((number, rest))
}

fn season_keywords() -> &'static [&'static str] {
    &[
        "시즌",
        "シーズン",
        "сезон",
        "season",
        "sæson",
        "saison",
        "staffel",
        "series",
        "stagione",
        "säsong",
        "seizoen",
        "seasong",
        "sezon",
        "sezona",
        "sezóna",
        "sezonul",
        "série",
        "séria",
        "serie",
        "seria",
        "temporada",
        "kausi",
    ]
}

#[derive(Clone, Debug)]
struct EpisodeTmdbTarget {
    episode_id: String,
    current_title: String,
    path: String,
    series_title: String,
    series_year: Option<i64>,
    episode_number: i64,
    episode_number_end: Option<i64>,
    refresh_policy: MetadataRefreshPolicy,
}

#[derive(Clone, Debug)]
struct EpisodeTmdbTask {
    tmdb_id: String,
    season_number: i64,
    display_order: Option<String>,
    language: String,
    targets_by_episode: HashMap<i64, Vec<EpisodeTmdbTarget>>,
}

type TmdbPersonData = (String, String, String, Option<String>, Option<String>);

#[derive(Clone, Debug, serde::Deserialize)]
struct EpisodeTmdbResponse {
    id: Option<i64>,
    #[allow(dead_code)]
    episode_number: Option<i64>,
    name: Option<String>,
    overview: Option<String>,
    still_path: Option<String>,
    air_date: Option<String>,
    vote_average: Option<f64>,
    #[serde(default)]
    guest_stars: Vec<EpisodeTmdbCastMember>,
    #[serde(default)]
    crew: Vec<EpisodeTmdbCrewMember>,
    credits: Option<EpisodeTmdbCredits>,
    external_ids: Option<EpisodeTmdbExternalIds>,
    videos: Option<EpisodeTmdbVideos>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct EpisodeTmdbVideos {
    #[serde(default)]
    results: Vec<EpisodeTmdbVideo>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct EpisodeTmdbVideo {
    key: String,
    name: String,
    site: String,
    #[serde(rename = "type")]
    video_type: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct EpisodeTmdbCredits {
    #[serde(default)]
    cast: Vec<EpisodeTmdbCastMember>,
    #[serde(default)]
    guest_stars: Vec<EpisodeTmdbCastMember>,
    #[serde(default)]
    crew: Vec<EpisodeTmdbCrewMember>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct EpisodeTmdbCastMember {
    id: i64,
    name: String,
    character: Option<String>,
    profile_path: Option<String>,
    #[serde(default)]
    order: i64,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct EpisodeTmdbCrewMember {
    id: i64,
    name: String,
    department: Option<String>,
    job: Option<String>,
    profile_path: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct EpisodeTmdbExternalIds {
    imdb_id: Option<String>,
    tvdb_id: Option<i64>,
    tvrage_id: Option<i64>,
}

fn tmdb_credit_people(
    credits: Option<&EpisodeTmdbCredits>,
    guest_stars: &[EpisodeTmdbCastMember],
    crew: &[EpisodeTmdbCrewMember],
    tmdb_base_url: Option<&str>,
) -> Vec<TmdbPersonData> {
    let mut people = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut cast = credits
        .into_iter()
        .flat_map(|credits| credits.cast.iter())
        .collect::<Vec<_>>();
    cast.sort_by_key(|person| person.order);
    let mut guests = credits
        .into_iter()
        .flat_map(|credits| credits.guest_stars.iter())
        .chain(guest_stars.iter())
        .collect::<Vec<_>>();
    guests.sort_by_key(|person| person.order);

    for (person_type, person) in cast
        .into_iter()
        .take(TMDB_DEFAULT_MAX_CAST_MEMBERS)
        .map(|person| ("Actor", person))
        .chain(
            guests
                .into_iter()
                .take(TMDB_DEFAULT_MAX_CAST_MEMBERS)
                .map(|person| ("GuestStar", person)),
        )
    {
        if !seen.insert((person.id, person_type)) {
            continue;
        }
        people.push((
            person.name.clone(),
            person.character.clone().unwrap_or_default(),
            person_type.to_string(),
            person
                .profile_path
                .as_deref()
                .map(|path| tmdb::image_url(tmdb_base_url, "w185", path)),
            Some(person.id.to_string()),
        ));
    }

    for person in credits
        .into_iter()
        .flat_map(|credits| credits.crew.iter())
        .chain(crew.iter())
        .filter(|person| {
            crate::jellyfin::providers::tmdb_crew_person_type(
                person.department.as_deref().unwrap_or_default(),
                person.job.as_deref().unwrap_or_default(),
            )
            .is_some()
        })
        .take(TMDB_DEFAULT_MAX_CREW_MEMBERS)
    {
        let Some(person_type) = crate::jellyfin::providers::tmdb_crew_person_type(
            person.department.as_deref().unwrap_or_default(),
            person.job.as_deref().unwrap_or_default(),
        ) else {
            continue;
        };
        if !seen.insert((person.id, person_type)) {
            continue;
        }
        people.push((
            person.name.clone(),
            person.job.clone().unwrap_or_default(),
            person_type.to_string(),
            person
                .profile_path
                .as_deref()
                .map(|path| tmdb::image_url(tmdb_base_url, "w185", path)),
            Some(person.id.to_string()),
        ));
    }
    people
}

#[derive(serde::Deserialize)]
struct SeasonTmdbResponse {
    id: Option<i64>,
    poster_path: Option<String>,
    overview: Option<String>,
    air_date: Option<String>,
    credits: Option<EpisodeTmdbCredits>,
    external_ids: Option<EpisodeTmdbExternalIds>,
}

#[derive(serde::Deserialize)]
struct TmdbSeriesEpisodeGroupsResponse {
    episode_groups: Option<TmdbEpisodeGroups>,
}

#[derive(serde::Deserialize)]
struct TmdbEpisodeGroups {
    #[serde(default)]
    results: Vec<TmdbEpisodeGroupSummary>,
}

#[derive(serde::Deserialize)]
struct TmdbEpisodeGroupSummary {
    id: String,
    #[serde(rename = "type")]
    group_type: Option<i64>,
}

#[derive(serde::Deserialize)]
struct TmdbEpisodeGroupCollection {
    #[serde(default)]
    groups: Vec<TmdbEpisodeGroup>,
}

#[derive(serde::Deserialize)]
struct TmdbEpisodeGroup {
    order: i64,
    #[serde(default)]
    episodes: Vec<TmdbEpisodeGroupEpisode>,
}

#[derive(serde::Deserialize)]
struct TmdbEpisodeGroupEpisode {
    order: i64,
    episode_number: i64,
    season_number: i64,
}

async fn normalize_episode_years_from_premiere_dates(db: &sea_orm::DatabaseConnection) {
    match MediaItems::find()
        .filter(media_items::Column::ItemType.eq("Episode"))
        .filter(media_items::Column::PremiereDate.is_not_null())
        .all(db)
        .await
    {
        Ok(items) => {
            let mut updated = 0u64;
            for item in items {
                let Some(year) = item.premiere_date.as_deref().and_then(year_from_yyyy_mm_dd)
                else {
                    continue;
                };
                if item.production_year == Some(year) {
                    continue;
                }
                let mut active: media_items::ActiveModel = item.into();
                active.production_year = Set(Some(year));
                active.updated_at = Set(now_unix());
                if active.update(db).await.is_ok() {
                    updated += 1;
                }
            }
            if updated > 0 {
                tracing::info!(
                    count = updated,
                    "normalized episode production years from premiere dates"
                );
            }
        }
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
    policy: MetadataRefreshPolicy,
) -> anyhow::Result<bool> {
    if !policy.refresh_metadata && !policy.refresh_images {
        return Ok(true);
    }
    let Some(row) = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT s.id AS season_id,
                      s.title,
                      s.path,
                      s.season_number,
                      s.production_year,
                      s.premiere_date,
                      s.tmdb_metadata_version,
                      COALESCE(
                          NULLIF(s.preferred_metadata_language, ''),
                          NULLIF(series.preferred_metadata_language, '')
                      ) AS preferred_metadata_language,
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
    let metadata_language = match row
        .get_opt_str("preferred_metadata_language")
        .ok()
        .flatten()
    {
        Some(language) => language,
        None => preferred_metadata_language_for_item(db, item_id).await,
    };
    let season_metadata_is_current = row.get_opt_str("season_tmdb_id").ok().flatten().is_some()
        && row.get_i64("tmdb_metadata_version").unwrap_or_default() == TMDB_METADATA_VERSION
        && stored_season_number.is_some();
    if (!policy.refresh_metadata || season_metadata_is_current)
        && (!policy.refresh_images || has_primary)
        && !policy.force_refresh
    {
        return Ok(true);
    }

    let Some(season) = fetch_tmdb_season_endpoint(
        client,
        api_key,
        tmdb_base_url,
        &series_tmdb_id,
        season_number,
        &metadata_language,
    )
    .await?
    else {
        tracing::debug!(
            "TMDb season metadata not found for series tmdb-{series_tmdb_id} season {season_number}"
        );
        return Ok(true);
    };

    apply_season_tmdb_response(
        db,
        client,
        tmdb_base_url,
        item_id,
        season_number,
        has_primary,
        &season,
        policy,
    )
    .await?;
    Ok(true)
}

async fn fetch_tmdb_season_endpoint(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    season_number: i64,
    language: &str,
) -> anyhow::Result<Option<SeasonTmdbResponse>> {
    let url = tmdb::api_url(
        tmdb_base_url,
        &format!("tv/{series_tmdb_id}/season/{season_number}"),
    );
    let response = client
        .get(&url)
        .query(&[
            ("api_key", api_key),
            ("language", language),
            ("append_to_response", "credits,external_ids"),
        ])
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json().await?))
}

async fn apply_season_tmdb_response(
    db: &sea_orm::DatabaseConnection,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    season_id: &str,
    season_number: i64,
    _has_primary: bool,
    season: &SeasonTmdbResponse,
    policy: MetadataRefreshPolicy,
) -> anyhow::Result<bool> {
    let preserve_existing_metadata = policy.preserves_metadata();
    let premiere_date = season.air_date.as_deref().and_then(normalize_yyyy_mm_dd);
    let production_year = premiere_date.as_deref().and_then(year_from_yyyy_mm_dd);
    let mut primary_downloaded = false;
    let mut cast_locked = false;
    if policy.refresh_metadata
        && let Some(tmdb_season_id) = season.id
    {
        crate::db::provider_ids::upsert(db, season_id, "Tmdb", &tmdb_season_id.to_string()).await?;
    }
    if policy.refresh_metadata
        && let Some(item) = MediaItems::find_by_id(season_id.to_string())
            .one(db)
            .await?
    {
        cast_locked = metadata_field_locked(&item, "Cast");
        let mut active: media_items::ActiveModel = item.clone().into();
        // Jellyfin's TMDb plugin defaults ImportSeasonName to false, so the
        // resolver's localized season name remains authoritative.
        active.season_number = Set(Some(season_number));
        if item.production_year.is_none() {
            active.production_year = Set(production_year);
        }
        if item.premiere_date.is_none() {
            active.premiere_date = Set(premiere_date.clone());
        }
        if !metadata_field_locked(&item, "Overview")
            && (!preserve_existing_metadata || item.overview.is_none())
            && let Some(overview) = season
                .overview
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            active.overview = Set(Some(overview.to_string()));
        }
        active.updated_at = Set(now_unix());
        active.update(db).await?;
    }
    if policy.refresh_metadata
        && let Some(external_ids) = season.external_ids.as_ref()
    {
        upsert_remote_provider_id(
            db,
            season_id,
            "Tvdb",
            external_ids.tvdb_id.map(|id| id.to_string()),
            preserve_existing_metadata,
        )
        .await?;
    }
    if policy.refresh_metadata && !cast_locked {
        if !preserve_existing_metadata {
            MediaPeople::delete_many()
                .filter(media_people::Column::ItemId.eq(season_id))
                .exec(db)
                .await?;
        }
        let people = tmdb_credit_people(season.credits.as_ref(), &[], &[], tmdb_base_url);
        upsert_tmdb_people(db, client, season_id, &people).await?;
    }
    if policy.refresh_images
        && remote_image_should_download(db, season_id, "Primary", policy.replace_images).await
    {
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
    if policy.refresh_metadata {
        mark_tmdb_metadata_current(db, season_id).await?;
    }
    Ok(primary_downloaded)
}

pub async fn fetch_and_apply_episode_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    policy: MetadataRefreshPolicy,
) -> anyhow::Result<bool> {
    if !policy.refresh_metadata && !policy.refresh_images {
        return Ok(true);
    }
    let Some(row) = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT e.id AS episode_id,
                      e.title AS episode_title,
                      e.path AS episode_path,
                      COALESCE(
                          e.season_number,
                          CASE WHEN parent.item_type = 'Season' THEN parent.season_number ELSE NULL END
                      ) AS season_number,
                      e.episode_number,
                      e.episode_number_end,
                      e.production_year,
                      e.premiere_date,
                      e.tmdb_metadata_version,
                      COALESCE(
                          NULLIF(e.preferred_metadata_language, ''),
                          NULLIF(parent.preferred_metadata_language, ''),
                          NULLIF(series.preferred_metadata_language, '')
                      ) AS preferred_metadata_language,
                      series.title AS series_title,
                      series.production_year AS series_year,
                      series.display_order AS series_display_order,
                      p.provider_item_id AS tmdb_id,
                      ep.provider_item_id AS episode_tmdb_id,
                      CASE WHEN EXISTS (
                          SELECT 1 FROM image_assets ia
                          WHERE ia.item_id = e.id AND ia.image_type = 'Primary'
                      ) THEN 1 ELSE 0 END AS has_primary
               FROM media_items e
               LEFT JOIN media_items parent ON parent.id = e.parent_id
               LEFT JOIN media_items series ON (
                   (parent.item_type = 'Season' AND series.id = parent.parent_id)
                   OR (parent.item_type = 'Series' AND series.id = parent.id)
               )
               LEFT JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
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
    let episode_number_end = row
        .get_opt_i64("episode_number_end")
        .ok()
        .flatten()
        .filter(|end| *end > episode_number);

    let current_title = row.get_str("episode_title").unwrap_or_default();
    let series_title = row.get_str("series_title").unwrap_or_default();
    let series_year = row.get_opt_i64("series_year").ok().flatten();
    let series_display_order = row.get_opt_str("series_display_order").ok().flatten();
    let metadata_language = match row
        .get_opt_str("preferred_metadata_language")
        .ok()
        .flatten()
    {
        Some(language) => language,
        None => preferred_metadata_language_for_item(db, item_id).await,
    };
    let episode_metadata_is_current = row.get_opt_str("episode_tmdb_id").ok().flatten().is_some()
        && row.get_i64("tmdb_metadata_version").unwrap_or_default() == TMDB_METADATA_VERSION;
    let has_primary = row.get_i64("has_primary").unwrap_or_default() != 0;
    if (!policy.refresh_metadata || episode_metadata_is_current)
        && (!policy.refresh_images || has_primary)
        && !policy.force_refresh
    {
        return Ok(true);
    }

    let Some(episode) = fetch_tmdb_episode_range_with_jellyfin_order(
        client,
        api_key,
        tmdb_base_url,
        &tmdb_id,
        season_number,
        episode_number,
        episode_number_end,
        series_display_order.as_deref(),
        &metadata_language,
    )
    .await?
    else {
        tracing::debug!(
            "TMDb episode metadata not found for series tmdb-{tmdb_id} S{season_number}E{episode_number}"
        );
        return Ok(true);
    };
    let target = EpisodeTmdbTarget {
        episode_id: row.get_str("episode_id")?,
        current_title,
        path: row.get_str("episode_path").unwrap_or_default(),
        series_title,
        series_year,
        episode_number,
        episode_number_end,
        refresh_policy: policy,
    };
    apply_episode_tmdb_response(db, client, tmdb_base_url, &episode, &target, policy).await?;
    Ok(true)
}

async fn fetch_tmdb_episode_range_with_jellyfin_order(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    season_number: i64,
    episode_number: i64,
    episode_number_end: Option<i64>,
    display_order: Option<&str>,
    language: &str,
) -> anyhow::Result<Option<EpisodeTmdbResponse>> {
    let Some(episode_number_end) = episode_number_end.filter(|end| *end > episode_number) else {
        return fetch_tmdb_episode_with_jellyfin_order(
            client,
            api_key,
            tmdb_base_url,
            series_tmdb_id,
            season_number,
            episode_number,
            display_order,
            language,
        )
        .await;
    };

    let mut episodes = Vec::new();
    for number in episode_number..=episode_number_end {
        if let Some(episode) = fetch_tmdb_episode_with_jellyfin_order(
            client,
            api_key,
            tmdb_base_url,
            series_tmdb_id,
            season_number,
            number,
            display_order,
            language,
        )
        .await?
        {
            episodes.push(episode);
        }
    }
    Ok(merge_tmdb_episode_responses(episodes))
}

async fn fetch_tmdb_episode_with_jellyfin_order(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    season_number: i64,
    episode_number: i64,
    display_order: Option<&str>,
    language: &str,
) -> anyhow::Result<Option<EpisodeTmdbResponse>> {
    let (mapped_season, mapped_episode) = match resolve_tmdb_group_episode(
        client,
        api_key,
        tmdb_base_url,
        series_tmdb_id,
        season_number,
        episode_number,
        display_order,
    )
    .await?
    {
        Some((mapped_season, mapped_episode)) => {
            if mapped_season != season_number || mapped_episode != episode_number {
                tracing::debug!(
                    "TMDb episode group mapped tmdb-{series_tmdb_id} S{season_number}E{episode_number} ({}) to S{mapped_season}E{mapped_episode}",
                    display_order.unwrap_or_default()
                );
            }
            (mapped_season, mapped_episode)
        }
        None => (season_number, episode_number),
    };

    fetch_tmdb_episode_endpoint(
        client,
        api_key,
        tmdb_base_url,
        series_tmdb_id,
        mapped_season,
        mapped_episode,
        language,
    )
    .await
}

async fn fetch_tmdb_episode_endpoint(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    season_number: i64,
    episode_number: i64,
    language: &str,
) -> anyhow::Result<Option<EpisodeTmdbResponse>> {
    let url = tmdb::api_url(
        tmdb_base_url,
        &format!("tv/{series_tmdb_id}/season/{season_number}/episode/{episode_number}"),
    );
    let response = client
        .get(&url)
        .query(&[
            ("api_key", api_key),
            ("language", language),
            ("append_to_response", "credits,external_ids,videos"),
        ])
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json().await?))
}

async fn resolve_tmdb_group_episode(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    season_number: i64,
    episode_number: i64,
    display_order: Option<&str>,
) -> anyhow::Result<Option<(i64, i64)>> {
    let Some(group_type) = tmdb_episode_group_type(display_order) else {
        return Ok(None);
    };
    let Some(group) =
        fetch_tmdb_episode_group_cached(client, api_key, tmdb_base_url, series_tmdb_id, group_type)
            .await?
    else {
        return Ok(None);
    };
    Ok(resolve_tmdb_episode_group_mapping(
        &group,
        season_number,
        episode_number,
    ))
}

async fn fetch_tmdb_episode_group_cached(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    group_type: i64,
) -> anyhow::Result<Option<Arc<TmdbEpisodeGroupCollection>>> {
    let cache_key = format!(
        "{}:{group_type}",
        tmdb::api_url(tmdb_base_url, &format!("tv/{series_tmdb_id}"))
    );
    let cell = {
        let cache = EPISODE_GROUP_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache.lock().await;
        if cache.len() >= MAX_EPISODE_GROUP_CACHE_ENTRIES && !cache.contains_key(&cache_key) {
            cache.clear();
        }
        cache
            .entry(cache_key)
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };

    let group = cell
        .get_or_try_init(|| async {
            fetch_tmdb_episode_group(client, api_key, tmdb_base_url, series_tmdb_id, group_type)
                .await
                .map(|group| group.map(Arc::new))
        })
        .await?;
    Ok(group.clone())
}

async fn fetch_tmdb_episode_group(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    group_type: i64,
) -> anyhow::Result<Option<TmdbEpisodeGroupCollection>> {
    let Some(group_id) =
        fetch_tmdb_episode_group_id(client, api_key, tmdb_base_url, series_tmdb_id, group_type)
            .await?
    else {
        return Ok(None);
    };
    let url = tmdb::api_url(tmdb_base_url, &format!("tv/episode_group/{group_id}"));
    let response = client
        .get(&url)
        .query(&[("api_key", api_key), ("language", "zh-CN")])
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json().await?))
}

fn tmdb_episode_group_type(display_order: Option<&str>) -> Option<i64> {
    match display_order?.trim() {
        "originalAirDate" | "OriginalAirDate" => Some(1),
        "absolute" | "Absolute" => Some(2),
        "dvd" | "DVD" => Some(3),
        "digital" | "Digital" => Some(4),
        "storyArc" | "StoryArc" => Some(5),
        "production" | "Production" => Some(6),
        "tv" | "TV" => Some(7),
        _ => None,
    }
}

fn resolve_tmdb_episode_group_mapping(
    group: &TmdbEpisodeGroupCollection,
    season_number: i64,
    episode_number: i64,
) -> Option<(i64, i64)> {
    group
        .groups
        .iter()
        .find(|group| group.order == season_number)
        .and_then(|group| {
            group
                .episodes
                .iter()
                .find(|episode| episode.order == episode_number - 1)
        })
        .map(|episode| (episode.season_number, episode.episode_number))
}

async fn fetch_tmdb_episode_group_id(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    series_tmdb_id: &str,
    group_type: i64,
) -> anyhow::Result<Option<String>> {
    let url = tmdb::api_url(tmdb_base_url, &format!("tv/{series_tmdb_id}"));
    let response = client
        .get(&url)
        .query(&[
            ("api_key", api_key),
            ("language", "zh-CN"),
            ("append_to_response", "episode_groups"),
        ])
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let series: TmdbSeriesEpisodeGroupsResponse = response.error_for_status()?.json().await?;
    Ok(series.episode_groups.and_then(|episode_groups| {
        episode_groups
            .results
            .into_iter()
            .find(|group| group.group_type == Some(group_type))
            .map(|group| group.id)
    }))
}

async fn apply_episode_tmdb_response(
    db: &sea_orm::DatabaseConnection,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
    episode: &EpisodeTmdbResponse,
    target: &EpisodeTmdbTarget,
    policy: MetadataRefreshPolicy,
) -> anyhow::Result<()> {
    let preserve_existing_metadata = policy.preserves_metadata();
    if policy.refresh_metadata
        && let Some(tmdb_episode_id) = episode.id
    {
        upsert_remote_provider_id(
            db,
            &target.episode_id,
            "Tmdb",
            Some(tmdb_episode_id.to_string()),
            preserve_existing_metadata,
        )
        .await?;
    }

    let mut cast_locked = false;
    if policy.refresh_metadata
        && let Some(item) = MediaItems::find_by_id(target.episode_id.clone())
            .one(db)
            .await?
    {
        cast_locked = metadata_field_locked(&item, "Cast");
        let mut active: media_items::ActiveModel = item.clone().into();
        if !metadata_field_locked(&item, "Name")
            && (!preserve_existing_metadata || item.title.trim().is_empty())
            && let Some(title) = episode_title_candidate(
                episode.name.as_deref(),
                &target.current_title,
                &target.series_title,
                target.series_year,
                target.episode_number,
                &target.path,
            )
        {
            active.title = Set(title);
        }

        if !metadata_field_locked(&item, "Overview")
            && (!preserve_existing_metadata || item.overview.is_none())
            && let Some(overview) = episode
                .overview
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            active.overview = Set(Some(overview.to_string()));
        }

        let premiere_date = episode.air_date.as_deref().and_then(normalize_yyyy_mm_dd);
        let production_year = premiere_date.as_deref().and_then(year_from_yyyy_mm_dd);
        let community_rating = episode.vote_average.filter(|rating| *rating > 0.0);
        if item.production_year.is_none() {
            active.production_year = Set(production_year);
        }
        if item.premiere_date.is_none() {
            active.premiere_date = Set(premiere_date);
        }
        if item.community_rating.is_none() {
            active.community_rating = Set(community_rating);
        }
        active.remote_trailers = Set(merge_remote_trailers(
            item.remote_trailers.as_deref(),
            &episode_remote_trailers(episode),
            !preserve_existing_metadata,
        ));
        active.updated_at = Set(now_unix());
        active.update(db).await?;
    }

    if policy.refresh_metadata
        && let Some(external_ids) = episode.external_ids.as_ref()
    {
        for (provider, provider_item_id) in [
            ("Imdb", external_ids.imdb_id.clone()),
            ("Tvdb", external_ids.tvdb_id.map(|id| id.to_string())),
            ("TvRage", external_ids.tvrage_id.map(|id| id.to_string())),
        ] {
            upsert_remote_provider_id(
                db,
                &target.episode_id,
                provider,
                provider_item_id,
                preserve_existing_metadata,
            )
            .await?;
        }
    }
    if policy.refresh_metadata && !cast_locked {
        if !preserve_existing_metadata {
            MediaPeople::delete_many()
                .filter(media_people::Column::ItemId.eq(&target.episode_id))
                .exec(db)
                .await?;
        }
        let people = tmdb_credit_people(
            episode.credits.as_ref(),
            &episode.guest_stars,
            &episode.crew,
            tmdb_base_url,
        );
        upsert_tmdb_people(db, client, &target.episode_id, &people).await?;
    }

    if policy.refresh_images
        && let Some(still) = episode.still_path.as_deref()
        && remote_image_should_download(db, &target.episode_id, "Primary", policy.replace_images)
            .await
    {
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
    if policy.refresh_metadata {
        mark_tmdb_metadata_current(db, &target.episode_id).await?;
    }
    Ok(())
}

fn episode_remote_trailers(episode: &EpisodeTmdbResponse) -> Vec<serde_json::Value> {
    episode
        .videos
        .as_ref()
        .into_iter()
        .flat_map(|videos| videos.results.iter())
        .filter(|video| {
            video.site.eq_ignore_ascii_case("youtube")
                && (video.video_type.eq_ignore_ascii_case("trailer")
                    || video.video_type.eq_ignore_ascii_case("teaser"))
        })
        .map(|video| {
            serde_json::json!({
                "Name": video.name,
                "Url": format!("https://www.youtube.com/watch?v={}", video.key),
            })
        })
        .collect()
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

    let library_options = load_tmdb_library_provider_options(db).await?;

    normalize_episode_years_from_premiere_dates(db).await;

    // First: download season images for all seasons
    download_season_images(db, api_key, client, tmdb_base_url, &library_options).await;
    let default_metadata_language =
        crate::db::settings::get_non_empty_or_default(db, "PreferredMetadataLanguage", "zh-CN")
            .await;

    let rows = db.query_all_raw(crate::db::helpers::pg_statement(
        r#"SELECT e.id as episode_id,
                  e.library_id,
                  e.title as episode_title,
                  e.path as episode_path,
                  COALESCE(
                      e.season_number,
                      CASE WHEN parent.item_type = 'Season' THEN parent.season_number ELSE NULL END
                  ) AS season_number,
                  e.episode_number,
                  e.episode_number_end,
                  series.title as series_title,
                  series.production_year as series_year,
                  series.display_order as series_display_order,
                  COALESCE(
                      NULLIF(e.preferred_metadata_language, ''),
                      NULLIF(parent.preferred_metadata_language, ''),
                      NULLIF(series.preferred_metadata_language, '')
                  ) AS preferred_metadata_language,
                  p.provider_item_id as tmdb_id,
                  CASE WHEN EXISTS (
                      SELECT 1 FROM image_assets ia
                      WHERE ia.item_id = e.id AND ia.image_type = 'Primary'
                  ) THEN 1 ELSE 0 END AS has_primary
           FROM media_items e
           JOIN media_items parent ON parent.id = e.parent_id
           JOIN media_items series ON (
               (parent.item_type = 'Season' AND series.id = parent.parent_id)
               OR (parent.item_type = 'Series' AND series.id = parent.id)
           )
           JOIN provider_ids p ON p.item_id = series.id AND p.provider = 'Tmdb'
           LEFT JOIN provider_ids ep ON ep.item_id = e.id AND ep.provider = 'Tmdb'
           WHERE e.item_type = 'Episode'
             AND e.lock_data = 0
             AND COALESCE(
                 e.season_number,
                 CASE WHEN parent.item_type = 'Season' THEN parent.season_number ELSE NULL END
             ) IS NOT NULL
             AND e.episode_number IS NOT NULL
             AND (
                 ep.provider_item_id IS NULL
                 OR e.tmdb_metadata_version < ?
                 OR e.production_year IS NULL
                 OR e.premiere_date IS NULL
                 OR (
                     e.production_year <> CAST(SUBSTRING(e.premiere_date FROM 1 FOR 4) AS BIGINT)
                 )
	                 OR e.title = series.title
	                 OR (series.production_year IS NOT NULL AND e.title = CONCAT(series.title, ' ', series.production_year))
	                 OR (e.production_year IS NOT NULL AND e.title = CONCAT(series.title, ' ', e.production_year))
	                 OR NOT EXISTS (
	                     SELECT 1 FROM image_assets ia
	                     WHERE ia.item_id = e.id AND ia.image_type = 'Primary'
	                 )
	             )"#,
        vec![TMDB_METADATA_VERSION.into()],
    )).await?;

    // Group local versions by season so complete episode requests can run with
    // bounded concurrency and each response can update every matching version.
    let mut tasks_by_key: HashMap<String, EpisodeTmdbTask> = HashMap::new();

    for row in &rows {
        let Ok(library_id) = row.get_str("library_id") else {
            continue;
        };
        let provider_options = library_options
            .get(&library_id)
            .cloned()
            .unwrap_or_default();
        let Some(refresh_policy) = provider_options.automatic_policy("Episode", false) else {
            continue;
        };
        if !refresh_policy.refresh_metadata && row.get_i64("has_primary").unwrap_or_default() != 0 {
            continue;
        }
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
        let episode_number_end = row
            .get_opt_i64("episode_number_end")
            .ok()
            .flatten()
            .filter(|end| *end > en);
        let series_title: String = row.get_str("series_title").unwrap_or_default();
        let series_year = row.get_opt_i64("series_year").ok().flatten();
        let series_display_order = row.get_opt_str("series_display_order").ok().flatten();
        let metadata_language = row
            .get_opt_str("preferred_metadata_language")
            .ok()
            .flatten()
            .unwrap_or_else(|| default_metadata_language.clone());
        let Ok(tmdb_id) = row.get_str("tmdb_id") else {
            continue;
        };

        let key = format!(
            "{}:{}:{}:{}",
            tmdb_id,
            sn,
            series_display_order.as_deref().unwrap_or_default(),
            metadata_language
        );
        let target = EpisodeTmdbTarget {
            episode_id,
            current_title: episode_title,
            path: episode_path,
            series_title,
            series_year,
            episode_number: en,
            episode_number_end,
            refresh_policy,
        };
        tasks_by_key
            .entry(key)
            .and_modify(|task| {
                task.targets_by_episode
                    .entry(en)
                    .or_default()
                    .push(target.clone());
            })
            .or_insert_with(|| EpisodeTmdbTask {
                tmdb_id,
                season_number: sn,
                display_order: series_display_order,
                language: metadata_language,
                targets_by_episode: HashMap::from([(en, vec![target])]),
            });
    }

    let tasks = tasks_by_key.into_values().collect::<Vec<_>>();
    let total = tasks.len();
    tracing::info!("Episode TMDb batch: {total} missing season group(s) to fetch");

    let api_key = api_key.to_string();

    // Process in concurrent batches (10 seasons at a time)
    let mut count = 0usize;
    let mut processed_seasons = 0usize;
    for chunk in tasks.chunks(10) {
        let futures: Vec<_> = chunk
            .iter()
            .map(|task| {
                let api_key = api_key.clone();
                let task = task.clone();
                let client = (*client).clone();
                async move {
                    let mut episodes_by_number = HashMap::new();
                    let needed_episode_numbers = task
                        .targets_by_episode
                        .values()
                        .flat_map(|targets| targets.iter().flat_map(episode_numbers_for_target))
                        .collect::<std::collections::BTreeSet<_>>();
                    for episode_number in needed_episode_numbers {
                        if let Ok(Some(episode)) = fetch_tmdb_episode_with_jellyfin_order(
                            &client,
                            &api_key,
                            tmdb_base_url,
                            &task.tmdb_id,
                            task.season_number,
                            episode_number,
                            task.display_order.as_deref(),
                            &task.language,
                        )
                        .await
                        {
                            episodes_by_number.insert(episode_number, episode);
                        }
                    }

                    Some((episodes_by_number, task))
                }
            })
            .collect();

        let results: Vec<_> = futures_util::future::join_all(futures).await;
        for result in results.into_iter().flatten() {
            let (episodes_by_number, task) = result;
            processed_seasons += 1;
            for targets in task.targets_by_episode.values() {
                for target in targets {
                    let Some(ep) = tmdb_episode_response_for_target(&episodes_by_number, target)
                    else {
                        continue;
                    };
                    match apply_episode_tmdb_response(
                        db,
                        client,
                        tmdb_base_url,
                        &ep,
                        target,
                        target.refresh_policy,
                    )
                    .await
                    {
                        Ok(()) => count += 1,
                        Err(error) => {
                            tracing::warn!(
                                "failed to apply episode TMDb metadata for {}: {error:#}",
                                target.episode_id
                            );
                        }
                    }
                }
            }
        }
        if processed_seasons % 10 == 0 || processed_seasons == total {
            tracing::info!(
                "Episode TMDb progress: {processed_seasons}/{total} season group(s), applied {count} episode group(s)"
            );
        }
    }

    tracing::info!("TMDb episode metadata fetched for {count} episodes");
    Ok(count)
}

fn episode_numbers_for_target(target: &EpisodeTmdbTarget) -> impl Iterator<Item = i64> {
    let end = target
        .episode_number_end
        .filter(|end| *end > target.episode_number)
        .unwrap_or(target.episode_number);
    target.episode_number..=end
}

fn tmdb_episode_response_for_target(
    episodes_by_number: &HashMap<i64, EpisodeTmdbResponse>,
    target: &EpisodeTmdbTarget,
) -> Option<EpisodeTmdbResponse> {
    let episodes = episode_numbers_for_target(target)
        .filter_map(|episode_number| episodes_by_number.get(&episode_number).cloned())
        .collect::<Vec<_>>();
    merge_tmdb_episode_responses(episodes)
}

fn merge_tmdb_episode_responses(episodes: Vec<EpisodeTmdbResponse>) -> Option<EpisodeTmdbResponse> {
    let mut episodes = episodes.into_iter();
    let mut merged = episodes.next()?;
    let mut names = Vec::new();
    if let Some(name) = merged
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        names.push(name.to_string());
    }
    let mut overviews = Vec::new();
    if let Some(overview) = merged
        .overview
        .as_deref()
        .map(str::trim)
        .filter(|overview| !overview.is_empty())
    {
        overviews.push(overview.to_string());
    }

    for episode in episodes {
        if let Some(name) = episode
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            names.push(name.to_string());
        }
        if let Some(overview) = episode
            .overview
            .as_deref()
            .map(str::trim)
            .filter(|overview| !overview.is_empty())
        {
            overviews.push(overview.to_string());
        }
    }

    if !names.is_empty() {
        merged.name = Some(names.join(" / "));
    }
    if !overviews.is_empty() {
        merged.overview = Some(overviews.join(" / "));
    }
    Some(merged)
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

async fn preferred_metadata_language_for_item(db: &DatabaseConnection, item_id: &str) -> String {
    let item_language = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT COALESCE(
                       NULLIF(item.preferred_metadata_language, ''),
                       NULLIF(parent.preferred_metadata_language, ''),
                       NULLIF(grandparent.preferred_metadata_language, '')
                   ) AS language
               FROM media_items item
               LEFT JOIN media_items parent ON parent.id = item.parent_id
               LEFT JOIN media_items grandparent ON grandparent.id = parent.parent_id
               WHERE item.id = ?"#,
            vec![item_id.into()],
        ))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get_opt_str("language").ok().flatten())
        .filter(|language| !language.trim().is_empty());

    match item_language {
        Some(language) => language,
        None => {
            crate::db::settings::get_non_empty_or_default(db, "PreferredMetadataLanguage", "zh-CN")
                .await
        }
    }
}

async fn preferred_metadata_country_code_for_item(
    db: &DatabaseConnection,
    item_id: &str,
) -> String {
    let item_country = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT COALESCE(
                       NULLIF(item.preferred_metadata_country_code, ''),
                       NULLIF(parent.preferred_metadata_country_code, ''),
                       NULLIF(grandparent.preferred_metadata_country_code, '')
                   ) AS country_code
               FROM media_items item
               LEFT JOIN media_items parent ON parent.id = item.parent_id
               LEFT JOIN media_items grandparent ON grandparent.id = parent.parent_id
               WHERE item.id = ?"#,
            vec![item_id.into()],
        ))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get_opt_str("country_code").ok().flatten())
        .filter(|country| !country.trim().is_empty());

    match item_country {
        Some(country) => country,
        None => {
            crate::db::settings::get_non_empty_or_default(db, "MetadataCountryCode", "CN").await
        }
    }
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

async fn mark_tmdb_metadata_current(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    let item = MediaItems::find_by_id(item_id.to_string())
        .one(db)
        .await?
        .with_context(|| {
            format!("cannot mark missing media item {item_id} TMDb metadata current")
        })?;
    if item.tmdb_metadata_version == TMDB_METADATA_VERSION {
        return Ok(());
    }
    let mut active: media_items::ActiveModel = item.into();
    active.tmdb_metadata_version = Set(TMDB_METADATA_VERSION);
    active.updated_at = Set(now_unix());
    active.update(db).await?;
    Ok(())
}

/// Parse a folder name into (title, optional year) by cleaning provider tags
/// e.g. "X战警 (2000) {tmdb-36657}" → ("X战警", Some(2000))
pub(crate) fn parse_lookup_title_year(path: &Path) -> Option<(String, Option<i64>)> {
    let name = if path.is_file()
        || crate::strm::is_strm_path(path)
        || crate::library::classify::classify_media_path(path, "movies").is_some()
    {
        path.file_stem()?.to_str()?
    } else {
        path.file_name()?.to_str()?
    };
    let cleaned = clean_provider_tags(name);
    let year = lookup_year_regex()
        .captures(name)
        .and_then(|captures| captures.name("year"))
        .and_then(|year| year.as_str().parse::<i64>().ok());
    let fallback_title = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let naming_path = std::path::PathBuf::from(format!("{name}.mkv"));
    let parsed_title = crate::library::naming::parse_media_name(&naming_path, "movies").title;
    let title = if parsed_title.is_empty() {
        fallback_title
    } else {
        parsed_title
    };
    if title.is_empty() {
        return None;
    }
    Some((title, year))
}

fn lookup_year_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        regex::Regex::new(
            r"(?:^|[^0-9])(?P<year>18[89][0-9]|19[0-9]{2}|20[0-9]{2}|2100)(?:[^0-9]|$)",
        )
        .expect("lookup year regex must compile")
    })
}

pub(crate) fn metadata_field_locked(item: &media_items::Model, field: &str) -> bool {
    metadata_field_locked_storage(item.locked_fields.as_deref(), field)
}

fn metadata_field_locked_storage(value: Option<&str>, field: &str) -> bool {
    value
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(field))
        })
}

fn merge_metadata_string_list(
    existing: Option<&str>,
    incoming: &[String],
    replace: bool,
) -> Option<String> {
    let mut values = if replace {
        Vec::new()
    } else {
        existing
            .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
            .unwrap_or_default()
    };
    for value in incoming.iter().map(|value| value.trim()) {
        if value.is_empty()
            || values
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(value))
        {
            continue;
        }
        values.push(value.to_string());
    }
    if values.is_empty() {
        None
    } else {
        serde_json::to_string(&values).ok()
    }
}

fn merge_remote_trailers(
    existing: Option<&str>,
    incoming: &[serde_json::Value],
    replace: bool,
) -> Option<String> {
    let mut values = if replace {
        Vec::new()
    } else {
        existing
            .and_then(|value| serde_json::from_str::<Vec<serde_json::Value>>(value).ok())
            .unwrap_or_default()
    };
    for trailer in incoming {
        let Some(url) = trailer
            .get("Url")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            continue;
        };
        if values.iter().any(|existing| {
            existing
                .get("Url")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|existing| existing.eq_ignore_ascii_case(url))
        }) {
            continue;
        }
        values.push(trailer.clone());
    }
    (!values.is_empty())
        .then(|| serde_json::to_string(&values).ok())
        .flatten()
}

async fn upsert_tmdb_people(
    db: &DatabaseConnection,
    client: &reqwest::Client,
    item_id: &str,
    metadata_people: &[TmdbPersonData],
) -> anyhow::Result<()> {
    if metadata_people.is_empty() {
        return Ok(());
    }

    let mut seen_names = std::collections::HashSet::new();
    let unique_people = metadata_people
        .iter()
        .filter(|(name, _, _, _, _)| seen_names.insert(name.trim().to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let now = now_unix();
    People::insert_many(unique_people.iter().map(|(name, _, _, _, tmdb_person_id)| {
        people::ActiveModel {
            id: Set(crate::util::stable_text_id(&format!(
                "people:{}",
                name.trim().to_ascii_lowercase()
            ))),
            name: Set(name.clone()),
            tmdb_id: Set(tmdb_person_id.clone()),
            created_at: Set(now),
            ..Default::default()
        }
    }))
    .on_conflict(
        OnConflict::column(people::Column::Name)
            .update_column(people::Column::TmdbId)
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;

    let names = unique_people
        .iter()
        .map(|(name, _, _, _, _)| name.clone())
        .collect::<Vec<_>>();
    let person_ids = People::find()
        .filter(people::Column::Name.is_in(names))
        .all(db)
        .await?
        .into_iter()
        .map(|person| (person.name.to_ascii_lowercase(), person.id))
        .collect::<HashMap<_, _>>();

    let mut seen_links = std::collections::HashSet::new();
    let links = metadata_people
        .iter()
        .enumerate()
        .filter_map(|(sort_order, (name, role, person_type, _, _))| {
            let person_id = person_ids.get(&name.to_ascii_lowercase())?.clone();
            if !seen_links.insert((person_id.clone(), person_type.to_ascii_lowercase())) {
                return None;
            }
            Some(media_people::ActiveModel {
                item_id: Set(item_id.to_string()),
                person_id: Set(person_id),
                role: Set(Some(role.clone())),
                person_type: Set(person_type.clone()),
                sort_order: Set(i64::try_from(sort_order).unwrap_or(i64::MAX)),
            })
        })
        .collect::<Vec<_>>();
    if !links.is_empty() {
        MediaPeople::insert_many(links)
            .on_conflict(
                OnConflict::columns([
                    media_people::Column::ItemId,
                    media_people::Column::PersonId,
                    media_people::Column::PersonType,
                ])
                .update_columns([media_people::Column::Role, media_people::Column::SortOrder])
                .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
    }

    let image_jobs = unique_people
        .into_iter()
        .filter_map(|(name, _, _, image_url, _)| {
            Some((
                person_ids.get(&name.to_ascii_lowercase())?.clone(),
                name,
                image_url?,
            ))
        })
        .collect::<Vec<_>>();
    futures_util::stream::iter(image_jobs)
        .map(|(person_id, name, image_url)| async move {
            if let Err(error) =
                download_and_save_tmdb_image(db, client, &person_id, &image_url, "Primary").await
            {
                tracing::warn!("failed to download person image for {name}: {error:#}");
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    Ok(())
}

async fn upsert_remote_provider_id(
    db: &DatabaseConnection,
    item_id: &str,
    provider: &str,
    provider_item_id: Option<String>,
    preserve_existing_metadata: bool,
) -> anyhow::Result<()> {
    let Some(provider_item_id) = provider_item_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    if preserve_existing_metadata
        && ProviderIds::find()
            .filter(provider_ids::Column::ItemId.eq(item_id))
            .filter(provider_ids::Column::Provider.eq(provider))
            .one(db)
            .await?
            .is_some()
    {
        return Ok(());
    }
    crate::db::provider_ids::upsert(db, item_id, provider, &provider_item_id).await
}

/// Search TMDb by name+year and return the first match's TMDb ID
async fn lookup_tmdb_id_by_name(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    name: &str,
    year: Option<i64>,
    is_tv: bool,
    language: &str,
) -> anyhow::Result<Option<String>> {
    let url = tmdb::api_url(
        tmdb_base_url,
        if is_tv { "search/tv" } else { "search/movie" },
    );
    let mut request = client.get(&url).query(&[
        ("api_key", api_key),
        ("query", name),
        ("language", language),
    ]);
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
    let rows = db.query_all_raw(crate::db::helpers::pg_statement(
        r#"SELECT mi.id, mi.title, mi.path, mi.item_type FROM media_items mi WHERE mi.item_type IN ('Movie', 'Series') AND mi.lock_data = 0 AND NOT EXISTS (SELECT 1 FROM provider_ids p WHERE p.item_id = mi.id AND p.provider = 'Tmdb')"#,
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
            parse_lookup_title_year(Path::new(&path_str)).unwrap_or_else(|| (title.clone(), None));
        if should_skip_name_based_tmdb_lookup(&name) {
            tracing::debug!("fill_missing_tmdb: skipped generic folder name '{name}'");
            continue;
        }

        let metadata_language = preferred_metadata_language_for_item(db, &item_id).await;
        match lookup_tmdb_id_by_name(
            client,
            api_key,
            tmdb_base_url,
            &name,
            year,
            is_tv,
            &metadata_language,
        )
        .await
        {
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
                    MetadataRefreshPolicy::automatic(false),
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
        .query_all_raw(crate::db::helpers::pg_statement(
            r#"SELECT mi.id, mi.title, mi.item_type, mi.locked_fields, p.provider_item_id AS tmdb_id
               FROM media_items mi
               JOIN provider_ids p ON p.item_id = mi.id AND p.provider = 'Tmdb'
               WHERE mi.item_type IN ('Movie', 'Series') AND mi.lock_data = 0"#,
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
        if metadata_field_locked_storage(
            row.get_opt_str("locked_fields").ok().flatten().as_deref(),
            "Name",
        ) {
            continue;
        }
        let item_type = row.get_str("item_type").unwrap_or_default();
        let tmdb_id = match row.get_str("tmdb_id") {
            Ok(value) => value,
            Err(_) => continue,
        };

        match fetch_tmdb_display_name(client, api_key, tmdb_base_url, &tmdb_id, &item_type).await {
            Ok(Some(title)) if title != current_title => {
                if let Some(item) = MediaItems::find_by_id(item_id.clone()).one(db).await? {
                    let mut active: media_items::ActiveModel = item.into();
                    active.title = Set(title);
                    active.updated_at = Set(now_unix());
                    active.update(db).await?;
                }
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
    policy: MetadataRefreshPolicy,
) -> anyhow::Result<()> {
    if !policy.refresh_metadata && !policy.refresh_images {
        return Ok(());
    }
    let preserve_existing_metadata = policy.preserves_metadata();
    let is_tv = item_type == "Series" || item_type == "Season" || item_type == "Episode";
    let metadata_language = preferred_metadata_language_for_item(db, item_id).await;
    let metadata_country_code = preferred_metadata_country_code_for_item(db, item_id).await;

    // Step 1: check for existing TMDb ID already stored (e.g. from NFO parsing)
    let mut tmdb_id = lookup_stored_tmdb_id(db, item_id)
        .await
        .ok()
        .flatten()
        .or_else(|| extract_tmdb_id(path));

    // Jellyfin resolves known external IDs through TMDb /find before falling
    // back to a name search. This avoids ambiguous localized/remake matches.
    if policy.refresh_metadata && tmdb_id.is_none() {
        match lookup_tmdb_id_by_stored_external_ids(
            db,
            item_id,
            client,
            api_key,
            tmdb_base_url,
            is_tv,
            &metadata_language,
        )
        .await
        {
            Ok(Some(id)) => {
                crate::db::provider_ids::upsert(db, item_id, "Tmdb", &id).await?;
                tmdb_id = Some(id);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    "TMDb external-id lookup failed for {item_type} {item_id}: {}",
                    redact_tmdb_error(&error)
                );
                return Ok(());
            }
        }
    }

    // Step 2: name-based search fallback — only for Movie and Series (Season names like "Season 1" are not searchable)
    if policy.refresh_metadata
        && tmdb_id.is_none()
        && (item_type == "Movie" || item_type == "Series")
    {
        if let Some((name, year)) = parse_lookup_title_year(path) {
            if should_skip_name_based_tmdb_lookup(&name) {
                tracing::debug!("TMDb name search skipped generic folder name '{name}'");
                return Ok(());
            }
            match lookup_tmdb_id_by_name(
                client,
                api_key,
                tmdb_base_url,
                &name,
                year,
                is_tv,
                &metadata_language,
            )
            .await
            {
                Ok(Some(id)) => {
                    tmdb_id = Some(id);
                    // Store the found TMDb ID so next time we don't need to search
                    crate::db::provider_ids::upsert(db, item_id, "Tmdb", tmdb_id.as_ref().unwrap())
                        .await?;
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
    if policy.refresh_metadata {
        crate::db::provider_ids::upsert(db, item_id, "Tmdb", &tmdb_id).await?;
    }
    let metadata = if is_tv {
        providers::tmdb_tv_details(
            client,
            api_key,
            &tmdb_id,
            tmdb_base_url,
            &metadata_language,
            &metadata_country_code,
        )
        .await
    } else {
        providers::tmdb_movie_details(
            client,
            api_key,
            &tmdb_id,
            tmdb_base_url,
            &metadata_language,
            &metadata_country_code,
        )
        .await
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
        .filter(|s| !s.is_empty())
        .map(|value| normalize_tmdb_overview(value, is_tv));
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
    let tags: Vec<String> = metadata
        .get("Tags")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let production_locations: Vec<String> = metadata
        .get("ProductionLocations")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let people: Vec<TmdbPersonData> = metadata
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
                        p.get("ProviderIds")
                            .and_then(|ids| ids.get("Tmdb"))
                            .and_then(|id| id.as_str())
                            .filter(|id| !id.is_empty())
                            .map(ToString::to_string),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let people_with_images = people
        .iter()
        .filter(|(_, _, _, image, _)| image.is_some())
        .count();
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
    let original_title = metadata
        .get("OriginalTitle")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let tagline = metadata
        .get("Tagline")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let end_date = metadata
        .get("EndDate")
        .and_then(|value| value.as_str())
        .and_then(normalize_yyyy_mm_dd);
    let collection_name = metadata
        .get("CollectionName")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let original_language = metadata
        .get("OriginalLanguage")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let series_status = metadata
        .get("Status")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let home_page_url = metadata
        .get("HomePageUrl")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"));
    let remote_trailers = metadata
        .get("RemoteTrailers")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut genres_locked = false;
    let mut studios_locked = false;
    let mut tags_locked = false;
    let mut cast_locked = false;
    if policy.refresh_metadata
        && let Some(item) = MediaItems::find_by_id(item_id.to_string()).one(db).await?
    {
        genres_locked = metadata_field_locked(&item, "Genres");
        studios_locked = metadata_field_locked(&item, "Studios");
        tags_locked = metadata_field_locked(&item, "Tags");
        let production_locations_locked = metadata_field_locked(&item, "ProductionLocations");
        cast_locked = metadata_field_locked(&item, "Cast");
        let mut active: media_items::ActiveModel = item.clone().into();
        if !metadata_field_locked(&item, "Name")
            && (!preserve_existing_metadata || item.title.trim().is_empty())
            && let Some(title) = title
        {
            active.title = Set(title.to_string());
        }
        if !metadata_field_locked(&item, "Overview") {
            if preserve_existing_metadata {
                if item.overview.is_none() && overview.is_some() {
                    active.overview = Set(overview);
                }
            } else {
                active.overview = Set(overview);
            }
        }
        if !preserve_existing_metadata || item.production_year.is_none() {
            active.production_year = Set(year);
        }
        if !preserve_existing_metadata || item.premiere_date.is_none() {
            active.premiere_date = Set(premiere_date.clone());
        }
        if !preserve_existing_metadata || item.community_rating.is_none() {
            active.community_rating = Set(community_rating);
        }
        if !preserve_existing_metadata || item.original_title.is_none() {
            active.original_title = Set(original_title.map(ToString::to_string));
        }
        if !preserve_existing_metadata || item.tagline.is_none() {
            active.tagline = Set(tagline.map(ToString::to_string));
        }
        if !preserve_existing_metadata || item.end_date.is_none() {
            active.end_date = Set(end_date);
        }
        if !preserve_existing_metadata || item.collection_name.is_none() {
            active.collection_name = Set(collection_name.map(ToString::to_string));
        }
        if !metadata_field_locked(&item, "OriginalLanguage")
            && (!preserve_existing_metadata || item.original_language.is_none())
        {
            active.original_language = Set(original_language.map(ToString::to_string));
        }
        if item_type == "Series" && (!preserve_existing_metadata || item.series_status.is_none()) {
            active.series_status = Set(series_status.map(ToString::to_string));
        }
        if !preserve_existing_metadata || item.home_page_url.is_none() {
            active.home_page_url = Set(home_page_url.map(ToString::to_string));
        }
        active.remote_trailers = Set(merge_remote_trailers(
            item.remote_trailers.as_deref(),
            &remote_trailers,
            !preserve_existing_metadata,
        ));
        if !production_locations_locked {
            active.production_locations = Set(merge_metadata_string_list(
                item.production_locations.as_deref(),
                &production_locations,
                !preserve_existing_metadata,
            ));
        }
        if !metadata_field_locked(&item, "OfficialRating")
            && (!preserve_existing_metadata || item.official_rating.is_none())
        {
            active.official_rating = Set(official_rating.map(ToString::to_string));
        }
        if item_type == "Series"
            && !metadata_field_locked(&item, "Runtime")
            && (!preserve_existing_metadata || item.runtime_ticks.is_none())
        {
            active.runtime_ticks = Set(runtime_ticks);
        }
        active.updated_at = Set(now_unix());
        active.update(db).await?;
    }

    // Store provider IDs
    if policy.refresh_metadata
        && let Some(remote_provider_ids) = metadata.get("ProviderIds")
    {
        if let Some(obj) = remote_provider_ids.as_object() {
            let existing_providers = if preserve_existing_metadata {
                ProviderIds::find()
                    .filter(provider_ids::Column::ItemId.eq(item_id))
                    .all(db)
                    .await?
                    .into_iter()
                    .map(|provider_id| provider_id.provider.to_ascii_lowercase())
                    .collect::<std::collections::HashSet<_>>()
            } else {
                std::collections::HashSet::new()
            };
            for (provider, id) in obj {
                if let Some(id_str) = id.as_str().filter(|s| !s.is_empty()) {
                    if existing_providers.contains(&provider.to_ascii_lowercase()) {
                        continue;
                    }
                    crate::db::provider_ids::upsert(db, item_id, provider.as_str(), id_str).await?;
                }
            }
        }
    }

    let preserve_local_genres = policy.refresh_metadata
        && preserve_existing_metadata
        && !genres_locked
        && MediaGenres::find()
            .filter(media_genres::Column::ItemId.eq(item_id))
            .one(db)
            .await?
            .is_some();
    if policy.refresh_metadata && !preserve_existing_metadata {
        if !genres_locked {
            MediaGenres::delete_many()
                .filter(media_genres::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        if !studios_locked {
            MediaStudios::delete_many()
                .filter(media_studios::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        if !tags_locked {
            MediaTags::delete_many()
                .filter(media_tags::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        if !cast_locked {
            MediaPeople::delete_many()
                .filter(media_people::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
    }

    if policy.refresh_metadata && !genres_locked && !preserve_local_genres {
        crate::library::storage::merge_named_relations(
            db,
            item_id,
            "genres",
            "media_genres",
            "genre_id",
            &genres,
        )
        .await?;
    }
    if policy.refresh_metadata && !studios_locked {
        crate::library::storage::merge_named_relations(
            db,
            item_id,
            "studios",
            "media_studios",
            "studio_id",
            &studios,
        )
        .await?;
    }
    // Jellyfin merges local NFO tags with remote tags, while a normal remote
    // refresh replaces stale links. The links were cleared above when needed.
    if policy.refresh_metadata && !tags_locked {
        crate::library::storage::merge_named_relations(
            db,
            item_id,
            "tags",
            "media_tags",
            "tag_id",
            &tags,
        )
        .await?;
    }

    if policy.refresh_metadata && !cast_locked {
        upsert_tmdb_people(db, client, item_id, &people).await?;
    }

    // Download poster and backdrop images
    if policy.refresh_images
        && let Some(image_url) = metadata
            .get("ImageUrl")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    {
        if remote_image_should_download(db, item_id, "Primary", policy.replace_images).await {
            let _ = download_and_save_tmdb_image(db, client, item_id, image_url, "Primary").await;
        }
    }
    // Also try to get backdrop from the original TMDb API response
    if policy.refresh_images
        && let Some(backdrop) = metadata.get("BackdropUrl").and_then(|v| v.as_str())
    {
        if remote_image_should_download(db, item_id, "Backdrop", policy.replace_images).await {
            let _ = download_and_save_tmdb_image(db, client, item_id, backdrop, "Backdrop").await;
        }
    }

    // Fetch additional images (logo, banner, art) from TMDb /images endpoint
    let images_url = if is_tv {
        tmdb::api_url(tmdb_base_url, &format!("tv/{tmdb_id}/images"))
    } else {
        tmdb::api_url(tmdb_base_url, &format!("movie/{tmdb_id}/images"))
    };
    let image_languages = tmdb_image_languages_param(&metadata_language);
    if policy.refresh_images
        && let Ok(resp) = client
            .get(&images_url)
            .query(&[
                ("api_key", api_key),
                ("include_image_language", image_languages.as_str()),
            ])
            .send()
            .await
    {
        if let Ok(images) = resp.json::<TmdbImagesResponse>().await {
            // Download logo (clearlogo)
            if let Some(logo) = images.preferred_logo(&metadata_language) {
                if remote_image_should_download(db, item_id, "Logo", policy.replace_images).await {
                    let url = tmdb::image_url(tmdb_base_url, "w500", &logo.file_path);
                    let _ = download_and_save_tmdb_image(db, client, item_id, &url, "Logo").await;
                }
            }
            // Jellyfin maps language-bearing backdrops (usually title art) to Thumb.
            if let Some(thumb) = images.preferred_thumb(&metadata_language)
                && remote_image_should_download(db, item_id, "Thumb", policy.replace_images).await
            {
                let url = tmdb::image_url(tmdb_base_url, "w1280", &thumb.file_path);
                let _ = download_and_save_tmdb_image(db, client, item_id, &url, "Thumb").await;
            }
        }
    }

    // For Season items, fetch season-specific poster from TMDb
    if policy.refresh_images && item_type == "Season" {
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

    if policy.refresh_metadata {
        mark_tmdb_metadata_current(db, item_id).await?;
    }
    Ok(())
}

async fn remote_image_should_download(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    image_type: &str,
    replace_image: bool,
) -> bool {
    let existing = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .filter(image_assets::Column::ImageType.eq(image_type))
        .one(db)
        .await
        .ok()
        .flatten();
    let Some(existing) = existing else {
        return true;
    };
    if !replace_image {
        return false;
    }
    is_tmdb_image_asset(existing.etag.as_deref(), existing.path.as_deref())
}

fn is_tmdb_image_asset(etag: Option<&str>, path: Option<&str>) -> bool {
    etag.is_some_and(|etag| etag.starts_with("tmdb:"))
        || path.is_some_and(|path| {
            Path::new(path)
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_tmdb"))
        })
}

pub async fn remove_tmdb_image_assets(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<()> {
    let assets = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(item_id))
        .all(db)
        .await?;
    for asset in assets {
        if !is_tmdb_image_asset(asset.etag.as_deref(), asset.path.as_deref()) {
            continue;
        }
        ImageAssets::delete_by_id(asset.id).exec(db).await?;
        if let Some(path) = asset.path.map(PathBuf::from)
            && path.starts_with(Path::new("data/images"))
        {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct TmdbFindResponse {
    #[serde(default)]
    movie_results: Vec<TmdbFindResult>,
    #[serde(default)]
    tv_results: Vec<TmdbFindResult>,
}

#[derive(serde::Deserialize)]
struct TmdbFindResult {
    id: i64,
}

async fn lookup_tmdb_id_by_stored_external_ids(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    is_tv: bool,
    metadata_language: &str,
) -> anyhow::Result<Option<String>> {
    for (providers, external_source) in [
        (&["Imdb", "IMDB"][..], "imdb_id"),
        (&["Tvdb", "TVDB"][..], "tvdb_id"),
    ] {
        let mut external_id = None;
        for provider in providers {
            if let Some(id) = crate::db::provider_ids::get(db, item_id, provider).await? {
                external_id = Some(id);
                break;
            }
        }
        let Some(external_id) = external_id else {
            continue;
        };
        if let Some(id) = lookup_tmdb_id_by_external_id(
            client,
            api_key,
            tmdb_base_url,
            &external_id,
            external_source,
            is_tv,
            metadata_language,
        )
        .await?
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

async fn lookup_tmdb_id_by_external_id(
    client: &reqwest::Client,
    api_key: &str,
    tmdb_base_url: Option<&str>,
    external_id: &str,
    external_source: &str,
    is_tv: bool,
    metadata_language: &str,
) -> anyhow::Result<Option<String>> {
    let response = client
        .get(tmdb::api_url(
            tmdb_base_url,
            &format!("find/{}", external_id.trim()),
        ))
        .query(&[
            ("api_key", api_key),
            ("external_source", external_source),
            ("language", metadata_language),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TmdbFindResponse>()
        .await?;
    Ok(tmdb_find_result_id(&response, is_tv))
}

fn tmdb_find_result_id(response: &TmdbFindResponse, is_tv: bool) -> Option<String> {
    if is_tv {
        response.tv_results.first()
    } else {
        response.movie_results.first()
    }
    .map(|result| result.id.to_string())
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
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!(
        "{}_{}_tmdb.{}",
        crate::util::stable_text_id(item_id),
        image_type.to_ascii_lowercase(),
        ext
    ));
    tokio::fs::write(&path, &bytes).await?;
    let now = crate::util::now_unix();
    ImageAssets::insert(image_assets::ActiveModel {
        id: Set(crate::util::stable_text_id(&format!(
            "image-asset:{item_id}:{image_type}:0"
        ))),
        item_id: Set(item_id.to_string()),
        image_type: Set(image_type.to_string()),
        image_index: Set(0),
        path: Set(Some(path.to_string_lossy().to_string())),
        etag: Set(Some(format!("tmdb:{}", crate::util::stable_text_id(url)))),
        width: Set(None),
        height: Set(None),
        size_bytes: Set(Some(i64::try_from(bytes.len()).unwrap_or(i64::MAX))),
        created_at: Set(now),
        updated_at: Set(now),
    })
    .on_conflict(
        OnConflict::columns([
            image_assets::Column::ItemId,
            image_assets::Column::ImageType,
            image_assets::Column::ImageIndex,
        ])
        .update_columns([
            image_assets::Column::Path,
            image_assets::Column::Etag,
            image_assets::Column::SizeBytes,
            image_assets::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

fn extract_season_number(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    parse_season_number(name)
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
    fn preferred_logo(&self, metadata_language: &str) -> Option<&TmdbImageEntry> {
        preferred_tmdb_image(&self.logos, metadata_language, |_| true)
    }

    fn preferred_thumb(&self, metadata_language: &str) -> Option<&TmdbImageEntry> {
        preferred_tmdb_image(&self.backdrops, metadata_language, |entry| {
            !image_has_no_language(entry)
        })
    }
}

fn preferred_tmdb_image<'a>(
    entries: &'a [TmdbImageEntry],
    metadata_language: &str,
    include: impl Fn(&TmdbImageEntry) -> bool,
) -> Option<&'a TmdbImageEntry> {
    let preferred = tmdb_image_language(metadata_language);
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| include(entry))
        .max_by_key(|(index, entry)| {
            (
                tmdb_image_language_priority(entry, preferred),
                (entry.vote_average.unwrap_or_default() * 10.0).round() as i64,
                entry.vote_count.unwrap_or_default(),
                std::cmp::Reverse(*index),
            )
        })
        .map(|(_, entry)| entry)
}

fn tmdb_image_language_priority(entry: &TmdbImageEntry, preferred: &str) -> i64 {
    if image_language_matches(entry, preferred) {
        4
    } else if image_language_matches(entry, "en") {
        3
    } else if image_has_no_language(entry) {
        2
    } else {
        0
    }
}

fn normalize_tmdb_overview(value: &str, is_tv: bool) -> String {
    if is_tv {
        value.to_string()
    } else {
        value.replace("\n\n", "\n")
    }
}

fn tmdb_image_language(metadata_language: &str) -> &str {
    metadata_language
        .split(['-', '_'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("en")
}

fn tmdb_image_languages_param(metadata_language: &str) -> String {
    let language = metadata_language.trim();
    if language.is_empty() || language.eq_ignore_ascii_case("en") {
        "en,null".to_string()
    } else {
        format!("{language},null,en")
    }
}

fn image_language_matches(entry: &TmdbImageEntry, language: &str) -> bool {
    entry
        .iso_639_1
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(language))
}

fn image_has_no_language(entry: &TmdbImageEntry) -> bool {
    entry
        .iso_639_1
        .as_deref()
        .is_none_or(|language| language.is_empty() || language.eq_ignore_ascii_case("xx"))
}

#[derive(serde::Deserialize, Default)]
struct TmdbImageEntry {
    file_path: String,
    #[serde(default)]
    iso_639_1: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    vote_count: Option<i64>,
}

async fn get_parent_series_tmdb_id(
    db: &DatabaseConnection,
    season_item_id: &str,
) -> Option<String> {
    let season = MediaItems::find_by_id(season_item_id.to_string())
        .one(db)
        .await
        .ok()??;
    crate::db::provider_ids::get(db, &season.parent_id, "Tmdb")
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
    language: &str,
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
        .query(&[
            ("api_key", api_key),
            ("query", name),
            ("language", language),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.results.into_iter().next().map(|p| p.id.to_string()))
}

/// Fetch person details from TMDb and update the database
#[derive(serde::Deserialize)]
struct TmdbPersonResponse {
    biography: Option<String>,
    imdb_id: Option<String>,
    homepage: Option<String>,
    birthday: Option<String>,
    deathday: Option<String>,
    place_of_birth: Option<String>,
}

async fn apply_tmdb_person_response(
    db: &sea_orm::DatabaseConnection,
    person_id: &str,
    tmdb_id: &str,
    response: TmdbPersonResponse,
) -> anyhow::Result<()> {
    if let Some(person) = People::find_by_id(person_id.to_string()).one(db).await? {
        let mut active: people::ActiveModel = person.into();
        if let Some(biography) = response.biography.filter(|value| !value.is_empty()) {
            active.overview = Set(Some(biography));
        }
        active.tmdb_id = Set(Some(tmdb_id.to_string()));
        active.imdb_id = Set(response.imdb_id.filter(|value| !value.trim().is_empty()));
        active.home_page_url = Set(response.homepage.filter(|value| !value.trim().is_empty()));
        active.premiere_date = Set(response.birthday.as_deref().and_then(normalize_yyyy_mm_dd));
        active.end_date = Set(response.deathday.as_deref().and_then(normalize_yyyy_mm_dd));
        active.production_locations = Set(response
            .place_of_birth
            .filter(|value| !value.trim().is_empty())
            .and_then(|value| serde_json::to_string(&vec![value]).ok()));
        active.update(db).await?;
    }
    Ok(())
}

pub async fn fetch_person_tmdb(
    db: &sea_orm::DatabaseConnection,
    person_id: &str,
    tmdb_id: &str,
    api_key: &str,
    client: &reqwest::Client,
    tmdb_base_url: Option<&str>,
) -> anyhow::Result<()> {
    let url = tmdb::api_url(tmdb_base_url, &format!("person/{tmdb_id}"));
    let metadata_language =
        crate::db::settings::get_non_empty_or_default(db, "PreferredMetadataLanguage", "zh-CN")
            .await;
    let response: TmdbPersonResponse = client
        .get(&url)
        .query(&[
            ("api_key", api_key),
            ("language", metadata_language.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    apply_tmdb_person_response(db, person_id, tmdb_id, response).await?;
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
    let metadata_language =
        crate::db::settings::get_non_empty_or_default(db, "PreferredMetadataLanguage", "zh-CN")
            .await;
    match search_person_tmdb(
        person_name,
        api_key,
        client,
        tmdb_base_url,
        &metadata_language,
    )
    .await
    {
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
    let people = People::find()
        .all(db)
        .await
        .context("failed to list people without biography")?;

    let mut count = 0;
    for person in people
        .into_iter()
        .filter(|person| {
            (person.overview.is_none() || person.tmdb_id.is_none()) && !person.name.is_empty()
        })
        .take(50)
    {
        try_fetch_person_tmdb(db, &person.id, &person.name, api_key, client, tmdb_base_url).await;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::{
        EpisodeTmdbCastMember, EpisodeTmdbCredits, EpisodeTmdbCrewMember, EpisodeTmdbResponse,
        EpisodeTmdbTarget, EpisodeTmdbVideo, EpisodeTmdbVideos, MetadataRefreshPolicy,
        TmdbEpisodeGroup, TmdbEpisodeGroupCollection, TmdbEpisodeGroupEpisode, TmdbFindResponse,
        TmdbImageEntry, TmdbImagesResponse, TmdbLibraryProviderOptions, TmdbPersonResponse,
        apply_tmdb_person_response, clean_provider_tags, clean_title_with_year,
        episode_remote_trailers, episode_title_candidate, extract_tmdb_id, is_tmdb_image_asset,
        local_episode_title_from_path, merge_metadata_string_list, merge_remote_trailers,
        metadata_field_locked_storage, normalize_tmdb_overview, parse_lookup_title_year,
        parse_season_number, redact_tmdb_api_key, resolve_tmdb_episode_group_mapping,
        should_skip_name_based_tmdb_lookup, tmdb_credit_people, tmdb_episode_group_type,
        tmdb_episode_response_for_target, tmdb_find_result_id, upsert_tmdb_people,
    };
    use crate::{
        entities::{
            media_people::{self, Entity as MediaPeople},
            people::{self, Entity as People},
            provider_ids::{self, Entity as ProviderIds},
        },
        library::storage::{ScannedMediaItem, upsert_media_item},
    };
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
    use std::{collections::HashMap, path::Path};

    #[test]
    fn tmdb_error_redaction_hides_api_key_query_values() {
        let message = "error sending request for url (https://api.themoviedb.org/3/movie/1?api_key=secret123&language=zh-CN)";
        let redacted = redact_tmdb_api_key(message);
        assert!(!redacted.contains("secret123"));
        assert!(redacted.contains("api_key=<redacted>&language=zh-CN"));
    }

    #[test]
    fn library_type_options_control_tmdb_metadata_and_images_independently() {
        let options = TmdbLibraryProviderOptions::from_json(&serde_json::json!({
            "TypeOptions": [
                {
                    "Type": "Movie",
                    "MetadataFetchers": ["TheMovieDb"],
                    "ImageFetchers": []
                },
                {
                    "Type": "Episode",
                    "MetadataFetchers": [],
                    "ImageFetchers": ["themoviedb"]
                }
            ]
        }));

        let movie = options.automatic_policy("Movie", false).unwrap();
        assert!(movie.refresh_metadata);
        assert!(!movie.refresh_images);
        let episode = options.automatic_policy("episode", false).unwrap();
        assert!(!episode.refresh_metadata);
        assert!(episode.refresh_images);
        assert!(options.automatic_policy("Series", false).is_some());
    }

    #[test]
    fn empty_library_type_provider_lists_disable_tmdb() {
        let options = TmdbLibraryProviderOptions::from_json(&serde_json::json!({
            "typeOptions": [{
                "type": "Season",
                "metadataFetchers": [],
                "imageFetchers": []
            }]
        }));

        assert!(options.automatic_policy("Season", false).is_none());
        let defaults = TmdbLibraryProviderOptions::default()
            .automatic_policy("Season", true)
            .unwrap();
        assert!(defaults.refresh_metadata);
        assert!(defaults.refresh_images);
        assert!(!defaults.replace_metadata);
    }

    #[test]
    fn tmdb_image_assets_recognize_prefixed_and_legacy_cache_records() {
        assert!(is_tmdb_image_asset(Some("tmdb:abc"), None));
        assert!(is_tmdb_image_asset(
            Some("legacy-uuid"),
            Some("data/images/item_primary_tmdb.jpg")
        ));
        assert!(!is_tmdb_image_asset(
            Some("sidecar"),
            Some("/media/Movie/poster.jpg")
        ));
    }

    #[test]
    fn tmdb_find_selects_result_for_requested_media_type() {
        let response: TmdbFindResponse = serde_json::from_str(
            r#"{
                "movie_results": [{"id": 101}],
                "tv_results": [{"id": 202}]
            }"#,
        )
        .unwrap();

        assert_eq!(
            tmdb_find_result_id(&response, false).as_deref(),
            Some("101")
        );
        assert_eq!(tmdb_find_result_id(&response, true).as_deref(), Some("202"));
    }

    #[test]
    fn tmdb_movie_overview_collapses_double_newlines_only_for_movies() {
        assert_eq!(normalize_tmdb_overview("one\n\ntwo", false), "one\ntwo");
        assert_eq!(normalize_tmdb_overview("one\n\ntwo", true), "one\n\ntwo");
    }

    #[test]
    fn tmdb_images_follow_item_language_then_official_fallback_order() {
        let images = TmdbImagesResponse {
            logos: vec![
                TmdbImageEntry {
                    file_path: "/en.png".to_string(),
                    iso_639_1: Some("en".to_string()),
                    ..Default::default()
                },
                TmdbImageEntry {
                    file_path: "/neutral.png".to_string(),
                    iso_639_1: None,
                    ..Default::default()
                },
                TmdbImageEntry {
                    file_path: "/ja.png".to_string(),
                    iso_639_1: Some("ja".to_string()),
                    vote_average: Some(7.2),
                    vote_count: Some(20),
                },
                TmdbImageEntry {
                    file_path: "/ja-rated.png".to_string(),
                    iso_639_1: Some("ja".to_string()),
                    vote_average: Some(7.24),
                    vote_count: Some(30),
                },
            ],
            backdrops: vec![
                TmdbImageEntry {
                    file_path: "/en-backdrop.jpg".to_string(),
                    iso_639_1: Some("en".to_string()),
                    ..Default::default()
                },
                TmdbImageEntry {
                    file_path: "/ja-backdrop.jpg".to_string(),
                    iso_639_1: Some("ja".to_string()),
                    ..Default::default()
                },
            ],
            posters: Vec::new(),
        };

        assert_eq!(
            images.preferred_logo("ja-JP").unwrap().file_path,
            "/ja-rated.png"
        );
        assert_eq!(images.preferred_logo("fr-FR").unwrap().file_path, "/en.png");
        assert_eq!(
            images.preferred_thumb("ja-JP").unwrap().file_path,
            "/ja-backdrop.jpg"
        );
    }

    #[tokio::test]
    async fn tmdb_people_are_batch_linked_without_invalid_media_provider_ids() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item_id = format!("movie-people-{}", uuid::Uuid::new_v4().simple());
        let item = ScannedMediaItem {
            id: item_id.clone(),
            title: "People Movie".to_string(),
            path: format!("/tmp/{item_id}.mkv"),
            item_type: "Movie".to_string(),
            modified_at: 1,
            created_at: 1,
            ..Default::default()
        };
        upsert_media_item(&db, &item).await.unwrap();
        let people = vec![
            (
                "Actor One".to_string(),
                "Lead".to_string(),
                "Actor".to_string(),
                None,
                Some("101".to_string()),
            ),
            (
                "Director One".to_string(),
                "Director".to_string(),
                "Director".to_string(),
                None,
                Some("202".to_string()),
            ),
        ];

        upsert_tmdb_people(&db, &reqwest::Client::new(), &item_id, &people)
            .await
            .unwrap();

        assert_eq!(
            MediaPeople::find()
                .filter(media_people::Column::ItemId.eq(&item_id))
                .count(&db)
                .await
                .unwrap(),
            2
        );
        let actor = People::find()
            .filter(people::Column::Name.eq("Actor One"))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actor.tmdb_id.as_deref(), Some("101"));
        assert_eq!(
            ProviderIds::find()
                .filter(provider_ids::Column::ItemId.eq(&actor.id))
                .count(&db)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn tmdb_person_fields_round_trip_through_postgresql() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let id = format!("person-{}", uuid::Uuid::new_v4().simple());
        People::insert(people::ActiveModel {
            id: Set(id.clone()),
            name: Set(format!("Person {id}")),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        apply_tmdb_person_response(
            &db,
            &id,
            "98765",
            TmdbPersonResponse {
                biography: Some("Biography".to_string()),
                imdb_id: Some("nm1234567".to_string()),
                homepage: Some("https://person.example".to_string()),
                birthday: Some("1980-02-03".to_string()),
                deathday: Some("2024-04-05".to_string()),
                place_of_birth: Some("Shanghai, China".to_string()),
            },
        )
        .await
        .unwrap();

        let person = People::find_by_id(&id).one(&db).await.unwrap().unwrap();
        assert_eq!(person.overview.as_deref(), Some("Biography"));
        assert_eq!(person.tmdb_id.as_deref(), Some("98765"));
        assert_eq!(person.imdb_id.as_deref(), Some("nm1234567"));
        assert_eq!(
            person.home_page_url.as_deref(),
            Some("https://person.example")
        );
        assert_eq!(person.premiere_date.as_deref(), Some("1980-02-03"));
        assert_eq!(person.end_date.as_deref(), Some("2024-04-05"));
        assert_eq!(
            person.production_locations.as_deref(),
            Some(r#"["Shanghai, China"]"#)
        );
    }

    #[test]
    fn metadata_string_lists_replace_or_merge_like_jellyfin() {
        let existing = r#"["United States","Japan"]"#;
        assert_eq!(
            merge_metadata_string_list(
                Some(existing),
                &["japan".to_string(), "Canada".to_string()],
                false,
            )
            .as_deref(),
            Some(r#"["United States","Japan","Canada"]"#)
        );
        assert_eq!(
            merge_metadata_string_list(Some(existing), &["Canada".to_string()], true).as_deref(),
            Some(r#"["Canada"]"#)
        );
        assert_eq!(merge_metadata_string_list(Some(existing), &[], true), None);
    }

    #[test]
    fn remote_trailers_merge_local_nfo_and_tmdb_without_duplicates() {
        let existing = r#"[{"Url":"https://www.youtube.com/watch?v=local"}]"#;
        let incoming = vec![
            serde_json::json!({
                "Name": "Duplicate",
                "Url": "https://www.youtube.com/watch?v=local"
            }),
            serde_json::json!({
                "Name": "TMDb Trailer",
                "Url": "https://www.youtube.com/watch?v=remote"
            }),
        ];
        let merged = merge_remote_trailers(Some(existing), &incoming, false).unwrap();
        let merged: Vec<serde_json::Value> = serde_json::from_str(&merged).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["Url"], "https://www.youtube.com/watch?v=local");
        assert_eq!(merged[1]["Name"], "TMDb Trailer");
    }

    #[test]
    fn tmdb_episode_credits_preserve_cast_order_deduplicate_guests_and_map_crew() {
        let cast_member = |id: i64, name: &str, order: i64| EpisodeTmdbCastMember {
            id,
            name: name.to_string(),
            character: Some(format!("Role {name}")),
            profile_path: Some(format!("/{id}.jpg")),
            order,
        };
        let crew_member =
            |id: i64, name: &str, department: &str, job: &str| EpisodeTmdbCrewMember {
                id,
                name: name.to_string(),
                department: Some(department.to_string()),
                job: Some(job.to_string()),
                profile_path: None,
            };
        let duplicate_guest = cast_member(3, "Guest One", 1);
        let credits = EpisodeTmdbCredits {
            cast: vec![cast_member(2, "Cast Two", 2), cast_member(1, "Cast One", 0)],
            guest_stars: vec![duplicate_guest.clone()],
            crew: vec![
                crew_member(5, "Director", "Directing", "Director"),
                crew_member(6, "Writer", "Writing", "Screenplay"),
            ],
        };
        let people = tmdb_credit_people(
            Some(&credits),
            &[cast_member(4, "Guest Zero", 0), duplicate_guest],
            &[
                crew_member(7, "Producer", "Production", "Producer"),
                crew_member(8, "Ignored", "Camera", "Director"),
            ],
            Some("https://image.tmdb.test"),
        );

        let summary = people
            .iter()
            .map(|(name, _, person_type, _, id)| {
                (name.as_str(), person_type.as_str(), id.as_deref())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                ("Cast One", "Actor", Some("1")),
                ("Cast Two", "Actor", Some("2")),
                ("Guest Zero", "GuestStar", Some("4")),
                ("Guest One", "GuestStar", Some("3")),
                ("Director", "Director", Some("5")),
                ("Writer", "Writer", Some("6")),
                ("Producer", "Producer", Some("7")),
            ]
        );
        assert_eq!(
            people[0].3.as_deref(),
            Some("https://image.tmdb.test/t/p/w185/1.jpg")
        );
    }

    #[test]
    fn tmdb_episode_external_ids_follow_appended_response_contract() {
        let episode: EpisodeTmdbResponse = serde_json::from_value(serde_json::json!({
            "id": 42,
            "episode_number": 3,
            "external_ids": {
                "imdb_id": "tt1234567",
                "tvdb_id": 7654321,
                "tvrage_id": 2468
            }
        }))
        .unwrap();
        let ids = episode.external_ids.unwrap();
        assert_eq!(ids.imdb_id.as_deref(), Some("tt1234567"));
        assert_eq!(ids.tvdb_id, Some(7_654_321));
        assert_eq!(ids.tvrage_id, Some(2_468));
    }

    #[test]
    fn clean_title_with_year_keeps_title_and_year() {
        assert_eq!(
            clean_title_with_year("Movie Name (2024) {tmdb-123}"),
            ("Movie Name".to_string(), Some(2024))
        );
        assert_eq!(
            clean_provider_tags("Show Name [tvdbid-123] {anidbid=456}"),
            "Show Name"
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
    fn lookup_name_uses_jellyfin_file_naming_cleanup() {
        assert_eq!(
            parse_lookup_title_year(Path::new("Movie.Name.Extended.2024.2160p.BluRay.x265.mkv")),
            Some(("Movie Name".to_string(), Some(2024)))
        );
        assert_eq!(
            parse_lookup_title_year(Path::new("Movie.Name (2024) {tmdb-123}")),
            Some(("Movie Name".to_string(), Some(2024)))
        );
    }

    #[test]
    fn metadata_field_locks_are_case_insensitive() {
        assert!(metadata_field_locked_storage(
            Some(r#"["Name","Overview"]"#),
            "overview"
        ));
        assert!(!metadata_field_locked_storage(
            Some(r#"["Name","Overview"]"#),
            "Genres"
        ));
        assert!(!metadata_field_locked_storage(Some("invalid"), "Name"));
    }

    #[test]
    fn season_number_parser_handles_common_folder_names() {
        assert_eq!(parse_season_number("Season 1"), Some(1));
        assert_eq!(parse_season_number("season_2"), Some(2));
        assert_eq!(parse_season_number("第3季"), Some(3));
        assert_eq!(parse_season_number("灵笼 第一季（2019）"), Some(1));
        assert_eq!(parse_season_number("灵笼 第二季（2025）"), Some(2));
        assert_eq!(parse_season_number("第十二季"), Some(12));
        assert_eq!(parse_season_number("S04"), Some(4));
        assert_eq!(parse_season_number("Specials"), Some(0));
        assert_eq!(parse_season_number("01"), Some(1));
        assert_eq!(parse_season_number("1st Season"), Some(1));
        assert_eq!(parse_season_number("Staffel 2"), Some(2));
        assert_eq!(parse_season_number("Temporada 3"), Some(3));
        assert_eq!(parse_season_number("Season11080p"), Some(1));
        assert_eq!(parse_season_number("Season 1 E01"), None);
    }

    #[test]
    fn tmdb_episode_range_merges_names_and_overviews_like_jellyfin() {
        let target = EpisodeTmdbTarget {
            episode_id: "episode-1".to_string(),
            current_title: "Show".to_string(),
            path: "/media/Show/S01E01-E03.mkv".to_string(),
            series_title: "Show".to_string(),
            series_year: Some(2024),
            episode_number: 1,
            episode_number_end: Some(3),
            refresh_policy: MetadataRefreshPolicy::automatic(false),
        };
        let episodes = HashMap::from([
            (
                1,
                episode_response(101, 1, "One", "Overview one", "2024-01-01", 7.1),
            ),
            (
                2,
                episode_response(102, 2, "Two", "Overview two", "2024-01-08", 7.2),
            ),
            (
                3,
                episode_response(103, 3, "Three", "Overview three", "2024-01-15", 7.3),
            ),
        ]);

        let merged = tmdb_episode_response_for_target(&episodes, &target).unwrap();

        assert_eq!(merged.id, Some(101));
        assert_eq!(merged.episode_number, Some(1));
        assert_eq!(merged.name.as_deref(), Some("One / Two / Three"));
        assert_eq!(
            merged.overview.as_deref(),
            Some("Overview one / Overview two / Overview three")
        );
        assert_eq!(merged.air_date.as_deref(), Some("2024-01-01"));
        assert_eq!(merged.vote_average, Some(7.1));
    }

    #[test]
    fn tmdb_episode_group_type_matches_jellyfin_display_orders() {
        assert_eq!(tmdb_episode_group_type(Some("originalAirDate")), Some(1));
        assert_eq!(tmdb_episode_group_type(Some("absolute")), Some(2));
        assert_eq!(tmdb_episode_group_type(Some("dvd")), Some(3));
        assert_eq!(tmdb_episode_group_type(Some("digital")), Some(4));
        assert_eq!(tmdb_episode_group_type(Some("storyArc")), Some(5));
        assert_eq!(tmdb_episode_group_type(Some("production")), Some(6));
        assert_eq!(tmdb_episode_group_type(Some("tv")), Some(7));
        assert_eq!(tmdb_episode_group_type(Some("airdate")), None);
        assert_eq!(tmdb_episode_group_type(None), None);
    }

    #[test]
    fn tmdb_episode_group_mapping_uses_zero_based_episode_order_like_jellyfin() {
        let group = TmdbEpisodeGroupCollection {
            groups: vec![TmdbEpisodeGroup {
                order: 1,
                episodes: vec![TmdbEpisodeGroupEpisode {
                    order: 74,
                    episode_number: 23,
                    season_number: 2,
                }],
            }],
        };

        assert_eq!(
            resolve_tmdb_episode_group_mapping(&group, 1, 75),
            Some((2, 23))
        );
        assert_eq!(resolve_tmdb_episode_group_mapping(&group, 1, 74), None);
    }

    fn episode_response(
        id: i64,
        episode_number: i64,
        name: &str,
        overview: &str,
        air_date: &str,
        vote_average: f64,
    ) -> EpisodeTmdbResponse {
        EpisodeTmdbResponse {
            id: Some(id),
            episode_number: Some(episode_number),
            name: Some(name.to_string()),
            overview: Some(overview.to_string()),
            still_path: None,
            air_date: Some(air_date.to_string()),
            vote_average: Some(vote_average),
            guest_stars: Vec::new(),
            crew: Vec::new(),
            credits: None,
            external_ids: None,
            videos: None,
        }
    }

    #[test]
    fn tmdb_episode_trailers_follow_jellyfin_video_filter() {
        let mut episode = episode_response(101, 1, "One", "Overview", "2024-01-01", 7.1);
        episode.videos = Some(EpisodeTmdbVideos {
            results: vec![
                EpisodeTmdbVideo {
                    key: "trailer".to_string(),
                    name: "Trailer".to_string(),
                    site: "YouTube".to_string(),
                    video_type: "Trailer".to_string(),
                },
                EpisodeTmdbVideo {
                    key: "teaser".to_string(),
                    name: "Teaser".to_string(),
                    site: "youtube".to_string(),
                    video_type: "teaser".to_string(),
                },
                EpisodeTmdbVideo {
                    key: "clip".to_string(),
                    name: "Clip".to_string(),
                    site: "YouTube".to_string(),
                    video_type: "Clip".to_string(),
                },
            ],
        });

        let trailers = episode_remote_trailers(&episode);
        assert_eq!(trailers.len(), 2);
        assert_eq!(
            trailers[0]["Url"],
            "https://www.youtube.com/watch?v=trailer"
        );
        assert_eq!(trailers[1]["Name"], "Teaser");
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
