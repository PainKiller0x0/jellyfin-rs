use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
};

use anyhow::Context;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, PaginatorTrait, QueryFilter, Set, TransactionTrait, sea_query::OnConflict,
};

use crate::{
    entities::{
        image_assets::{self, Entity as ImageAssets},
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
    pub normalized_episode_metadata: usize,
    pub normalized_dimension_years: usize,
}

impl ProviderReconcileStats {
    pub fn changed(self) -> bool {
        self.merged_series > 0
            || self.merged_movies > 0
            || self.normalized_series_dates > 0
            || self.normalized_episode_metadata > 0
            || self.normalized_dimension_years > 0
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

    let tmdb_ids_by_item: HashMap<String, String> = provider_ids
        .iter()
        .map(|provider_id| {
            (
                provider_id.item_id.clone(),
                provider_id.provider_item_id.clone(),
            )
        })
        .collect();

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

    // A newly discovered cloud copy may not have a TMDb row yet.  Match it to
    // an already identified Series only when the normalized title maps to one
    // TMDb identity in the same library.  This covers folders such as
    // "Title" and "Title 第四季" without merging ambiguous same-name shows.
    let mut known_series_identities: HashMap<(String, String), HashSet<String>> = HashMap::new();
    for item in roots.values().filter(|item| item.item_type == "Series") {
        let Some(tmdb_id) = tmdb_ids_by_item.get(&item.id) else {
            continue;
        };
        let identity = normalize_series_identity(&item.title);
        if !identity.is_empty() {
            known_series_identities
                .entry((item.library_id.clone(), identity))
                .or_default()
                .insert(tmdb_id.clone());
        }
    }

    for item in roots.values().filter(|item| item.item_type == "Series") {
        if tmdb_ids_by_item.contains_key(&item.id) {
            continue;
        }
        let identity = normalize_series_identity(&item.title);
        let Some(tmdb_ids) = known_series_identities
            .get(&(item.library_id.clone(), identity))
            .filter(|ids| ids.len() == 1)
        else {
            continue;
        };
        let Some(tmdb_id) = tmdb_ids.iter().next() else {
            continue;
        };
        groups
            .entry((
                item.item_type.clone(),
                item.library_id.clone(),
                tmdb_id.clone(),
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
    stats.normalized_dimension_years = normalize_dimension_years(&txn).await?;
    stats.normalized_episode_metadata = normalize_episode_metadata(&txn).await?;
    txn.commit()
        .await
        .context("failed to commit provider reconciliation transaction")?;
    Ok(stats)
}

/// Clears production years that were parsed from a video dimension, such as
/// `1920X1040`, before the filename parser excluded resolution tokens.
async fn normalize_dimension_years(db: &DatabaseTransaction) -> anyhow::Result<usize> {
    let items = MediaItems::find()
        .filter(media_items::Column::ItemType.eq("Episode"))
        .all(db)
        .await
        .context("failed to load episodes for dimension year normalization")?;
    let mut changed = 0usize;

    for item in items {
        let Some(year) = item.production_year else {
            continue;
        };
        if !crate::library::metadata::is_video_dimension_year(
            std::path::Path::new(&item.path),
            year,
        ) {
            continue;
        }

        let mut active: media_items::ActiveModel = item.clone().into();
        active.production_year = Set(None);
        active.updated_at = Set(now_unix());
        active
            .update(db)
            .await
            .with_context(|| format!("failed to normalize dimension year: {}", item.id))?;
        changed += 1;
    }

    Ok(changed)
}

/// Repairs episode rows that were ingested before metadata was available or
/// before duplicate cloud roots were merged.  A cloud STRM filename can leave
/// the preferred episode represented as the literal container name (`mp4`,
/// `mkv`, ...), while an alternate version already has the real episode title.
/// Keep the preferred file for playback, but borrow safe descriptive metadata
/// from the alternate version when the preferred row is incomplete.
async fn normalize_episode_metadata(db: &DatabaseTransaction) -> anyhow::Result<usize> {
    let items = MediaItems::find()
        .all(db)
        .await
        .context("failed to load media items for episode metadata normalization")?;
    if items.is_empty() {
        return Ok(0);
    }

    let by_id: HashMap<&str, &media_items::Model> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut children_by_parent: HashMap<&str, Vec<&media_items::Model>> = HashMap::new();
    for item in &items {
        if !item.parent_id.is_empty() {
            children_by_parent
                .entry(item.parent_id.as_str())
                .or_default()
                .push(item);
        }
    }

    let provider_rows = ProviderIds::find()
        .all(db)
        .await
        .context("failed to load provider IDs for episode metadata normalization")?;
    let mut provider_ids_by_item: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for row in provider_rows {
        provider_ids_by_item
            .entry(row.item_id)
            .or_default()
            .push((row.provider, row.provider_item_id));
    }

    let mut changed = 0usize;
    for episode in items.iter().filter(|item| item.item_type == "Episode") {
        let Some(series) = ancestor_series(episode, &by_id) else {
            continue;
        };
        let alternate = children_by_parent
            .get(episode.id.as_str())
            .into_iter()
            .flatten()
            .filter(|item| item.item_type == "Video")
            .filter(|item| !is_generic_episode_title(&item.title))
            .max_by_key(|item| {
                (
                    item.overview.is_some(),
                    provider_ids_by_item.contains_key(item.id.as_str()),
                    item.title.chars().count(),
                )
            });

        let needs_series_name = episode
            .series_name
            .as_deref()
            .is_none_or(|value| value.trim().is_empty());
        let needs_title = is_generic_episode_title(&episode.title);
        let mut active: media_items::ActiveModel = episode.clone().into();
        let mut item_changed = false;

        if needs_series_name {
            active.series_name = Set(Some(series.title.clone()));
            item_changed = true;
        }

        if needs_title {
            if let Some(alternate) = alternate {
                active.title = Set(alternate.title.clone());
                item_changed = true;
                if episode.overview.is_none() && alternate.overview.is_some() {
                    active.overview = Set(alternate.overview.clone());
                }
                if episode.original_title.is_none() && alternate.original_title.is_some() {
                    active.original_title = Set(alternate.original_title.clone());
                }
                if episode.production_year.is_none() && alternate.production_year.is_some() {
                    active.production_year = Set(alternate.production_year);
                }
                if episode.premiere_date.is_none() && alternate.premiere_date.is_some() {
                    active.premiere_date = Set(alternate.premiere_date.clone());
                }
                if episode.photo_metadata.is_none() && alternate.photo_metadata.is_some() {
                    active.photo_metadata = Set(alternate.photo_metadata.clone());
                }
            }
        } else if episode.overview.is_none() {
            if let Some(alternate) = alternate {
                if alternate.overview.is_some() {
                    active.overview = Set(alternate.overview.clone());
                    item_changed = true;
                }
            }
        }

        if item_changed {
            active.updated_at = Set(now_unix());
            active
                .update(db)
                .await
                .with_context(|| format!("failed to normalize episode metadata: {}", episode.id))?;
            changed += 1;
        }

        if let Some(alternate) = alternate {
            if let Some(provider_ids) = provider_ids_by_item.get(alternate.id.as_str()) {
                let missing: Vec<_> = provider_ids
                    .iter()
                    .filter(|(provider, _)| {
                        !provider_ids_by_item
                            .get(episode.id.as_str())
                            .is_some_and(|existing| {
                                existing.iter().any(|(name, _)| name == provider)
                            })
                    })
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    ProviderIds::insert_many(missing.iter().map(|(provider, provider_item_id)| {
                        provider_ids::ActiveModel {
                            item_id: Set(episode.id.clone()),
                            provider: Set(provider.clone()),
                            provider_item_id: Set(provider_item_id.clone()),
                        }
                    }))
                    .exec_without_returning(db)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to copy alternate episode provider IDs: {}",
                            episode.id
                        )
                    })?;
                    changed += 1;
                }
            }
        }
    }

    Ok(changed)
}

fn ancestor_series<'a>(
    item: &'a media_items::Model,
    by_id: &HashMap<&str, &'a media_items::Model>,
) -> Option<&'a media_items::Model> {
    let mut parent_id = item.parent_id.as_str();
    let mut visited = HashSet::new();
    while !parent_id.is_empty() && visited.insert(parent_id) {
        let parent = by_id.get(parent_id).copied()?;
        if parent.item_type == "Series" {
            return Some(parent);
        }
        parent_id = parent.parent_id.as_str();
    }
    None
}

