use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use regex::Regex;

use crate::{library::naming::parse_media_name, util::stable_item_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtraType {
    Unknown,
    Clip,
    Trailer,
    BehindTheScenes,
    DeletedScene,
    Interview,
    Scene,
    Sample,
    ThemeSong,
    ThemeVideo,
    Featurette,
    Short,
}

impl ExtraType {
    pub fn as_jellyfin_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Clip => "Clip",
            Self::Trailer => "Trailer",
            Self::BehindTheScenes => "BehindTheScenes",
            Self::DeletedScene => "DeletedScene",
            Self::Interview => "Interview",
            Self::Scene => "Scene",
            Self::Sample => "Sample",
            Self::ThemeSong => "ThemeSong",
            Self::ThemeVideo => "ThemeVideo",
            Self::Featurette => "Featurette",
            Self::Short => "Short",
        }
    }

    pub fn from_jellyfin_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unknown" => Some(Self::Unknown),
            "clip" => Some(Self::Clip),
            "trailer" => Some(Self::Trailer),
            "behindthescenes" | "behind the scenes" => Some(Self::BehindTheScenes),
            "deletedscene" | "deleted scene" => Some(Self::DeletedScene),
            "interview" => Some(Self::Interview),
            "scene" => Some(Self::Scene),
            "sample" => Some(Self::Sample),
            "themesong" | "theme song" => Some(Self::ThemeSong),
            "themevideo" | "theme video" => Some(Self::ThemeVideo),
            "featurette" => Some(Self::Featurette),
            "short" => Some(Self::Short),
            _ => None,
        }
    }

    pub fn is_display_special_feature(self) -> bool {
        matches!(
            self,
            Self::Unknown
                | Self::BehindTheScenes
                | Self::Clip
                | Self::DeletedScene
                | Self::Interview
                | Self::Sample
                | Self::Scene
                | Self::Featurette
                | Self::Short
        )
    }
}

pub fn classify_media_path(path: &Path, collection_type: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    // STRM files are classified from their resolved content by the scanner.
    if extension == "strm" {
        return Some(
            match collection_type {
                "tvshows" | "tv" => "Episode",
                "music" => "Audio",
                "books" => "AudioBook",
                "musicvideos" => "MusicVideo",
                "trailers" => "Trailer",
                "movies" => "Video",
                _ => "Movie",
            }
            .to_string(),
        );
    }
    if is_audio_extension(&extension) {
        return Some(
            if collection_type == "books" {
                "AudioBook"
            } else {
                "Audio"
            }
            .to_string(),
        );
    }
    if collection_type == "books" && is_book_extension(&extension) {
        return Some("Book".to_string());
    }
    if matches!(collection_type, "photos" | "homevideos")
        && is_photo_extension(&extension)
        && is_standalone_photo(path)
    {
        return Some("Photo".to_string());
    }
    if is_video_extension(&extension) {
        if collection_type == "trailers" {
            return Some("Trailer".to_string());
        }
        if let Some(extra_type) = video_extra_type(path, false) {
            return Some(
                match extra_type {
                    ExtraType::Trailer => "Trailer",
                    _ => "Video",
                }
                .to_string(),
            );
        }
        return Some(
            match collection_type {
                "tvshows" | "tv" => "Episode",
                "music" | "books" => return None,
                "musicvideos" => "MusicVideo",
                "homevideos" | "photos" => "Video",
                "trailers" => "Trailer",
                // In movie libraries, files are "Video" — the folder itself is the "Movie"
                "movies" => "Video",
                _ => "Movie",
            }
            .to_string(),
        );
    }
    None
}

fn is_book_extension(extension: &str) -> bool {
    matches!(
        extension,
        "azw" | "azw3" | "cb7" | "cbr" | "cbt" | "cbz" | "epub" | "mobi" | "pdf"
    )
}

pub fn book_file_for_directory(path: &Path) -> Option<PathBuf> {
    let mut books = std::fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate.is_file()
                && candidate
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| is_book_extension(&extension.to_ascii_lowercase()))
        });
    let book = books.next()?;
    books.next().is_none().then_some(book)
}

