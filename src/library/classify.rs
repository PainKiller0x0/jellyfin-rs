use std::path::Path;

use crate::util::stable_item_id;

pub fn classify_media_path(path: &Path, library_id: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "avi" | "wmv" | "webm" | "ts" | "m2ts" | "flv"
    ) {
        return Some(
            if library_id == "tvshows" {
                "Episode"
            } else {
                "Movie"
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
    path.parent()
        .filter(|parent| *parent != root)
        .map(stable_item_id)
        .unwrap_or_else(|| library_id.to_string())
}

pub fn tv_folder_type(path: &Path, root: &Path, library_id: &str) -> &'static str {
    if library_id != "tvshows" {
        return "Folder";
    }
    let depth = path
        .strip_prefix(root)
        .ok()
        .map(|relative| relative.components().count())
        .unwrap_or_default();
    if depth == 1 {
        "Series"
    } else if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_season_folder_name)
    {
        "Season"
    } else {
        "Folder"
    }
}

fn is_season_folder_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("season ")
        || name.starts_with("season_")
        || name.starts_with('s') && name[1..].chars().all(|c| c.is_ascii_digit())
}
