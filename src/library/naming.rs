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

pub fn parse_media_name(path: &Path, collection_type: &str) -> ParsedName {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if collection_type == "tvshows" || collection_type == "tv" {
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
                EpisodeStyle::Named => episode_title_after_marker(stem)
                    .unwrap_or_else(|| clean_title(title_part.unwrap_or(stem))),
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

    if let Some((episode, ending)) = parse_chinese_episode_number(stem) {
        parsed.episode_number = Some(episode);
        parsed.ending_episode_number = ending;
        parsed.title = clean_title(stem);
        parsed.version = parse_version(stem);
        parsed.extended_video_types = parse_extended_video_types(stem);
        return parsed;
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

fn parse_chinese_episode_number(value: &str) -> Option<(i64, Option<i64>)> {
    let number = r#"(?:[0-9]{1,4}|[零〇一二两三四五六七八九十百千]+)"#;
    let regex = Regex::new(&format!(
        r#"第\s*(?P<number>{number})\s*(?:(?:[集话話回]\s*)?(?:到|至|[-~－—])\s*第?\s*(?P<ending>{number})\s*)?[集话話回]"#
    ))
    .expect("Chinese episode regex must compile");
    let captures = regex.captures(value)?;
    let episode = crate::util::parse_chinese_number(captures.name("number")?.as_str())?;
    let ending = captures
        .name("ending")
        .and_then(|value| crate::util::parse_chinese_number(value.as_str()));
    Some((episode, ending))
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
        (
            r#"(?i)^(?P<episode>[0-9]{1,4})$"#,
            EpisodeStyle::TrailingTitle,
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
    let mut title = replace_title_separators(value);
    title = Regex::new(
        r#"(?i)[\{\[\(]\s*(?:tmdb(?:id)?|douban(?:id)?|imdb(?:id)?|tvdb(?:id)?)\s*[-=]\s*[^\}\]\)]+[\}\]\)]"#,
    )
    .expect("provider tag regex must compile")
    .replace_all(&title, " ")
    .into_owned();
    title = Regex::new(r#"[\{\[\(]\s*((?:18|19|20)[0-9]{2}|2100)\s*[\}\]\)]"#)
        .expect("year tag regex must compile")
        .replace_all(&title, " ")
        .into_owned();
    for token in [
        "2160p",
        "4k",
        "1080p",
        "720p",
        "480p",
        "hdr10+",
        "hdr10",
        "hdr",
        "dovi",
        "x264",
        "x265",
        "h264",
        "h265",
        "h 264",
        "h 265",
        "hevc",
        "avc",
        "aac",
        "dts",
        "dts hd",
        "dts hd ma",
        "ac3",
        "eac3",
        "ddp",
        "ddp5.1",
        "ddp7.1",
        "ddp2.0",
        "truehd",
        "truehd5.1",
        "truehd7.1",
        "truehd2.0",
        "atmos",
        "flac",
        "mp3",
        "pcm",
        "bluray",
        "blu ray",
        "bdrip",
        "web dl",
        "web-dl",
        "webrip",
        "hdtv",
        "remux",
        "amzn",
        "nf",
        "hulu",
        "hiveweb",
        "pure@hiveweb",
        "ctrlhd",
        "mteam",
        "hq",
        "edr",
        "telesync",
        "10bit",
        "8bit",
        "proper",
        "repack",
        "extended",
        "director s cut",
        "directors cut",
        "multi",
        "subs",
    ] {
        let pattern = Regex::new(&format!(
            r#"(?i)(^|[ ._\-\[\(\{{]){}($|[ ._\-\]\)\}}])"#,
            regex::escape(token)
        ))
        .expect("clean regex must compile");
        title = pattern.replace_all(&title, " ").into_owned();
    }
    title = remove_nonleading_year_tokens(&title);
    title = Regex::new(
        r#"(?i)(^|[ ._\-\[\(\{])(?:[257](?:[ .]1|[ .]0)|10\s*bit|8\s*bit)($|[ ._\-\]\)\}])"#,
    )
    .expect("audio channel regex must compile")
    .replace_all(&title, " ")
    .into_owned();
    title = Regex::new(r#"(?i)(^|[ ._\-\[\(\{])[0-9]{3,4}x[0-9]{3,4}($|[ ._\-\]\)\}])"#)
        .expect("dimension regex must compile")
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

fn episode_title_after_marker(stem: &str) -> Option<String> {
    for pattern in [
        r#"(?i)s[0-9]{1,4}[ ._\-\]]*e[0-9]{1,3}(?:[ ._\-]*(?:e|x)?[0-9]{1,3})?"#,
        r#"(?i)[0-9]{1,4}x[0-9]{1,3}(?:[ ._\-]*(?:x|e)?[0-9]{1,3})?"#,
    ] {
        let regex = Regex::new(pattern).ok()?;
        if let Some(marker) = regex.find(stem) {
            let tail = stem[marker.end()..]
                .trim_matches(|c: char| c.is_whitespace() || ".-_[](){}".contains(c));
            let title = clean_title(tail);
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

fn replace_title_separators(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '_' | '　' => result.push(' '),
            '.' => {
                let previous = value[..index].chars().next_back();
                let next = chars.peek().map(|(_, next)| *next);
                if previous.is_some_and(|c| c.is_ascii_digit())
                    && next.is_some_and(|c| c.is_ascii_digit())
                {
                    result.push('.');
                } else {
                    result.push(' ');
                }
            }
            _ => result.push(ch),
        }
    }
    result
}

fn remove_nonleading_year_tokens(value: &str) -> String {
    let regex = Regex::new(r#"(?i)(^|[ ._\-\[\(\{])((?:18|19|20)[0-9]{2}|2100)($|[ ._\-\]\)\}])"#)
        .expect("bare year regex must compile");
    regex
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let Some(matched) = captures.get(0) else {
                return " ".to_string();
            };
            let has_title_before = value[..matched.start()]
                .chars()
                .any(|c| c.is_alphanumeric() || is_cjk(c));
            if has_title_before {
                " ".to_string()
            } else {
                matched.as_str().to_string()
            }
        })
        .into_owned()
}

fn is_cjk(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
        || ('\u{3400}'..='\u{4dbf}').contains(&ch)
        || ('\u{3040}'..='\u{30ff}').contains(&ch)
        || ('\u{ac00}'..='\u{d7af}').contains(&ch)
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
        assert_eq!(parsed.title, "Title");

        let parsed = parse_media_name(Path::new("Show - 1x03 - Name.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(3));
        assert_eq!(parsed.title, "Name");
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

        let parsed = parse_media_name(Path::new("01.strm"), "tvshows");
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.title, "01");

        let parsed = parse_media_name(Path::new("第10集.strm"), "tvshows");
        assert_eq!(parsed.episode_number, Some(10));
        assert_eq!(parsed.title, "第10集");

        let parsed = parse_media_name(Path::new("第十二集.strm"), "tvshows");
        assert_eq!(parsed.episode_number, Some(12));
        assert_eq!(parsed.title, "第十二集");

        let parsed = parse_media_name(Path::new("第1集到第3集.strm"), "tvshows");
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.ending_episode_number, Some(3));

        let parsed = parse_media_name(Path::new("第十二集至第十五集.strm"), "tvshows");
        assert_eq!(parsed.episode_number, Some(12));
        assert_eq!(parsed.ending_episode_number, Some(15));
    }

    #[test]
    fn parses_stack_parts_and_versions() {
        let parsed = parse_media_name(Path::new("Movie.Name.Extended.4K.HDR.CD2.mkv"), "movies");
        assert_eq!(parsed.title, "Movie Name");
        assert_eq!(parsed.stack_key.as_deref(), Some("movie name"));
        assert_eq!(parsed.stack_part, Some(2));
        assert_eq!(parsed.version.as_deref(), Some("4K HDR Extended"));
    }

    #[test]
    fn cleans_provider_and_technical_tokens_from_movie_names() {
        let parsed = parse_media_name(
            Path::new("1.89的凶手 (2024) 2160p.h265.AAC{tmdb-1249569}.strm"),
            "movies",
        );
        assert_eq!(parsed.title, "1.89的凶手");

        let parsed = parse_media_name(
            Path::new(
                "F1：狂飙飞车.F1.The.Movie.2025.2160p.BluRay.DoVi.x265.10bit.Atmos.TrueHD7.1{tmdb-911430}.strm",
            ),
            "movies",
        );
        assert_eq!(parsed.title, "F1：狂飙飞车 F1 The Movie");
    }
}