pub fn audiobook_file_for_directory(path: &Path) -> Option<PathBuf> {
    let mut audio = std::fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate.is_file()
                && candidate
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| is_audio_extension(&extension.to_ascii_lowercase()))
        });
    let audiobook = audio.next()?;
    audio.next().is_none().then_some(audiobook)
}

fn is_photo_extension(extension: &str) -> bool {
    matches!(
        extension,
        "avif" | "bmp" | "gif" | "jpeg" | "jpg" | "png" | "tif" | "tiff" | "webp"
    )
}

fn is_standalone_photo(path: &Path) -> bool {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let lower = stem.to_ascii_lowercase();
    if [
        "folder",
        "thumb",
        "landscape",
        "fanart",
        "backdrop",
        "poster",
        "cover",
        "logo",
        "default",
    ]
    .into_iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return false;
    }

    let Some(parent) = path.parent() else {
        return true;
    };
    std::fs::read_dir(parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| candidate != path && candidate.is_file())
        .filter(|candidate| {
            candidate
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| is_video_extension(&extension.to_ascii_lowercase()))
        })
        .all(|video| {
            let video_stem = video
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            !lower.starts_with(&video_stem)
        })
}

fn is_video_extension(extension: &str) -> bool {
    matches!(
        extension,
        "001"
            | "3g2"
            | "3gp"
            | "amv"
            | "asf"
            | "asx"
            | "avi"
            | "bin"
            | "bivx"
            | "divx"
            | "disc"
            | "dv"
            | "dvr-ms"
            | "f4v"
            | "fli"
            | "flv"
            | "ifo"
            | "img"
            | "iso"
            | "m2t"
            | "m2ts"
            | "m2v"
            | "m4v"
            | "mkv"
            | "mk3d"
            | "mp4"
            | "mpe"
            | "mpeg"
            | "mpg"
            | "m3u8"
            | "m3u"
            | "mov"
            | "mts"
            | "mxf"
            | "nrg"
            | "nsv"
            | "nuv"
            | "ogg"
            | "ogm"
            | "ogv"
            | "pva"
            | "qt"
            | "rec"
            | "rm"
            | "rmvb"
            | "svq3"
            | "tp"
            | "ts"
            | "ty"
            | "viv"
            | "vob"
            | "vp3"
            | "webm"
            | "wmv"
            | "wtv"
            | "xvid"
    )
}

pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(is_audio_extension)
}

fn is_audio_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "669"
            | "3gp"
            | "aa"
            | "aac"
            | "aax"
            | "ac3"
            | "act"
            | "adp"
            | "adplug"
            | "adx"
            | "afc"
            | "aif"
            | "aifc"
            | "aiff"
            | "alac"
            | "amf"
            | "amr"
            | "ape"
            | "ast"
            | "au"
            | "awb"
            | "cda"
            | "cue"
            | "dmf"
            | "dsf"
            | "dsm"
            | "dsp"
            | "dts"
            | "dvf"
            | "eac3"
            | "ec3"
            | "far"
            | "flac"
            | "gdm"
            | "gsm"
            | "gym"
            | "hps"
            | "imf"
            | "it"
            | "m15"
            | "m4a"
            | "m4b"
            | "mac"
            | "med"
            | "mka"
            | "mmf"
            | "mod"
            | "mogg"
            | "mp2"
            | "mp3"
            | "mpa"
            | "mpc"
            | "mpp"
            | "mp+"
            | "msv"
            | "nmf"
            | "nsf"
            | "oga"
            | "ogg"
            | "okt"
            | "opus"
            | "pls"
            | "ra"
            | "rf64"
            | "s3m"
            | "sfx"
            | "shn"
            | "sid"
            | "stm"
            | "ult"
            | "uni"
            | "vox"
            | "wav"
            | "wma"
            | "wv"
            | "xm"
            | "xsp"
            | "ymf"
    )
}

