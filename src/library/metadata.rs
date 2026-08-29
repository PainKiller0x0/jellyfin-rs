use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;

use crate::util::{normalize_yyyy_mm_dd, year_from_yyyy_mm_dd};

#[derive(Default)]
pub struct ParsedMetadata {
    pub has_nfo: bool,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
    pub custom_rating: Option<String>,
    pub video_3d_format: Option<String>,
    pub original_title: Option<String>,
    pub sort_name: Option<String>,
    pub forced_sort_name: Option<String>,
    pub lock_data: Option<bool>,
    pub locked_fields: Vec<String>,
    pub tagline: Option<String>,
    pub collection_name: Option<String>,
    pub original_language: Option<String>,
    pub preferred_metadata_language: Option<String>,
    pub preferred_metadata_country_code: Option<String>,
    pub series_status: Option<String>,
    pub air_days: Vec<String>,
    pub air_time: Option<String>,
    pub home_page_url: Option<String>,
    pub remote_trailers: Vec<String>,
    pub production_locations: Vec<String>,
    pub production_year: Option<i64>,
    pub premiere_date: Option<String>,
    pub end_date: Option<String>,
    pub created_at: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub has_subtitles: Option<bool>,
    pub watched: Option<bool>,
    pub play_count: Option<i64>,
    pub last_played_at: Option<i64>,
    pub display_order: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub ending_episode_number: Option<i64>,
    pub airs_before_episode_number: Option<i64>,
    pub airs_after_season_number: Option<i64>,
    pub airs_before_season_number: Option<i64>,
    pub series_name: Option<String>,
    pub community_rating: Option<f64>,
    pub critic_rating: Option<f64>,
    pub provider_ids: Vec<(String, String)>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub studios: Vec<String>,
    pub people: Vec<ParsedPerson>,
    pub images: Vec<ParsedImage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedPerson {
    pub name: String,
    pub role: Option<String>,
    pub person_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedImage {
    pub image_type: String,
    pub path: String,
}

#[allow(dead_code)]
pub async fn parse_sidecar_metadata(path: &Path) -> ParsedMetadata {
    parse_sidecar_metadata_for_item(path, "").await
}

pub async fn parse_sidecar_metadata_for_item(path: &Path, item_type: &str) -> ParsedMetadata {
    let mut metadata = parse_filename_metadata(path);
    if let Some(nfo) = read_sidecar_nfo_for_item(path, item_type).await {
        metadata.has_nfo = true;
        merge_nfo_metadata_for_item(&mut metadata, &nfo, item_type);
    }
    metadata
}

fn parse_filename_metadata(path: &Path) -> ParsedMetadata {
    let title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled")
        .replace(['.', '_'], " ");
    let production_year = find_year(&title);
    let title = production_year
        .map(|year| {
            title
                .replace(&format!("({year})"), "")
                .replace(&format!("[{year}]"), "")
                .replace(&year.to_string(), "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or(title);

    ParsedMetadata {
        title: Some(title),
        production_year,
        provider_ids: provider_ids_from_path(path),
        ..Default::default()
    }
}

pub fn provider_ids_from_path(path: &Path) -> Vec<(String, String)> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        let mut rest = name;
        while let Some((_, after_open)) = rest.split_once(open) {
            let Some((tag, after_close)) = after_open.split_once(close) else {
                break;
            };
            if let Some((provider, id)) = provider_id_from_tag(tag) {
                upsert_provider_value(&mut ids, provider, id);
            }
            rest = after_close;
        }
    }
    ids.sort();
    ids
}

fn provider_id_from_tag(tag: &str) -> Option<(&'static str, String)> {
    let tag = tag.trim();
    let lower = tag.to_ascii_lowercase();
    for (provider, prefixes) in [
        ("Tmdb", ["tmdb-", "tmdb=", "tmdbid-", "tmdbid="].as_slice()),
        (
            "Douban",
            ["douban-", "douban=", "doubanid-", "doubanid="].as_slice(),
        ),
        ("IMDB", ["imdb-", "imdb=", "imdbid-", "imdbid="].as_slice()),
        ("Tvdb", ["tvdb-", "tvdb=", "tvdbid-", "tvdbid="].as_slice()),
        (
            "TvMaze",
            ["tvmaze-", "tvmaze=", "tvmazeid-", "tvmazeid="].as_slice(),
        ),
        (
            "TvRage",
            ["tvrage-", "tvrage=", "tvrageid-", "tvrageid="].as_slice(),
        ),
        (
            "AniDB",
            ["anidb-", "anidb=", "anidbid-", "anidbid="].as_slice(),
        ),
        (
            "AniList",
            ["anilist-", "anilist=", "anilistid-", "anilistid="].as_slice(),
        ),
        (
            "AniSearch",
            ["anisearch-", "anisearch=", "anisearchid-", "anisearchid="].as_slice(),
        ),
    ] {
        for prefix in prefixes {
            if lower.starts_with(prefix) {
                let value = tag[prefix.len()..].trim();
                if !value.is_empty() {
                    return Some((provider, value.to_string()));
                }
            }
        }
    }
    None
}

fn upsert_provider_value(ids: &mut Vec<(String, String)>, provider: &str, value: String) {
    ids.retain(|(existing, _)| existing != provider);
    ids.push((provider.to_string(), value));
}

async fn read_sidecar_nfo_for_item(path: &Path, item_type: &str) -> Option<String> {
    for candidate in sidecar_nfo_candidates(path, item_type) {
        if let Ok(contents) = tokio::fs::read_to_string(&candidate).await {
            return Some(contents);
        }
    }
    None
}

fn sidecar_nfo_candidates(path: &Path, item_type: &str) -> Vec<PathBuf> {
    match item_type {
        "Folder" => Vec::new(),
        "Series" => vec![path.join("tvshow.nfo")],
        "Season" => vec![path.join("season.nfo")],
        "MusicArtist" => vec![path.join("artist.nfo")],
        "MusicAlbum" => vec![path.join("album.nfo")],
        "Movie" if path.is_dir() => vec![path.join("movie.nfo"), path.with_extension("nfo")],
        "Movie" | "Episode" | "Video" | "Trailer" | "Audio" | "AudioBook" | "MusicVideo" => {
            vec![path.with_extension("nfo")]
        }
        _ if path.is_dir() => vec![
            path.join("movie.nfo"),
            path.join("tvshow.nfo"),
            path.join("season.nfo"),
            path.with_extension("nfo"),
        ],
        _ => {
            let mut candidates = vec![path.with_extension("nfo")];
            if let Some(parent) = path.parent() {
                candidates.push(parent.join("movie.nfo"));
            }
            candidates
        }
    }
}

fn merge_nfo_metadata(metadata: &mut ParsedMetadata, nfo: &str) {
    merge_nfo_metadata_for_item(metadata, nfo, "");
}

fn merge_nfo_metadata_for_item(metadata: &mut ParsedMetadata, nfo: &str, item_type: &str) {
    let full_nfo = nfo;
    let episode_blocks = (item_type == "Episode")
        .then(|| blocks(full_nfo, "episodedetails"))
        .unwrap_or_default();
    let nfo = episode_blocks
        .first()
        .map(|block| block.contents.as_str())
        .unwrap_or(full_nfo);

    metadata.original_title = first_tag(nfo, &["originaltitle"]).or(metadata.original_title.take());
    metadata.title = first_tag(nfo, &["title", "localtitle", "name"])
        .or_else(|| metadata.original_title.clone())
        .or(metadata.title.take());
    if item_type == "Season" {
        metadata.title = first_tag(nfo, &["seasonname"]).or(metadata.title.take());
    }
    metadata.sort_name = first_tag(nfo, &["sortname"]).or(metadata.sort_name.take());
    metadata.forced_sort_name = first_tag(nfo, &["sorttitle"]).or(metadata.forced_sort_name.take());
    metadata.lock_data = first_tag(nfo, &["lockdata"])
        .and_then(|value| parse_nfo_bool(&value))
        .or(metadata.lock_data);
    metadata.locked_fields = first_tag(nfo, &["lockedfields"])
        .map(|value| split_locked_fields(&value))
        .filter(|fields| !fields.is_empty())
        .unwrap_or_else(|| std::mem::take(&mut metadata.locked_fields));
    metadata.overview = first_tag(nfo, &["biography", "plot", "review", "outline", "overview"]);
    metadata.official_rating = first_tag(
        nfo,
        &["mpaa", "contentrating", "content_rating", "certification"],
    );
    metadata.custom_rating = first_tag(nfo, &["customrating"]).or(metadata.custom_rating.take());
    metadata.video_3d_format = first_tag(nfo, &["format3d"])
        .and_then(|value| video_3d_format_from_nfo(&value))
        .or(metadata.video_3d_format.take());
    metadata.tagline = first_tag(nfo, &["tagline"]).or(metadata.tagline.take());
    metadata.collection_name = collection_name_from_nfo(nfo).or(metadata.collection_name.take());
    metadata.original_language =
        first_tag(nfo, &["originallanguage"]).or(metadata.original_language.take());
    metadata.preferred_metadata_language =
        first_tag(nfo, &["language"]).or(metadata.preferred_metadata_language.take());
    metadata.preferred_metadata_country_code =
        first_tag(nfo, &["countrycode"]).or(metadata.preferred_metadata_country_code.take());
    metadata.series_status = (item_type == "Series")
        .then(|| first_tag(nfo, &["status"]).and_then(|value| normalize_series_status(&value)))
        .flatten()
        .or(metadata.series_status.take());
    if item_type == "Series" {
        metadata.air_days = first_tag(nfo, &["airs_dayofweek"])
            .map(|value| normalize_air_days(&value))
            .unwrap_or_else(|| std::mem::take(&mut metadata.air_days));
        metadata.air_time = first_tag(nfo, &["airs_time"]).or(metadata.air_time.take());
    }
    metadata.home_page_url = first_tag(nfo, &["website", "homepage"])
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .or(metadata.home_page_url.take());
    metadata.remote_trailers = tags(nfo, "trailer")
        .into_iter()
        .filter_map(|value| normalize_trailer_url(&value))
        .collect();
    metadata.premiere_date = first_tag(nfo, &["aired", "formed", "premiered", "releasedate"])
        .and_then(|value| normalize_nfo_date(&value))
        .or(metadata.premiere_date.clone());
    metadata.end_date = first_tag(nfo, &["enddate"])
        .and_then(|value| normalize_nfo_date(&value))
        .or(metadata.end_date.clone());
    metadata.created_at = first_tag(nfo, &["dateadded"])
        .and_then(|value| parse_nfo_datetime_unix(&value))
        .or(metadata.created_at);
    metadata.watched = first_tag(nfo, &["watched"])
        .and_then(|value| parse_nfo_bool(&value))
        .or(metadata.watched);
    metadata.play_count = first_i64_tag(nfo, &["playcount"])
        .filter(|count| *count >= 0)
        .or(metadata.play_count);
    metadata.last_played_at = first_tag(nfo, &["lastplayed"])
        .and_then(|value| parse_nfo_datetime_unix(&value))
        .or(metadata.last_played_at);
    metadata.production_year = first_tag(nfo, &["year"])
        .and_then(|value| parse_nfo_i64(&value).or_else(|| find_year(&value)))
        .filter(|year| *year > 1850)
        .or_else(|| {
            metadata
                .premiere_date
                .as_deref()
                .and_then(year_from_yyyy_mm_dd)
        })
        .or(metadata.production_year);
    metadata.runtime_ticks = first_tag(nfo, &["runtime", "duration"])
        .and_then(|value| parse_runtime_ticks(&value))
        .or(metadata.runtime_ticks);
    metadata.aspect_ratio = first_tag(nfo, &["aspectratio"]).or(metadata.aspect_ratio.take());
    let stream_details = blocks(nfo, "fileinfo")
        .into_iter()
        .next()
        .and_then(|file_info| {
            blocks(&file_info.contents, "streamdetails")
                .into_iter()
                .next()
        });
    if let Some(video) = stream_details
        .as_ref()
        .and_then(|details| blocks(&details.contents, "video").into_iter().next())
    {
        metadata.video_3d_format = first_tag(&video.contents, &["format3d"])
            .and_then(|value| video_3d_format_from_nfo(&value))
            .or(metadata.video_3d_format.take());
        metadata.aspect_ratio =
            first_tag(&video.contents, &["aspect"]).or(metadata.aspect_ratio.take());
        metadata.width = first_i64_tag(&video.contents, &["width"])
            .filter(|value| *value > 0)
            .or(metadata.width);
        metadata.height = first_i64_tag(&video.contents, &["height"])
            .filter(|value| *value > 0)
            .or(metadata.height);
        metadata.runtime_ticks = first_i64_tag(&video.contents, &["durationinseconds"])
            .filter(|value| *value >= 0)
            .and_then(|seconds| seconds.checked_mul(10_000_000))
            .or(metadata.runtime_ticks);
    }
    if matches!(
        item_type,
        "Movie" | "Episode" | "Video" | "Trailer" | "MusicVideo"
    ) {
        metadata.has_subtitles = stream_details
            .as_ref()
            .is_some_and(|details| {
                blocks(&details.contents, "subtitle")
                    .into_iter()
                    .any(|subtitle| first_tag(&subtitle.contents, &["language"]).is_some())
            })
            .then_some(true)
            .or(metadata.has_subtitles);
    }
    metadata.display_order = first_tag(nfo, &["displayorder"]).or(metadata.display_order.take());
    metadata.critic_rating = first_f64_tag(nfo, &["criticrating"])
        .or_else(|| rating_node_score(nfo, RatingKind::Critic));
    metadata.community_rating = first_f64_tag(nfo, &["communityrating"])
        .filter(|rating| (0.0..=10.0).contains(rating))
        .or_else(|| first_f64_tag(nfo, &["rating"]))
        .or_else(|| rating_node_score(nfo, RatingKind::Community));
    metadata.season_number =
        first_i64_tag(nfo, &["season", "seasonnumber"]).or(metadata.season_number);
    metadata.episode_number =
        first_i64_tag(nfo, &["episode", "episodenumber"]).or(metadata.episode_number);
    metadata.ending_episode_number = first_i64_tag(nfo, &["episodenumberend", "episodeend"])
        .or_else(|| ending_episode_number_from_episode_blocks(full_nfo))
        .or(metadata.ending_episode_number);
    if item_type == "Episode" {
        metadata.title = joined_episode_value(&episode_blocks, &["title", "localtitle", "name"])
            .or(metadata.title.take());
        metadata.original_title = joined_episode_value(&episode_blocks, &["originaltitle"])
            .or(metadata.original_title.take());
        metadata.overview = joined_episode_value(
            &episode_blocks,
            &["biography", "plot", "review", "outline", "overview"],
        )
        .or(metadata.overview.take());
        metadata.airs_before_episode_number =
            first_i64_tag(nfo, &["airsbefore_episode", "displayepisode"])
                .or(metadata.airs_before_episode_number);
        metadata.airs_after_season_number =
            first_i64_tag(nfo, &["airsafter_season", "displayafterseason"])
                .or(metadata.airs_after_season_number);
        metadata.airs_before_season_number =
            first_i64_tag(nfo, &["airsbefore_season", "displayseason"])
                .or(metadata.airs_before_season_number);
        metadata.series_name = first_tag(nfo, &["showtitle"]).or(metadata.series_name.take());
    }

    metadata.genres = tags(nfo, "genre")
        .into_iter()
        .flat_map(|value| split_slash_separated_array(&value))
        .collect();
    metadata.tags = tags(nfo, "tag");
    metadata.tags.extend(tags(nfo, "style"));
    metadata.studios = tags(nfo, "studio");
    metadata.production_locations = tags(nfo, "country")
        .into_iter()
        .flat_map(|value| split_slash_separated_array(&value))
        .collect();
    metadata.people = people(nfo);
    if item_type == "MusicVideo" {
        metadata.collection_name = first_tag(nfo, &["album"]).or(metadata.collection_name.take());
        for artist in tags(nfo, "artist") {
            let artist = artist.trim();
            if artist.is_empty()
                || metadata.people.iter().any(|person| {
                    person.person_type.eq_ignore_ascii_case("Artist")
                        && person.name.eq_ignore_ascii_case(artist)
                })
            {
                continue;
            }
            metadata.people.push(ParsedPerson {
                name: artist.to_string(),
                role: None,
                person_type: "Artist".to_string(),
            });
        }
    }
    metadata.images = nfo_images(nfo);
    for (provider, provider_item_id) in provider_ids(nfo, item_type) {
        upsert_provider_value(&mut metadata.provider_ids, &provider, provider_item_id);
    }
}

fn joined_episode_value(blocks: &[TagBlock], names: &[&str]) -> Option<String> {
    let values = blocks
        .iter()
        .filter_map(|block| first_tag(&block.contents, names))
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join(" / "))
}

fn normalize_air_days(value: &str) -> Vec<String> {
    if value.trim().eq_ignore_ascii_case("daily") {
        return [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
    }

    [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ]
    .into_iter()
    .find(|day| day.eq_ignore_ascii_case(value.trim()))
    .map(|day| vec![day.to_string()])
    .unwrap_or_default()
}

fn collection_name_from_nfo(nfo: &str) -> Option<String> {
    let block = blocks(nfo, "set").into_iter().next()?;
    first_tag(&block.contents, &["name"]).or_else(|| {
        let value = decode_xml_text(&block.contents).trim().to_string();
        (!value.is_empty() && !value.contains('<')).then_some(value)
    })
}

fn normalize_series_status(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "continuing" | "pilot" | "returning" | "returning series" => Some("Continuing".to_string()),
        "ended" | "cancelled" | "canceled" => Some("Ended".to_string()),
        "unreleased" => Some("Unreleased".to_string()),
        _ => None,
    }
}

fn normalize_trailer_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    for prefix in [
        "plugin://plugin.video.youtube/?action=play_video&videoid=",
        "plugin://plugin.video.youtube/play/?video_id=",
    ] {
        if value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let key = value[prefix.len()..].trim();
            return (!key.is_empty()).then(|| format!("https://www.youtube.com/watch?v={key}"));
        }
    }
    (value.starts_with("http://") || value.starts_with("https://")).then(|| value.to_string())
}

