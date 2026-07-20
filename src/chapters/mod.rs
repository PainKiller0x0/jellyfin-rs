use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use crate::entities::{
    chapters::{self, Entity as Chapters},
    media_items::{self, Entity as MediaItems},
};
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
    let chapters = Chapters::find()
        .filter(chapters::Column::ItemId.eq(item_id))
        .order_by_asc(chapters::Column::StartPositionTicks)
        .all(db)
        .await?;

    Ok(chapters.into_iter().map(ChapterInfo::from).collect())
}

/// Save chapters for an item. Deletes existing chapters and inserts new ones.
pub async fn save_chapters(
    db: &DatabaseConnection,
    item_id: &str,
    chapters: &[ChapterInfo],
) -> anyhow::Result<()> {
    Chapters::delete_many()
        .filter(chapters::Column::ItemId.eq(item_id))
        .exec(db)
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
        Chapters::insert(chapter_active_model(
            id,
            item_id.to_string(),
            ch.start_position_ticks,
            ch.name.clone(),
            ch.marker_type.clone(),
            ch.source.clone(),
            now,
        ))
        .exec_without_returning(db)
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
    Chapters::delete_many()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::MarkerType.is_not_null())
        .exec(db)
        .await?;
    Ok(())
}

/// Get intro markers (start, end) for an item.
pub async fn get_intro_markers(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<(i64, i64)>> {
    let markers = Chapters::find()
        .filter(chapters::Column::ItemId.eq(item_id))
        .filter(chapters::Column::MarkerType.is_in(["IntroStart", "IntroEnd"]))
        .all(db)
        .await?;

    let start = markers
        .iter()
        .find(|marker| marker.marker_type.as_deref() == Some("IntroStart"))
        .map(|marker| marker.start_position_ticks);
    let end = markers
        .iter()
        .find(|marker| marker.marker_type.as_deref() == Some("IntroEnd"))
        .map(|marker| marker.start_position_ticks);

    match (start, end) {
        (Some(start), Some(end)) if start < end => Ok(Some((start, end))),
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
    let season_id = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .map(|item| item.parent_id);

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(season_id))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .order_by_asc(media_items::Column::EpisodeNumber)
        .all(db)
        .await?;

    let now = now_unix();
    for episode in &episodes {
        let ep_id = episode.id.clone();

        // Check if this episode already has markers with a non-behavior source
        let existing = get_intro_markers(db, &ep_id).await?;
        if ep_id != episode_id {
            if let Some((_s, _e)) = existing {
                // Skip if it already has markers (from any source)
                continue;
            }
        }

        // Remove existing intro markers
        Chapters::delete_many()
            .filter(chapters::Column::ItemId.eq(&ep_id))
            .filter(chapters::Column::MarkerType.is_in(["IntroStart", "IntroEnd"]))
            .exec(db)
            .await?;

        // Insert new markers
        let start_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroStart:{intro_start}"
        )));
        let end_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:IntroEnd:{intro_end}"
        )));

        Chapters::insert_many([
            chapter_active_model(
                start_id,
                ep_id.clone(),
                intro_start,
                "IntroStart".to_string(),
                Some("IntroStart".to_string()),
                source.to_string(),
                now,
            ),
            chapter_active_model(
                end_id,
                ep_id,
                intro_end,
                "IntroEnd".to_string(),
                Some("IntroEnd".to_string()),
                source.to_string(),
                now,
            ),
        ])
        .exec_without_returning(db)
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
    let season_id = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .map(|item| item.parent_id);

    let Some(season_id) = season_id else {
        return Ok(());
    };

    let episodes = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(season_id))
        .filter(media_items::Column::ItemType.eq("Episode"))
        .order_by_asc(media_items::Column::EpisodeNumber)
        .all(db)
        .await?;

    // Calculate credits duration from the source episode
    let source_runtime = MediaItems::find_by_id(episode_id)
        .one(db)
        .await?
        .and_then(|item| item.runtime_ticks)
        .unwrap_or(0);

    let credits_duration = source_runtime - credits_start;
    if credits_duration <= 0 {
        return Ok(());
    }

    let now = now_unix();
    for episode in &episodes {
        let ep_id = episode.id.clone();
        let ep_runtime = episode.runtime_ticks.unwrap_or(0);
        let ep_credits_start = ep_runtime - credits_duration;

        // Remove existing credits marker
        Chapters::delete_many()
            .filter(chapters::Column::ItemId.eq(&ep_id))
            .filter(chapters::Column::MarkerType.eq("CreditsStart"))
            .exec(db)
            .await?;

        let marker_id = stable_item_id(std::path::Path::new(&format!(
            "{ep_id}:CreditsStart:{ep_credits_start}"
        )));
        Chapters::insert(chapter_active_model(
            marker_id,
            ep_id,
            ep_credits_start,
            "CreditsStart".to_string(),
            Some("CreditsStart".to_string()),
            source.to_string(),
            now,
        ))
        .exec_without_returning(db)
        .await?;
    }

    Ok(())
}

impl From<chapters::Model> for ChapterInfo {
    fn from(model: chapters::Model) -> Self {
        Self {
            id: model.id,
            item_id: model.item_id,
            start_position_ticks: model.start_position_ticks,
            name: model.name,
            marker_type: model.marker_type,
            source: model.source,
        }
    }
}

fn chapter_active_model(
    id: String,
    item_id: String,
    start_position_ticks: i64,
    name: String,
    marker_type: Option<String>,
    source: String,
    now: i64,
) -> chapters::ActiveModel {
    chapters::ActiveModel {
        id: Set(id),
        item_id: Set(item_id),
        start_position_ticks: Set(start_position_ticks),
        name: Set(name),
        marker_type: Set(marker_type),
        source: Set(source),
        created_at: Set(now),
        updated_at: Set(now),
    }
}
