use std::{path::Path, sync::OnceLock};

use regex::Regex;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedName {
    pub title: String,
    pub premiere_date: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub ending_episode_number: Option<i64>,
    pub stack_key: Option<String>,
    pub stack_part: Option<i64>,
    pub version: Option<String>,
    pub video_3d_format: Option<String>,
    pub extended_video_types: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedBookName {
    pub title: String,
    pub series_name: Option<String>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub production_year: Option<i64>,
}

/// Match Emby.Naming.Book.BookFileNameParser, including comic volume/chapter suffixes.
pub fn parse_book_name(value: &str) -> ParsedBookName {
    let patterns = [
        r#"^(?P<series>.+?)(?:\s\((?:[0-9]{4})\))?\s#(?P<index>[0-9]+)(?:\.0)?(?:\s\(of\s[0-9]+\))?(?:\s\((?P<year>[0-9]{4})\))?$"#,
        r#"^(?P<title>.+?)\s\((?P<series>.+?),\s#(?P<index>[0-9]+)\)(?:\.0)?(?:\s\((?P<year>[0-9]{4})\))?$"#,
        r#"^(?P<index>[0-9]+)(?:\.0)?\s-\s(?P<title>.+?)(?:\s\((?P<year>[0-9]{4})\))?$"#,
        r#"^(?P<title>.*)\((?P<year>[0-9]{4})\)$"#,
        r#"^(?P<title>.*)$"#,
    ];

    for pattern in patterns {
        let regex = Regex::new(pattern).expect("book name regex must compile");
        let Some(captures) = regex.captures(value) else {
            continue;
        };
        let title = captures
            .name("title")
            .map(|capture| capture.as_str().trim().to_string())
            .unwrap_or_default();
        let comic =
            Regex::new(r#"^(?P<title>.+?)(?:\sv(?P<volume>[0-9]+))?(?:\sc(?P<chapter>[0-9]+))?$"#)
                .expect("comic book regex must compile");
        let comic_captures = (!title.is_empty())
            .then(|| comic.captures(&title))
            .flatten();
        let comic_number = |name: &str| {
            comic_captures
                .as_ref()
                .and_then(|captures| captures.name(name))
                .and_then(|capture| capture.as_str().parse::<i64>().ok())
        };
        let comic_chapter = comic_number("chapter");
        let comic_volume = comic_number("volume");
        return ParsedBookName {
            title,
            series_name: captures
                .name("series")
                .map(|capture| capture.as_str().trim().to_string()),
            index_number: captures
                .name("index")
                .and_then(|capture| capture.as_str().parse::<i64>().ok())
                .or(comic_chapter),
            parent_index_number: comic_volume,
            production_year: captures
                .name("year")
                .and_then(|capture| capture.as_str().parse::<i64>().ok()),
        };
    }

    ParsedBookName::default()
}

pub fn parse_media_name(path: &Path, collection_type: &str) -> ParsedName {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if collection_type == "tvshows" || collection_type == "tv" {
        let parser_path = episode_parser_path(path);
        let parser_stem = Path::new(&parser_path)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        parse_episode_name(&parser_path, parser_stem, folder)
    } else {
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        parse_video_name(stem)
    }
}

fn parse_episode_name(path: &str, stem: &str, folder: &str) -> ParsedName {
    let normalized = normalize_separators(stem);
    let mut parsed = ParsedName {
        video_3d_format: parse_video_3d_format(path),
        ..ParsedName::default()
    };

    if let Some((premiere_date, title)) = parse_episode_date(stem) {
        parsed.premiere_date = Some(premiere_date);
        parsed.title = title;
        parsed.version = parse_version(stem);
        parsed.extended_video_types = parse_extended_video_types(stem);
        return parsed;
    }

    for (pattern, style) in episode_patterns() {
        if let Some(captures) = pattern.captures(path) {
            parsed.season_number = capture_i64(&captures, &["season", "seasonnumber"], &[]);
            if parsed
                .season_number
                .is_some_and(is_invalid_episode_season_number)
            {
                continue;
            }
            parsed.episode_number = capture_i64(&captures, &["episode", "epnumber"], &[]);
            if parsed.episode_number.is_none() {
                continue;
            }
            parsed.ending_episode_number =
                capture_i64(&captures, &["ending", "endingepnumber"], &[])
                    .filter(|_| ending_episode_capture_is_valid(path, &captures));

            let title_part = captures.name("title").map(|value| value.as_str());
            parsed.title = match style {
                EpisodeStyle::Named => episode_title_after_marker(stem)
                    .unwrap_or_else(|| clean_title(title_part.unwrap_or(stem))),
                EpisodeStyle::TrailingTitle => trailing_episode_title(path, stem, &captures),
                EpisodeStyle::FolderTitle => clean_title(folder),
                EpisodeStyle::SeriesName => clean_title(
                    captures
                        .name("seriesname")
                        .map(|value| value.as_str())
                        .unwrap_or(stem),
                ),
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

    if let Some((episode, ending)) = parse_chinese_episode_number(stem) {
        parsed.episode_number = Some(episode);
        parsed.ending_episode_number = ending;
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

fn parse_episode_date(value: &str) -> Option<(String, String)> {
    static YEAR_FIRST: OnceLock<Regex> = OnceLock::new();
    static DAY_FIRST: OnceLock<Regex> = OnceLock::new();
    let year_first = YEAR_FIRST.get_or_init(|| {
        Regex::new(r"(?P<year>[0-9]{4})[._ -](?P<month>[0-9]{2})[._ -](?P<day>[0-9]{2})")
            .expect("year-first episode date regex must compile")
    });
    let day_first = DAY_FIRST.get_or_init(|| {
        Regex::new(r"(?P<day>[0-9]{2})[._ -](?P<month>[0-9]{2})[._ -](?P<year>[0-9]{4})")
            .expect("day-first episode date regex must compile")
    });
    let captures = year_first
        .captures(value)
        .or_else(|| day_first.captures(value))?;
    let year = captures.name("year")?.as_str().parse::<i32>().ok()?;
    let month = captures.name("month")?.as_str().parse::<u32>().ok()?;
    let day = captures.name("day")?.as_str().parse::<u32>().ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let matched = captures.get(0)?;
    let mut without_date = value.to_string();
    without_date.replace_range(matched.range(), " ");
    Some((
        date.format("%Y-%m-%d").to_string(),
        clean_title(&without_date),
    ))
}

fn episode_parser_path(path: &Path) -> String {
    let mut path = path.to_string_lossy().replace('\\', "/");
    // SmartStrm preserves the source container in a wrapper before `.strm`,
    // for example `EP64.(mp4).strm` or `01.(mkv).strm`.  Normalize that
    // representation to a regular media filename so Jellyfin naming rules can
    // still extract the episode number.
    if path.to_ascii_lowercase().ends_with(".strm") {
        let without_strm = &path[..path.len() - ".strm".len()];
        static WRAPPED_CONTAINER: OnceLock<Regex> = OnceLock::new();
        let wrapped_container = WRAPPED_CONTAINER.get_or_init(|| {
            Regex::new(r#"(?i)\.\((?P<extension>[a-z0-9]{2,5})\)$"#)
                .expect("wrapped STRM container regex must compile")
        });
        if wrapped_container.is_match(without_strm) {
            path = wrapped_container
                .replace(without_strm, ".$extension")
                .into_owned();
        }
    }
    if path.contains('/') {
        path
    } else {
        format!("/{path}")
    }
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
        video_3d_format: parse_video_3d_format(stem),
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
            // Stack identity must retain edition/version tokens. Jellyfin groups
            // "Movie 4K CD1/CD2" separately from "Movie 1080p CD1/CD2" even
            // though both editions have the same cleaned display title.
            parsed.stack_key = Some(stack_key(title));
            parsed.stack_part = captures
                .name("part")
                .and_then(|value| part_number(value.as_str()));
            break;
        }
    }

    parsed
}

/// Match Jellyfin's Format3DParser rules and enum mapping.
pub fn parse_video_3d_format(value: &str) -> Option<String> {
    const DELIMITERS: &[char] = &['(', ')', '-', '.', '_', '[', ']', ' '];
    let tokens = value
        .split(DELIMITERS)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    for (preceding, token, format) in [
        (Some("3d"), "hsbs", "HalfSideBySide"),
        (Some("3d"), "sbs", "HalfSideBySide"),
        (Some("3d"), "htab", "HalfTopAndBottom"),
        (Some("3d"), "tab", "HalfTopAndBottom"),
        (None, "fsbs", "FullSideBySide"),
        (None, "hsbs", "HalfSideBySide"),
        (None, "sbs", "HalfSideBySide"),
        (None, "ftab", "FullTopAndBottom"),
        (None, "htab", "HalfTopAndBottom"),
        (None, "tab", "HalfTopAndBottom"),
        (None, "sbs3d", "HalfSideBySide"),
        (None, "mvc", "MVC"),
    ] {
        let matched = match preceding {
            Some(preceding) => tokens.windows(2).any(|pair| {
                pair[0].eq_ignore_ascii_case(preceding) && pair[1].eq_ignore_ascii_case(token)
            }),
            None => tokens
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(token)),
        };
        if matched {
            return Some(format.to_string());
        }
    }
    None
}

#[derive(Clone, Copy)]
enum EpisodeStyle {
    Named,
    TrailingTitle,
    FolderTitle,
    SeriesName,
}

fn episode_patterns() -> &'static [(Regex, EpisodeStyle)] {
    static PATTERNS: OnceLock<Vec<(Regex, EpisodeStyle)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
        (
            // Season marker and episode marker separated by a translated/localized title,
            // e.g. `Home Temptation S02 回家的欲望 EP01`.
            r#"(?i).*(?:/)(?P<title>.*?)[._ -]s(?P<season>[0-9]{1,4})\b[^/]*?[._ -]ep_?(?P<episode>[0-9]{1,4})(?:[._ -]+(?P<name>[^/]+))?$"#,
            EpisodeStyle::Named,
        ),
        (
            // Jellyfin/Kodi standard: foo.s01.e01, foo.s01_e01, S01E02 foo, S01 - E02.
            r#"(?i).*(?:/)(?P<title>.*?)[s](?P<season>[0-9]{1,4})[\]\[ ._-]*[e](?P<episode>[0-9]{1,4})(?:[\]\[ ._-]*(?:[ex]|-[ex]?)(?P<ending>[0-9]{1,4}))?[^/]*$"#,
            EpisodeStyle::Named,
        ),
        (
            // Kodi standard: foo.ep01, foo.EP_01.
            r#"(?i).*(?:/)(?P<title>.*?)[._ -]ep_?(?P<episode>[0-9]{1,4})(?:[._ -]+(?P<name>[^/]+))?$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // Kodi standard: foo.E01., foo.e01.
            r#"(?i).*(?:/)(?P<title>.*?)[._ -]e(?P<episode>[0-9]{1,4})(?:[._ -]+(?P<name>[^/]+))?$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // Multiple 1x02 - 1x03 / 1x02 - 1e03 naming.
            r#"(?i).*(?:/)(?P<title>.*?)(?P<season>[0-9]{1,4})x(?P<episode>[0-9]{1,3})(?:\s*-\s*[0-9]{1,4}[xe](?P<ending>[0-9]{1,3}))+[^/]*$"#,
            EpisodeStyle::Named,
        ),
        (
            // 1x02 / 01x02 / 2009x03.
            r#"(?i).*(?:/)(?P<title>.*?)(?P<season>[0-9]{1,4})x(?P<episode>[0-9]{1,4})(?:[\]\[ ._-]*(?:[ex]|-[ex]?)(?P<ending>[0-9]{1,4}))?[^/]*$"#,
            EpisodeStyle::Named,
        ),
        (
            // Series Season X Episode Y - Title / Series S3 E9 - Title.
            r#"(?i).*(?:/)(?P<title>.*?)(?:season|s)[ ._-]*(?P<season>[0-9]{1,4})[ ._-]+(?:episode|e)[ ._-]*(?P<episode>[0-9]{1,4})(?:[ ._-]*(?P<ending>[0-9]{1,4}))?[^/]*$"#,
            EpisodeStyle::Named,
        ),
        (
            // Episode 16 / Episode 16 - Title.
            r#"(?i).*(?:/)episode[ ._-]*(?P<episode>[0-9]{1,4})(?:[ ._-]*(?P<ending>[0-9]{1,4}))?(?:[ ._-]+(?P<name>[^/]+))?$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // Season 1/01 episode title.avi.
            r#"(?i).*(?:/)season[._ ](?P<season>[0-9]{1,4})/(?P<episode>[0-9]{1,3})(?:[ ._-]+(?P<name>[^/]+))?$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // Name - 101.mkv and anime-style absolute episode numbers.
            r#"(?i).*(?:/)(?P<seriesname>[^/]+?)[\s_]+-[\s_]+(?P<episode>[0-9]{1,4})(?:-(?P<ending>[0-9]{2,4}))?(?:[\s_]*(?:\[[^\]]+\]|\([^\)]+\)))*[\s_]*(?:\.[^.]+)?$"#,
            EpisodeStyle::SeriesName,
        ),
        (
            // blah - 01.avi / blah - 01 - title.avi.
            r#"(?i).*(?:/)(?P<seriesname>[^/]+?)\s+-\s+(?P<episode>[0-9]{1,3})(?:-(?P<ending>[0-9]{2,3}))?(?P<name>[^/]*)$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // 01 - blah.avi / 01.blah.avi.
            r#"(?i).*(?:/)(?P<episode>[0-9]{1,3})(?:-(?P<ending>[0-9]{2,3}))?\s?[-.]\s?(?P<name>[^/]+)$"#,
            EpisodeStyle::TrailingTitle,
        ),
        (
            // [Group] Anime Name [04][1080p].
            r#"(?i).*(?:/)(?:\[[^\]]+\]\s*)?(?P<title>\[[^\]]+\]|[^\[\]/]+?)\s*\[(?P<episode>[0-9]{1,4})\][^/]*$"#,
            EpisodeStyle::Named,
        ),
        (
            r#"(?i).*(?:/)\[(?P<episode>[0-9]{1,4})\][^/]*$"#,
            EpisodeStyle::FolderTitle,
        ),
        (
            r#"(?i).*(?:/)(?P<episode>[0-9]{1,4})\.[^.]+$"#,
            EpisodeStyle::TrailingTitle,
        ),
        ]
        .into_iter()
        .map(|(pattern, style)| (Regex::new(pattern).expect("episode regex must compile"), style))
        .collect()
    })
}

fn capture_i64(captures: &regex::Captures<'_>, names: &[&str], indexes: &[usize]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| {
            captures
                .name(name)
                .and_then(|value| parse_number_token(value.as_str()))
        })
        .or_else(|| {
            indexes.iter().find_map(|index| {
                captures
                    .get(*index)
                    .and_then(|value| parse_number_token(value.as_str()))
            })
        })
}

