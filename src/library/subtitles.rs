use std::path::Path;

use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::util::{now_unix, stable_text_id};

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

    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle'",
        vec![item_id.into()],
    ))
    .await
    .context("failed to clear existing subtitle streams")?;
    let mut subtitle_index = 1i64;
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
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, path, is_external, created_at) VALUES (?, ?, ?, 'Subtitle', ?, ?, ?, ?, 1, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET codec = excluded.codec, language = excluded.language, title = excluded.title, path = excluded.path, is_external = excluded.is_external"#,
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
                "srt" | "ass" | "ssa" | "vtt" | "sub"
            )
        })
        .unwrap_or_default()
}

fn is_sidecar_for_media(path: &Path, media_stem: &str) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem == media_stem || stem.starts_with(&format!("{media_stem}.")))
        .unwrap_or_default()
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