pub fn folder_video_type(path: &Path) -> Option<&'static str> {
    let video_ts = child_directory_case_insensitive(path, "VIDEO_TS");
    if video_ts.as_deref().is_some_and(Path::is_dir)
        && video_ts
            .as_ref()
            .and_then(|path| std::fs::read_dir(path).ok())
            .is_some_and(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("vob"))
                })
            })
    {
        return Some("Dvd");
    }

    if child_directory_case_insensitive(path, "BDMV").is_some() {
        return Some("BluRay");
    }

    if child_file_case_insensitive(path, "VIDEO_TS.IFO").is_some() {
        return Some("Dvd");
    }

    None
}

pub fn file_video_type(path: &Path) -> Option<&'static str> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("video_ts.ifo"))
    {
        return Some("Dvd");
    }

    if is_video_stub(path) {
        return match stub_type_token(path).as_deref() {
            Some("dvd") => Some("Dvd"),
            Some("bluray") => Some("BluRay"),
            _ => None,
        };
    }

    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("iso" | "img") => Some("Iso"),
        _ => None,
    }
}

pub fn is_video_stub(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("disc"))
}

fn stub_type_token(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Path::new(stem)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .filter(|extension| matches!(extension.as_str(), "dvd" | "hddvd" | "bluray"))
}

pub fn iso_type_for_path(path: &Path) -> Option<&'static str> {
    iso_type_from_name(path).or_else(|| iso_type_from_udf(path))
}

pub fn iso_type_from_name(path: &Path) -> Option<&'static str> {
    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    if lower_path.contains("dvd") {
        Some("Dvd")
    } else if lower_path.contains("bluray") {
        Some("BluRay")
    } else {
        None
    }
}

fn iso_type_from_udf(path: &Path) -> Option<&'static str> {
    use bdinfo_rs_core::vfs::{
        BdDir,
        udf::source::{PathIso, UdfSource},
    };

    if !path.is_file() {
        return None;
    }
    let udf = UdfSource::open(Box::new(PathIso::new(path))).ok()?;
    let directories = udf.root().get_directories().ok()?;
    if directories
        .iter()
        .any(|directory| directory.name().eq_ignore_ascii_case("VIDEO_TS"))
    {
        Some("Dvd")
    } else if directories
        .iter()
        .any(|directory| directory.name().eq_ignore_ascii_case("BDMV"))
    {
        Some("BluRay")
    } else {
        None
    }
}

fn child_directory_case_insensitive(parent: &Path, name: &str) -> Option<PathBuf> {
    child_path_case_insensitive(parent, name, true)
}

fn child_file_case_insensitive(parent: &Path, name: &str) -> Option<PathBuf> {
    child_path_case_insensitive(parent, name, false)
}

fn child_path_case_insensitive(parent: &Path, name: &str, directory: bool) -> Option<PathBuf> {
    std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            (if directory {
                path.is_dir()
            } else {
                path.is_file()
            }) && path
                .file_name()
                .and_then(|candidate| candidate.to_str())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        })
}

