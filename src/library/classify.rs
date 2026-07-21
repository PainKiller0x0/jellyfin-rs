use std::path::{Path, PathBuf};

use crate::{library::naming::parse_media_name, util::stable_item_id};

pub fn classify_media_path(path: &Path, collection_type: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    // STRM files are classified from their resolved content by the scanner.
    if extension == "strm" {
        return Some(
            match collection_type {
                "tvshows" | "tv" => "Episode",
                "music" => "Audio",
                "trailers" => "Trailer",
                "movies" => "Video",
                _ => "Movie",
            }
            .to_string(),
        );
    }
    if matches!(
        extension.as_str(),
        "mkv"
            | "mp4"
            | "m4v"
            | "mov"
            | "avi"
            | "wmv"
            | "webm"
            | "ts"
            | "m2ts"
            | "flv"
            | "m3u8"
            | "m3u"
    ) {
        return Some(
            match collection_type {
                "tvshows" | "tv" => "Episode",
                "music" => "Audio",
                "trailers" => "Trailer",
                // In movie libraries, files are "Video" — the folder itself is the "Movie"
                "movies" => "Video",
                _ => "Movie",
            }
            .to_string(),
        );
    }
    if matches!(
        extension.as_str(),
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "ape" | "alac"
    ) {
        return Some("Audio".to_string());
    }
    None
}

pub fn parent_id_for_path(path: &Path, root: &Path, library_id: &str) -> String {
    parent_path_for_path(path, root)
        .as_deref()
        .map(stable_item_id)
        .unwrap_or_else(|| library_id.to_string())
}

pub fn parent_id_for_scanned_file(
    path: &Path,
    root: &Path,
    library_id: &str,
    collection_type: &str,
    item_type: &str,
) -> String {
    if matches!(collection_type, "tvshows" | "tv") && item_type == "Episode" {
        let mut current = parent_path_for_path(path, root);
        while let Some(parent) = current {
            let parent_name = parent
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let parent_type = tv_folder_type(&parent, root, collection_type);
            if matches!(parent_type, "Season" | "Series") {
                return stable_item_id(&parent);
            }
            if !is_quality_or_range_folder_name(parent_name) {
                break;
            }
            current = parent_path_for_path(&parent, root);
        }
    }

    parent_id_for_path(path, root, library_id)
}

fn parent_path_for_path(path: &Path, root: &Path) -> Option<PathBuf> {
    let norm_path = super::path_utils::normalize_path(&path.to_string_lossy());
    let norm_root = super::path_utils::normalize_path(&root.to_string_lossy());
    let path = std::path::Path::new(&norm_path);
    let root = std::path::Path::new(&norm_root);
    path.parent()
        .filter(|parent| *parent != root)
        .map(PathBuf::from)
}

pub fn tv_folder_type(path: &Path, root: &Path, collection_type: &str) -> &'static str {
    if collection_type == "movies" {
        return movie_folder_type(path, root);
    }
    if collection_type != "tvshows" && collection_type != "tv" {
        return "Folder";
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if path_depth(path, root) > 1 && is_season_folder_name(name) {
        "Season"
    } else if is_quality_or_range_folder_name(name)
        || (is_grouping_folder_name(name) && !directory_has_direct_tv_episode_file(path))
    {
        "Folder"
    } else if directory_has_tv_content(path) || looks_like_series_folder_name(name) {
        "Series"
    } else {
        "Folder"
    }
}

fn movie_folder_type(path: &Path, _root: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if is_grouping_folder_name(name) {
        "Folder"
    } else if directory_is_movie_folder(path) || looks_like_movie_folder_name(name) {
        "Movie"
    } else {
        "Folder"
    }
}

fn path_depth(path: &Path, root: &Path) -> usize {
    let norm_path = super::path_utils::normalize_path(&path.to_string_lossy());
    let norm_root = super::path_utils::normalize_path(&root.to_string_lossy());
    std::path::Path::new(&norm_path)
        .strip_prefix(std::path::Path::new(&norm_root))
        .ok()
        .map(|relative| relative.components().count())
        .unwrap_or_default()
}

