use std::{path::Path, sync::OnceLock};

use anyhow::Context;
use regex::Regex;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};

use crate::{
    entities::media_streams::{self, Entity as MediaStreams},
    util::{now_unix, stable_text_id},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ExternalAudioMetadata {
    language: Option<String>,
    title: Option<String>,
    is_default: bool,
    is_forced: bool,
    is_hearing_impaired: bool,
}

pub async fn upsert_sidecar_audio(
    db: &DatabaseConnection,
    media_path: &Path,
    item_id: &str,
) -> anyhow::Result<()> {
    let candidates = external_audio_candidates(media_path);
    let streams = tokio::task::spawn_blocking(move || {
        let mut streams = Vec::new();
        for (path, metadata) in candidates {
            let Some(probe) = crate::library::probe::probe_media(&path) else {
                tracing::warn!("failed to probe external audio file {}", path.display());
                continue;
            };
            for mut stream in probe
                .streams
                .into_iter()
                .filter(|stream| stream.stream_type == "Audio")
            {
                stream.language = metadata.language.clone().or(stream.language);
                stream.title = metadata.title.clone().or(stream.title);
                stream.is_default = metadata.is_default;
                stream.is_forced |= metadata.is_forced;
                stream.is_hearing_impaired |= metadata.is_hearing_impaired;
                streams.push((path.to_string_lossy().to_string(), stream));
            }
        }
        streams
    })
    .await
    .context("external audio probe task failed")?;

    crate::library::storage::replace_external_audio_streams(db, item_id, &streams).await
}

pub async fn clear_sidecar_audio(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    crate::library::storage::replace_external_audio_streams(db, item_id, &[]).await
}

fn external_audio_candidates(
    media_path: &Path,
) -> Vec<(std::path::PathBuf, ExternalAudioMetadata)> {
    let Some(media_stem) = media_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    let mut candidates = sidecar_files(media_path)
        .into_iter()
        .filter_map(|entry| {
            if !crate::library::classify::is_audio_path(&entry) {
                return None;
            }
            let stem = entry.file_stem()?.to_str()?;
            external_audio_metadata(stem, media_stem).map(|metadata| (entry, metadata))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates
}

fn sidecar_files(media_path: &Path) -> Vec<std::path::PathBuf> {
    let Some(parent) = media_path.parent() else {
        return Vec::new();
    };
    std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect()
}

fn external_audio_metadata(stem: &str, media_stem: &str) -> Option<ExternalAudioMetadata> {
    let prefix = stem.get(..media_stem.len())?;
    if !prefix.eq_ignore_ascii_case(media_stem) {
        return None;
    }
    let suffix = stem.get(media_stem.len()..)?;
    if !suffix.is_empty() && !suffix.starts_with('.') {
        return None;
    }

    let mut metadata = ExternalAudioMetadata::default();
    let mut title = Vec::new();
    for token in suffix.trim_start_matches('.').split('.') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "default" => metadata.is_default = true,
            "foreign" | "forced" => metadata.is_forced = true,
            "cc" | "hi" | "sdh" => metadata.is_hearing_impaired = true,
            _ => {
                if let Some(language) = normalize_external_language(token) {
                    metadata.language.get_or_insert(language);
                } else {
                    title.push(token.to_string());
                }
            }
        }
    }
    metadata.title = (!title.is_empty()).then(|| title.join("."));
    Some(metadata)
}

fn normalize_external_language(language: &str) -> Option<String> {
    let language = language.to_ascii_lowercase();
    let normalized =
        normalize_common_external_language(&language).or_else(|| match language.as_str() {
            "fr" | "fra" | "fre" => Some("fr"),
            "de" | "deu" | "ger" => Some("de"),
            "es" | "spa" => Some("es"),
            "it" | "ita" => Some("it"),
            "pt" | "por" | "pt-br" => Some(language.as_str()),
            "ru" | "rus" => Some("ru"),
            _ => None,
        })?;
    Some(normalized.to_string())
}

fn normalize_common_external_language(language: &str) -> Option<&'static str> {
    match language {
        "chs" | "zh" | "zho" | "chi" | "cn" => Some("zh-CN"),
        "cht" | "tc" | "zh-tw" => Some("zh-TW"),
        "en" | "eng" => Some("en"),
        "ja" | "jpn" => Some("ja"),
        "ko" | "kor" => Some("ko"),
        _ => None,
    }
}