pub fn should_skip_disc_structure_entry(path: &Path, root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| matches!(name.to_ascii_lowercase().as_str(), "bdmv" | "video_ts"))
    }) {
        return true;
    }

    if path.is_file()
        && path.parent().is_some_and(|parent| parent != root)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name == "video_ts.ifo" || name.ends_with(".vob") || name.ends_with(".bup")
            })
        && path
            .parent()
            .and_then(folder_video_type)
            .is_some_and(|video_type| video_type == "Dvd")
    {
        return true;
    }

    false
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
    is_extra: bool,
) -> String {
    if is_extra
        && let Some(parent) = parent_path_for_path(path, root)
        && parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_extra_folder_name)
    {
        return parent_id_for_path(&parent, root, library_id);
    }

    if matches!(collection_type, "tvshows" | "tv") && item_type == "Episode" {
        let mut current = parent_path_for_path(path, root);
        let mut series_parent = None;
        while let Some(parent) = current {
            let parent_type = tv_folder_type(&parent, root, collection_type);
            if parent_type == "Season" {
                return stable_item_id(&parent);
            }
            if parent_type == "Series" && series_parent.is_none() {
                series_parent = Some(stable_item_id(&parent));
            }
            current = parent_path_for_path(&parent, root);
        }
        if let Some(series_parent) = series_parent {
            return series_parent;
        }
    }

    if collection_type == "music"
        && item_type == "Audio"
        && let Some(parent) = parent_path_for_path(path, root)
        && is_music_multi_part_folder(&parent)
        && let Some(album) = parent_path_for_path(&parent, root)
    {
        return stable_item_id(&album);
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
    if path != root && matches!(collection_type, "photos" | "homevideos") {
        return if directory_has_standalone_photo(path) {
            "PhotoAlbum"
        } else {
            "Folder"
        };
    }
    if path != root && collection_type == "music" {
        if path.join("artist.nfo").is_file() {
            return "MusicArtist";
        }
        if is_music_multi_part_folder(path) {
            return "Folder";
        }
        if directory_has_direct_audio(path) {
            return "MusicAlbum";
        }
        if directory_has_music_album(path) {
            return "MusicArtist";
        }
        if directory_is_multi_disc_album(path) {
            return "MusicAlbum";
        }
        return "Folder";
    }
    if collection_type != "tvshows" && collection_type != "tv" {
        return "Folder";
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let parent_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str());
    if path_depth(path, root) > 1 && season_number_from_folder_name(name, parent_name).is_some() {
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

fn directory_has_standalone_photo(path: &Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .any(|candidate| {
            candidate.is_file()
                && candidate
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| is_photo_extension(&extension.to_ascii_lowercase()))
                && is_standalone_photo(&candidate)
        })
}

fn directory_has_direct_audio(path: &Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .any(|candidate| {
            candidate.is_file()
                && candidate
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| is_audio_extension(&extension.to_ascii_lowercase()))
        })
}

fn directory_has_music_album(path: &Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .any(|candidate| {
            candidate.is_dir()
                && !is_music_multi_part_folder(&candidate)
                && (directory_has_direct_audio(&candidate)
                    || directory_is_multi_disc_album(&candidate))
        })
}

fn directory_is_multi_disc_album(path: &Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .any(|candidate| {
            candidate.is_dir()
                && is_music_multi_part_folder(&candidate)
                && directory_has_direct_audio(&candidate)
        })
}

pub fn is_music_multi_part_folder(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let normalized = Regex::new(r#"[-\.\(\)\s]+"#)
        .expect("music multi-part regex must compile")
        .replace_all(name, " ");
    let normalized = normalized.trim_start();
    [
        "cd",
        "digital media",
        "disc",
        "disk",
        "vol",
        "volume",
        "part",
        "act",
    ]
    .iter()
    .any(|prefix| {
        normalized
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            && normalized[prefix.len()..]
                .trim()
                .split(' ')
                .next()
                .is_some_and(|number| number.parse::<i64>().is_ok())
    })
}

fn movie_folder_type(path: &Path, _root: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if is_grouping_folder_name(name) || is_extra_folder_name(name) {
        "Folder"
    } else if folder_video_type(path).is_some()
        || directory_is_movie_folder(path)
        || looks_like_movie_folder_name(name)
    {
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
                .is_some_and(|name| season_number_from_folder_name(name, None).is_some())
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
        provider_tag_prefixes().iter().any(|prefix| {
            tag.strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('='))
        })
    })
}

