use std::path::Path;

use regex::Regex;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedName {
    pub title: String,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub ending_episode_number: Option<i64>,
    pub stack_key: Option<String>,
    pub stack_part: Option<i64>,
    pub version: Option<String>,
    pub extended_video_types: Vec<String>,
}

pub fn parse_media_name(path: &Path, library_id: &str) -> ParsedName {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if library_id == "tvshows" {
        parse_episode_name(stem, folder)
    } else {
        parse_video_name(stem)
    }
}

fn parse_episode_name(stem: &str, folder: &str) -> ParsedName {
    let normalized = normalize_separators(stem);
    let mut parsed = ParsedName::default();

    for (pattern, style) in episode_patterns() {
        if let Some(captures) = pattern.captures(&normalized) {
            parsed.season_number = captures
                .name("season")
                .and_then(|value| value.as_str().parse().ok());
            parsed.episode_number = captures
                .name("episode")
                .and_then(|value| value.as_str().parse().ok());
            parsed.ending_episode_number = captures
                .name("ending")
                .and_then(|value| value.as_str().parse().ok());

            let title_part = captures.name("title").map(|value| value.as_str());
            parsed.title = match style {
                EpisodeStyle::Named => clean_title(title_part.unwrap_or(stem)),
                EpisodeStyle::TrailingTitle => clean_title(
                    captures
                        .name("name")
                        .map(|value| value.as_str())
                        .unwrap_or(stem),
                ),
                EpisodeStyle::FolderTitle => clean_title(folder),
            };
            if parsed.title.is_empty() {
                parsed.title = clean_title(stem);
            }
            parsed.version = parse_version(stem);
            parsed.extended_video_types = parse_extended_video_types(stem);
            return parsed;
        }
    }

    if let Some((season, episode)) = parse_compact_episode(&normalized) {
        parsed.season_number = Some(season);
        parsed.episode_number = Some(episode);
        parsed.title = clean_title(stem);
        parsed.version = parse_version(stem);
        parsed.extended_video_types = parse_extended_video_types(stem);
        return parsed;
    }

    parsed.title = clean_title(stem);
    parsed.version = parse_version(stem);
    parsed.extended_video_types = parse_extended_video_types(stem);
    parsed
}

fn parse_video_name(stem: &str) -> ParsedName {
    let normalized = normalize_separators(stem);
    let mut parsed = ParsedName {
        title: clean_title(stem),
        version: parse_version(stem),
        extended_video_types: parse_extended_video_types(stem),
        ..ParsedName::default()
    };

    let stack_patterns = [
        r#"(?i)^(?P<title>.+?)[ ._\-\[\(]+(?P<kind>cd|dvd|part|pt|disc|disk)[ ._\-]*(?P<part>[0-9]+|[a-d])[\]\)]?$"#,
        r#"(?i)^(?P<title>.+?)[ ._\-\[\(]+(?P<part>[0-9]+)[ ._\-]*(?P<kind>of)[ ._\-]*[0-9]+[\]\)]?$"#,
    ];
    for pattern in stack_patterns {
        let regex = Regex::new(pattern).expect("stack regex must compile");
        if let Some(captures) = regex.captures(&normalized) {
            let title = captures
                .name("title")
                .map(|value| value.as_str())
                .unwrap_or(stem);
            parsed.title = clean_title(title);
            parsed.stack_key = Some(stack_key(&parsed.title));
            parsed.stack_part = captures
                .name("part")
                .and_then(|value| part_number(value.as_str()));
            break;
        }
    }

    parsed
}

#[derive(Clone, Copy)]
enum EpisodeStyle {
    Named,
    TrailingTitle,
    FolderTitle,
}