fn directory_is_movie_folder(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    let mut videos = Vec::new();
    let mut has_non_extra_subdir = false;
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_dir() {
            if !child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_extra_folder_name)
            {
                has_non_extra_subdir = true;
            }
            continue;
        }
        if child.is_file()
            && classify_media_path(&child, "movies").as_deref() == Some("Video")
            && !is_ignored_video_file(&child)
        {
            videos.push(child);
        }
    }
    !has_non_extra_subdir && video_files_are_one_movie(&videos)
}

fn video_files_are_one_movie(videos: &[PathBuf]) -> bool {
    match videos {
        [] => false,
        [_] => true,
        _ => {
            let parsed: Vec<_> = videos
                .iter()
                .map(|path| parse_media_name(path, "movies"))
                .collect();
            let first_stack = parsed.first().and_then(|value| value.stack_key.as_deref());
            if first_stack.is_some()
                && parsed
                    .iter()
                    .all(|value| value.stack_key.as_deref() == first_stack)
            {
                return true;
            }
            let Some(first_title) = parsed.first().map(|value| value.title.trim()) else {
                return false;
            };
            !first_title.is_empty()
                && parsed
                    .iter()
                    .all(|value| value.title.trim().eq_ignore_ascii_case(first_title))
        }
    }
}

fn directory_has_tv_content(path: &Path) -> bool {
    directory_has_season_folder(path) || directory_has_direct_tv_episode_file(path)
}

fn directory_has_season_folder(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let child = entry.path();
        child.is_dir()
            && child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_season_folder_name)
    })
}

fn directory_has_direct_tv_episode_file(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let child = entry.path();
        child.is_file()
            && (child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("tvshow.nfo"))
                || (classify_media_path(&child, "tvshows").as_deref() == Some("Episode")
                    && parse_media_name(&child, "tvshows").episode_number.is_some()))
    })
}

fn looks_like_series_folder_name(name: &str) -> bool {
    has_provider_tag(name) || has_year_tag(name)
}

fn looks_like_movie_folder_name(name: &str) -> bool {
    has_provider_tag(name) || has_year_tag(name)
}

fn has_provider_tag(name: &str) -> bool {
    bracket_tags(name).any(|tag| {
        let tag = tag.trim().to_ascii_lowercase();
        tag.starts_with("tmdb-")
            || tag.starts_with("tmdbid-")
            || tag.starts_with("tmdbid=")
            || tag.starts_with("douban-")
            || tag.starts_with("doubanid-")
            || tag.starts_with("doubanid=")
    })
}

fn has_year_tag(name: &str) -> bool {
    bracket_tags(name).any(|tag| {
        tag.trim()
            .parse::<i64>()
            .is_ok_and(|year| (1880..=2100).contains(&year))
    })
}

fn bracket_tags(name: &str) -> impl Iterator<Item = &str> {
    [('{', '}'), ('[', ']'), ('(', ')')]
        .into_iter()
        .filter_map(move |(open, close)| {
            let (_, after_open) = name.rsplit_once(open)?;
            let (tag, _) = after_open.split_once(close)?;
            Some(tag)
        })
}

fn is_grouping_folder_name(name: &str) -> bool {
    let folded = name
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .to_ascii_lowercase();
    folded.chars().count() == 1
        || matches!(
            folded.as_str(),
            "国产"
                | "大陆"
                | "内地"
                | "港台"
                | "港剧"
                | "台剧"
                | "欧美"
                | "美剧"
                | "英剧"
                | "韩剧"
                | "日剧"
                | "动漫"
                | "动画"
                | "综艺"
        )
}

fn is_season_folder_name(name: &str) -> bool {
    let name = name
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .to_ascii_lowercase();
    let first_token = name
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-' | '|' | '｜'))
        .next()
        .unwrap_or_default();
    if matches!(name.as_str(), "specials" | "extras") {
        return true;
    }
    name.starts_with("season ")
        || name.starts_with("season_")
        || name.starts_with("season-")
        || first_token.starts_with('s')
            && first_token[1..].chars().all(|c| c.is_ascii_digit())
            && first_token.len() > 1
        || has_chinese_season_marker(&name)
}

fn has_chinese_season_marker(name: &str) -> bool {
    name.split_once('第')
        .and_then(|(_, rest)| rest.split_once('季').map(|(number, _)| number.trim()))
        .is_some_and(|number| crate::util::parse_chinese_number(number).is_some())
}

