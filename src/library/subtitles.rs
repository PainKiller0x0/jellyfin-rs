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

pub async fn upsert_sidecar_subtitles(
    db: &DatabaseConnection,
    media_path: &Path,
    item_id: &str,
) -> anyhow::Result<()> {
    let Some(parent) = media_path.parent() else {
        return Ok(());
    };
    let Some(media_stem) = media_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(());
    };
    clear_sidecar_subtitles(db, item_id).await?;
    let mut subtitle_index = next_external_subtitle_index(db, item_id).await?;
    for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_file() || !is_subtitle_path(&path) || !is_sidecar_for_media(&path, media_stem) {
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
        .filter(media_streams::Column::IsExternal.eq(1))
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
        "srt" | "ass" | "ssa" | "vtt" | "sub" | "smi" | "sami" | "mpl"
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
    let normalized = match language.as_str() {
        "chs" | "zh" | "zho" | "chi" | "cn" => "zh-CN",
        "cht" | "tc" | "zh-tw" => "zh-TW",
        "en" | "eng" => "en",
        "ja" | "jpn" => "ja",
        "ko" | "kor" => "ko",
        other if other.len() >= 2 && other.len() <= 8 => other,
        _ => return None,
    };
    Some(normalized.to_string())
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