fn video_3d_format_from_nfo(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "hsbs" | "sbs" | "sbs3d" => Some("HalfSideBySide".to_string()),
        "htab" | "tab" => Some("HalfTopAndBottom".to_string()),
        "ftab" => Some("FullTopAndBottom".to_string()),
        "fsbs" => Some("FullSideBySide".to_string()),
        "mvc" => Some("MVC".to_string()),
        _ => None,
    }
}

fn nfo_images(nfo: &str) -> Vec<ParsedImage> {
    let fanart_blocks = blocks(nfo, "fanart");
    let fanart_ranges = fanart_blocks
        .iter()
        .map(|block| block.open_start..block.close_end)
        .collect::<Vec<_>>();
    let mut images = Vec::new();
    let mut seen_types = HashSet::new();

    for block in blocks(nfo, "thumb") {
        if fanart_ranges
            .iter()
            .any(|range| range.contains(&block.open_start))
        {
            continue;
        }
        push_nfo_image(&mut images, &mut seen_types, &block, "thumb");
    }

    for fanart in fanart_blocks {
        for block in blocks(&fanart.contents, "thumb") {
            push_nfo_image(&mut images, &mut seen_types, &block, "fanart");
        }
    }

    images
}

fn push_nfo_image(
    images: &mut Vec<ParsedImage>,
    seen_types: &mut HashSet<String>,
    block: &TagBlock,
    parent_node: &str,
) {
    let mut aspect = attribute_value(&block.opening_tag, "aspect").unwrap_or_default();
    if aspect.trim().is_empty() && parent_node.eq_ignore_ascii_case("fanart") {
        aspect = "fanart".to_string();
    } else if aspect.trim().is_empty() {
        aspect = "poster".to_string();
    }
    if aspect.contains('.') {
        return;
    }

    let value = decode_xml_text(&block.contents).trim().to_string();
    if value.is_empty() || !is_absolute_image_location(&value) {
        return;
    }

    let image_type = nfo_image_type(&aspect).to_string();
    if !seen_types.insert(image_type.clone()) {
        return;
    }
    images.push(ParsedImage {
        image_type,
        path: value,
    });
}