fn is_quality_or_range_folder_name(name: &str) -> bool {
    let trimmed = name
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .to_ascii_lowercase();
    if trimmed.is_empty() {
        return false;
    }
    let compact = trimmed
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '.' | '_' | '-' | '|' | '｜'))
        .collect::<String>();
    if matches!(
        compact.as_str(),
        "480p" | "720p" | "1080p" | "2160p" | "4k" | "uhd"
    ) {
        return true;
    }
    if compact.contains("高码")
        || compact.contains("高码率")
        || compact.contains("国语中字")
        || compact.contains("中字")
    {
        return compact.contains("4k")
            || compact.contains("1080")
            || compact.contains("2160")
            || compact.contains("sdr")
            || compact.contains("hdr")
            || compact.contains("dv")
            || compact.contains("版");
    }
    let range = regex::Regex::new(
        r#"(?i)^[0-9]{1,4}\s*-\s*[0-9]{1,4}\s*集?(?:[ ._\-]*(?:4k|2160p|1080p|720p))?$"#,
    )
    .expect("range folder regex must compile");
    if range.is_match(&trimmed) {
        return true;
    }
    let chinese_number = r#"(?:[0-9]{1,4}|[零〇一二两三四五六七八九十百千]+)"#;
    let chinese_range = regex::Regex::new(&format!(
        r#"(?i)^第\s*{chinese_number}\s*(?:集\s*)?(?:到|至|[-~－—])\s*第?\s*{chinese_number}\s*集?(?:[ ._\-]*(?:4k|2160p|1080p|720p))?$"#
    ))
    .expect("Chinese range folder regex must compile");
    chinese_range.is_match(&trimmed)
}

fn is_extra_folder_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "extras"
            | "extra"
            | "trailers"
            | "trailer"
            | "samples"
            | "sample"
            | "behind the scenes"
            | "behindthescenes"
            | "deleted scenes"
            | "deleted scene"
            | "featurettes"
            | "featurette"
            | "interviews"
            | "interview"
            | "scenes"
            | "scene"
            | "shorts"
            | "short"
    )
}

fn is_ignored_video_file(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().contains("sample"))
}

#[cfg(test)]
mod tests {
    use super::{classify_media_path, tv_folder_type};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn trailers_library_video_files_are_trailers() {
        assert_eq!(
            classify_media_path(Path::new("sample.mp4"), "trailers").as_deref(),
            Some("Trailer")
        );
    }

    #[test]
    fn tv_grouping_folders_are_not_series() {
        let root = Path::new("/media/电视剧/国产");
        assert_eq!(tv_folder_type(&root.join("数"), root, "tvshows"), "Folder");
        assert_eq!(
            tv_folder_type(
                &root.join("4 in love (2012) {tmdbid-42026}"),
                root,
                "tvshows"
            ),
            "Series"
        );
    }

    #[test]
    fn movie_grouping_folders_are_not_movies() {
        let root = Path::new("/media/电影");
        assert_eq!(tv_folder_type(&root.join("国产"), root, "movies"), "Folder");
        assert_eq!(
            tv_folder_type(&root.join("国产/数"), root, "movies"),
            "Folder"
        );
        assert_eq!(
            tv_folder_type(
                &root.join("国产/数/Movie Name (2024) {tmdbid-123}"),
                root,
                "movies"
            ),
            "Movie"
        );
    }

