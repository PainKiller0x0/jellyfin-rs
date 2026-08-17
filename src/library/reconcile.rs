use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use anyhow::Context;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, Set, TransactionTrait,
};

use crate::{
    entities::{
        media_items::{self, Entity as MediaItems},
        provider_ids::{self, Entity as ProviderIds},
    },
    util::now_unix,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct ProviderReconcileStats {
    pub merged_series: usize,
    pub merged_movies: usize,
    pub merged_versions: usize,
    pub normalized_series_dates: usize,
}

impl ProviderReconcileStats {
    pub fn changed(self) -> bool {
        self.merged_series > 0 || self.merged_movies > 0 || self.normalized_series_dates > 0
    }
}

/// Reconciles duplicate top-level media created by separate cloud roots.
///
/// Provider IDs are used only as a positive identity signal.  The operation is
/// scoped to a library and preserves every physical file as a child version:
/// movies become `Video` children of the representative movie, while matching
/// TV episodes become alternate `Video` children of the representative episode.
pub async fn reconcile_provider_duplicates(
    db: &DatabaseConnection,
) -> anyhow::Result<ProviderReconcileStats> {
    let all_items = MediaItems::find()
        .all(db)
        .await
        .context("failed to load media items for provider reconciliation")?;
    if all_items.is_empty() {
        return Ok(ProviderReconcileStats::default());
    }

    let roots: HashMap<String, media_items::Model> = all_items
        .iter()
        .filter(|item| matches!(item.item_type.as_str(), "Movie" | "Series"))
        .map(|item| (item.id.clone(), item.clone()))
        .collect();
    if roots.is_empty() {
        return Ok(ProviderReconcileStats::default());
    }

    let provider_ids = ProviderIds::find()
        .filter(provider_ids::Column::Provider.eq("Tmdb"))
        .all(db)
        .await
        .context("failed to load TMDb IDs for provider reconciliation")?;

    let mut groups: HashMap<(String, String, String), Vec<String>> = HashMap::new();
    for provider_id in provider_ids {
        let Some(item) = roots.get(&provider_id.item_id) else {
            continue;
        };
        groups
            .entry((
                item.item_type.clone(),
                item.library_id.clone(),
                provider_id.provider_item_id,
            ))
            .or_default()
            .push(item.id.clone());
    }

    let mut children_by_parent: HashMap<String, Vec<String>> = HashMap::new();
    for item in &all_items {
        if !item.parent_id.is_empty() {
            children_by_parent
                .entry(item.parent_id.clone())
                .or_default()
                .push(item.id.clone());
        }
    }
    let mut descendant_counts = HashMap::new();
    for item in roots.values() {
        let count = descendant_count(&item.id, &children_by_parent, &mut descendant_counts);
        descendant_counts.insert(item.id.clone(), count);
    }

    let txn = db
        .begin()
        .await
        .context("failed to begin provider reconciliation transaction")?;
    let mut stats = ProviderReconcileStats::default();
    let mut absorbed = HashSet::new();

    for ((item_type, _library_id, _tmdb_id), ids) in groups {
        let candidates: Vec<&media_items::Model> = ids
            .iter()
            .filter_map(|id| roots.get(id))
            .filter(|item| !absorbed.contains(&item.id))
            .collect();
        if candidates.len() < 2 {
            continue;
        }

        let representative = if item_type == "Series" {
            candidates
                .iter()
                .min_by_key(|item| {
                    (
                        // Xunlei is the preferred source whenever the same
                        // TMDb title exists in both cloud libraries.  Keep
                        // this before completeness so a partial Xunlei
                        // release still becomes the primary item and the
                        // Quark copy remains an alternate version.
                        source_fallback_key(&item.path),
                        Reverse(descendant_counts.get(&item.id).copied().unwrap_or_default()),
                        item.created_at,
                        item.path.len(),
                        item.id.clone(),
                    )
                })
                .expect("candidate list is non-empty")
        } else {
            candidates
                .iter()
                .min_by_key(|item| {
                    (
                        // Keep the same source policy for movies as for
                        // series: Xunlei primary, Quark as the alternate.
                        source_fallback_key(&item.path),
                        item.title.chars().count(),
                        item.created_at,
                        item.path.len(),
                        item.id.clone(),
                    )
                })
                .expect("candidate list is non-empty")
        };

        for duplicate in &candidates {
            if duplicate.id == representative.id {
                continue;
            }
            if item_type == "Series" {
                merge_series(&txn, &representative.id, &duplicate.id).await?;
                stats.merged_series += 1;
            } else {
                move_item(
                    &txn,
                    &duplicate.id,
                    &representative.id,
                    Some("Video"),
                    Some(0),
                )
                .await?;
                stats.merged_movies += 1;
                stats.merged_versions += 1;
            }
            absorbed.insert(duplicate.id.clone());
        }
    }

    stats.normalized_series_dates =
        normalize_series_date_ranges(&txn, &all_items, &children_by_parent).await?;
    txn.commit()
        .await
        .context("failed to commit provider reconciliation transaction")?;
    Ok(stats)
}

fn contains_xunlei_marker(path: &str) -> bool {
    path.contains("迅雷") || path.to_ascii_lowercase().contains("xunlei")
}

fn source_fallback_key(path: &str) -> bool {
    !contains_xunlei_marker(path)
}

fn descendant_count(
    item_id: &str,
    children_by_parent: &HashMap<String, Vec<String>>,
    memo: &mut HashMap<String, usize>,
) -> usize {
    if let Some(count) = memo.get(item_id) {
        return *count;
    }
    let count = children_by_parent
        .get(item_id)
        .into_iter()
        .flatten()
        .map(|child_id| 1 + descendant_count(child_id, children_by_parent, memo))
        .sum();
    memo.insert(item_id.to_string(), count);
    count
}

async fn merge_series<C: ConnectionTrait>(
    db: &C,
    representative_id: &str,
    duplicate_id: &str,
) -> anyhow::Result<()> {
    let duplicate_seasons = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(duplicate_id))
        .filter(media_items::Column::ItemType.eq("Season"))
        .all(db)
        .await
        .with_context(|| format!("failed to load duplicate series seasons: {duplicate_id}"))?;
    let representative_seasons = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(representative_id))
        .filter(media_items::Column::ItemType.eq("Season"))
        .all(db)
        .await
        .with_context(|| format!("failed to load representative seasons: {representative_id}"))?;

    for duplicate_season in duplicate_seasons {
        let target_season = representative_seasons
            .iter()
            .find(|season| season.season_number == duplicate_season.season_number);
        let Some(target_season) = target_season else {
            move_item(db, &duplicate_season.id, representative_id, None, None).await?;
            continue;
        };

        let target_episodes = MediaItems::find()
            .filter(media_items::Column::ParentId.eq(&target_season.id))
            .filter(media_items::Column::ItemType.eq("Episode"))
            .all(db)
            .await
            .with_context(|| {
                format!(
                    "failed to load target season episodes: {}",
                    target_season.id
                )
            })?;
        let duplicate_children = MediaItems::find()
            .filter(media_items::Column::ParentId.eq(&duplicate_season.id))
            .all(db)
            .await
            .with_context(|| {
                format!(
                    "failed to load duplicate season children: {}",
                    duplicate_season.id
                )
            })?;

        for child in duplicate_children {
            let target_episode = if child.item_type == "Episode" {
                target_episodes.iter().find(|episode| {
                    episode.season_number == child.season_number
                        && episode.episode_number == child.episode_number
                })
            } else {
                None
            };
            if let Some(target_episode) = target_episode {
                move_item(db, &child.id, &target_episode.id, Some("Video"), Some(0)).await?;
            } else {
                move_item(db, &child.id, &target_season.id, None, None).await?;
            }
        }

        if MediaItems::find()
            .filter(media_items::Column::ParentId.eq(&duplicate_season.id))
            .count(db)
            .await?
            == 0
        {
            MediaItems::delete_by_id(duplicate_season.id)
                .exec(db)
                .await
                .context("failed to remove empty duplicate season")?;
        }
    }

    let direct_children = MediaItems::find()
        .filter(media_items::Column::ParentId.eq(duplicate_id))
        .all(db)
        .await
        .with_context(|| format!("failed to load duplicate series children: {duplicate_id}"))?;
    for child in direct_children {
        move_item(db, &child.id, representative_id, None, None).await?;
    }

    if MediaItems::find()
        .filter(media_items::Column::ParentId.eq(duplicate_id))
        .count(db)
        .await?
        == 0
    {
        MediaItems::delete_by_id(duplicate_id.to_string())
            .exec(db)
            .await
            .with_context(|| format!("failed to remove duplicate series: {duplicate_id}"))?;
    }
    Ok(())
}

