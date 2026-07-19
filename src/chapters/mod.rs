use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::{Deserialize, Serialize};

use crate::db::helpers::pg_statement;
use crate::db::row_ext::QueryResultExt;
use crate::util::{now_unix, stable_item_id};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterInfo {
    pub id: String,
    pub item_id: String,
    pub start_position_ticks: i64,
    pub name: String,
    pub marker_type: Option<String>,
    pub source: String,
}

/// Get all chapters for an item, ordered by start_position_ticks.
pub async fn get_chapters(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Vec<ChapterInfo>> {
    let rows = db
        .query_all(pg_statement(
            "SELECT id, item_id, start_position_ticks, name, marker_type, source FROM chapters WHERE item_id = ? ORDER BY start_position_ticks ASC",
            vec![item_id.into()],
        ))
        .await?;

    Ok(rows
        .iter()
        .map(|row| ChapterInfo {
            id: row.get_str("id").unwrap_or_default(),
            item_id: row.get_str("item_id").unwrap_or_default(),
            start_position_ticks: row.get_i64("start_position_ticks").unwrap_or(0),
            name: row.get_str("name").unwrap_or_default(),
            marker_type: row.get_str("marker_type").ok(),
            source: row
                .get_str("source")
                .unwrap_or_else(|_| "manual".to_string()),
        })
        .collect())
}

/// Save chapters for an item. Deletes existing chapters and inserts new ones.
pub async fn save_chapters(
    db: &DatabaseConnection,
    item_id: &str,
    chapters: &[ChapterInfo],
) -> anyhow::Result<()> {
    // Delete existing chapters
    db.execute(pg_statement(
        "DELETE FROM chapters WHERE item_id = ?",
        vec![item_id.into()],
    ))
    .await?;

    let now = now_unix();
    for ch in chapters {
        let id = if ch.id.is_empty() {
            stable_item_id(std::path::Path::new(&format!(
                "{}:{}:{}",
                item_id, ch.start_position_ticks, ch.name
            )))
        } else {
            ch.id.clone()
        };
        db.execute(pg_statement(
            "INSERT INTO chapters (id, item_id, start_position_ticks, name, marker_type, source, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                id.into(),
                item_id.into(),
                ch.start_position_ticks.into(),
                ch.name.clone().into(),
                ch.marker_type.clone().into(),
                ch.source.clone().into(),
                now.into(),
                now.into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

/// Clear all intro/credits markers for an item.
#[allow(dead_code)]
pub async fn clear_intro_credits_markers(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<()> {
    db.execute(pg_statement(
        "DELETE FROM chapters WHERE item_id = ? AND marker_type IS NOT NULL",
        vec![item_id.into()],
    ))
    .await?;
    Ok(())
}

/// Get intro markers (start, end) for an item.
pub async fn get_intro_markers(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<(i64, i64)>> {
    let start_row = db.query_one(pg_statement(
        "SELECT start_position_ticks FROM chapters WHERE item_id = ? AND marker_type = 'IntroStart' LIMIT 1",
        vec![item_id.into()],
    )).await?;
    let end_row = db.query_one(pg_statement(
        "SELECT start_position_ticks FROM chapters WHERE item_id = ? AND marker_type = 'IntroEnd' LIMIT 1",
        vec![item_id.into()],
    )).await?;

    match (start_row, end_row) {
        (Some(s), Some(e)) => {
            let start = s.get_i64("start_position_ticks").unwrap_or(0);
            let end = e.get_i64("start_position_ticks").unwrap_or(0);
            if start < end {
                Ok(Some((start, end)))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// Update intro markers for an episode and propagate to siblings in the same season.
#[allow(dead_code)]
pub async fn update_intro_for_season(
    db: &DatabaseConnection,
    episode_id: &str,
    intro_start: i64,
    intro_end: i64,
    source: &str,
) -> anyhow::Result<()> {
    if intro_start >= intro_end {
        return Ok(());
    }

    // Find all episodes in the same season
    let season_id = db
        .query_one(pg_statement(
            "SELECT parent_id FROM media_items WHERE id = ?",
            vec![episode_id.into()],
        ))
        .await?
        .and_then(|r| r.get_str("parent_id").ok());

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = db
        .query_all(pg_statement(
            "SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Episode' ORDER BY episode_number ASC",
            vec![season_id.into()],
        ))
        .await?;

    let now = now_unix();
    for ep_row in &episodes {
        let ep_id = ep_row.get_str("id").unwrap_or_default();

        // Check if this episode already has markers with a non-behavior source
        let existing = get_intro_markers(db, &ep_id).await?;
        if ep_id != episode_id {
            if let Some((_s, _e)) = existing {
                // Skip if it already has markers (from any source)
                continue;
            }
        }

        // Remove existing intro markers
        db.execute(pg_statement(
            "DELETE FROM chapters WHERE item_id = ? AND marker_type IN ('IntroStart', 'IntroEnd')",
            vec![ep_id.clone().into()],
        ))
        .await?;

        // Insert new markers
        let start_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroStart:{intro_start}"
        )));
        let end_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroEnd:{intro_end}"
        )));

        db.execute(pg_statement(
            "INSERT INTO chapters (id, item_id, start_position_ticks, name, marker_type, source, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                start_id.into(),
                ep_id.clone().into(),
                intro_start.into(),
                "IntroStart".into(),
                "IntroStart".into(),
                source.into(),
                now.into(),
                now.into(),
            ],
        ))
        .await?;

        db.execute(pg_statement(
            "INSERT INTO chapters (id, item_id, start_position_ticks, name, marker_type, source, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                end_id.into(),
                ep_id.into(),
                intro_end.into(),
                "IntroEnd".into(),
                "IntroEnd".into(),
                source.into(),
                now.into(),
                now.into(),
            ],
        ))
        .await?;
    }

    Ok(())
}

/// Update credits marker for an episode and propagate to siblings in the same season.
#[allow(dead_code)]
pub async fn update_credits_for_season(
    db: &DatabaseConnection,
    episode_id: &str,
    credits_start: i64,
    source: &str,
) -> anyhow::Result<()> {
    let season_id = db
        .query_one(pg_statement(
            "SELECT parent_id FROM media_items WHERE id = ?",
            vec![episode_id.into()],
        ))
        .await?
        .and_then(|r| r.get_str("parent_id").ok());

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = db
        .query_all(pg_statement(
            "SELECT id, runtime_ticks FROM media_items WHERE parent_id = ? AND item_type = 'Episode' ORDER BY episode_number ASC",
            vec![season_id.into()],
        ))
        .await?;

    // Calculate credits duration from the source episode
    let source_runtime = db
        .query_one(pg_statement(
            "SELECT runtime_ticks FROM media_items WHERE id = ?",
            vec![episode_id.into()],
        ))
        .await?
        .and_then(|r| r.get_i64("runtime_ticks").ok())
        .unwrap_or(0);

    let credits_duration = source_runtime - credits_start;
    if credits_duration <= 0 {
        return Ok(());
    }

    let now = now_unix();
    for ep_row in &episodes {
        let ep_id = ep_row.get_str("id").unwrap_or_default();
        let ep_runtime = ep_row.get_i64("runtime_ticks").unwrap_or(0);
        let ep_credits_start = ep_runtime - credits_duration;

        // Remove existing credits marker
        db.execute(pg_statement(
            "DELETE FROM chapters WHERE item_id = ? AND marker_type = 'CreditsStart'",
            vec![ep_id.clone().into()],
        ))
        .await?;

        let marker_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:CreditsStart:{ep_credits_start}"
        )));
        db.execute(pg_statement(
            "INSERT INTO chapters (id, item_id, start_position_ticks, name, marker_type, source, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                marker_id.into(),
                ep_id.into(),
                ep_credits_start.into(),
                "CreditsStart".into(),
                "CreditsStart".into(),
                source.into(),
                now.into(),
                now.into(),
            ],
        ))
        .await?;
    }

    Ok(())
}