pub async fn upsert_sidecar_subtitles(
    db: &DatabaseConnection,
    media_path: &Path,
    item_id: &str,
) -> anyhow::Result<()> {
    let Some(media_stem) = media_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(());
    };
    clear_sidecar_subtitles(db, item_id).await?;
    let mut subtitle_index = next_external_subtitle_index(db, item_id).await?;
    for path in sidecar_files(media_path) {
        if !is_subtitle_path(&path) || !is_sidecar_for_media(&path, media_stem) {
            continue;
        }
        upsert_subtitle_stream(db, item_id, subtitle_index, &path).await?;
        subtitle_index += 1;
    }
    Ok(())
}

pub async fn clear_sidecar_subtitles(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<()> {
    let streams = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::IsExternal.eq(1_i64))
        .all(db)
        .await
        .context("failed to read existing subtitle streams")?;

    for stream in streams {
        if stream.stream_type == "Subtitle"
            || stream
                .path
                .as_deref()
                .map(is_subtitle_path_str)
                .unwrap_or(false)
        {
            MediaStreams::delete_by_id(stream.id)
                .exec(db)
                .await
                .context("failed to clear existing subtitle stream")?;
        }
    }
    Ok(())
}

async fn next_external_subtitle_index(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<i64> {
    let stream = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .order_by_desc(media_streams::Column::StreamIndex)
        .one(db)
        .await
        .context("failed to find next subtitle stream index")?;
    Ok(stream
        .map(|stream| stream.stream_index.saturating_add(1))
        .unwrap_or(0))
}

async fn upsert_subtitle_stream(
    db: &DatabaseConnection,
    item_id: &str,
    stream_index: i64,
    path: &Path,
) -> anyhow::Result<()> {
    let path_string = path.to_string_lossy().to_string();
    let codec = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_else(|| "srt".to_string());
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Subtitle")
        .to_string();
    let language = infer_subtitle_language(path);

    if let Some(existing) = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::StreamIndex.eq(stream_index))
        .one(db)
        .await
        .with_context(|| format!("failed to read subtitle stream: {}", path.display()))?
    {
        let mut active: media_streams::ActiveModel = existing.into();
        active.stream_type = Set("Subtitle".to_string());
        active.codec = Set(Some(codec));
        active.language = Set(language);
        active.title = Set(Some(title));
        active.path = Set(Some(path_string));
        active.is_external = Set(1);
        active
            .update(db)
            .await
            .with_context(|| format!("failed to update subtitle stream: {}", path.display()))?;
    } else {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(stable_text_id(&format!("subtitle:{item_id}:{path_string}"))),
            item_id: Set(item_id.to_string()),
            stream_index: Set(stream_index),
            stream_type: Set("Subtitle".to_string()),
            codec: Set(Some(codec)),
            language: Set(language),
            title: Set(Some(title)),
            path: Set(Some(path_string)),
            is_external: Set(1),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to insert subtitle stream: {}", path.display()))?;
    }
    Ok(())
}

fn is_subtitle_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(is_subtitle_extension)
        .unwrap_or_default()
}

fn is_subtitle_path_str(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(is_subtitle_extension)
        .unwrap_or(false)
}

fn is_subtitle_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "ass" | "mks" | "sami" | "smi" | "srt" | "ssa" | "sub" | "sup" | "vtt" | "mpl"
    )
}

fn is_sidecar_for_media(path: &Path, media_stem: &str) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| {
            stem == media_stem
                || stem.starts_with(&format!("{media_stem}."))
                || same_episode_sidecar(stem, media_stem)
        })
        .unwrap_or_default()
}

fn same_episode_sidecar(subtitle_stem: &str, media_stem: &str) -> bool {
    let Some(media) = episode_key(media_stem) else {
        return false;
    };
    let Some(subtitle) = episode_key(subtitle_stem) else {
        return false;
    };
    media.season == subtitle.season
        && media.episode == subtitle.episode
        && compatible_episode_prefix(&media.prefix, &subtitle.prefix)
}

struct EpisodeKey {
    prefix: String,
    season: i64,
    episode: i64,
}

fn episode_key(stem: &str) -> Option<EpisodeKey> {
    for regex in episode_regexes() {
        let Some(captures) = regex.captures(stem) else {
            continue;
        };
        return Some(EpisodeKey {
            prefix: normalize_episode_prefix(
                captures
                    .name("prefix")
                    .map(|value| value.as_str())
                    .unwrap_or(""),
            ),
            season: captures.name("season")?.as_str().parse().ok()?,
            episode: captures.name("episode")?.as_str().parse().ok()?,
        });
    }
    None
}