fn provider_tag_prefixes() -> &'static [&'static str] {
    &[
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

fn season_number_from_folder_name(name: &str, parent_name: Option<&str>) -> Option<i64> {
    if let Some(number) = season_prefix_regex()
        .captures(name)
        .and_then(|captures| captures.name("number"))
        .and_then(|number| number.as_str().parse::<i64>().ok())
    {
        return Some(number);
    }

    let original = name
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '-'))
        .to_ascii_lowercase();
    let mut clean_name = clean_season_name(&original);
    if let Some(parent_name) = parent_name {
        let clean_parent = clean_season_name(parent_name);
        if !clean_parent.is_empty() {
            clean_name = clean_name.replace(&clean_parent, "");
        }
    }

    if matches!(clean_name.as_str(), "specials" | "extras") {
        return Some(0);
    }
    if let Ok(number) = clean_name.parse::<i64>() {
        return Some(number);
    }
    if let Some(number) = parse_keyword_season_number(&clean_name) {
        return Some(number);
    }
    chinese_season_number(&original)
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

fn chinese_season_number(name: &str) -> Option<i64> {
    name.split_once('第')
        .and_then(|(_, rest)| rest.split_once('季').map(|(number, _)| number.trim()))
        .and_then(crate::util::parse_chinese_number)
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

pub fn is_extra_folder_name(name: &str) -> bool {
    extra_type_for_directory(name).is_some()
}

pub fn video_extra_type(path: &Path, is_audio: bool) -> Option<ExtraType> {
    let parent = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase());
    if let Some(parent) = parent.as_deref()
        && let Some(kind) = extra_type_for_directory(parent)
        && extra_type_matches_media(kind, is_audio)
    {
        return Some(kind);
    }

    let stem = path.file_stem()?.to_str()?;
    let stem_lower = stem.to_ascii_lowercase();
    let trimmed_stem = stem_lower.trim_end_matches(|c: char| c.is_ascii_digit());
    for rule in filename_extra_rules() {
        if !extra_type_matches_media(rule.extra_type, is_audio) {
            continue;
        }
        let matched = match rule.rule_type {
            ExtraRuleType::Filename => stem_lower.eq_ignore_ascii_case(rule.token),
            ExtraRuleType::Suffix => trimmed_stem.ends_with(rule.token),
        };
        if matched {
            return Some(rule.extra_type);
        }
    }
    None
}

fn extra_type_for_directory(name: &str) -> Option<ExtraType> {
    match normalize_extra_token(name).as_str() {
        "trailers" | "trailer" => Some(ExtraType::Trailer),
        "backdrops" => Some(ExtraType::ThemeVideo),
        "theme-music" | "thememusic" => Some(ExtraType::ThemeSong),
        "behindthescenes" => Some(ExtraType::BehindTheScenes),
        "deletedscenes" | "deletedscene" => Some(ExtraType::DeletedScene),
        "interviews" | "interview" => Some(ExtraType::Interview),
        "scenes" | "scene" => Some(ExtraType::Scene),
        "samples" | "sample" => Some(ExtraType::Sample),
        "shorts" | "short" => Some(ExtraType::Short),
        "featurettes" | "featurette" => Some(ExtraType::Featurette),
        "extras" | "extra" | "other" => Some(ExtraType::Unknown),
        "clips" | "clip" => Some(ExtraType::Clip),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ExtraRuleType {
    Filename,
    Suffix,
}

#[derive(Clone, Copy)]
struct FilenameExtraRule {
    extra_type: ExtraType,
    rule_type: ExtraRuleType,
    token: &'static str,
}

fn filename_extra_rules() -> &'static [FilenameExtraRule] {
    &[
        FilenameExtraRule {
            extra_type: ExtraType::Trailer,
            rule_type: ExtraRuleType::Filename,
            token: "trailer",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Sample,
            rule_type: ExtraRuleType::Filename,
            token: "sample",
        },
        FilenameExtraRule {
            extra_type: ExtraType::ThemeSong,
            rule_type: ExtraRuleType::Filename,
            token: "theme",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Trailer,
            rule_type: ExtraRuleType::Suffix,
            token: "-trailer",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Trailer,
            rule_type: ExtraRuleType::Suffix,
            token: ".trailer",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Trailer,
            rule_type: ExtraRuleType::Suffix,
            token: "_trailer",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Trailer,
            rule_type: ExtraRuleType::Suffix,
            token: "- trailer",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Sample,
            rule_type: ExtraRuleType::Suffix,
            token: "-sample",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Sample,
            rule_type: ExtraRuleType::Suffix,
            token: ".sample",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Sample,
            rule_type: ExtraRuleType::Suffix,
            token: "_sample",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Sample,
            rule_type: ExtraRuleType::Suffix,
            token: "- sample",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Scene,
            rule_type: ExtraRuleType::Suffix,
            token: "-scene",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Clip,
            rule_type: ExtraRuleType::Suffix,
            token: "-clip",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Interview,
            rule_type: ExtraRuleType::Suffix,
            token: "-interview",
        },
        FilenameExtraRule {
            extra_type: ExtraType::BehindTheScenes,
            rule_type: ExtraRuleType::Suffix,
            token: "-behindthescenes",
        },
        FilenameExtraRule {
            extra_type: ExtraType::DeletedScene,
            rule_type: ExtraRuleType::Suffix,
            token: "-deleted",
        },
        FilenameExtraRule {
            extra_type: ExtraType::DeletedScene,
            rule_type: ExtraRuleType::Suffix,
            token: "-deletedscene",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Featurette,
            rule_type: ExtraRuleType::Suffix,
            token: "-featurette",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Short,
            rule_type: ExtraRuleType::Suffix,
            token: "-short",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Unknown,
            rule_type: ExtraRuleType::Suffix,
            token: "-extra",
        },
        FilenameExtraRule {
            extra_type: ExtraType::Unknown,
            rule_type: ExtraRuleType::Suffix,
            token: "-other",
        },
    ]
}

fn extra_type_matches_media(extra_type: ExtraType, is_audio: bool) -> bool {
    matches!(extra_type, ExtraType::ThemeSong) == is_audio
}

fn normalize_extra_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['\\', '/'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("")
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
    fn jellyfin_video_extensions_are_recognized() {
        for file in [
            "movie.iso",
            "movie.vob",
            "movie.rmvb",
            "movie.ogm",
            "movie.wtv",
        ] {
            assert_eq!(
                classify_media_path(Path::new(file), "movies").as_deref(),
                Some("Video"),
                "{file}"
            );
        }
    }

    #[test]
    fn jellyfin_disc_stubs_are_video_placeholders() {
        for (path, video_type) in [
            ("Movie.disc", None),
            ("Movie.dvd.disc", Some("Dvd")),
            ("Movie.hddvd.disc", None),
            ("Movie.bluray.disc", Some("BluRay")),
        ] {
            assert_eq!(
                classify_media_path(Path::new(path), "movies").as_deref(),
                Some("Video")
            );
            assert!(super::is_video_stub(Path::new(path)));
            assert_eq!(super::file_video_type(Path::new(path)), video_type);
        }
    }

    #[test]
    fn disc_structure_video_types_follow_jellyfin_resolver_names() {
        let root = test_dir("disc_structure_video_types_follow_jellyfin_resolver_names");
        let dvd = root.join("Dvd Movie");
        let video_ts = dvd.join("VIDEO_TS");
        let bd = root.join("Bd Movie");
        fs::create_dir_all(&video_ts).unwrap();
        fs::create_dir_all(bd.join("BDMV")).unwrap();
        fs::write(video_ts.join("VTS_01_1.VOB"), []).unwrap();

        assert_eq!(super::folder_video_type(&dvd), Some("Dvd"));
        assert_eq!(super::folder_video_type(&bd), Some("BluRay"));
        assert_eq!(super::tv_folder_type(&dvd, &root, "movies"), "Movie");
        assert_eq!(super::tv_folder_type(&bd, &root, "movies"), "Movie");
        assert_eq!(super::file_video_type(Path::new("movie.iso")), Some("Iso"));
        assert_eq!(
            super::file_video_type(Path::new("VIDEO_TS.IFO")),
            Some("Dvd")
        );
        assert_eq!(
            super::iso_type_for_path(Path::new("/media/Movie.bluray.iso")),
            Some("BluRay")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn disc_structure_entries_are_owned_by_parent_video() {
        let root = Path::new("/media/movies");

        assert!(super::should_skip_disc_structure_entry(
            &root.join("Movie/BDMV/index.bdmv"),
            root
        ));
        assert!(super::should_skip_disc_structure_entry(
            &root.join("Movie/VIDEO_TS/VTS_01_1.VOB"),
            root
        ));
        assert!(!super::should_skip_disc_structure_entry(
            &root.join("Movie/Movie.mkv"),
            root
        ));
    }

    #[test]
    fn jellyfin_audio_extensions_are_recognized() {
        for file in [
            "track.m4b",
            "track.mka",
            "track.wma",
            "track.wv",
            "track.ra",
        ] {
            assert_eq!(
                classify_media_path(Path::new(file), "music").as_deref(),
                Some("Audio"),
                "{file}"
            );
        }
    }

    #[test]
    fn jellyfin_video_extras_do_not_become_movies_or_episodes() {
        assert_eq!(
            classify_media_path(Path::new("/media/Movie/trailers/Trailer.mp4"), "movies")
                .as_deref(),
            Some("Trailer")
        );
        assert_eq!(
            classify_media_path(Path::new("/media/Movie/Movie-trailer2.mp4"), "movies").as_deref(),
            Some("Trailer")
        );
        assert_eq!(
            classify_media_path(Path::new("/media/Show/extras/Behind.mp4"), "tvshows").as_deref(),
            Some("Video")
        );
        assert_eq!(
            classify_media_path(Path::new("/media/Show/trailers/Trailer.mp4"), "tvshows")
                .as_deref(),
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
        assert_eq!(
            tv_folder_type(&root.join("Anime [anidbid-123]"), root, "tvshows"),
            "Series"
        );
        assert_eq!(
            tv_folder_type(&root.join("Show [tvdbid=456]"), root, "tvshows"),
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
    fn movie_extra_folders_stay_folders() {
        let root = test_dir("movie_extra_folders_stay_folders");
        let movie = root.join("Movie");
        let trailers = movie.join("trailers");
        fs::create_dir_all(&trailers).unwrap();
        fs::write(movie.join("Movie.mkv"), []).unwrap();
        fs::write(trailers.join("Trailer.mkv"), []).unwrap();

        assert_eq!(tv_folder_type(&movie, &root, "movies"), "Movie");
        assert_eq!(tv_folder_type(&trailers, &root, "movies"), "Folder");

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
            tv_folder_type(&root.join("国产/剧名/Season1"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/01"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/1st Season"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/Staffel 2"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/Temporada 3"), root, "tvshows"),
            "Season"
        );
        assert_eq!(
            tv_folder_type(&root.join("国产/剧名/剧名 Season 4"), root, "tvshows"),
            "Season"
        );
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
            super::parent_id_for_scanned_file(
                &episode, root, "library", "tvshows", "Episode", false,
            ),
            crate::util::stable_item_id(&season)
        );
    }

    #[test]
    fn tv_episode_parent_walks_arbitrary_nested_folders_to_season() {
        let root = Path::new("/media/tv");
        let season = root.join("Show/Season 1");
        let episode = season.join("Story Arc/Disc A/Show S01E01.mkv");

        assert_eq!(
            super::parent_id_for_scanned_file(
                &episode, root, "library", "tvshows", "Episode", false,
            ),
            crate::util::stable_item_id(&season)
        );
    }

    #[test]
    fn tv_episode_parent_skips_range_folder_inside_series() {
        let root = Path::new("/media/动漫/国漫");
        let episode = root.join("S/双生武魂 (2025){tmdb-290681}/第1集到第20集.4K/第1集.strm");
        let series = root.join("S/双生武魂 (2025){tmdb-290681}");

        assert_eq!(
            super::parent_id_for_scanned_file(
                &episode, root, "library", "tvshows", "Episode", false,
            ),
            crate::util::stable_item_id(&series)
        );
    }

    #[test]
    fn extra_folder_file_parent_is_owner_folder() {
        let root = Path::new("/media/movies");
        let extra = root.join("Movie/extras/Behind the Scenes.mkv");
        let movie = root.join("Movie");

        assert_eq!(
            super::parent_id_for_scanned_file(&extra, root, "library", "movies", "Video", true),
            crate::util::stable_item_id(&movie)
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
    fn tv_extra_folders_do_not_create_series_from_extra_videos() {
        let root = test_dir("tv_extra_folders_do_not_create_series_from_extra_videos");
        let group = root.join("trailers");
        fs::create_dir_all(&group).unwrap();
        fs::write(group.join("Trailer.mkv"), []).unwrap();

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

    #[test]
    fn specialized_library_files_follow_jellyfin_item_types() {
        assert_eq!(
            classify_media_path(Path::new("/books/Novel.epub"), "books").as_deref(),
            Some("Book")
        );
        assert_eq!(
            classify_media_path(Path::new("/books/Novel.m4b"), "books").as_deref(),
            Some("AudioBook")
        );
        assert_eq!(
            classify_media_path(Path::new("/musicvideos/Song.mkv"), "musicvideos").as_deref(),
            Some("MusicVideo")
        );
        assert_eq!(
            classify_media_path(Path::new("/music/Artist/Album/video.mp4"), "music"),
            None
        );
    }

    #[test]
    fn photo_resolver_ignores_artwork_owned_by_video() {
        let root = test_dir("photo_resolver_ignores_artwork_owned_by_video");
        let album = root.join("Album");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("clip.mp4"), []).unwrap();
        fs::write(album.join("clip-poster.jpg"), []).unwrap();
        fs::write(album.join("holiday.jpg"), []).unwrap();

        assert_eq!(
            classify_media_path(&album.join("clip-poster.jpg"), "photos"),
            None
        );
        assert_eq!(
            classify_media_path(&album.join("holiday.jpg"), "photos").as_deref(),
            Some("Photo")
        );
        assert_eq!(tv_folder_type(&album, &root, "photos"), "PhotoAlbum");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn music_folder_resolver_distinguishes_artist_and_album() {
        let root = test_dir("music_folder_resolver_distinguishes_artist_and_album");
        let artist = root.join("Artist");
        let album = artist.join("Album");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("01 Song.flac"), []).unwrap();

        assert_eq!(tv_folder_type(&artist, &root, "music"), "MusicArtist");
        assert_eq!(tv_folder_type(&album, &root, "music"), "MusicAlbum");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn single_book_directory_and_music_multi_disc_rules_follow_jellyfin() {
        let root = test_dir("single_book_directory_and_music_multi_disc_rules_follow_jellyfin");
        let book = root.join("2 - The Sign of the Four (1890)");
        fs::create_dir_all(&book).unwrap();
        fs::write(book.join("content.epub"), []).unwrap();
        assert_eq!(
            super::book_file_for_directory(&book).as_deref(),
            Some(book.join("content.epub").as_path())
        );
        fs::write(book.join("second.pdf"), []).unwrap();
        assert_eq!(super::book_file_for_directory(&book), None);

        let artist = root.join("Artist");
        let album = artist.join("Album");
        let disc = album.join("Disc 01 (Bonus)");
        fs::create_dir_all(&disc).unwrap();
        fs::write(disc.join("01 Song.flac"), []).unwrap();
        assert_eq!(tv_folder_type(&artist, &root, "music"), "MusicArtist");
        assert_eq!(tv_folder_type(&album, &root, "music"), "MusicAlbum");
        assert_eq!(tv_folder_type(&disc, &root, "music"), "Folder");
        assert_eq!(
            super::parent_id_for_scanned_file(
                &disc.join("01 Song.flac"),
                &root,
                "library",
                "music",
                "Audio",
                false,
            ),
            crate::util::stable_item_id(&album)
        );

        let _ = fs::remove_dir_all(root);
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