fn episode_patterns() -> Vec<(Regex, EpisodeStyle)> {
    [
        (
            r#"(?i)^(?P<title>.*?)[ ._\-\[]*s(?P<season>[0-9]{1,4})[ ._\-\]]*e(?P<episode>[0-9]{1,3})(?:[ ._\-]*(?:e|x)?(?P<ending>[0-9]{1,3}))?(?:\b|[^0-9]).*$"#,
            EpisodeStyle::Named,
        ),
        (
            r#"(?i)^(?P<title>.*?)[ ._\-\[]*(?P<season>[0-9]{1,4})x(?P<episode>[0-9]{1,3})(?:[ ._\-]*(?:x|e)?(?P<ending>[0-9]{1,3}))?(?:\b|[^0-9]).*$"#,
            EpisodeStyle::Named,
        ),
        (
            r#"(?i)^(?P<title>.*?)(?:season|s)[ ._\-]*(?P<season>[0-9]{1,4})[ ._\-]+(?:episode|e)[ ._\-]*(?P<episode>[0-9]{1,3})(?:[ ._\-]*(?P<ending>[0-9]{1,3}))?.*$"#,
            EpisodeStyle::Named,
        ),
        (
            r#"(?i)^episode[ ._\-]*(?P<episode>[0-9]{1,4})(?:[ ._\-]*(?P<ending>[0-9]{1,4}))?(?:[ ._\-]+(?P<name>.+))?$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            r#"(?i)^(?P<episode>[0-9]{1,3})(?:[ ._\-]*(?P<ending>[0-9]{2,3}))?[ ._\-]+(?P<name>.+)$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            r#"(?i)^\[(?:[^\]]+)\][ ._\-]*(?P<title>.+?)[ ._\-]*\[(?P<episode>[0-9]{1,4})\].*$"#,
            EpisodeStyle::Named,
        ),
        (
            r#"(?i)^\[(?P<episode>[0-9]{1,4})\].*$"#,
            EpisodeStyle::FolderTitle,
        ),
    ]
    .into_iter()
    .map(|(pattern, style)| (Regex::new(pattern).expect("episode regex must compile"), style))
    .collect()
}