fn episode_regexes() -> &'static [Regex; 2] {
    static REGEXES: OnceLock<[Regex; 2]> = OnceLock::new();
    REGEXES.get_or_init(|| {
        [
            Regex::new(
                r#"(?i)^(?P<prefix>.*?)[ ._\-\[]*s(?P<season>[0-9]{1,4})[ ._\-\]]*e(?P<episode>[0-9]{1,3})(?:\b|[^0-9]).*$"#,
            )
            .expect("episode subtitle regex must compile"),
            Regex::new(
                r#"(?i)^(?P<prefix>.*?)[ ._\-\[]*(?P<season>[0-9]{1,4})x(?P<episode>[0-9]{1,3})(?:\b|[^0-9]).*$"#,
            )
            .expect("episode subtitle regex must compile"),
        ]
    })
}

fn compatible_episode_prefix(media_prefix: &str, subtitle_prefix: &str) -> bool {
    media_prefix.is_empty()
        || subtitle_prefix.is_empty()
        || media_prefix == subtitle_prefix
        || media_prefix.starts_with(&format!("{subtitle_prefix} "))
        || subtitle_prefix.starts_with(&format!("{media_prefix} "))
}

fn normalize_episode_prefix(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .replace(['.', '_', '-', '[', ']', '(', ')'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn infer_subtitle_language(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let language = stem.rsplit('.').next()?;
    if language == stem {
        return None;
    }
    let language = language.to_ascii_lowercase();
    if let Some(normalized) = normalize_common_external_language(&language) {
        return Some(normalized.to_string());
    }
    (2..=8).contains(&language.len()).then_some(language)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::entities::{
        libraries::{self, Entity as Libraries},
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn sidecar_match_allows_short_episode_subtitle_for_long_video_version() {
        assert!(is_sidecar_for_media(
            Path::new("绝命毒师.2008.S01E01.chs.ass"),
            "绝命毒师.2008.S01E01.第1集.1080p.BluRay.Remux.SDR.H.264"
        ));
        assert!(!is_sidecar_for_media(
            Path::new("绝命毒师.2008.S01E02.chs.ass"),
            "绝命毒师.2008.S01E01.第1集.1080p.BluRay.Remux.SDR.H.264"
        ));
    }

    #[test]
    fn jellyfin_subtitle_extensions_are_recognized() {
        for file in ["movie.mks", "movie.sup", "movie.sami"] {
            assert!(is_subtitle_path(Path::new(file)), "{file}");
        }
    }

    #[test]
    fn external_audio_name_flags_follow_jellyfin_parser() {
        let metadata =
            external_audio_metadata("Movie.eng.commentary.default.forced.sdh", "Movie").unwrap();
        assert_eq!(metadata.language.as_deref(), Some("en"));
        assert_eq!(metadata.title.as_deref(), Some("commentary"));
        assert!(metadata.is_default);
        assert!(metadata.is_forced);
        assert!(metadata.is_hearing_impaired);

        assert!(external_audio_metadata("Movie.flac", "Movie").is_some());
        assert!(external_audio_metadata("MovieExtended.eng", "Movie").is_none());
    }

    #[test]
    fn sidecar_subtitle_language_reuses_common_aliases() {
        assert_eq!(
            infer_subtitle_language(Path::new("Movie.jpn.ass")).as_deref(),
            Some("ja")
        );
        assert_eq!(
            infer_subtitle_language(Path::new("Movie.cht.ass")).as_deref(),
            Some("zh-TW")
        );
        assert_eq!(
            infer_subtitle_language(Path::new("Movie.rus.ass")).as_deref(),
            Some("rus")
        );
    }

    #[tokio::test]
    async fn external_audio_streams_are_stable_after_embedded_streams() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        Libraries::insert(libraries::ActiveModel {
            id: Set("external-audio-lib".to_string()),
            name: Set("Movies".to_string()),
            collection_type: Set("movies".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        MediaItems::insert(media_items::ActiveModel {
            id: Set("external-audio-movie".to_string()),
            title: Set("Movie".to_string()),
            path: Set("/tmp/external-audio-movie.mkv".to_string()),
            library_id: Set("external-audio-lib".to_string()),
            parent_id: Set("external-audio-lib".to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(0),
            is_public: Set(1),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set("external-audio-video".to_string()),
            item_id: Set("external-audio-movie".to_string()),
            stream_index: Set(0),
            stream_type: Set("Video".to_string()),
            codec: Set(Some("h264".to_string())),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        let streams = vec![(
            "/tmp/external-audio-movie.eng.flac".to_string(),
            crate::library::probe::ProbedStream {
                stream_index: 0,
                stream_type: "Audio".to_string(),
                codec: Some("flac".to_string()),
                language: Some("en".to_string()),
                ..Default::default()
            },
        )];

        for _ in 0..2 {
            crate::library::storage::replace_external_audio_streams(
                &db,
                "external-audio-movie",
                &streams,
            )
            .await
            .unwrap();
            let external = MediaStreams::find()
                .filter(media_streams::Column::ItemId.eq("external-audio-movie"))
                .filter(media_streams::Column::IsExternal.eq(1_i64))
                .one(&db)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(external.stream_index, 1);
            assert_eq!(external.stream_type, "Audio");
            assert_eq!(external.codec.as_deref(), Some("flac"));
            assert_eq!(external.language.as_deref(), Some("en"));
            assert_eq!(
                external.path.as_deref(),
                Some("/tmp/external-audio-movie.eng.flac")
            );
        }
    }

    #[tokio::test]
    async fn sidecar_scan_preserves_embedded_subtitles_and_audio_stream_indices() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        Libraries::insert(libraries::ActiveModel {
            id: Set("lib".to_string()),
            name: Set("Movies".to_string()),
            collection_type: Set("movies".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        MediaItems::insert(media_items::ActiveModel {
            id: Set("movie".to_string()),
            title: Set("Movie".to_string()),
            path: Set("/tmp/movie.mkv".to_string()),
            library_id: Set("lib".to_string()),
            parent_id: Set("lib".to_string()),
            item_type: Set("Movie".to_string()),
            is_folder: Set(0),
            is_public: Set(1),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        for (id, index, stream_type, codec, language, title, is_external) in [
            ("video", 0_i64, "Video", "h264", None, None, 0_i64),
            ("audio", 1_i64, "Audio", "aac", Some("eng"), None, 0_i64),
            (
                "embedded-sub",
                2_i64,
                "Subtitle",
                "ass",
                Some("chi"),
                Some("Embedded Chinese"),
                0_i64,
            ),
        ] {
            MediaStreams::insert(media_streams::ActiveModel {
                id: Set(id.to_string()),
                item_id: Set("movie".to_string()),
                stream_index: Set(index),
                stream_type: Set(stream_type.to_string()),
                codec: Set(Some(codec.to_string())),
                language: Set(language.map(ToString::to_string)),
                title: Set(title.map(ToString::to_string)),
                is_external: Set(is_external),
                created_at: Set(1),
                ..Default::default()
            })
            .exec_without_returning(&db)
            .await
            .unwrap();
        }
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set("bad-old-external-subtitle".to_string()),
            item_id: Set("movie".to_string()),
            stream_index: Set(7),
            stream_type: Set("Audio".to_string()),
            codec: Set(Some("srt".to_string())),
            language: Set(Some("eng".to_string())),
            title: Set(Some("Old bad subtitle".to_string())),
            path: Set(Some("/tmp/movie.old.srt".to_string())),
            is_external: Set(1),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        let dir = std::env::temp_dir().join(format!(
            "jellyfin-rs-subtitle-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        let media_path = dir.join("movie.mkv");
        let subtitle_path = dir.join("movie.zh.srt");
        fs::write(&subtitle_path, "1\n00:00:01,000 --> 00:00:02,000\nhello\n").unwrap();

        upsert_sidecar_subtitles(&db, &media_path, "movie")
            .await
            .unwrap();

        let rows = MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq("movie"))
            .order_by_asc(media_streams::Column::StreamIndex)
            .all(&db)
            .await
            .unwrap();
        let streams = rows
            .iter()
            .map(|row| {
                (
                    row.stream_index,
                    row.stream_type.clone(),
                    row.codec.clone(),
                    row.language.clone(),
                    row.title.clone(),
                    row.is_external,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(streams.len(), 4);
        assert_eq!(streams[1].1, "Audio");
        assert_eq!(streams[1].5, 0);
        assert_eq!(streams[2].1, "Subtitle");
        assert_eq!(streams[2].4.as_deref(), Some("Embedded Chinese"));
        assert_eq!(streams[2].5, 0);
        assert_eq!(streams[3].0, 3);
        assert_eq!(streams[3].1, "Subtitle");
        assert_eq!(streams[3].3.as_deref(), Some("zh-CN"));
        assert_eq!(streams[3].4.as_deref(), Some("movie.zh.srt"));
        assert_eq!(streams[3].5, 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