fn is_generic_episode_title(title: &str) -> bool {
    let normalized = title.trim().trim_start_matches('.').to_ascii_lowercase();
    normalized.is_empty()
        || matches!(
            normalized.as_str(),
            "mp4" | "mkv" | "avi" | "mov" | "flv" | "m4v" | "ts" | "webm" | "strm"
        )
}

fn contains_xunlei_marker(path: &str) -> bool {
    path.contains("迅雷") || path.to_ascii_lowercase().contains("xunlei")
}

fn source_fallback_key(path: &str) -> bool {
    !contains_xunlei_marker(path)
}

/// Returns the display priority for an item source. Xunlei is the preferred
/// source, while other cloud roots remain fallback versions.
pub(crate) fn source_priority_score(path: &str) -> u8 {
    u8::from(!source_fallback_key(path))
}

fn normalize_series_identity(value: &str) -> String {
    let value = value.trim();
    let value = strip_trailing_year_suffix(value);
    let value = strip_chinese_season_suffix(value);
    let value = strip_latin_season_suffix(value);
    value
        .chars()
        .map(normalize_series_character)
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_series_character(character: char) -> char {
    match character {
        // TMDb and cloud-folder naming commonly vary between the character
        // used in "菈菈" and its homophone "拉拉".
        '菈' => '拉',
        _ => character,
    }
}

fn strip_trailing_year_suffix(value: &str) -> &str {
    let Some((open, close)) = value.rfind('(').zip(value.strip_suffix(')')) else {
        return value;
    };
    let year = &close[open + '('.len_utf8()..];
    if year.len() == 4 && year.chars().all(|character| character.is_ascii_digit()) {
        return value[..open].trim_end();
    }
    value
}

fn strip_chinese_season_suffix(value: &str) -> &str {
    let Some(index) = value.rfind('第') else {
        return value;
    };
    let suffix = value[index..].trim();
    let Some(number) = suffix
        .strip_prefix('第')
        .and_then(|suffix| suffix.strip_suffix('季'))
    else {
        return value;
    };
    if !number.is_empty()
        && number.chars().all(|character| {
            character.is_ascii_digit() || "零〇一二三四五六七八九十百千万两".contains(character)
        })
    {
        return value[..index].trim_end();
    }
    value
}

fn strip_latin_season_suffix(value: &str) -> &str {
    let lowercase = value.to_ascii_lowercase();
    for marker in ["season", "s"] {
        let Some(index) = lowercase.rfind(marker) else {
            continue;
        };
        if index > 0
            && lowercase[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let number = lowercase[index + marker.len()..].trim();
        if !number.is_empty() && number.chars().all(|character| character.is_ascii_digit()) {
            return value[..index].trim_end_matches([' ', '-', '_']);
        }
    }
    value
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
    preserve_series_metadata(db, representative_id, duplicate_id).await?;

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

/// Keep metadata from an already identified duplicate when the preferred
/// source (currently Xunlei) was scanned first without provider metadata.
/// Without this, choosing the preferred source as representative would delete
/// the only TMDb-linked title, overview, and image assets together with the
/// duplicate row.
async fn preserve_series_metadata<C: ConnectionTrait>(
    db: &C,
    representative_id: &str,
    duplicate_id: &str,
) -> anyhow::Result<()> {
    let Some(representative) = MediaItems::find_by_id(representative_id.to_string())
        .one(db)
        .await?
    else {
        return Ok(());
    };
    let Some(duplicate) = MediaItems::find_by_id(duplicate_id.to_string())
        .one(db)
        .await?
    else {
        return Ok(());
    };

    let representative_tmdb = ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(representative_id))
        .filter(provider_ids::Column::Provider.eq("Tmdb"))
        .one(db)
        .await?;
    let duplicate_tmdb = ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(duplicate_id))
        .filter(provider_ids::Column::Provider.eq("Tmdb"))
        .one(db)
        .await?;
    if representative_tmdb.is_some() || duplicate_tmdb.is_none() {
        return Ok(());
    }

    let mut active: media_items::ActiveModel = representative.into();
    active.title = Set(duplicate.title);
    active.overview = Set(duplicate.overview);
    active.official_rating = Set(duplicate.official_rating);
    active.custom_rating = Set(duplicate.custom_rating);
    active.original_title = Set(duplicate.original_title);
    active.sort_name = Set(duplicate.sort_name);
    active.forced_sort_name = Set(duplicate.forced_sort_name);
    active.tagline = Set(duplicate.tagline);
    active.collection_name = Set(duplicate.collection_name);
    active.original_language = Set(duplicate.original_language);
    active.preferred_metadata_language = Set(duplicate.preferred_metadata_language);
    active.preferred_metadata_country_code = Set(duplicate.preferred_metadata_country_code);
    active.series_status = Set(duplicate.series_status);
    active.air_days = Set(duplicate.air_days);
    active.air_time = Set(duplicate.air_time);
    active.home_page_url = Set(duplicate.home_page_url);
    active.remote_trailers = Set(duplicate.remote_trailers);
    active.production_locations = Set(duplicate.production_locations);
    active.production_year = Set(duplicate.production_year);
    active.premiere_date = Set(duplicate.premiere_date);
    active.end_date = Set(duplicate.end_date);
    active.photo_metadata = Set(duplicate.photo_metadata);
    active.display_order = Set(duplicate.display_order);
    active.community_rating = Set(duplicate.community_rating);
    active.critic_rating = Set(duplicate.critic_rating);
    active.tmdb_metadata_version = Set(duplicate.tmdb_metadata_version);
    active.updated_at = Set(now_unix());
    active
        .update(db)
        .await
        .with_context(|| format!("failed to preserve series metadata: {representative_id}"))?;

    let existing_providers = ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(representative_id))
        .all(db)
        .await?;
    let existing_provider_names: HashSet<String> = existing_providers
        .into_iter()
        .map(|provider| provider.provider)
        .collect();
    let duplicate_providers = ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(duplicate_id))
        .all(db)
        .await?;
    let missing_providers: Vec<_> = duplicate_providers
        .into_iter()
        .filter(|provider| !existing_provider_names.contains(&provider.provider))
        .map(|provider| provider_ids::ActiveModel {
            item_id: Set(representative_id.to_string()),
            provider: Set(provider.provider),
            provider_item_id: Set(provider.provider_item_id),
        })
        .collect();
    if !missing_providers.is_empty() {
        ProviderIds::insert_many(missing_providers)
            .exec_without_returning(db)
            .await
            .with_context(|| {
                format!("failed to preserve series provider IDs: {representative_id}")
            })?;
    }

    let duplicate_images = ImageAssets::find()
        .filter(image_assets::Column::ItemId.eq(duplicate_id))
        .all(db)
        .await?;
    for image in duplicate_images {
        ImageAssets::insert(image_assets::ActiveModel {
            id: Set(crate::util::stable_text_id(&format!(
                "image-asset:{representative_id}:{}:{}",
                image.image_type, image.image_index
            ))),
            item_id: Set(representative_id.to_string()),
            image_type: Set(image.image_type),
            image_index: Set(image.image_index),
            path: Set(image.path),
            etag: Set(image.etag),
            width: Set(image.width),
            height: Set(image.height),
            size_bytes: Set(image.size_bytes),
            created_at: Set(image.created_at),
            updated_at: Set(now_unix()),
        })
        .on_conflict(
            OnConflict::columns([
                image_assets::Column::ItemId,
                image_assets::Column::ImageType,
                image_assets::Column::ImageIndex,
            ])
            .update_columns([
                image_assets::Column::Path,
                image_assets::Column::Etag,
                image_assets::Column::Width,
                image_assets::Column::Height,
                image_assets::Column::SizeBytes,
                image_assets::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to preserve series image assets: {representative_id}"))?;
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
    use super::{normalize_series_identity, source_fallback_key, source_priority_score};

    #[test]
    fn xunlei_is_primary_and_quark_is_fallback() {
        assert!(!source_fallback_key("/media/迅雷-番/幼女战记 (2017)"));
        assert!(source_fallback_key("/media/番/幼女战记 (2017)"));
        assert_eq!(source_priority_score("/media/迅雷-番/幼女战记 (2017)"), 1);
        assert_eq!(source_priority_score("/media/番/幼女战记 (2017)"), 0);
    }

    #[test]
    fn season_suffixes_share_one_series_identity() {
        assert_eq!(
            normalize_series_identity("关于我转生变成史莱姆这档事"),
            normalize_series_identity("关于我转生变成史莱姆这档事 第四季")
        );
        assert_eq!(
            normalize_series_identity("Show (2026)"),
            normalize_series_identity("Show Season 4")
        );
        assert_eq!(
            normalize_series_identity("再见菈菈"),
            normalize_series_identity("再见，拉拉")
        );
    }
}