async fn move_item<C: ConnectionTrait>(
    db: &C,
    item_id: &str,
    parent_id: &str,
    item_type: Option<&str>,
    is_folder: Option<i64>,
) -> anyhow::Result<()> {
    let Some(item) = MediaItems::find_by_id(item_id.to_string()).one(db).await? else {
        return Ok(());
    };
    let mut active: media_items::ActiveModel = item.into();
    active.parent_id = Set(parent_id.to_string());
    if let Some(item_type) = item_type {
        active.item_type = Set(item_type.to_string());
    }
    if let Some(is_folder) = is_folder {
        active.is_folder = Set(is_folder);
    }
    active.updated_at = Set(now_unix());
    active
        .update(db)
        .await
        .with_context(|| format!("failed to move media item {item_id}"))?;
    Ok(())
}

async fn normalize_series_date_ranges<C: ConnectionTrait>(
    db: &C,
    all_items: &[media_items::Model],
    children_by_parent: &HashMap<String, Vec<String>>,
) -> anyhow::Result<usize> {
    let by_id: HashMap<&str, &media_items::Model> = all_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut changed = 0;
    for series in all_items.iter().filter(|item| item.item_type == "Series") {
        let Some(series_year) = series.production_year else {
            continue;
        };
        let Some(end_date) = series.end_date.as_deref() else {
            continue;
        };
        let Some(end_year) = parse_year(end_date) else {
            continue;
        };
        if end_year <= series_year {
            continue;
        }

        let mut descendants = Vec::new();
        collect_descendants(&series.id, children_by_parent, &mut descendants);
        let episode_years: Vec<i64> = descendants
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .filter(|item| item.item_type == "Episode")
            .filter_map(|item| {
                item.premiere_date
                    .as_deref()
                    .and_then(parse_year)
                    .or(item.production_year)
            })
            .collect();
        if episode_years.is_empty() || episode_years.iter().any(|year| *year > series_year) {
            continue;
        }

        let Some(item) = MediaItems::find_by_id(series.id.clone()).one(db).await? else {
            continue;
        };
        let mut active: media_items::ActiveModel = item.into();
        active.end_date = Set(None);
        active.updated_at = Set(now_unix());
        active
            .update(db)
            .await
            .with_context(|| format!("failed to normalize series date range: {}", series.id))?;
        changed += 1;
    }
    Ok(changed)
}

fn collect_descendants(
    item_id: &str,
    children_by_parent: &HashMap<String, Vec<String>>,
    output: &mut Vec<String>,
) {
    for child_id in children_by_parent.get(item_id).into_iter().flatten() {
        output.push(child_id.clone());
        collect_descendants(child_id, children_by_parent, output);
    }
}

fn parse_year(value: &str) -> Option<i64> {
    value.get(..4)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::source_fallback_key;

    #[test]
    fn xunlei_is_primary_and_quark_is_fallback() {
        assert!(!source_fallback_key("/media/迅雷-番/幼女战记 (2017)"));
        assert!(source_fallback_key("/media/番/幼女战记 (2017)"));
    }
}