fn nfo_image_type(aspect: &str) -> &'static str {
    match aspect.trim().to_ascii_lowercase().as_str() {
        "banner" => "Banner",
        "clearlogo" => "Logo",
        "discart" => "Disc",
        "landscape" => "Thumb",
        "clearart" => "Art",
        "fanart" => "Backdrop",
        _ => "Primary",
    }
}

fn is_absolute_image_location(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
        || Path::new(value).is_absolute()
}

fn provider_ids(nfo: &str, item_type: &str) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    for (provider, names) in nfo_provider_tag_names() {
        push_provider(&mut ids, provider, first_tag(nfo, names));
    }
    push_provider_id_tag_attributes(nfo, &mut ids);
    push_provider_links(nfo, item_type, &mut ids);
    if item_type == "Movie"
        && let Some(set) = blocks(nfo, "set").into_iter().next()
    {
        push_provider(
            &mut ids,
            "TmdbCollection",
            attribute_value(&set.opening_tag, "tmdbcolid"),
        );
    }

    for block in blocks(nfo, "uniqueid") {
        let provider = attribute_value(&block.opening_tag, "type").unwrap_or_default();
        let key = normalized_provider_name(&provider);
        push_provider(&mut ids, key.unwrap_or(&provider), Some(block.contents));
    }

    ids.sort();
    ids.dedup();
    ids
}

