use std::path::Path;

use crate::util::stable_item_id;

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
    let norm_path = super::path_utils::normalize_path(&path.to_string_lossy());
    let norm_root = super::path_utils::normalize_path(&root.to_string_lossy());
    let path = std::path::Path::new(&norm_path);
    let root = std::path::Path::new(&norm_root);
    path.parent()
        .filter(|parent| *parent != root)
        .map(stable_item_id)
        .unwrap_or_else(|| library_id.to_string())
}

pub fn tv_folder_type(path: &Path, root: &Path, collection_type: &str) -> &'static str {
    if collection_type == "movies" {
        // Movie libraries store each movie as a folder — classify as "Movie"
        return "Movie";
    }
    if collection_type != "tvshows" && collection_type != "tv" {
        return "Folder";
    }
    let norm_path = super::path_utils::normalize_path(&path.to_string_lossy());
    let norm_root = super::path_utils::normalize_path(&root.to_string_lossy());
    let depth = std::path::Path::new(&norm_path)
        .strip_prefix(std::path::Path::new(&norm_root))
        .ok()
        .map(|relative| relative.components().count())
        .unwrap_or_default();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if is_season_folder_name(name) {
        "Season"
    } else if directory_has_tv_content(path) || looks_like_series_folder_name(name) {
        "Series"
    } else if depth == 1 && !is_grouping_folder_name(name) {
        "Series"
    } else {
        "Folder"
    }
}

fn directory_has_tv_content(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let child = entry.path();
        if child.is_dir() {
            return child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_season_folder_name);
        }
        child.is_file() && classify_media_path(&child, "tvshows").as_deref() == Some("Episode")
    })
}

fn looks_like_series_folder_name(name: &str) -> bool {
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
    let name = name.to_ascii_lowercase();
    name.starts_with("season ")
        || name.starts_with("season_")
        || name.starts_with('s') && name[1..].chars().all(|c| c.is_ascii_digit())
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