    #[test]
    fn nested_movie_folder_with_video_file_is_movie() {
        let root = test_dir("nested_movie_folder_with_video_file_is_movie");
        let group = root.join("国产");
        let movie = group.join("无间道");
        fs::create_dir_all(&movie).unwrap();
        fs::write(movie.join("无间道.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "movies"), "Folder");
        assert_eq!(tv_folder_type(&movie, &root, "movies"), "Movie");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_movie_grouping_folder_with_nested_content_stays_folder() {
        let root = test_dir("unknown_movie_grouping_folder_with_nested_content_stays_folder");
        let group = root.join("4K");
        let movie = group.join("无间道");
        fs::create_dir_all(&movie).unwrap();
        fs::write(movie.join("无间道.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "movies"), "Folder");
        assert_eq!(tv_folder_type(&movie, &root, "movies"), "Movie");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mixed_movie_folder_with_different_videos_stays_folder() {
        let root = test_dir("mixed_movie_folder_with_different_videos_stays_folder");
        let group = root.join("动作");
        fs::create_dir_all(&group).unwrap();
        fs::write(group.join("Movie One.mkv"), []).unwrap();
        fs::write(group.join("Movie Two.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "movies"), "Folder");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stacked_movie_folder_stays_movie() {
        let root = test_dir("stacked_movie_folder_stays_movie");
        let movie = root.join("Bad Boys (2006)");
        fs::create_dir_all(&movie).unwrap();
        fs::write(movie.join("Bad Boys (2006) CD1.mkv"), []).unwrap();
        fs::write(movie.join("Bad Boys (2006) CD2.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&movie, &root, "movies"), "Movie");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_tv_folder_with_episode_file_is_series() {
        let root = test_dir("nested_tv_folder_with_episode_file_is_series");
        let group = root.join("数");
        let series = group.join("数");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("S01E01.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "tvshows"), "Folder");
        assert_eq!(tv_folder_type(&series, &root, "tvshows"), "Series");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tv_quality_and_range_folders_stay_folders() {
        let root = test_dir("tv_quality_and_range_folders_stay_folders");
        let series = root.join("剧名 (2025){tmdb-123}");
        let quality = series.join("4K DV 高码");
        let range = series.join("001-020集.4K");
        let chinese_range = series.join("第1集到第20集.4K");
        fs::create_dir_all(&quality).unwrap();
        fs::create_dir_all(&range).unwrap();
        fs::create_dir_all(&chinese_range).unwrap();
        fs::write(quality.join("剧名 S01E01.mkv"), []).unwrap();
        fs::write(range.join("01.mkv"), []).unwrap();
        fs::write(chinese_range.join("第1集.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&series, &root, "tvshows"), "Series");
        assert_eq!(tv_folder_type(&quality, &root, "tvshows"), "Folder");
        assert_eq!(tv_folder_type(&range, &root, "tvshows"), "Folder");
        assert_eq!(tv_folder_type(&chinese_range, &root, "tvshows"), "Folder");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn decorated_season_folder_stays_season() {
        let root = Path::new("/media/电视剧");
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/S01 高码率"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("动漫/灵笼/灵笼 第一季（2019）"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("动漫/灵笼/灵笼 第二季（2025）"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("动漫/剧名/第十二季"), root, "tvshows"),
            "Season"
        );
    }

    #[test]
    fn tv_episode_parent_skips_quality_folder_inside_season() {
        let root = Path::new("/media/动漫/国漫");
        let episode = root.join("L/灵笼{tmdb-91097}/灵笼 第一季（2019）/4K 高码率/第1集.strm");
        let season = root.join("L/灵笼{tmdb-91097}/灵笼 第一季（2019）");

        assert_eq!(
            super::parent_id_for_scanned_file(&episode, root, "library", "tvshows", "Episode"),
            crate::util::stable_item_id(&season)
        );
    }

    #[test]
    fn tv_episode_parent_skips_range_folder_inside_series() {
        let root = Path::new("/media/动漫/国漫");
        let episode = root.join("S/双生武魂 (2025){tmdb-290681}/第1集到第20集.4K/第1集.strm");
        let series = root.join("S/双生武魂 (2025){tmdb-290681}");

        assert_eq!(
            super::parent_id_for_scanned_file(&episode, root, "library", "tvshows", "Episode"),
            crate::util::stable_item_id(&series)
        );
    }

    #[test]
    fn unknown_tv_grouping_folder_with_nested_series_stays_folder() {
        let root = test_dir("unknown_tv_grouping_folder_with_nested_series_stays_folder");
        let group = root.join("4K");
        let series = group.join("Friends");
        fs::create_dir_all(series.join("Season 1")).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "tvshows"), "Folder");
        assert_eq!(tv_folder_type(&series, &root, "tvshows"), "Series");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tv_folder_with_unparsed_video_stays_folder() {
        let root = test_dir("tv_folder_with_unparsed_video_stays_folder");
        let group = root.join("字幕");
        fs::create_dir_all(&group).unwrap();
        fs::write(group.join("F字幕.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&group, &root, "tvshows"), "Folder");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_season_folder_stays_season() {
        let root = Path::new("/media/电视剧");
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/Season 1"), root, "tvshows"),
            "Season"
        );
    }

    fn test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jellyfin-rs-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