fn push_provider(ids: &mut Vec<(String, String)>, provider: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        upsert_provider_value(ids, provider, value.trim().to_string());
    }
}

fn nfo_provider_tag_names() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("Tmdb", &["tmdbid", "tmdb"]),
        ("Tvdb", &["tvdbid", "tvdb"]),
        ("IMDB", &["imdbid", "imdb", "imdb_id"]),
        ("TvMaze", &["tvmazeid", "tvmaze"]),
        ("TvRage", &["tvrageid", "tvrage"]),
        ("AniDB", &["anidbid", "anidb"]),
        ("AniList", &["anilistid", "anilist"]),
        ("AniSearch", &["anisearchid", "anisearch"]),
        ("Douban", &["doubanid", "douban"]),
        (
            "TmdbCollection",
            &["collectionnumber", "tmdbcolid", "tmdbcol"],
        ),
        ("MusicBrainzAlbum", &["musicbrainzalbumid"]),
        ("MusicBrainzAlbumArtist", &["musicbrainzalbumartistid"]),
        ("MusicBrainzReleaseGroup", &["musicbrainzreleasegroupid"]),
    ]
}

fn normalized_provider_name(provider: &str) -> Option<&'static str> {
    match provider
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_', '-'], "")
        .as_str()
    {
        "tmdb" | "tmdbid" => Some("Tmdb"),
        "tvdb" | "tvdbid" => Some("Tvdb"),
        "imdb" | "imdbid" => Some("IMDB"),
        "tvmaze" | "tvmazeid" => Some("TvMaze"),
        "tvrage" | "tvrageid" => Some("TvRage"),
        "anidb" | "anidbid" => Some("AniDB"),
        "anilist" | "anilistid" => Some("AniList"),
        "anisearch" | "anisearchid" => Some("AniSearch"),
        "douban" | "doubanid" => Some("Douban"),
        "collectionnumber" | "tmdbcol" | "tmdbcolid" | "tmdbcollection" => Some("TmdbCollection"),
        "musicbrainzalbum" | "musicbrainzalbumid" => Some("MusicBrainzAlbum"),
        "musicbrainzalbumartist" | "musicbrainzalbumartistid" => Some("MusicBrainzAlbumArtist"),
        "musicbrainzreleasegroup" | "musicbrainzreleasegroupid" => Some("MusicBrainzReleaseGroup"),
        _ => None,
    }
}

fn push_provider_id_tag_attributes(nfo: &str, ids: &mut Vec<(String, String)>) {
    for block in blocks(nfo, "id") {
        push_provider(
            ids,
            "Tmdb",
            attribute_value(&block.opening_tag, "TMDB")
                .or_else(|| attribute_value(&block.opening_tag, "tmdb")),
        );
        push_provider(
            ids,
            "Tvdb",
            attribute_value(&block.opening_tag, "TVDB")
                .or_else(|| attribute_value(&block.opening_tag, "tvdb")),
        );
        let imdb = attribute_value(&block.opening_tag, "IMDB")
            .or_else(|| attribute_value(&block.opening_tag, "imdb"))
            .or_else(|| {
                let value = decode_xml_text(&block.contents).trim().to_string();
                value.starts_with("tt").then_some(value)
            });
        push_provider(ids, "IMDB", imdb);
    }
    for block in blocks(nfo, "set") {
        push_provider(
            ids,
            "TmdbCollection",
            attribute_value(&block.opening_tag, "tmdbcolid"),
        );
    }
}

fn push_provider_links(nfo: &str, item_type: &str, ids: &mut Vec<(String, String)>) {
    if let Some(value) = first_regex_capture(imdb_id_regex(), nfo, "id") {
        push_provider(ids, "IMDB", Some(value));
    }
    match item_type {
        "Movie" => {
            if let Some(value) = first_regex_capture(tmdb_movie_url_regex(), nfo, "id") {
                push_provider(ids, "Tmdb", Some(value));
            }
        }
        "Series" => {
            if let Some(value) = first_regex_capture(tmdb_series_url_regex(), nfo, "id") {
                push_provider(ids, "Tmdb", Some(value));
            }
            if let Some(value) = first_regex_capture(tvdb_url_regex(), nfo, "id") {
                push_provider(ids, "Tvdb", Some(value));
            }
        }
        _ => {}
    }
    if let Some(value) = first_regex_capture(douban_url_regex(), nfo, "id") {
        push_provider(ids, "Douban", Some(value));
    }
}

fn first_regex_capture(regex: &Regex, value: &str, name: &str) -> Option<String> {
    regex
        .captures(value)
        .and_then(|captures| captures.name(name))
        .map(|matched| matched.as_str().to_string())
}

fn imdb_id_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)\b(?P<id>tt\d{7,9})\b").expect("valid imdb id regex"))
}

fn tmdb_movie_url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)themoviedb\.org/movie/(?P<id>\d+)").expect("valid tmdb movie url regex")
    })
}

fn tmdb_series_url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)themoviedb\.org/tv/(?P<id>\d+)").expect("valid tmdb series url regex")
    })
}

fn tvdb_url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)thetvdb\.com/(?:dereferrer/series/|\?id=)(?P<id>\d+)")
            .expect("valid tvdb url regex")
    })
}

fn douban_url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)movie\.douban\.com/subject/(?P<id>\d+)").expect("valid douban url regex")
    })
}

fn parse_nfo_i64(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok()
}