fn parse_compact_episode(value: &str) -> Option<(i64, i64)> {
    let regex = Regex::new(r#"(?i)(?:^|[^0-9])(?P<number>[0-9]{3,4})(?:[^0-9]|$)"#).ok()?;
    let captures = regex.captures(value)?;
    let digits = captures.name("number")?.as_str();
    let split = digits.len().saturating_sub(2);
    let season = digits[..split].parse().ok()?;
    let episode = digits[split..].parse().ok()?;
    if (200..1928).contains(&season) || season > 2500 {
        return None;
    }
    Some((season, episode))
}

fn parse_version(stem: &str) -> Option<String> {
    let normalized = normalize_separators(stem).to_ascii_lowercase();
    let mut tags = Vec::new();
    for (needle, label) in [
        ("2160p", "2160p"),
        ("4k", "4K"),
        ("1080p", "1080p"),
        ("720p", "720p"),
        ("480p", "480p"),
        ("hdr10+", "HDR10+"),
        ("hdr10", "HDR10"),
        ("hdr", "HDR"),
        ("dv", "DV"),
        ("dolby vision", "Dolby Vision"),
        ("remux", "Remux"),
        ("bluray", "BluRay"),
        ("blu ray", "BluRay"),
        ("web dl", "WEB-DL"),
        ("webrip", "WEBRip"),
        ("hdtv", "HDTV"),
        ("director", "Director's Cut"),
        ("extended", "Extended"),
        ("proper", "Proper"),
        ("repack", "Repack"),
    ] {
        if normalized.contains(needle) && !tags.iter().any(|tag| tag == &label) {
            tags.push(label);
        }
    }
    (!tags.is_empty()).then(|| tags.join(" "))
}

fn parse_extended_video_types(stem: &str) -> Vec<String> {
    let normalized = normalize_separators(stem).to_ascii_lowercase();
    let mut values = Vec::new();
    for (needle, label) in [
        ("3d", "3D"),
        ("hsbs", "HalfSideBySide"),
        ("sbs", "SideBySide"),
        ("htab", "HalfTopAndBottom"),
        ("tab", "TopAndBottom"),
        ("hdr10+", "HDR10Plus"),
        ("hdr10", "HDR10"),
        ("dolby vision", "DolbyVision"),
        (" dv ", "DolbyVision"),
        ("hdr", "HDR"),
        ("2160p", "UHD"),
        ("4k", "UHD"),
        ("remux", "Remux"),
        ("bluray", "BluRay"),
        ("blu ray", "BluRay"),
        ("dvd", "Dvd"),
        ("web dl", "WebDl"),
        ("webrip", "WebRip"),
    ] {
        if normalized.contains(needle) && !values.iter().any(|value| value == label) {
            values.push(label.to_string());
        }
    }
    values
}

fn clean_title(value: &str) -> String {
    let mut title = value.replace(['.', '_'], " ");
    for token in [
        "2160p",
        "4k",
        "1080p",
        "720p",
        "480p",
        "hdr10+",
        "hdr10",
        "hdr",
        "x264",
        "x265",
        "h264",
        "h265",
        "hevc",
        "aac",
        "dts",
        "ac3",
        "bluray",
        "blu ray",
        "bdrip",
        "web dl",
        "web-dl",
        "webrip",
        "hdtv",
        "remux",
        "proper",
        "repack",
        "extended",
        "director s cut",
        "directors cut",
        "multi",
        "subs",
    ] {
        let pattern = Regex::new(&format!(
            r#"(?i)(^|[ ._\-\[\(]){}($|[ ._\-\]\)])"#,
            regex::escape(token)
        ))
        .expect("clean regex must compile");
        title = pattern.replace_all(&title, " ").into_owned();
    }
    title = Regex::new(r#"\[[^\]]+\]|\([^\)]*\)"#)
        .expect("bracket regex must compile")
        .replace_all(&title, " ")
        .into_owned();
    title = Regex::new(r#"\s+"#)
        .expect("space regex must compile")
        .replace_all(&title, " ")
        .into_owned();
    title
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '.' | '_' | ','))
        .trim()
        .to_string()
}

fn normalize_separators(value: &str) -> String {
    value.replace(['_', '.'], " ").replace("-", " - ")
}

fn stack_key(title: &str) -> String {
    title
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn part_number(value: &str) -> Option<i64> {
    value.parse().ok().or_else(|| {
        value
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| i64::from(c.to_ascii_lowercase() as u8 - b'a' + 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_episode_numbers() {
        let parsed = parse_media_name(Path::new("Show.Name.S01E02.Title.1080p.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(2));
        assert_eq!(parsed.title, "Show Name");

        let parsed = parse_media_name(Path::new("Show - 1x03 - Name.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(3));
    }

    #[test]
    fn parses_multi_episode_ranges() {
        let parsed = parse_media_name(Path::new("Show.S02E03-E04.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(2));
        assert_eq!(parsed.episode_number, Some(3));
        assert_eq!(parsed.ending_episode_number, Some(4));

        let parsed = parse_media_name(Path::new("Show.2x05x06.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(2));
        assert_eq!(parsed.episode_number, Some(5));
        assert_eq!(parsed.ending_episode_number, Some(6));
    }

    #[test]
    fn parses_absolute_and_anime_episodes() {
        let parsed = parse_media_name(Path::new("Show.102.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(2));

        let parsed = parse_media_name(Path::new("[Group] Anime Name [04][1080p].mkv"), "tvshows");
        assert_eq!(parsed.episode_number, Some(4));
        assert_eq!(parsed.title, "Anime Name");
    }

    #[test]
    fn parses_stack_parts_and_versions() {
        let parsed = parse_media_name(Path::new("Movie.Name.Extended.4K.HDR.CD2.mkv"), "movies");
        assert_eq!(parsed.title, "Movie Name");
        assert_eq!(parsed.stack_key.as_deref(), Some("movie name"));
        assert_eq!(parsed.stack_part, Some(2));
        assert_eq!(parsed.version.as_deref(), Some("4K HDR Extended"));
    }
}
