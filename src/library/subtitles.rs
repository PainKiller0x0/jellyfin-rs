use std::{path::Path, sync::OnceLock};

use anyhow::Context;
use regex::Regex;
use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::{
    db::row_ext::QueryResultExt,
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
    db.execute(crate::db::helpers::pg_statement(
        r#"DELETE FROM media_streams
           WHERE item_id = ?
             AND is_external = 1
             AND (
                 stream_type = 'Subtitle'
                 OR LOWER(path) LIKE '%.srt'
                 OR LOWER(path) LIKE '%.ass'
                 OR LOWER(path) LIKE '%.ssa'
                 OR LOWER(path) LIKE '%.vtt'
                 OR LOWER(path) LIKE '%.sub'
                 OR LOWER(path) LIKE '%.smi'
                 OR LOWER(path) LIKE '%.sami'
                 OR LOWER(path) LIKE '%.mpl'
             )"#,
        vec![item_id.into()],
    ))
    .await
    .context("failed to clear existing subtitle streams")?;
    Ok(())
}

async fn next_external_subtitle_index(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<i64> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT COALESCE(MAX(stream_index), -1) + 1 AS next_index FROM media_streams WHERE item_id = ?",
            vec![item_id.into()],
        ))
        .await
        .context("failed to find next subtitle stream index")?;
    Ok(row
        .as_ref()
        .map(|row| row.get_i64("next_index"))
        .transpose()?
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
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, path, is_external, created_at) VALUES (?, ?, ?, 'Subtitle', ?, ?, ?, ?, 1, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = 'Subtitle', codec = excluded.codec, language = excluded.language, title = excluded.title, path = excluded.path, is_external = excluded.is_external"#,
        vec![
            stable_text_id(&format!("subtitle:{item_id}:{path_string}")).into(),
            item_id.into(),
            stream_index.into(),
            codec.into(),
            infer_subtitle_language(path).into(),
            title.into(),
            path_string.into(),
            now_unix().into(),
        ],
    ))
    .await
    .with_context(|| format!("failed to upsert subtitle stream: {}", path.display()))?;
    Ok(())
}

fn is_subtitle_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "srt" | "ass" | "ssa" | "vtt" | "sub" | "smi" | "sami" | "mpl"
            )
        })
        .unwrap_or_default()
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

    use sea_orm::ConnectionTrait;
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

        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES ('lib', 'Movies', 'movies', 1, 1)",
            vec![],
        ))
        .await
        .unwrap();
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, is_public, modified_at, created_at, updated_at) VALUES ('movie', 'Movie', '/tmp/movie.mkv', 'lib', 'lib', 'Movie', 0, 1, 1, 1, 1)",
            vec![],
        ))
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
            db.execute(crate::db::helpers::pg_statement(
                "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, is_external, created_at) VALUES (?, 'movie', ?, ?, ?, ?, ?, ?, 1)",
                vec![
                    id.into(),
                    index.into(),
                    stream_type.into(),
                    codec.into(),
                    language.into(),
                    title.into(),
                    is_external.into(),
                ],
            ))
            .await
            .unwrap();
        }
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, path, is_external, created_at) VALUES ('bad-old-external-subtitle', 'movie', 7, 'Audio', 'srt', 'eng', 'Old bad subtitle', '/tmp/movie.old.srt', 1, 1)",
            vec![],
        ))
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

        let rows = db
            .query_all(crate::db::helpers::pg_statement(
                "SELECT stream_index, stream_type, codec, language, title, is_external FROM media_streams WHERE item_id = 'movie' ORDER BY stream_index",
                vec![],
            ))
            .await
            .unwrap();
        let streams = rows
            .iter()
            .map(|row| {
                (
                    row.get_i64("stream_index").unwrap(),
                    row.get_str("stream_type").unwrap(),
                    row.get_str("codec").unwrap(),
                    row.get_opt_str("language").unwrap(),
                    row.get_opt_str("title").unwrap(),
                    row.get_i64("is_external").unwrap(),
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