fn parse_nfo_f64(value: &str) -> Option<f64> {
    value
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_nfo_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn split_locked_fields(value: &str) -> Vec<String> {
    value
        .split(['|', ',', ';'])
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn normalize_nfo_date(value: &str) -> Option<String> {
    let value = value.trim();
    let date_part = value
        .split(['T', ' '])
        .next()
        .unwrap_or(value)
        .trim()
        .replace(['/', '.'], "-");
    normalize_yyyy_mm_dd(&date_part).or_else(|| {
        chrono::NaiveDate::parse_from_str(&date_part, "%Y-%-m-%-d")
            .ok()
            .map(|date| date.format("%Y-%m-%d").to_string())
    })
}

fn parse_nfo_datetime_unix(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(date.timestamp());
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y/%m/%d %H:%M:%S", "%Y-%m-%d"] {
        if let Ok(date_time) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(date_time.and_utc().timestamp());
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(value, format) {
            return date
                .and_hms_opt(0, 0, 0)
                .map(|date_time| date_time.and_utc().timestamp());
        }
    }
    normalize_nfo_date(value).and_then(|date| {
        chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|date_time| date_time.and_utc().timestamp())
    })
}

fn parse_runtime_ticks(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.contains(':') {
        return parse_colon_runtime_seconds(value)
            .and_then(|seconds| seconds.checked_mul(10_000_000));
    }
    value
        .split_whitespace()
        .next()
        .and_then(|minutes| minutes.parse::<f64>().ok())
        .filter(|minutes| minutes.is_finite() && *minutes > 0.0)
        .map(|minutes| (minutes * 60.0 * 10_000_000.0).round() as i64)
}

fn parse_colon_runtime_seconds(value: &str) -> Option<i64> {
    let parts = value
        .split(':')
        .map(str::trim)
        .map(|part| part.parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    match parts.as_slice() {
        [minutes, seconds] => minutes.checked_mul(60)?.checked_add(*seconds),
        [hours, minutes, seconds] => hours
            .checked_mul(60)?
            .checked_add(*minutes)?
            .checked_mul(60)?
            .checked_add(*seconds),
        _ => None,
    }
    .filter(|seconds| *seconds > 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RatingKind {
    Community,
    Critic,
}

fn rating_node_score(nfo: &str, kind: RatingKind) -> Option<f64> {
    blocks(nfo, "rating").into_iter().find_map(|block| {
        let rating_name = attribute_value(&block.opening_tag, "name").unwrap_or_default();
        let lower_name = rating_name.to_ascii_lowercase();
        let is_tomato_critic = lower_name.contains("tomato") && !lower_name.contains("audience");
        if is_tomato_critic && lower_name.contains("avg") {
            return None;
        }
        let is_critic = is_tomato_critic;
        if (kind == RatingKind::Critic) != is_critic {
            return None;
        }
        first_tag(&block.contents, &["value"]).and_then(|value| parse_nfo_f64(&value))
    })
}

fn ending_episode_number_from_episode_blocks(nfo: &str) -> Option<i64> {
    let mut episodes = blocks(nfo, "episodedetails")
        .into_iter()
        .filter_map(|block| first_tag(&block.contents, &["episode"]))
        .filter_map(|value| parse_nfo_i64(&value));
    let first = episodes.next()?;
    Some(episodes.fold(first, i64::max))
}

fn people(nfo: &str) -> Vec<ParsedPerson> {
    let mut people = Vec::new();
    people.extend(named_people_from_tags(nfo, "director", "Director"));
    people.extend(named_people_from_tags(nfo, "writer", "Writer"));
    people.extend(tags(nfo, "credits").into_iter().flat_map(|value| {
        value
            .split('/')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|name| ParsedPerson {
                name: name.to_string(),
                role: None,
                person_type: "Writer".to_string(),
            })
            .collect::<Vec<_>>()
    }));
    people.extend(actor_blocks(nfo));
    people
}

fn named_people_from_tags(nfo: &str, tag_name: &str, person_type: &str) -> Vec<ParsedPerson> {
    tags(nfo, tag_name)
        .into_iter()
        .flat_map(|value| split_nfo_string_array(&value))
        .map(|name| ParsedPerson {
            name,
            role: None,
            person_type: person_type.to_string(),
        })
        .collect()
}

fn split_nfo_string_array(value: &str) -> Vec<String> {
    const COMMA_SEPARATOR: &[char] = &[','];
    const PIPE_SEMICOLON_SEPARATOR: &[char] = &['|', ';'];

    let value = value.trim();
    let separators = if value.contains('|') || value.contains(';') {
        PIPE_SEMICOLON_SEPARATOR
    } else {
        COMMA_SEPARATOR
    };
    value
        .trim_matches(|c| separators.contains(&c))
        .split(separators)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn split_slash_separated_array(value: &str) -> Vec<String> {
    value
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn actor_blocks(nfo: &str) -> Vec<ParsedPerson> {
    blocks(nfo, "actor")
        .into_iter()
        .filter_map(|block| {
            let name = first_tag(&block.contents, &["name"])?;
            Some(ParsedPerson {
                name,
                role: first_tag(&block.contents, &["role"]),
                person_type: first_tag(&block.contents, &["type"])
                    .unwrap_or_else(|| "Actor".to_string()),
            })
        })
        .collect()
}

fn first_tag(contents: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| tags(contents, name).into_iter().next())
}

fn first_i64_tag(contents: &str, names: &[&str]) -> Option<i64> {
    names.iter().find_map(|name| {
        tags(contents, name)
            .into_iter()
            .find_map(|value| parse_nfo_i64(&value))
    })
}

fn first_f64_tag(contents: &str, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| {
        tags(contents, name)
            .into_iter()
            .find_map(|value| parse_nfo_f64(&value))
    })
}

fn tags(contents: &str, name: &str) -> Vec<String> {
    blocks(contents, name)
        .into_iter()
        .map(|block| decode_xml_text(&block.contents))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn blocks(contents: &str, name: &str) -> Vec<TagBlock> {
    let lower = contents.to_ascii_lowercase();
    let mut cursor = 0usize;
    let mut result = Vec::new();
    let open_prefix = format!("<{name}");
    let close = format!("</{name}>");

    while let Some(relative_open) = lower[cursor..].find(&open_prefix) {
        let open_start = cursor + relative_open;
        let name_end = open_start + open_prefix.len();
        if !is_tag_name_boundary(lower[name_end..].chars().next()) {
            cursor = open_start + 1;
            continue;
        }
        let Some(open_end_relative) = lower[open_start..].find('>') else {
            break;
        };
        let open_end = open_start + open_end_relative + 1;
        let Some(close_relative) = lower[open_end..].find(&close) else {
            break;
        };
        let close_start = open_end + close_relative;
        let close_end = close_start + close.len();
        result.push(TagBlock {
            opening_tag: contents[open_start..open_end].to_string(),
            contents: contents[open_end..close_start].to_string(),
            open_start,
            close_end,
        });
        cursor = close_end;
    }

    result
}

fn is_tag_name_boundary(next: Option<char>) -> bool {
    next.is_some_and(|c| c == '>' || c == '/' || c.is_whitespace())
}

fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{}=", name.to_ascii_lowercase());
    let start = lower.find(&needle)? + needle.len();
    let quote = tag[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = tag[value_start..].find(quote)? + value_start;
    Some(tag[value_start..value_end].to_string())
}

fn find_year(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    let dimensions = video_dimension_regex();
    bytes
        .windows(4)
        .enumerate()
        .filter(|(start, digits)| {
            if !digits.iter().all(u8::is_ascii_digit) {
                return false;
            }
            let start = *start;
            let before = start.checked_sub(1).and_then(|index| bytes.get(index));
            let after = bytes.get(start + 4);
            !dimensions
                .find_iter(value)
                .any(|matched| matched.start() <= start && start < matched.end())
                && !before.is_some_and(|byte| byte.is_ascii_digit())
                && !after.is_some_and(|byte| byte.is_ascii_digit())
        })
        .filter_map(|(_, digits)| std::str::from_utf8(digits).ok())
        .filter_map(|digits| digits.parse::<i64>().ok())
        .find(|year| (1880..=2100).contains(year))
}

fn video_dimension_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)\b(?P<width>[0-9]{3,5})\s*[x×]\s*[0-9]{3,5}\b")
            .expect("video dimension regex must compile")
    })
}