fn parse_number_token(value: &str) -> Option<i64> {
    let digits = value
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn trailing_episode_title(path: &str, stem: &str, captures: &regex::Captures<'_>) -> String {
    let Some(name) = captures.name("name").map(|value| value.as_str()) else {
        return clean_title(stem);
    };
    let name = strip_captured_extension(path, name)
        .trim_matches(|c: char| c.is_whitespace() || ".-_[](){}".contains(c));
    if name.is_empty() {
        return clean_title(stem);
    }
    let extension_only_tail = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.eq_ignore_ascii_case(name))
        .unwrap_or(false);
    if extension_only_tail {
        clean_title(stem)
    } else {
        clean_title(name)
    }
}

fn strip_captured_extension<'a>(path: &str, value: &'a str) -> &'a str {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return value;
    };
    let suffix_len = extension.len() + 1;
    if value.len() > suffix_len
        && value
            .get(value.len() - extension.len()..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(extension))
        && value.as_bytes().get(value.len() - suffix_len) == Some(&b'.')
    {
        &value[..value.len() - suffix_len]
    } else {
        value
    }
}

fn is_invalid_episode_season_number(season: i64) -> bool {
    (200..1928).contains(&season) || season > 2500
}

fn ending_episode_capture_is_valid(path: &str, captures: &regex::Captures<'_>) -> bool {
    let Some(ending) = captures
        .name("ending")
        .or_else(|| captures.name("endingepnumber"))
    else {
        return true;
    };
    path[ending.end()..]
        .chars()
        .next()
        .is_none_or(|next| !next.is_ascii_digit() && !matches!(next, 'i' | 'I' | 'p' | 'P'))
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
        r#"(?i)[\{\[\(]\s*(?:tmdb(?:id)?|douban(?:id)?|imdb(?:id)?|tvdb(?:id)?|tvmaze(?:id)?|tvrage(?:id)?|anidb(?:id)?|anilist(?:id)?|anisearch(?:id)?)\s*[-=]\s*[^\}\]\)]+[\}\]\)]"#,
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
        "dvd",
        "hddvd",
        "bdrip",
        "web dl",
        "web-dl",
        "webrip",
        "hdtv",
        "remux",
        "3d",
        "fsbs",
        "hsbs",
        "sbs",
        "ftab",
        "htab",
        "tab",
        "sbs3d",
        "mvc",
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
    fn parses_jellyfin_book_names() {
        for (name, title, series, index, parent_index, year) in [
            (
                "Sherlock Holmes #2 (1890)",
                "",
                Some("Sherlock Holmes"),
                Some(2),
                None,
                Some(1890),
            ),
            (
                "A Study in Scarlet (Sherlock Holmes, #1) (1887)",
                "A Study in Scarlet",
                Some("Sherlock Holmes"),
                Some(1),
                None,
                Some(1887),
            ),
            (
                "2 - The Sign of the Four (1890)",
                "The Sign of the Four",
                None,
                Some(2),
                None,
                Some(1890),
            ),
            (
                "Captain Marvel Adventures v01 c120",
                "Captain Marvel Adventures v01 c120",
                None,
                Some(120),
                Some(1),
                None,
            ),
        ] {
            let parsed = parse_book_name(name);
            assert_eq!(parsed.title, title, "{name}");
            assert_eq!(parsed.series_name.as_deref(), series, "{name}");
            assert_eq!(parsed.index_number, index, "{name}");
            assert_eq!(parsed.parent_index_number, parent_index, "{name}");
            assert_eq!(parsed.production_year, year, "{name}");
        }
    }

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
    fn parses_jellyfin_kodi_episode_aliases() {
        let parsed = parse_media_name(Path::new("Show.EP_01.Title.mkv"), "tvshows");
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.title, "Title");

        let parsed = parse_media_name(Path::new("Show.E01.Title.1080p.mkv"), "tvshows");
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.title, "Title");

        let parsed = parse_media_name(Path::new("Show - 01 - Pilot.mkv"), "tvshows");
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.title, "Pilot");

        let parsed = parse_media_name(Path::new("/media/Show/Season 1/01 Pilot.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(1));
        assert_eq!(parsed.title, "Pilot");

        let parsed = parse_media_name(
            Path::new("Home Temptation S02 回家的欲望 EP01.1080P.WEB-DL.(mp4).strm"),
            "tvshows",
        );
        assert_eq!(parsed.season_number, Some(2));
        assert_eq!(parsed.episode_number, Some(1));
    }

    #[test]
    fn parses_smartstrm_wrapped_episode_aliases() {
        let parsed = parse_media_name(Path::new("美人心计/E01.(mkv).strm"), "tvshows");
        assert_eq!(parsed.season_number, None);
        assert_eq!(parsed.episode_number, Some(1));

        let parsed = parse_media_name(Path::new("知否知否应是绿肥红瘦/EP64.(mp4).strm"), "tvshows");
        assert_eq!(parsed.season_number, None);
        assert_eq!(parsed.episode_number, Some(64));
        assert_ne!(parsed.title, "mp4");

        let parsed = parse_media_name(Path::new("至尊红颜/01.(mp4).strm"), "tvshows");
        assert_eq!(parsed.season_number, None);
        assert_eq!(parsed.episode_number, Some(1));

        let parsed = parse_media_name(
            Path::new("假面骑士OOO（2010）/假面骑士 OOO.S01E27.(mkv).strm"),
            "tvshows",
        );
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(27));
        assert_ne!(parsed.title, "mkv");

        let parsed = parse_media_name(
            Path::new("知否知否应是绿肥红瘦/EP62(1).(mp4).strm"),
            "tvshows",
        );
        assert_eq!(parsed.season_number, None);
        assert_eq!(parsed.episode_number, Some(62));
        assert_ne!(parsed.title, "mp4");
    }

    #[test]
    fn parses_jellyfin_date_based_episodes() {
        let parsed = parse_media_name(Path::new("Daily.Show.2026.07.21.Guest.mkv"), "tvshows");
        assert_eq!(parsed.premiere_date.as_deref(), Some("2026-07-21"));
        assert_eq!(parsed.season_number, None);
        assert_eq!(parsed.episode_number, None);
        assert_eq!(parsed.title, "Daily Show Guest");

        let parsed = parse_media_name(Path::new("Daily Show 21-07-2026.mkv"), "tvshows");
        assert_eq!(parsed.premiere_date.as_deref(), Some("2026-07-21"));
        assert_eq!(parsed.episode_number, None);
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

        let parsed = parse_media_name(Path::new("Show.1x02 - 1x03.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(1));
        assert_eq!(parsed.episode_number, Some(2));
        assert_eq!(parsed.ending_episode_number, Some(3));

        let parsed = parse_media_name(Path::new("Show.S09E14-1080p.mkv"), "tvshows");
        assert_eq!(parsed.season_number, Some(9));
        assert_eq!(parsed.episode_number, Some(14));
        assert_eq!(parsed.ending_episode_number, None);
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
        assert_eq!(
            parsed.stack_key.as_deref(),
            Some("movie name extended 4k hdr")
        );
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

    #[test]
    fn parses_jellyfin_3d_format_rules() {
        for (name, expected) in [
            ("Super movie.3d.hsbs.mp4", Some("HalfSideBySide")),
            ("Super movie.3d.sbs.mp4", Some("HalfSideBySide")),
            ("Super movie.3d.htab.mp4", Some("HalfTopAndBottom")),
            ("Super movie.3d.tab.mp4", Some("HalfTopAndBottom")),
            ("Super movie.fsbs.mp4", Some("FullSideBySide")),
            ("Super movie.ftab.mp4", Some("FullTopAndBottom")),
            ("Super movie.sbs3d.mp4", Some("HalfSideBySide")),
            ("Super movie.3d.mvc.mp4", Some("MVC")),
            ("Super movie.3d.mp4", None),
            ("Super movie [3d].mp4", None),
        ] {
            assert_eq!(parse_video_3d_format(name).as_deref(), expected, "{name}");
        }

        let parsed = parse_media_name(Path::new("Oblivion.3d.hsbs.mkv"), "movies");
        assert_eq!(parsed.title, "Oblivion");
        assert_eq!(parsed.video_3d_format.as_deref(), Some("HalfSideBySide"));
    }
}