pub(crate) fn is_video_dimension_year(path: &Path, year: i64) -> bool {
    let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
        return false;
    };
    video_dimension_regex().captures_iter(name).any(|captures| {
        captures
            .name("width")
            .and_then(|width| width.as_str().parse::<i64>().ok())
            == Some(year)
    })
}

fn decode_xml_text(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn extracts_provider_ids_from_path_tags() {
        let ids = provider_ids_from_path(Path::new(
            "/media/Movie (2024) [tmdbid-123] {douban-456}.strm",
        ));
        assert_eq!(
            ids,
            vec![
                ("Douban".to_string(), "456".to_string()),
                ("Tmdb".to_string(), "123".to_string())
            ]
        );
    }

    #[test]
    fn provider_ids_from_path_accepts_jellyfin_attribute_aliases() {
        let ids = provider_ids_from_path(Path::new(
            "Show S01E01 [tmdb=123][tvmazeid-456][anidbid=789].mkv",
        ));

        assert!(ids.contains(&("Tmdb".to_string(), "123".to_string())));
        assert!(ids.contains(&("TvMaze".to_string(), "456".to_string())));
        assert!(ids.contains(&("AniDB".to_string(), "789".to_string())));
    }

    #[test]
    fn nfo_provider_ids_override_path_provider_ids() {
        let mut metadata = ParsedMetadata {
            provider_ids: provider_ids_from_path(Path::new("Movie {tmdb-1}.strm")),
            ..Default::default()
        };
        merge_nfo_metadata(
            &mut metadata,
            r#"<movie><uniqueid type="tmdb">2</uniqueid></movie>"#,
        );
        assert_eq!(
            metadata.provider_ids,
            vec![("Tmdb".to_string(), "2".to_string())]
        );
    }

    #[test]
    fn nfo_episode_numbers_override_filename_numbers() {
        let mut metadata = ParsedMetadata {
            season_number: Some(1),
            episode_number: Some(75),
            ..Default::default()
        };

        merge_nfo_metadata(
            &mut metadata,
            r#"<episodedetails><season>2</season><episode>3</episode><episodenumberend>4</episodenumberend></episodedetails>"#,
        );

        assert_eq!(metadata.season_number, Some(2));
        assert_eq!(metadata.episode_number, Some(3));
        assert_eq!(metadata.ending_episode_number, Some(4));
    }

    #[test]
    fn video_dimensions_are_not_parsed_as_production_year() {
        let metadata = parse_filename_metadata(Path::new(
            "[VCB-S][Ghost in the Shell S.A.C. 2nd GIG][01][1920X1040].(mkv).strm",
        ));

        assert_eq!(metadata.production_year, None);
    }

    #[test]
    fn real_year_is_kept_when_video_dimensions_are_present() {
        let metadata = parse_filename_metadata(Path::new(
            "Ghost in the Shell STAND ALONE COMPLEX (2002) [1920 X 1040].mkv",
        ));

        assert_eq!(metadata.production_year, Some(2002));
    }

    #[test]
    fn dimension_year_helper_matches_only_the_dimension_width() {
        let path = Path::new("episode [1920X1040].mkv");

        assert!(is_video_dimension_year(path, 1920));
        assert!(!is_video_dimension_year(path, 1040));
    }

    #[test]
    fn multi_episode_nfo_merges_only_official_episode_fields() {
        let mut metadata = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"
            <episodedetails>
                <episode>1</episode><title>Part One</title><originaltitle>One</originaltitle>
                <plot>First plot</plot><uniqueid type="tmdb">101</uniqueid>
                <actor><name>First Actor</name></actor>
            </episodedetails>
            <episodedetails>
                <episode>2</episode><title>Part Two</title><originaltitle>Two</originaltitle>
                <plot>Second plot</plot><uniqueid type="tmdb">102</uniqueid>
                <actor><name>Second Actor</name></actor>
            </episodedetails>
            "#,
            "Episode",
        );

        assert_eq!(metadata.title.as_deref(), Some("Part One / Part Two"));
        assert_eq!(metadata.original_title.as_deref(), Some("One / Two"));
        assert_eq!(
            metadata.overview.as_deref(),
            Some("First plot / Second plot")
        );
        assert_eq!(metadata.episode_number, Some(1));
        assert_eq!(metadata.ending_episode_number, Some(2));
        assert!(
            metadata
                .provider_ids
                .contains(&("Tmdb".into(), "101".into()))
        );
        assert!(
            !metadata
                .provider_ids
                .contains(&("Tmdb".into(), "102".into()))
        );
        assert_eq!(metadata.people.len(), 1);
        assert_eq!(metadata.people[0].name, "First Actor");
    }

    #[test]
    fn season_name_tag_overrides_generic_title() {
        let mut metadata = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"<season><title>Season 1</title><seasonname>Book One</seasonname><seasonnumber>1</seasonnumber></season>"#,
            "Season",
        );

        assert_eq!(metadata.title.as_deref(), Some("Book One"));
        assert_eq!(metadata.season_number, Some(1));
    }

    #[test]
    fn nfo_format3d_follows_jellyfin_values_and_overrides_name() {
        let mut metadata = ParsedMetadata {
            video_3d_format: Some("HalfSideBySide".to_string()),
            ..Default::default()
        };

        merge_nfo_metadata(
            &mut metadata,
            r#"<movie><fileinfo><streamdetails><video><format3d>FTAB</format3d></video></streamdetails></fileinfo></movie>"#,
        );

        assert_eq!(
            metadata.video_3d_format.as_deref(),
            Some("FullTopAndBottom")
        );
    }

    #[test]
    fn nfo_stream_details_supply_video_fallbacks_like_jellyfin() {
        let mut metadata = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"
            <movie>
                <runtime>90</runtime>
                <aspectratio>4:3</aspectratio>
                <fileinfo><streamdetails>
                    <video>
                        <aspect>16:9</aspect><width>1920</width><height>1080</height>
                        <durationinseconds>5401</durationinseconds>
                    </video>
                    <subtitle><language>eng</language></subtitle>
                </streamdetails></fileinfo>
            </movie>
            "#,
            "Movie",
        );

        assert_eq!(metadata.aspect_ratio.as_deref(), Some("16:9"));
        assert_eq!(metadata.width, Some(1920));
        assert_eq!(metadata.height, Some(1080));
        assert_eq!(metadata.runtime_ticks, Some(54_010_000_000));
        assert_eq!(metadata.has_subtitles, Some(true));
    }

    #[test]
    fn nfo_tv_ordering_and_item_language_follow_jellyfin() {
        let mut series = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut series,
            r#"<tvshow><language>ja-JP</language><countrycode>JP</countrycode><airs_dayofweek>Daily</airs_dayofweek><airs_time>23:30</airs_time></tvshow>"#,
            "Series",
        );
        assert_eq!(series.preferred_metadata_language.as_deref(), Some("ja-JP"));
        assert_eq!(
            series.preferred_metadata_country_code.as_deref(),
            Some("JP")
        );
        assert_eq!(series.air_days.len(), 7);
        assert_eq!(series.air_time.as_deref(), Some("23:30"));

        let mut episode = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut episode,
            r#"<episodedetails><airsbefore_episode>3</airsbefore_episode><airsafter_season>1</airsafter_season><airsbefore_season>2</airsbefore_season><showtitle>Example Show</showtitle></episodedetails>"#,
            "Episode",
        );
        assert_eq!(episode.airs_before_episode_number, Some(3));
        assert_eq!(episode.airs_after_season_number, Some(1));
        assert_eq!(episode.airs_before_season_number, Some(2));
        assert_eq!(episode.series_name.as_deref(), Some("Example Show"));
    }

    #[test]
    fn nfo_provider_ids_accept_jellyfin_aliases() {
        let mut metadata = ParsedMetadata {
            provider_ids: provider_ids_from_path(Path::new("Show [tvmazeid-1].mkv")),
            ..Default::default()
        };

        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"
            <tvshow>
                <tvmazeid>2</tvmazeid>
                <anidbid>3</anidbid>
                <uniqueid type="AniList">4</uniqueid>
                <tmdbcol>5</tmdbcol>
                https://www.themoviedb.org/tv/308871
                https://movie.douban.com/subject/35200000/
            </tvshow>
            "#,
            "Series",
        );

        assert!(
            metadata
                .provider_ids
                .contains(&("TvMaze".to_string(), "2".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("AniDB".to_string(), "3".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("AniList".to_string(), "4".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("TmdbCollection".to_string(), "5".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "308871".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("Douban".to_string(), "35200000".to_string()))
        );
    }

    #[test]
    fn nfo_id_tag_attributes_are_normalized() {
        let mut metadata = ParsedMetadata::default();

        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"<movie><id TMDB="123" TVDB="456">tt7654321</id><set tmdbcolid="789">Collection</set></movie>"#,
            "Movie",
        );

        assert!(
            metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "123".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("Tvdb".to_string(), "456".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("IMDB".to_string(), "tt7654321".to_string()))
        );
        assert!(
            metadata
                .provider_ids
                .contains(&("TmdbCollection".to_string(), "789".to_string()))
        );
        assert_eq!(metadata.collection_name.as_deref(), Some("Collection"));
    }

    #[test]
    fn nfo_series_status_language_homepage_and_trailers_follow_jellyfin() {
        let mut metadata = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"
            <tvshow>
                <status>Returning Series</status>
                <originallanguage>zh</originallanguage>
                <website>https://example.test/show</website>
                <trailer>plugin://plugin.video.youtube/play/?video_id=abc123</trailer>
                <trailer>plugin://plugin.video.youtube/?action=play_video&amp;videoid=legacy456</trailer>
            </tvshow>
            "#,
            "Series",
        );

        assert_eq!(metadata.series_status.as_deref(), Some("Continuing"));
        assert_eq!(metadata.original_language.as_deref(), Some("zh"));
        assert_eq!(
            metadata.home_page_url.as_deref(),
            Some("https://example.test/show")
        );
        assert_eq!(
            metadata.remote_trailers,
            [
                "https://www.youtube.com/watch?v=abc123",
                "https://www.youtube.com/watch?v=legacy456"
            ]
        );
    }

    #[test]
    fn nfo_people_include_directors_writers_credits_and_actors() {
        let mut metadata = ParsedMetadata::default();

        merge_nfo_metadata(
            &mut metadata,
            r#"
            <movie>
                <director>Director One, Director Two</director>
                <writer>Writer One|Writer Two;Writer Three</writer>
                <credits>Credit One / Credit Two</credits>
                <actor><name>Actor One</name><role>Lead</role></actor>
            </movie>
            "#,
        );

        assert!(
            metadata
                .people
                .iter()
                .any(|person| person.name == "Director One" && person.person_type == "Director")
        );
        assert!(
            metadata
                .people
                .iter()
                .any(|person| person.name == "Director Two" && person.person_type == "Director")
        );
        assert!(
            metadata
                .people
                .iter()
                .any(|person| person.name == "Writer Three" && person.person_type == "Writer")
        );
        assert!(
            metadata
                .people
                .iter()
                .any(|person| person.name == "Credit Two" && person.person_type == "Writer")
        );
        assert!(metadata.people.iter().any(|person| {
            person.name == "Actor One"
                && person.role.as_deref() == Some("Lead")
                && person.person_type == "Actor"
        }));
    }

    #[test]
    fn music_video_nfo_imports_album_artists_and_subtitle_presence() {
        let mut metadata = ParsedMetadata::default();
        merge_nfo_metadata_for_item(
            &mut metadata,
            r#"
            <musicvideo>
                <album>Live at Home</album>
                <artist>Artist One</artist>
                <artist>Artist Two</artist>
                <fileinfo><streamdetails>
                    <subtitle><language>zho</language></subtitle>
                </streamdetails></fileinfo>
            </musicvideo>
            "#,
            "MusicVideo",
        );

        assert_eq!(metadata.collection_name.as_deref(), Some("Live at Home"));
        assert_eq!(metadata.has_subtitles, Some(true));
        assert_eq!(
            metadata
                .people
                .iter()
                .map(|person| (person.name.as_str(), person.person_type.as_str()))
                .collect::<Vec<_>>(),
            [("Artist One", "Artist"), ("Artist Two", "Artist")]
        );
    }

    #[test]
    fn nfo_scalars_follow_jellyfin_rating_and_date_semantics() {
        let mut metadata = ParsedMetadata::default();

        merge_nfo_metadata(
            &mut metadata,
            r#"
            <movie>
                <title>NFO Movie</title>
                <originaltitle>Original NFO Movie</originaltitle>
                <review>Imported review</review>
                <sortname>NFO Movie Sort</sortname>
                <sorttitle>Forced Sort Movie</sorttitle>
                <lockdata>true</lockdata>
                <lockedfields>Name|Overview|Genres</lockedfields>
                <mpaa>PG-13</mpaa>
                <customrating>TV-MA</customrating>
                <tagline>Trust no one.</tagline>
                <country>United States / Japan</country>
                <genre>Drama / Mystery</genre>
                <style>Noir</style>
                <rating>8,4</rating>
                <criticrating>91</criticrating>
                <ratings>
                    <rating name="tomatometer"><value>92</value></rating>
                    <rating name="tomatometerallcritics_avg"><value>7.2</value></rating>
                    <rating name="audience"><value>8.6</value></rating>
                </ratings>
                <premiered>2024/7/2</premiered>
                <enddate>2024/8/3</enddate>
                <dateadded>2024-07-03 04:05:06</dateadded>
                <displayorder>dvd</displayorder>
                <runtime>90 min</runtime>
            </movie>
            "#,
        );

        assert_eq!(metadata.official_rating.as_deref(), Some("PG-13"));
        assert_eq!(
            metadata.original_title.as_deref(),
            Some("Original NFO Movie")
        );
        assert_eq!(metadata.overview.as_deref(), Some("Imported review"));
        assert_eq!(metadata.sort_name.as_deref(), Some("NFO Movie Sort"));
        assert_eq!(
            metadata.forced_sort_name.as_deref(),
            Some("Forced Sort Movie")
        );
        assert_eq!(metadata.lock_data, Some(true));
        assert_eq!(metadata.locked_fields, ["Name", "Overview", "Genres"]);
        assert_eq!(metadata.custom_rating.as_deref(), Some("TV-MA"));
        assert_eq!(metadata.tagline.as_deref(), Some("Trust no one."));
        assert_eq!(metadata.genres, ["Drama", "Mystery"]);
        assert_eq!(metadata.tags, ["Noir"]);
        assert_eq!(metadata.production_locations, ["United States", "Japan"]);
        assert_eq!(metadata.community_rating, Some(8.4));
        assert_eq!(metadata.critic_rating, Some(91.0));
        assert_eq!(metadata.premiere_date.as_deref(), Some("2024-07-02"));
        assert_eq!(metadata.end_date.as_deref(), Some("2024-08-03"));
        assert_eq!(
            metadata.created_at,
            chrono::NaiveDate::from_ymd_opt(2024, 7, 3)
                .unwrap()
                .and_hms_opt(4, 5, 6)
                .map(|date_time| date_time.and_utc().timestamp())
        );
        assert_eq!(metadata.display_order.as_deref(), Some("dvd"));
        assert_eq!(metadata.production_year, Some(2024));
        assert_eq!(metadata.runtime_ticks, Some(54_000_000_000));
    }

    #[test]
    fn nfo_images_follow_jellyfin_thumb_and_fanart_semantics() {
        let mut metadata = ParsedMetadata::default();

        merge_nfo_metadata(
            &mut metadata,
            r#"
            <movie>
                <thumb>https://img.example.test/poster.jpg</thumb>
                <thumb aspect="clearlogo">https://img.example.test/logo.png</thumb>
                <thumb aspect="season.poster">https://img.example.test/season.jpg</thumb>
                <fanart>
                    <thumb>https://img.example.test/backdrop.jpg</thumb>
                </fanart>
                <thumb aspect="poster">https://img.example.test/second-poster.jpg</thumb>
            </movie>
            "#,
        );

        assert_eq!(
            metadata.images,
            vec![
                ParsedImage {
                    image_type: "Primary".to_string(),
                    path: "https://img.example.test/poster.jpg".to_string(),
                },
                ParsedImage {
                    image_type: "Logo".to_string(),
                    path: "https://img.example.test/logo.png".to_string(),
                },
                ParsedImage {
                    image_type: "Backdrop".to_string(),
                    path: "https://img.example.test/backdrop.jpg".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn typed_sidecar_metadata_reads_jellyfin_folder_nfos() {
        let root = test_dir("typed_sidecar_metadata_reads_jellyfin_folder_nfos");
        let series = root.join("Series Name");
        let season = series.join("Season 02");
        let movie = root.join("Movie Name");
        fs::create_dir_all(&season).unwrap();
        fs::create_dir_all(&movie).unwrap();
        fs::write(
            series.join("tvshow.nfo"),
            "<tvshow><title>NFO Series</title><tmdbid>100</tmdbid></tvshow>",
        )
        .unwrap();
        fs::write(
            season.join("season.nfo"),
            "<season><seasonnumber>2</seasonnumber><title>NFO Season</title></season>",
        )
        .unwrap();
        fs::write(
            movie.join("movie.nfo"),
            "<movie><title>NFO Movie</title><tmdbid>200</tmdbid></movie>",
        )
        .unwrap();

        let series_metadata = parse_sidecar_metadata_for_item(&series, "Series").await;
        let season_metadata = parse_sidecar_metadata_for_item(&season, "Season").await;
        let movie_metadata = parse_sidecar_metadata_for_item(&movie, "Movie").await;

        assert!(series_metadata.has_nfo);
        assert_eq!(series_metadata.title.as_deref(), Some("NFO Series"));
        assert!(
            series_metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "100".to_string()))
        );
        assert!(season_metadata.has_nfo);
        assert_eq!(season_metadata.season_number, Some(2));
        assert_eq!(season_metadata.title.as_deref(), Some("NFO Season"));
        assert!(movie_metadata.has_nfo);
        assert_eq!(movie_metadata.title.as_deref(), Some("NFO Movie"));
        assert!(
            movie_metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "200".to_string()))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn movie_file_does_not_consume_parent_movie_nfo() {
        let root = test_dir("movie_file_does_not_consume_parent_movie_nfo");
        let movie = root.join("Movie One.mkv");
        fs::write(&movie, []).unwrap();
        fs::write(
            root.join("movie.nfo"),
            "<movie><title>Wrong Movie</title><tmdbid>999</tmdbid></movie>",
        )
        .unwrap();

        let metadata = parse_sidecar_metadata_for_item(&movie, "Movie").await;

        assert!(!metadata.has_nfo);
        assert!(
            !metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "999".to_string()))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn grouping_folder_does_not_consume_child_nfo() {
        let root = test_dir("grouping_folder_does_not_consume_child_nfo");
        fs::write(
            root.join("tvshow.nfo"),
            "<tvshow><title>Wrong Group</title><tmdbid>999</tmdbid></tvshow>",
        )
        .unwrap();

        let metadata = parse_sidecar_metadata_for_item(&root, "Folder").await;

        assert!(!metadata.has_nfo);
        assert_ne!(metadata.title.as_deref(), Some("Wrong Group"));
        assert!(
            !metadata
                .provider_ids
                .contains(&("Tmdb".to_string(), "999".to_string()))
        );

        let _ = fs::remove_dir_all(root);
    }

    fn test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jellyfin-rs-metadata-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}

struct TagBlock {
    opening_tag: String,
    contents: String,
    open_start: usize,
    close_end: usize,
}
