use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Context, bail};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, sea_query::OnConflict,
};

use crate::{
    entities::{
        genres::{self, Entity as Genres},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        studios::{self, Entity as Studios},
        tags::{self, Entity as Tags},
        user_data::{self, Entity as UserData},
        users::Entity as Users,
    },
    library::{
        metadata::ParsedMetadata,
        probe::{MediaProbe, ProbedStream},
    },
    util::{now_unix, stable_text_id},
};

#[derive(Default)]
pub struct ScannedMediaItem {
    pub id: String,
    pub title: String,
    pub path: String,
    pub library_id: String,
    pub parent_id: String,
    pub item_type: String,
    pub extra_type: Option<String>,
    pub video_type: Option<String>,
    pub iso_type: Option<String>,
    pub video_3d_format: Option<String>,
    pub is_folder: bool,
    pub container: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
    pub custom_rating: Option<String>,
    pub extended_video_type: Option<String>,
    pub original_title: Option<String>,
    pub sort_name: Option<String>,
    pub forced_sort_name: Option<String>,
    pub lock_data: Option<bool>,
    pub locked_fields: Vec<String>,
    pub tagline: Option<String>,
    pub collection_name: Option<String>,
    pub original_language: Option<String>,
    pub preferred_metadata_language: Option<String>,
    pub preferred_metadata_country_code: Option<String>,
    pub series_status: Option<String>,
    pub air_days: Vec<String>,
    pub air_time: Option<String>,
    pub home_page_url: Option<String>,
    pub remote_trailers: Vec<String>,
    pub production_locations: Vec<String>,
    pub production_year: Option<i64>,
    pub premiere_date: Option<String>,
    pub end_date: Option<String>,
    pub runtime_ticks: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub has_subtitles: bool,
    pub photo_metadata: Option<String>,
    pub display_order: Option<String>,
    pub size_bytes: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub episode_number_end: Option<i64>,
    pub airs_before_episode_number: Option<i64>,
    pub airs_after_season_number: Option<i64>,
    pub airs_before_season_number: Option<i64>,
    pub series_name: Option<String>,
    pub community_rating: Option<f64>,
    pub critic_rating: Option<f64>,
    pub modified_at: i64,
    pub created_at: i64,
}

pub struct CachedMediaProbe {
    pub runtime_ticks: Option<i64>,
    pub size_bytes: Option<i64>,
    pub title: String,
    pub overview: Option<String>,
    pub forced_sort_name: Option<String>,
    pub collection_name: Option<String>,
    pub production_year: Option<i64>,
    pub premiere_date: Option<String>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub series_name: Option<String>,
}

pub(crate) const MEDIA_PROBE_FAILURE_STREAM_TYPE: &str = "ProbeFailure";
const MISSING_CLEANUP_BATCH_SIZE: u64 = 500;
const MISSING_CLEANUP_PAUSE: Duration = Duration::from_millis(2);
const MEDIA_PROBE_FAILURE_CACHE_SECONDS: i64 = 7 * 24 * 60 * 60;

impl ScannedMediaItem {
    pub fn from_stored(item: &media_items::Model) -> Self {
        let string_list = |value: Option<&str>| {
            value
                .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
                .unwrap_or_default()
        };
        let remote_trailers = item
            .remote_trailers
            .as_deref()
            .and_then(|value| serde_json::from_str::<Vec<serde_json::Value>>(value).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| {
                value
                    .get("Url")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            })
            .collect();
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            path: item.path.clone(),
            library_id: item.library_id.clone(),
            parent_id: item.parent_id.clone(),
            item_type: item.item_type.clone(),
            extra_type: item.extra_type.clone(),
            video_type: item.video_type.clone(),
            iso_type: item.iso_type.clone(),
            video_3d_format: item.video_3d_format.clone(),
            is_folder: item.is_folder != 0,
            container: item.container.clone(),
            overview: item.overview.clone(),
            official_rating: item.official_rating.clone(),
            custom_rating: item.custom_rating.clone(),
            extended_video_type: item.extended_video_type.clone(),
            original_title: item.original_title.clone(),
            sort_name: item.sort_name.clone(),
            forced_sort_name: item.forced_sort_name.clone(),
            lock_data: Some(item.lock_data != 0),
            locked_fields: string_list(item.locked_fields.as_deref()),
            tagline: item.tagline.clone(),
            collection_name: item.collection_name.clone(),
            original_language: item.original_language.clone(),
            preferred_metadata_language: item.preferred_metadata_language.clone(),
            preferred_metadata_country_code: item.preferred_metadata_country_code.clone(),
            series_status: item.series_status.clone(),
            air_days: string_list(item.air_days.as_deref()),
            air_time: item.air_time.clone(),
            home_page_url: item.home_page_url.clone(),
            remote_trailers,
            production_locations: string_list(item.production_locations.as_deref()),
            production_year: item.production_year,
            premiere_date: item.premiere_date.clone(),
            end_date: item.end_date.clone(),
            runtime_ticks: item.runtime_ticks,
            aspect_ratio: item.aspect_ratio.clone(),
            width: item.width,
            height: item.height,
            has_subtitles: item.has_subtitles != 0,
            photo_metadata: item.photo_metadata.clone(),
            display_order: item.display_order.clone(),
            size_bytes: item.size_bytes,
            season_number: item.season_number,
            episode_number: item.episode_number,
            episode_number_end: item.episode_number_end,
            airs_before_episode_number: item.airs_before_episode_number,
            airs_after_season_number: item.airs_after_season_number,
            airs_before_season_number: item.airs_before_season_number,
            series_name: item.series_name.clone(),
            community_rating: item.community_rating,
            critic_rating: item.critic_rating,
            modified_at: item.modified_at,
            created_at: item.created_at,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn folder_with_type(
        id: String,
        library_id: String,
        parent_id: String,
        path: String,
        title: String,
        item_type: &str,
        modified_at: i64,
        created_at: i64,
        production_year: Option<i64>,
    ) -> Self {
        Self {
            id,
            title,
            path,
            library_id,
            parent_id,
            item_type: item_type.to_string(),
            extra_type: None,
            video_type: None,
            iso_type: None,
            video_3d_format: None,
            is_folder: true,
            container: None,
            overview: None,
            official_rating: None,
            custom_rating: None,
            extended_video_type: None,
            original_title: None,
            sort_name: None,
            forced_sort_name: None,
            lock_data: None,
            locked_fields: Vec::new(),
            tagline: None,
            collection_name: None,
            original_language: None,
            preferred_metadata_language: None,
            preferred_metadata_country_code: None,
            series_status: None,
            air_days: Vec::new(),
            air_time: None,
            home_page_url: None,
            remote_trailers: Vec::new(),
            production_locations: Vec::new(),
            production_year,
            premiere_date: None,
            end_date: None,
            runtime_ticks: None,
            aspect_ratio: None,
            width: None,
            height: None,
            has_subtitles: false,
            photo_metadata: None,
            display_order: None,
            size_bytes: None,
            season_number: None,
            episode_number: None,
            episode_number_end: None,
            airs_before_episode_number: None,
            airs_after_season_number: None,
            airs_before_season_number: None,
            series_name: None,
            community_rating: None,
            critic_rating: None,
            modified_at,
            created_at,
        }
    }
}

pub async fn cached_media_probe_if_current(
    db: &DatabaseConnection,
    path: &str,
    modified_at: i64,
    size_bytes: Option<i64>,
    allow_size_mismatch: bool,
) -> anyhow::Result<Option<CachedMediaProbe>> {
    let mut item_query = MediaItems::find()
        .filter(media_items::Column::Path.eq(path))
        .filter(media_items::Column::ModifiedAt.eq(modified_at));
    if !allow_size_mismatch {
        item_query = match size_bytes {
            Some(size_bytes) => item_query.filter(media_items::Column::SizeBytes.eq(size_bytes)),
            None => item_query.filter(media_items::Column::SizeBytes.is_null()),
        };
    }

    let Some(item) = item_query
        .one(db)
        .await
        .with_context(|| format!("failed to check cached media probe: {path}"))?
    else {
        return Ok(None);
    };

    let has_failed_probe = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::StreamType.eq(MEDIA_PROBE_FAILURE_STREAM_TYPE))
        .filter(media_streams::Column::CreatedAt.gt(now_unix() - MEDIA_PROBE_FAILURE_CACHE_SECONDS))
        .one(db)
        .await
        .with_context(|| format!("failed to check cached media probe failure: {path}"))?
        .is_some();
    if has_failed_probe {
        return Ok(Some(CachedMediaProbe {
            runtime_ticks: item.runtime_ticks,
            size_bytes: item.size_bytes,
            title: item.title,
            overview: item.overview,
            forced_sort_name: item.forced_sort_name,
            collection_name: item.collection_name,
            production_year: item.production_year,
            premiere_date: item.premiere_date,
            index_number: item.episode_number,
            parent_index_number: item.season_number,
            series_name: item.series_name,
        }));
    }

    let has_probe_stream = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::IsExternal.eq(0_i64))
        .filter(media_streams::Column::StreamType.ne(MEDIA_PROBE_FAILURE_STREAM_TYPE))
        .filter(
            Condition::any()
                .add(media_streams::Column::Codec.is_not_null())
                .add(media_streams::Column::Profile.is_not_null())
                .add(media_streams::Column::BitRate.is_not_null())
                .add(media_streams::Column::Width.is_not_null())
                .add(media_streams::Column::Height.is_not_null())
                .add(media_streams::Column::PixelFormat.is_not_null())
                .add(media_streams::Column::AverageFrameRate.is_not_null())
                .add(media_streams::Column::AspectRatio.is_not_null())
                .add(media_streams::Column::Channels.is_not_null())
                .add(media_streams::Column::SampleRate.is_not_null())
                .add(media_streams::Column::ChannelLayout.is_not_null())
                .add(media_streams::Column::ColorTransfer.is_not_null())
                .add(media_streams::Column::BitDepth.is_not_null()),
        )
        .one(db)
        .await
        .with_context(|| format!("failed to check cached media stream probe: {path}"))?
        .is_some();

    if !has_probe_stream {
        return Ok(None);
    }

    Ok(Some(CachedMediaProbe {
        runtime_ticks: item.runtime_ticks,
        size_bytes: item.size_bytes,
        title: item.title,
        overview: item.overview,
        forced_sort_name: item.forced_sort_name,
        collection_name: item.collection_name,
        production_year: item.production_year,
        premiere_date: item.premiere_date,
        index_number: item.episode_number,
        parent_index_number: item.season_number,
        series_name: item.series_name,
    }))
}

pub async fn upsert_media_item(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<String> {
    if let Some(existing) = MediaItems::find()
        .filter(media_items::Column::Path.eq(&item.path))
        .one(db)
        .await
        .with_context(|| format!("failed to read existing media item: {}", item.path))?
    {
        let id = existing.id.clone();
        if media_item_scan_matches(&existing, item)? {
            return Ok(id);
        }
        let preserve_scraped_episode_title = existing.item_type == "Episode"
            && item.item_type == "Episode"
            && existing.tmdb_metadata_version > 0
            && existing.overview.is_some();
        let preserve_folder_title = existing.item_type == item.item_type
            && existing.is_folder != 0
            && (existing.overview.is_some() || existing.premiere_date.is_some());
        let preserve_existing_title = preserve_folder_title || preserve_scraped_episode_title;
        let preserve_production_year = existing.item_type == item.item_type
            && existing.premiere_date.is_some()
            && item.premiere_date.is_none();
        let metadata_language_changed =
            item.preferred_metadata_language
                .as_deref()
                .is_some_and(|language| {
                    existing.preferred_metadata_language.as_deref() != Some(language)
                });

        let mut active: media_items::ActiveModel = existing.clone().into();
        active.title = Set(if preserve_existing_title {
            existing.title
        } else {
            item.title.clone()
        });
        active.library_id = Set(item.library_id.clone());
        active.parent_id = Set(item.parent_id.clone());
        active.item_type = Set(item.item_type.clone());
        active.extra_type = Set(item.extra_type.clone());
        active.video_type = Set(item.video_type.clone());
        active.iso_type = Set(item.iso_type.clone());
        active.video_3d_format = Set(item.video_3d_format.clone());
        active.is_folder = Set(item.is_folder as i64);
        active.container = Set(item.container.clone());
        active.overview = Set(item.overview.clone().or(existing.overview));
        active.official_rating = Set(item.official_rating.clone().or(existing.official_rating));
        active.custom_rating = Set(item.custom_rating.clone().or(existing.custom_rating));
        active.extended_video_type = Set(item.extended_video_type.clone());
        active.original_title = Set(item.original_title.clone().or(existing.original_title));
        active.sort_name = Set(item.sort_name.clone().or(existing.sort_name));
        active.forced_sort_name = Set(item.forced_sort_name.clone().or(existing.forced_sort_name));
        active.lock_data = Set(item
            .lock_data
            .map(|value| value as i64)
            .unwrap_or(existing.lock_data));
        active.locked_fields =
            Set(locked_fields_to_storage(&item.locked_fields).or(existing.locked_fields));
        active.tagline = Set(item.tagline.clone().or(existing.tagline));
        active.collection_name = Set(item.collection_name.clone().or(existing.collection_name));
        active.original_language = Set(item
            .original_language
            .clone()
            .or(existing.original_language));
        active.preferred_metadata_language = Set(item
            .preferred_metadata_language
            .clone()
            .or(existing.preferred_metadata_language));
        active.preferred_metadata_country_code = Set(item
            .preferred_metadata_country_code
            .clone()
            .or(existing.preferred_metadata_country_code));
        active.series_status = Set(item.series_status.clone().or(existing.series_status));
        active.air_days = Set(string_vec_to_storage(&item.air_days).or(existing.air_days));
        active.air_time = Set(item.air_time.clone().or(existing.air_time));
        active.home_page_url = Set(item.home_page_url.clone().or(existing.home_page_url));
        active.remote_trailers =
            Set(remote_trailers_to_storage(&item.remote_trailers).or(existing.remote_trailers));
        active.production_locations = Set(
            string_vec_to_storage(&item.production_locations).or(existing.production_locations)
        );
        active.premiere_date = Set(item.premiere_date.clone().or(existing.premiere_date));
        active.end_date = Set(item.end_date.clone().or(existing.end_date));
        active.production_year = Set(if preserve_production_year {
            existing.production_year
        } else {
            item.production_year.or(existing.production_year)
        });
        active.runtime_ticks = Set(item.runtime_ticks.or(existing.runtime_ticks));
        active.aspect_ratio = Set(item.aspect_ratio.clone().or(existing.aspect_ratio));
        active.width = Set(item.width.or(existing.width));
        active.height = Set(item.height.or(existing.height));
        active.has_subtitles = Set(item.has_subtitles as i64);
        active.photo_metadata = Set(item.photo_metadata.clone().or(existing.photo_metadata));
        active.display_order = Set(item.display_order.clone().or(existing.display_order));
        active.size_bytes = Set(item.size_bytes);
        active.season_number = Set(item.season_number);
        active.episode_number = Set(item.episode_number);
        active.episode_number_end = Set(item.episode_number_end);
        active.airs_before_episode_number = Set(item.airs_before_episode_number);
        active.airs_after_season_number = Set(item.airs_after_season_number);
        active.airs_before_season_number = Set(item.airs_before_season_number);
        active.series_name = Set(item.series_name.clone().or(existing.series_name));
        active.community_rating = Set(item.community_rating.or(existing.community_rating));
        active.critic_rating = Set(item.critic_rating.or(existing.critic_rating));
        if metadata_language_changed {
            active.tmdb_metadata_version = Set(0);
        }
        active.modified_at = Set(item.modified_at);
        active.updated_at = Set(now_unix());
        active
            .update(db)
            .await
            .with_context(|| format!("failed to update media item: {}", item.path))?;
        return Ok(id);
    }

    MediaItems::insert(media_items::ActiveModel {
        id: Set(item.id.clone()),
        title: Set(item.title.clone()),
        path: Set(item.path.clone()),
        library_id: Set(item.library_id.clone()),
        parent_id: Set(item.parent_id.clone()),
        item_type: Set(item.item_type.clone()),
        extra_type: Set(item.extra_type.clone()),
        video_type: Set(item.video_type.clone()),
        iso_type: Set(item.iso_type.clone()),
        video_3d_format: Set(item.video_3d_format.clone()),
        is_folder: Set(item.is_folder as i64),
        is_public: Set(1),
        container: Set(item.container.clone()),
        overview: Set(item.overview.clone()),
        official_rating: Set(item.official_rating.clone()),
        custom_rating: Set(item.custom_rating.clone()),
        extended_video_type: Set(item.extended_video_type.clone()),
        original_title: Set(item.original_title.clone()),
        sort_name: Set(item.sort_name.clone()),
        forced_sort_name: Set(item.forced_sort_name.clone()),
        lock_data: Set(item.lock_data.unwrap_or_default() as i64),
        locked_fields: Set(locked_fields_to_storage(&item.locked_fields)),
        tagline: Set(item.tagline.clone()),
        collection_name: Set(item.collection_name.clone()),
        original_language: Set(item.original_language.clone()),
        preferred_metadata_language: Set(item.preferred_metadata_language.clone()),
        preferred_metadata_country_code: Set(item.preferred_metadata_country_code.clone()),
        series_status: Set(item.series_status.clone()),
        air_days: Set(string_vec_to_storage(&item.air_days)),
        air_time: Set(item.air_time.clone()),
        home_page_url: Set(item.home_page_url.clone()),
        remote_trailers: Set(remote_trailers_to_storage(&item.remote_trailers)),
        production_locations: Set(string_vec_to_storage(&item.production_locations)),
        production_year: Set(item.production_year),
        premiere_date: Set(item.premiere_date.clone()),
        end_date: Set(item.end_date.clone()),
        runtime_ticks: Set(item.runtime_ticks),
        aspect_ratio: Set(item.aspect_ratio.clone()),
        width: Set(item.width),
        height: Set(item.height),
        has_subtitles: Set(item.has_subtitles as i64),
        photo_metadata: Set(item.photo_metadata.clone()),
        display_order: Set(item.display_order.clone()),
        size_bytes: Set(item.size_bytes),
        season_number: Set(item.season_number),
        episode_number: Set(item.episode_number),
        episode_number_end: Set(item.episode_number_end),
        airs_before_episode_number: Set(item.airs_before_episode_number),
        airs_after_season_number: Set(item.airs_after_season_number),
        airs_before_season_number: Set(item.airs_before_season_number),
        series_name: Set(item.series_name.clone()),
        community_rating: Set(item.community_rating),
        critic_rating: Set(item.critic_rating),
        modified_at: Set(item.modified_at),
        created_at: Set(item.created_at),
        updated_at: Set(now_unix()),
        ..Default::default()
    })
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to insert media item: {}", item.path))?;
    Ok(item.id.clone())
}

fn media_item_scan_matches(
    existing: &media_items::Model,
    item: &ScannedMediaItem,
) -> anyhow::Result<bool> {
    let existing_is_folder = existing.is_folder != 0;
    let preserve_scraped_episode_title = existing.item_type == "Episode"
        && item.item_type == "Episode"
        && existing.tmdb_metadata_version > 0
        && existing.overview.is_some();
    let preserve_folder_title = existing.item_type == item.item_type
        && existing_is_folder
        && (existing.overview.is_some() || existing.premiere_date.is_some());
    let preserve_existing_title = preserve_folder_title || preserve_scraped_episode_title;

    let title_matches = preserve_existing_title || existing.title == item.title;
    let overview_matches = option_str_eq(
        existing.overview.as_deref(),
        item.overview.as_deref().or(existing.overview.as_deref()),
    );
    let official_rating_matches = option_str_eq(
        existing.official_rating.as_deref(),
        item.official_rating
            .as_deref()
            .or(existing.official_rating.as_deref()),
    );
    let target_custom_rating = item
        .custom_rating
        .as_deref()
        .or(existing.custom_rating.as_deref());
    let target_premiere_date = item
        .premiere_date
        .as_deref()
        .or(existing.premiere_date.as_deref());
    let target_end_date = item.end_date.as_deref().or(existing.end_date.as_deref());
    let preserve_production_year = existing.item_type == item.item_type
        && existing.premiere_date.is_some()
        && item.premiere_date.is_none();
    let target_production_year = if preserve_production_year {
        existing.production_year
    } else {
        item.production_year.or(existing.production_year)
    };
    let target_runtime_ticks = item.runtime_ticks.or(existing.runtime_ticks);
    let target_community_rating = item.community_rating.or(existing.community_rating);
    let target_critic_rating = item.critic_rating.or(existing.critic_rating);
    let target_original_title = item
        .original_title
        .as_deref()
        .or(existing.original_title.as_deref());
    let target_sort_name = item.sort_name.as_deref().or(existing.sort_name.as_deref());
    let target_forced_sort_name = item
        .forced_sort_name
        .as_deref()
        .or(existing.forced_sort_name.as_deref());
    let target_lock_data = item
        .lock_data
        .map(|value| value as i64)
        .unwrap_or(existing.lock_data);
    let target_locked_fields = locked_fields_to_storage(&item.locked_fields)
        .as_deref()
        .map(str::to_string)
        .or_else(|| existing.locked_fields.clone());
    let target_tagline = item.tagline.as_deref().or(existing.tagline.as_deref());
    let target_collection_name = item
        .collection_name
        .as_deref()
        .or(existing.collection_name.as_deref());
    let target_original_language = item
        .original_language
        .as_deref()
        .or(existing.original_language.as_deref());
    let target_preferred_metadata_language = item
        .preferred_metadata_language
        .as_deref()
        .or(existing.preferred_metadata_language.as_deref());
    let target_preferred_metadata_country_code = item
        .preferred_metadata_country_code
        .as_deref()
        .or(existing.preferred_metadata_country_code.as_deref());
    let target_series_status = item
        .series_status
        .as_deref()
        .or(existing.series_status.as_deref());
    let target_air_days =
        string_vec_to_storage(&item.air_days).or_else(|| existing.air_days.clone());
    let target_air_time = item.air_time.as_deref().or(existing.air_time.as_deref());
    let target_home_page_url = item
        .home_page_url
        .as_deref()
        .or(existing.home_page_url.as_deref());
    let target_remote_trailers = remote_trailers_to_storage(&item.remote_trailers)
        .or_else(|| existing.remote_trailers.clone());
    let target_production_locations = string_vec_to_storage(&item.production_locations)
        .as_deref()
        .map(str::to_string)
        .or_else(|| existing.production_locations.clone());
    let target_display_order = item
        .display_order
        .as_deref()
        .or(existing.display_order.as_deref());
    let target_aspect_ratio = item
        .aspect_ratio
        .as_deref()
        .or(existing.aspect_ratio.as_deref());
    let target_width = item.width.or(existing.width);
    let target_height = item.height.or(existing.height);
    let target_has_subtitles = item.has_subtitles;
    let target_photo_metadata = item
        .photo_metadata
        .as_deref()
        .or(existing.photo_metadata.as_deref());
    let target_series_name = item
        .series_name
        .as_deref()
        .or(existing.series_name.as_deref());

    Ok(title_matches
        && existing.library_id == item.library_id
        && existing.parent_id == item.parent_id
        && existing.item_type == item.item_type
        && existing.extra_type == item.extra_type
        && existing.video_type == item.video_type
        && existing.iso_type == item.iso_type
        && existing.video_3d_format == item.video_3d_format
        && existing_is_folder == item.is_folder
        && option_str_eq(existing.container.as_deref(), item.container.as_deref())
        && overview_matches
        && official_rating_matches
        && option_str_eq(existing.custom_rating.as_deref(), target_custom_rating)
        && option_str_eq(
            existing.extended_video_type.as_deref(),
            item.extended_video_type.as_deref(),
        )
        && option_str_eq(existing.original_title.as_deref(), target_original_title)
        && option_str_eq(existing.sort_name.as_deref(), target_sort_name)
        && option_str_eq(
            existing.forced_sort_name.as_deref(),
            target_forced_sort_name,
        )
        && existing.lock_data == target_lock_data
        && existing.locked_fields == target_locked_fields
        && option_str_eq(existing.tagline.as_deref(), target_tagline)
        && option_str_eq(existing.collection_name.as_deref(), target_collection_name)
        && option_str_eq(
            existing.original_language.as_deref(),
            target_original_language,
        )
        && option_str_eq(
            existing.preferred_metadata_language.as_deref(),
            target_preferred_metadata_language,
        )
        && option_str_eq(
            existing.preferred_metadata_country_code.as_deref(),
            target_preferred_metadata_country_code,
        )
        && option_str_eq(existing.series_status.as_deref(), target_series_status)
        && existing.air_days == target_air_days
        && option_str_eq(existing.air_time.as_deref(), target_air_time)
        && option_str_eq(existing.home_page_url.as_deref(), target_home_page_url)
        && existing.remote_trailers == target_remote_trailers
        && existing.production_locations == target_production_locations
        && option_str_eq(existing.premiere_date.as_deref(), target_premiere_date)
        && option_str_eq(existing.end_date.as_deref(), target_end_date)
        && existing.production_year == target_production_year
        && existing.runtime_ticks == target_runtime_ticks
        && option_str_eq(existing.aspect_ratio.as_deref(), target_aspect_ratio)
        && existing.width == target_width
        && existing.height == target_height
        && (existing.has_subtitles != 0) == target_has_subtitles
        && option_str_eq(existing.photo_metadata.as_deref(), target_photo_metadata)
        && option_str_eq(existing.display_order.as_deref(), target_display_order)
        && existing.size_bytes == item.size_bytes
        && existing.season_number == item.season_number
        && existing.episode_number == item.episode_number
        && existing.episode_number_end == item.episode_number_end
        && existing.airs_before_episode_number == item.airs_before_episode_number
        && existing.airs_after_season_number == item.airs_after_season_number
        && existing.airs_before_season_number == item.airs_before_season_number
        && option_str_eq(existing.series_name.as_deref(), target_series_name)
        && existing.community_rating == target_community_rating
        && existing.critic_rating == target_critic_rating
        && existing.modified_at == item.modified_at)
}

fn option_str_eq(left: Option<&str>, right: Option<&str>) -> bool {
    left == right
}

fn locked_fields_to_storage(fields: &[String]) -> Option<String> {
    string_vec_to_storage(fields)
}

fn string_vec_to_storage(values: &[String]) -> Option<String> {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        serde_json::to_string(&values).ok()
    }
}

fn remote_trailers_to_storage(values: &[String]) -> Option<String> {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|url| serde_json::json!({ "Url": url }))
        .collect::<Vec<_>>();
    (!values.is_empty())
        .then(|| serde_json::to_string(&values).ok())
        .flatten()
}

pub async fn upsert_media_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    crate::db::provider_ids::upsert_many(db, item_id, &metadata.provider_ids).await?;

    upsert_named_relations(
        db,
        item_id,
        "genres",
        "media_genres",
        "genre_id",
        &metadata.genres,
        metadata.has_nfo,
    )
    .await?;
    upsert_named_relations(
        db,
        item_id,
        "tags",
        "media_tags",
        "tag_id",
        &metadata.tags,
        metadata.has_nfo,
    )
    .await?;
    upsert_named_relations(
        db,
        item_id,
        "studios",
        "media_studios",
        "studio_id",
        &metadata.studios,
        metadata.has_nfo,
    )
    .await?;
    upsert_people(db, item_id, metadata, metadata.has_nfo).await?;
    import_nfo_user_data(db, item_id, metadata).await?;
    Ok(())
}

pub async fn apply_local_metadata_refresh(
    db: &DatabaseConnection,
    item: &media_items::Model,
    metadata: &ParsedMetadata,
    replace_metadata: bool,
) -> anyhow::Result<()> {
    if !metadata.has_nfo || item.lock_data != 0 {
        return Ok(());
    }

    let locked = |field: &str| crate::library::tmdb_metadata::metadata_field_locked(item, field);
    let mut active: media_items::ActiveModel = item.clone().into();
    if !locked("Name")
        && let Some(title) = metadata
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        active.title = Set(title.to_string());
    }
    macro_rules! refresh_optional {
        ($field:ident, $incoming:expr, $locked_field:literal) => {
            if !locked($locked_field) && (replace_metadata || item.$field.is_none()) {
                active.$field = Set($incoming.clone());
            }
        };
    }
    refresh_optional!(overview, metadata.overview, "Overview");
    refresh_optional!(official_rating, metadata.official_rating, "OfficialRating");
    refresh_optional!(custom_rating, metadata.custom_rating, "OfficialRating");
    refresh_optional!(original_title, metadata.original_title, "Name");
    refresh_optional!(sort_name, metadata.sort_name, "SortName");
    refresh_optional!(forced_sort_name, metadata.forced_sort_name, "SortName");
    refresh_optional!(tagline, metadata.tagline, "Tagline");
    refresh_optional!(collection_name, metadata.collection_name, "Collections");
    refresh_optional!(
        original_language,
        metadata.original_language,
        "OriginalLanguage"
    );
    refresh_optional!(
        preferred_metadata_language,
        metadata.preferred_metadata_language,
        "MetadataLanguage"
    );
    refresh_optional!(
        preferred_metadata_country_code,
        metadata.preferred_metadata_country_code,
        "MetadataCountryCode"
    );
    refresh_optional!(series_status, metadata.series_status, "Status");
    refresh_optional!(air_time, metadata.air_time, "Schedule");
    refresh_optional!(home_page_url, metadata.home_page_url, "HomePageUrl");
    refresh_optional!(premiere_date, metadata.premiere_date, "PremiereDate");
    refresh_optional!(end_date, metadata.end_date, "EndDate");
    refresh_optional!(aspect_ratio, metadata.aspect_ratio, "AspectRatio");
    refresh_optional!(display_order, metadata.display_order, "DisplayOrder");
    refresh_optional!(series_name, metadata.series_name, "Name");

    if !locked("Schedule") && (replace_metadata || item.air_days.is_none()) {
        active.air_days = Set(string_vec_to_storage(&metadata.air_days));
    }
    if !locked("RemoteTrailers") && (replace_metadata || item.remote_trailers.is_none()) {
        active.remote_trailers = Set(remote_trailers_to_storage(&metadata.remote_trailers));
    }
    if !locked("ProductionLocations") && (replace_metadata || item.production_locations.is_none()) {
        active.production_locations = Set(string_vec_to_storage(&metadata.production_locations));
    }

    macro_rules! refresh_copy {
        ($field:ident, $incoming:expr, $locked_field:literal) => {
            if !locked($locked_field) && (replace_metadata || item.$field.is_none()) {
                active.$field = Set($incoming);
            }
        };
    }
    refresh_copy!(production_year, metadata.production_year, "ProductionYear");
    refresh_copy!(runtime_ticks, metadata.runtime_ticks, "Runtime");
    refresh_copy!(width, metadata.width, "Width");
    refresh_copy!(height, metadata.height, "Height");
    refresh_copy!(season_number, metadata.season_number, "IndexNumber");
    refresh_copy!(episode_number, metadata.episode_number, "IndexNumber");
    refresh_copy!(
        episode_number_end,
        metadata.ending_episode_number,
        "IndexNumber"
    );
    refresh_copy!(
        airs_before_episode_number,
        metadata.airs_before_episode_number,
        "IndexNumber"
    );
    refresh_copy!(
        airs_after_season_number,
        metadata.airs_after_season_number,
        "IndexNumber"
    );
    refresh_copy!(
        airs_before_season_number,
        metadata.airs_before_season_number,
        "IndexNumber"
    );
    refresh_copy!(
        community_rating,
        metadata.community_rating,
        "CommunityRating"
    );
    refresh_copy!(critic_rating, metadata.critic_rating, "CriticRating");
    if !locked("Subtitles")
        && let Some(has_subtitles) = metadata.has_subtitles
        && (replace_metadata || item.has_subtitles == 0)
    {
        active.has_subtitles = Set(has_subtitles as i64);
    }
    if let Some(lock_data) = metadata.lock_data {
        active.lock_data = Set(lock_data as i64);
    }
    if !metadata.locked_fields.is_empty() {
        active.locked_fields = Set(locked_fields_to_storage(&metadata.locked_fields));
    }
    active.updated_at = Set(now_unix());
    active.update(db).await?;

    crate::db::provider_ids::upsert_many(db, &item.id, &metadata.provider_ids).await?;
    if !locked("Genres") {
        upsert_named_relations(
            db,
            &item.id,
            "genres",
            "media_genres",
            "genre_id",
            &metadata.genres,
            replace_metadata,
        )
        .await?;
    }
    if !locked("Tags") {
        upsert_named_relations(
            db,
            &item.id,
            "tags",
            "media_tags",
            "tag_id",
            &metadata.tags,
            replace_metadata,
        )
        .await?;
    }
    if !locked("Studios") {
        upsert_named_relations(
            db,
            &item.id,
            "studios",
            "media_studios",
            "studio_id",
            &metadata.studios,
            replace_metadata,
        )
        .await?;
    }
    if !locked("Cast") {
        upsert_people(db, &item.id, metadata, replace_metadata).await?;
    }
    import_nfo_user_data(db, &item.id, metadata).await
}

pub async fn upsert_probed_audio_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    crate::db::provider_ids::insert_missing_many(db, item_id, &metadata.provider_ids).await?;

    upsert_named_relations(
        db,
        item_id,
        "genres",
        "media_genres",
        "genre_id",
        &metadata.genres,
        false,
    )
    .await?;
    upsert_named_relations(
        db,
        item_id,
        "studios",
        "media_studios",
        "studio_id",
        &metadata.studios,
        false,
    )
    .await?;
    upsert_people(db, item_id, metadata, false).await?;
    Ok(())
}

async fn import_nfo_user_data(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    if !metadata.has_nfo
        || (metadata.watched.is_none()
            && metadata.play_count.is_none()
            && metadata.last_played_at.is_none())
    {
        return Ok(());
    }

    let Some(configuration) = crate::db::settings::get(db, "named_config:xbmcmetadata")
        .await?
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
    else {
        return Ok(());
    };
    let Some(user_id) = configuration
        .get("UserId")
        .or_else(|| configuration.get("userId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
    else {
        return Ok(());
    };
    if Users::find_by_id(user_id.to_string())
        .one(db)
        .await?
        .is_none()
    {
        return Ok(());
    }

    let now = now_unix();
    if let Some(existing) = UserData::find_by_id((user_id.to_string(), item_id.to_string()))
        .one(db)
        .await?
    {
        let mut active: user_data::ActiveModel = existing.into();
        if let Some(watched) = metadata.watched {
            active.played = Set(watched as i64);
        }
        if let Some(play_count) = metadata.play_count {
            active.play_count = Set(play_count);
        }
        if let Some(last_played_at) = metadata.last_played_at {
            active.last_played_at = Set(Some(last_played_at));
        }
        active.updated_at = Set(now);
        active.update(db).await?;
    } else {
        UserData::insert(user_data::ActiveModel {
            user_id: Set(user_id.to_string()),
            item_id: Set(item_id.to_string()),
            played: Set(metadata.watched.unwrap_or(false) as i64),
            play_count: Set(metadata.play_count.unwrap_or_default()),
            last_played_at: Set(metadata.last_played_at),
            updated_at: Set(now),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await?;
    }
    Ok(())
}

async fn upsert_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    values: &[String],
    replace_empty: bool,
) -> anyhow::Result<()> {
    let kind = NamedRelationKind::from_tables(table, relation_table, relation_column)?;
    if values.is_empty() && !replace_empty {
        return Ok(());
    }
    let relation_values = resolve_named_values(db, kind, table, values).await?;
    let desired_ids = relation_values
        .iter()
        .map(|(id, _value)| id.as_str())
        .collect::<HashSet<_>>();
    if relation_ids_match(db, item_id, kind, &desired_ids).await? {
        return Ok(());
    }

    clear_named_relations(db, item_id, kind)
        .await
        .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;

    upsert_named_values(db, kind, &relation_values)
        .await
        .with_context(|| format!("failed to batch upsert {table} for item: {item_id}"))?;
    link_named_relations(db, kind, item_id, &relation_values)
        .await
        .with_context(|| format!("failed to batch link {table} to item: {item_id}"))?;
    Ok(())
}

pub(crate) async fn merge_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    values: &[String],
) -> anyhow::Result<()> {
    let kind = NamedRelationKind::from_tables(table, relation_table, relation_column)?;
    let relation_values = resolve_named_values(db, kind, table, values).await?;
    link_named_relations(db, kind, item_id, &relation_values)
        .await
        .with_context(|| format!("failed to batch link {table} to item: {item_id}"))
}

#[derive(Clone, Copy)]
enum NamedRelationKind {
    Genre,
    Tag,
    Studio,
}

impl NamedRelationKind {
    fn from_tables(
        table: &str,
        relation_table: &str,
        relation_column: &str,
    ) -> anyhow::Result<Self> {
        match (table, relation_table, relation_column) {
            ("genres", "media_genres", "genre_id") => Ok(Self::Genre),
            ("tags", "media_tags", "tag_id") => Ok(Self::Tag),
            ("studios", "media_studios", "studio_id") => Ok(Self::Studio),
            _ => bail!(
                "unsupported named relation mapping: {table}/{relation_table}/{relation_column}"
            ),
        }
    }
}

async fn clear_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
) -> anyhow::Result<()> {
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::delete_many()
                .filter(media_genres::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        NamedRelationKind::Tag => {
            MediaTags::delete_many()
                .filter(media_tags::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::delete_many()
                .filter(media_studios::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
    }
    Ok(())
}

async fn upsert_named_values(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    values: &[(String, String)],
) -> anyhow::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let now = now_unix();
    match kind {
        NamedRelationKind::Genre => {
            Genres::insert_many(values.iter().map(|(id, value)| genres::ActiveModel {
                id: Set(id.clone()),
                name: Set(value.clone()),
                created_at: Set(now),
            }))
            .on_conflict(OnConflict::new().do_nothing().to_owned())
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Tag => {
            Tags::insert_many(values.iter().map(|(id, value)| tags::ActiveModel {
                id: Set(id.clone()),
                name: Set(value.clone()),
                created_at: Set(now),
            }))
            .on_conflict(OnConflict::new().do_nothing().to_owned())
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Studio => {
            Studios::insert_many(values.iter().map(|(id, value)| studios::ActiveModel {
                id: Set(id.clone()),
                name: Set(value.clone()),
                created_at: Set(now),
            }))
            .on_conflict(OnConflict::new().do_nothing().to_owned())
            .exec_without_returning(db)
            .await?;
        }
    }
    Ok(())
}

async fn resolve_named_values(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    table: &str,
    values: &[String],
) -> anyhow::Result<Vec<(String, String)>> {
    let mut seen = HashSet::new();
    let candidates = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(value.to_ascii_lowercase()))
        .map(|value| {
            (
                stable_text_id(&format!("{table}:{}", value.to_ascii_lowercase())),
                value.to_string(),
            )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    upsert_named_values(db, kind, &candidates).await?;
    let ids = candidates
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let existing = match kind {
        NamedRelationKind::Genre => Genres::find()
            .filter(genres::Column::Id.is_in(ids.clone()))
            .all(db)
            .await?
            .into_iter()
            .map(|value| (value.id, value.name))
            .collect::<HashMap<_, _>>(),
        NamedRelationKind::Tag => Tags::find()
            .filter(tags::Column::Id.is_in(ids.clone()))
            .all(db)
            .await?
            .into_iter()
            .map(|value| (value.id, value.name))
            .collect::<HashMap<_, _>>(),
        NamedRelationKind::Studio => Studios::find()
            .filter(studios::Column::Id.is_in(ids))
            .all(db)
            .await?
            .into_iter()
            .map(|value| (value.id, value.name))
            .collect::<HashMap<_, _>>(),
    };

    candidates
        .into_iter()
        .map(|(id, name)| {
            existing
                .get(&id)
                .cloned()
                .map(|_| (id, name.clone()))
                .ok_or_else(|| {
                    anyhow::anyhow!("failed to resolve {table} row after upsert: {name}")
                })
        })
        .collect()
}

async fn link_named_relations(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    item_id: &str,
    values: &[(String, String)],
) -> anyhow::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::insert_many(values.iter().map(|(relation_id, _)| {
                media_genres::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    genre_id: Set(relation_id.clone()),
                }
            }))
            .on_conflict(
                OnConflict::columns([media_genres::Column::ItemId, media_genres::Column::GenreId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Tag => {
            MediaTags::insert_many(
                values
                    .iter()
                    .map(|(relation_id, _)| media_tags::ActiveModel {
                        item_id: Set(item_id.to_string()),
                        tag_id: Set(relation_id.clone()),
                    }),
            )
            .on_conflict(
                OnConflict::columns([media_tags::Column::ItemId, media_tags::Column::TagId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::insert_many(values.iter().map(|(relation_id, _)| {
                media_studios::ActiveModel {
                    item_id: Set(item_id.to_string()),
                    studio_id: Set(relation_id.clone()),
                }
            }))
            .on_conflict(
                OnConflict::columns([
                    media_studios::Column::ItemId,
                    media_studios::Column::StudioId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
    }
    Ok(())
}

async fn relation_ids_match(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
    desired_ids: &HashSet<&str>,
) -> anyhow::Result<bool> {
    let existing_ids = match kind {
        NamedRelationKind::Genre => MediaGenres::find()
            .filter(media_genres::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.genre_id)
            .collect::<Vec<_>>(),
        NamedRelationKind::Tag => MediaTags::find()
            .filter(media_tags::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.tag_id)
            .collect::<Vec<_>>(),
        NamedRelationKind::Studio => MediaStudios::find()
            .filter(media_studios::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.studio_id)
            .collect::<Vec<_>>(),
    };
    if existing_ids.len() != desired_ids.len() {
        return Ok(false);
    }
    for relation_id in existing_ids {
        if !desired_ids.contains(relation_id.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn upsert_people(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
    replace_empty: bool,
) -> anyhow::Result<()> {
    if metadata.people.is_empty() && !replace_empty {
        return Ok(());
    }
    let mut people = Vec::new();
    for (sort_order, person) in metadata.people.iter().enumerate() {
        let relation = PersonRelation {
            person_id: stable_text_id(&format!(
                "people:{}",
                person.name.trim().to_ascii_lowercase()
            )),
            role: person.role.clone(),
            person_type: person.person_type.clone(),
            sort_order: i64::try_from(sort_order).unwrap_or(i64::MAX),
        };
        // TMDb can return the same person more than once with the same type.
        // De-duplicate before the batch upsert; PostgreSQL rejects a single
        // INSERT that would update the same unique key twice.
        if people.iter().any(|existing: &DesiredPerson| {
            existing.relation.person_id == relation.person_id
                && existing.relation.person_type == relation.person_type
        }) {
            continue;
        }
        people.push(DesiredPerson {
            relation,
            name: person.name.trim().to_string(),
        });
    }
    if people_match(db, item_id, &people).await? {
        return Ok(());
    }

    MediaPeople::delete_many()
        .filter(media_people::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    if people.is_empty() {
        return Ok(());
    }
    People::insert_many(people.iter().map(|person| people::ActiveModel {
        id: Set(person.relation.person_id.clone()),
        name: Set(person.name.clone()),
        created_at: Set(now_unix()),
        ..Default::default()
    }))
    .on_conflict_do_nothing()
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to batch upsert people for item: {item_id}"))?;
    MediaPeople::insert_many(people.iter().map(|person| media_people::ActiveModel {
        item_id: Set(item_id.to_string()),
        person_id: Set(person.relation.person_id.clone()),
        role: Set(person.relation.role.clone()),
        person_type: Set(person.relation.person_type.clone()),
        sort_order: Set(person.relation.sort_order),
    }))
    .on_conflict(
        OnConflict::columns([
            media_people::Column::ItemId,
            media_people::Column::PersonId,
            media_people::Column::PersonType,
        ])
        .update_columns([media_people::Column::Role, media_people::Column::SortOrder])
        .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to batch link people to item: {item_id}"))?;
    Ok(())
}

struct DesiredPerson {
    relation: PersonRelation,
    name: String,
}

#[derive(Debug, PartialEq, Eq)]
struct PersonRelation {
    person_id: String,
    role: Option<String>,
    person_type: String,
    sort_order: i64,
}

async fn people_match(
    db: &DatabaseConnection,
    item_id: &str,
    people: &[DesiredPerson],
) -> anyhow::Result<bool> {
    let existing_people = MediaPeople::find()
        .filter(media_people::Column::ItemId.eq(item_id))
        .order_by_asc(media_people::Column::SortOrder)
        .order_by_asc(media_people::Column::PersonId)
        .order_by_asc(media_people::Column::PersonType)
        .all(db)
        .await
        .with_context(|| format!("failed to read people for item: {item_id}"))?;
    if existing_people.len() != people.len() {
        return Ok(false);
    }
    for (existing_person, person) in existing_people.iter().zip(people.iter()) {
        let existing = PersonRelation {
            person_id: existing_person.person_id.clone(),
            role: existing_person.role.clone(),
            person_type: existing_person.person_type.clone(),
            sort_order: existing_person.sort_order,
        };
        if existing != person.relation {
            return Ok(false);
        }
    }
    Ok(true)
}

pub async fn upsert_default_media_stream(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<()> {
    if item.is_folder {
        return Ok(());
    }
    let stream_type = if matches!(item.item_type.as_str(), "Audio" | "AudioBook") {
        "Audio"
    } else {
        "Video"
    };
    let stream_id = stable_text_id(&format!("stream:{}:0", item.id));
    delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
    if default_media_stream_matches(db, &item.id, stream_type).await? {
        return Ok(());
    }
    if let Some(existing) = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::StreamIndex.eq(0_i64))
        .one(db)
        .await
        .with_context(|| format!("failed to read default media stream: {}", item.path))?
    {
        let mut active: media_streams::ActiveModel = existing.into();
        active.stream_type = Set(stream_type.to_string());
        active.is_external = Set(0);
        active
            .update(db)
            .await
            .with_context(|| format!("failed to update default media stream: {}", item.path))?;
    } else {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(stream_id),
            item_id: Set(item.id.clone()),
            stream_index: Set(0),
            stream_type: Set(stream_type.to_string()),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to insert default media stream: {}", item.path))?;
    }
    Ok(())
}

pub async fn upsert_failed_media_probe(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<()> {
    if item.is_folder {
        return Ok(());
    }
    let stream_id = stable_text_id(&format!("stream:{}:probe-failure", item.id));
    delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
    if let Some(existing) = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::StreamType.eq(MEDIA_PROBE_FAILURE_STREAM_TYPE))
        .one(db)
        .await
        .with_context(|| format!("failed to read failed media probe marker: {}", item.path))?
    {
        let mut active: media_streams::ActiveModel = existing.into();
        active.created_at = Set(now_unix());
        active.update(db).await.with_context(|| {
            format!("failed to update failed media probe marker: {}", item.path)
        })?;
    } else {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(stream_id),
            item_id: Set(item.id.clone()),
            stream_index: Set(-1),
            stream_type: Set(MEDIA_PROBE_FAILURE_STREAM_TYPE.to_string()),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to insert failed media probe marker: {}", item.path))?;
    }
    Ok(())
}

async fn default_media_stream_matches(
    db: &DatabaseConnection,
    item_id: &str,
    stream_type: &str,
) -> anyhow::Result<bool> {
    let stream = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::StreamIndex.eq(0_i64))
        .one(db)
        .await
        .with_context(|| format!("failed to read default media stream: {item_id}"))?;
    let Some(stream) = stream else {
        return Ok(false);
    };
    Ok(stream.stream_type == stream_type && stream.is_external == 0)
}

pub async fn upsert_probed_media_streams(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    if item.is_folder || probe.streams.is_empty() {
        return Ok(false);
    }
    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
        delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
    }
    if probed_media_streams_match(db, &item.id, probe).await? {
        return Ok(true);
    }

    MediaStreams::delete_many()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::IsExternal.eq(0_i64))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
        if let Some(existing) = MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq(&item.id))
            .filter(media_streams::Column::StreamIndex.eq(stream.stream_index))
            .one(db)
            .await
            .with_context(|| format!("failed to read probed stream for item: {}", item.id))?
        {
            let mut active: media_streams::ActiveModel = existing.into();
            apply_probed_stream(&mut active, stream);
            active
                .update(db)
                .await
                .with_context(|| format!("failed to update probed stream for item: {}", item.id))?;
        } else {
            let mut active = media_streams::ActiveModel {
                id: Set(stream_id),
                item_id: Set(item.id.clone()),
                stream_index: Set(stream.stream_index),
                created_at: Set(now_unix()),
                ..Default::default()
            };
            apply_probed_stream(&mut active, stream);
            MediaStreams::insert(active)
                .exec_without_returning(db)
                .await
                .with_context(|| format!("failed to insert probed stream for item: {}", item.id))?;
        }
    }

    Ok(true)
}

pub async fn replace_external_audio_streams(
    db: &DatabaseConnection,
    item_id: &str,
    streams: &[(String, ProbedStream)],
) -> anyhow::Result<()> {
    MediaStreams::delete_many()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::IsExternal.eq(1_i64))
        .filter(media_streams::Column::StreamType.eq("Audio"))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear external audio streams: {item_id}"))?;

    let mut stream_index = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .order_by_desc(media_streams::Column::StreamIndex)
        .one(db)
        .await
        .with_context(|| format!("failed to find next external audio index: {item_id}"))?
        .map(|stream| stream.stream_index.saturating_add(1))
        .unwrap_or(0);

    for (path, stream) in streams {
        let mut active = media_streams::ActiveModel {
            id: Set(stable_text_id(&format!(
                "external-audio:{item_id}:{path}:{}",
                stream.stream_index
            ))),
            item_id: Set(item_id.to_string()),
            stream_index: Set(stream_index),
            created_at: Set(now_unix()),
            ..Default::default()
        };
        apply_probed_stream(&mut active, stream);
        active.path = Set(Some(path.clone()));
        active.is_external = Set(1);
        MediaStreams::insert(active)
            .exec_without_returning(db)
            .await
            .with_context(|| format!("failed to insert external audio stream: {path}"))?;
        stream_index = stream_index.saturating_add(1);
    }
    Ok(())
}

pub async fn refresh_external_lyric_stream(
    db: &DatabaseConnection,
    media_path: &std::path::Path,
    item_id: &str,
) -> anyhow::Result<()> {
    MediaStreams::delete_many()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::IsExternal.eq(1_i64))
        .filter(media_streams::Column::StreamType.eq("Lyric"))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear external lyric stream: {item_id}"))?;

    let lyric = [
        ("lrc", media_path.with_extension("lrc")),
        ("txt", media_path.with_extension("txt")),
    ]
    .into_iter()
    .find(|(_, path)| path.is_file());
    let Some((codec, path)) = lyric else {
        return Ok(());
    };
    let stream_index = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .order_by_desc(media_streams::Column::StreamIndex)
        .one(db)
        .await?
        .map(|stream| stream.stream_index.saturating_add(1))
        .unwrap_or(0);
    let path_string = path.to_string_lossy().to_string();
    MediaStreams::insert(media_streams::ActiveModel {
        id: Set(stable_text_id(&format!(
            "external-lyric:{item_id}:{path_string}"
        ))),
        item_id: Set(item_id.to_string()),
        stream_index: Set(stream_index),
        stream_type: Set("Lyric".to_string()),
        codec: Set(Some(codec.to_string())),
        path: Set(Some(path_string)),
        is_external: Set(1),
        created_at: Set(now_unix()),
        ..Default::default()
    })
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to insert external lyric stream: {}", path.display()))?;
    Ok(())
}

fn apply_probed_stream(active: &mut media_streams::ActiveModel, stream: &ProbedStream) {
    active.stream_type = Set(stream.stream_type.clone());
    active.codec = Set(stream.codec.clone());
    active.profile = Set(stream.profile.clone());
    active.codec_tag = Set(stream.codec_tag.clone());
    active.language = Set(stream.language.clone());
    active.title = Set(stream.title.clone());
    active.comment = Set(stream.comment.clone());
    active.bit_rate = Set(stream.bit_rate);
    active.width = Set(stream.width);
    active.height = Set(stream.height);
    active.aspect_ratio = Set(stream.aspect_ratio.clone());
    active.average_frame_rate = Set(stream.average_frame_rate);
    active.real_frame_rate = Set(stream.real_frame_rate);
    active.reference_frame_rate = Set(stream.reference_frame_rate);
    active.channels = Set(stream.channels);
    active.channel_layout = Set(stream.channel_layout.clone());
    active.sample_rate = Set(stream.sample_rate);
    active.bit_depth = Set(stream.bit_depth);
    active.ref_frames = Set(stream.ref_frames);
    active.is_interlaced = Set(stream.is_interlaced as i64);
    active.is_avc = Set(stream.is_avc.map(|value| value as i64));
    active.is_anamorphic = Set(stream.is_anamorphic.map(|value| value as i64));
    active.pixel_format = Set(stream.pixel_format.clone());
    active.level = Set(stream.level);
    active.color_range = Set(stream.color_range.clone());
    active.color_space = Set(stream.color_space.clone());
    active.color_transfer = Set(stream.color_transfer.clone());
    active.color_primaries = Set(stream.color_primaries.clone());
    active.time_base = Set(stream.time_base.clone());
    active.codec_time_base = Set(stream.codec_time_base.clone());
    active.nal_length_size = Set(stream.nal_length_size.clone());
    active.rotation = Set(stream.rotation);
    active.video_range = Set(stream.video_range.clone());
    active.video_range_type = Set(stream.video_range_type.clone());
    active.hdr10_plus_present_flag = Set(stream.hdr10_plus_present_flag.map(|value| value as i64));
    active.is_default = Set(stream.is_default as i64);
    active.is_forced = Set(stream.is_forced as i64);
    active.is_hearing_impaired = Set(stream.is_hearing_impaired as i64);
    active.is_original = Set(stream.is_original.map(|value| value as i64));
    active.path = Set(None);
    active.is_external = Set(0);
}

async fn probed_media_streams_match(
    db: &DatabaseConnection,
    item_id: &str,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    let streams = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::IsExternal.eq(0_i64))
        .order_by_asc(media_streams::Column::StreamIndex)
        .all(db)
        .await
        .with_context(|| format!("failed to read probed media streams: {item_id}"))?;
    if streams.len() != probe.streams.len() {
        return Ok(false);
    }
    for (existing, stream) in streams.iter().zip(probe.streams.iter()) {
        if !probed_stream_matches(existing, stream)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn probed_stream_matches(
    existing: &media_streams::Model,
    stream: &ProbedStream,
) -> anyhow::Result<bool> {
    Ok(existing.stream_index == stream.stream_index
        && existing.stream_type == stream.stream_type
        && option_str_eq(existing.codec.as_deref(), stream.codec.as_deref())
        && option_str_eq(existing.profile.as_deref(), stream.profile.as_deref())
        && option_str_eq(existing.codec_tag.as_deref(), stream.codec_tag.as_deref())
        && option_str_eq(existing.language.as_deref(), stream.language.as_deref())
        && option_str_eq(existing.title.as_deref(), stream.title.as_deref())
        && option_str_eq(existing.comment.as_deref(), stream.comment.as_deref())
        && existing.bit_rate == stream.bit_rate
        && existing.width == stream.width
        && existing.height == stream.height
        && option_str_eq(
            existing.aspect_ratio.as_deref(),
            stream.aspect_ratio.as_deref(),
        )
        && option_f64_eq(existing.average_frame_rate, stream.average_frame_rate)
        && option_f64_eq(existing.real_frame_rate, stream.real_frame_rate)
        && option_f64_eq(existing.reference_frame_rate, stream.reference_frame_rate)
        && existing.channels == stream.channels
        && option_str_eq(
            existing.channel_layout.as_deref(),
            stream.channel_layout.as_deref(),
        )
        && existing.sample_rate == stream.sample_rate
        && existing.bit_depth == stream.bit_depth
        && existing.ref_frames == stream.ref_frames
        && i64_bool(existing.is_interlaced) == stream.is_interlaced
        && opt_i64_bool(existing.is_avc) == stream.is_avc
        && opt_i64_bool(existing.is_anamorphic) == stream.is_anamorphic
        && option_str_eq(
            existing.pixel_format.as_deref(),
            stream.pixel_format.as_deref(),
        )
        && existing.level == stream.level
        && option_str_eq(
            existing.color_range.as_deref(),
            stream.color_range.as_deref(),
        )
        && option_str_eq(
            existing.color_space.as_deref(),
            stream.color_space.as_deref(),
        )
        && option_str_eq(
            existing.color_transfer.as_deref(),
            stream.color_transfer.as_deref(),
        )
        && option_str_eq(
            existing.color_primaries.as_deref(),
            stream.color_primaries.as_deref(),
        )
        && option_str_eq(existing.time_base.as_deref(), stream.time_base.as_deref())
        && option_str_eq(
            existing.codec_time_base.as_deref(),
            stream.codec_time_base.as_deref(),
        )
        && option_str_eq(
            existing.nal_length_size.as_deref(),
            stream.nal_length_size.as_deref(),
        )
        && existing.rotation == stream.rotation
        && option_str_eq(
            existing.video_range.as_deref(),
            stream.video_range.as_deref(),
        )
        && option_str_eq(
            existing.video_range_type.as_deref(),
            stream.video_range_type.as_deref(),
        )
        && opt_i64_bool(existing.hdr10_plus_present_flag) == stream.hdr10_plus_present_flag
        && i64_bool(existing.is_default) == stream.is_default
        && i64_bool(existing.is_forced) == stream.is_forced
        && i64_bool(existing.is_hearing_impaired) == stream.is_hearing_impaired
        && opt_i64_bool(existing.is_original) == stream.is_original
        && existing.path.is_none()
        && existing.is_external == 0)
}

fn i64_bool(value: i64) -> bool {
    value != 0
}

fn opt_i64_bool(value: Option<i64>) -> Option<bool> {
    value.map(i64_bool)
}

fn option_f64_eq(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right || (left.is_nan() && right.is_nan()),
        (None, None) => true,
        _ => false,
    }
}

async fn delete_stale_generated_stream_id(
    db: &DatabaseConnection,
    stream_id: &str,
    item_id: &str,
) -> anyhow::Result<()> {
    MediaStreams::delete_many()
        .filter(media_streams::Column::Id.eq(stream_id))
        .filter(media_streams::Column::ItemId.ne(item_id))
        .filter(media_streams::Column::IsExternal.eq(0_i64))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear stale generated stream id: {stream_id}"))?;
    Ok(())
}

pub async fn remove_missing_media_items(
    db: &DatabaseConnection,
    library_ids: &[String],
    seen_paths: &[String],
) -> anyhow::Result<()> {
    if library_ids.is_empty() {
        return Ok(());
    }

    let seen_paths = seen_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let library_ids = library_ids
        .iter()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if library_ids.is_empty() {
        return Ok(());
    }

    let mut last_id = None::<String>;
    loop {
        let mut query = MediaItems::find()
            .filter(media_items::Column::LibraryId.is_in(library_ids.clone()))
            .order_by_asc(media_items::Column::Id)
            .limit(MISSING_CLEANUP_BATCH_SIZE);
        if let Some(last_id) = last_id.as_deref() {
            query = query.filter(media_items::Column::Id.gt(last_id));
        }

        let items = query
            .all(db)
            .await
            .context("failed to list media items for cleanup")?;
        if items.is_empty() {
            break;
        }

        last_id = items.last().map(|item| item.id.clone());
        for item in items {
            let id = item.id.clone();
            let path = item.path.clone();
            if seen_paths.contains(path.as_str()) || cleanup_path_exists(&path).await {
                continue;
            }
            MediaItems::delete_by_id(id)
                .exec(db)
                .await
                .with_context(|| format!("failed to delete missing media item: {path}"))?;
        }

        tokio::task::yield_now().await;
        tokio::time::sleep(MISSING_CLEANUP_PAUSE).await;
    }
    Ok(())
}

async fn cleanup_path_exists(path: &str) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::debug!("skipping missing-media cleanup for unreadable path {path}: {error}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ScannedMediaItem, apply_local_metadata_refresh, cached_media_probe_if_current,
        upsert_default_media_stream, upsert_failed_media_probe, upsert_media_item,
        upsert_media_metadata, upsert_probed_media_streams,
    };
    use crate::entities::{
        genres::{self, Entity as Genres},
        media_genres,
        media_items::{self, Entity as MediaItems},
        media_people,
        media_streams::{self, Entity as MediaStreams},
        media_studios, media_tags,
        user_data::Entity as UserData,
        users::{self, Entity as Users},
    };
    use crate::library::metadata::{ParsedMetadata, ParsedPerson};
    use crate::library::probe::{MediaProbe, ProbedStream};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};

    #[tokio::test]
    async fn media_item_upsert_skips_unchanged_conflict_update() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("movie-1", "Movie");

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");

        let mut active: media_items::ActiveModel = MediaItems::find_by_id(item.id.clone())
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.updated_at = Set(123);
        active.update(&db).await.unwrap();

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.updated_at, 123);

        item.title = "Renamed Movie".to_string();
        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.title, "Renamed Movie");
        assert_ne!(row.updated_at, 123);
    }

    #[tokio::test]
    async fn media_item_upsert_preserves_scraped_episode_title_on_rescan() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("episode-scraped-title", "第一集");
        item.item_type = "Episode".to_string();
        item.season_number = Some(1);
        item.episode_number = Some(1);
        upsert_media_item(&db, &item).await.unwrap();

        let mut active: media_items::ActiveModel = MediaItems::find_by_id(item.id.clone())
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.overview = Set(Some("已从 TMDB 获取的单集简介".to_string()));
        active.tmdb_metadata_version = Set(1);
        active.update(&db).await.unwrap();

        item.title = "mp4".to_string();
        upsert_media_item(&db, &item).await.unwrap();

        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.title, "第一集");
        assert_eq!(row.overview.as_deref(), Some("已从 TMDB 获取的单集简介"));
    }

    #[tokio::test]
    async fn media_item_upsert_persists_episode_number_end() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("episode-range", "Episode Range");
        item.item_type = "Episode".to_string();
        item.season_number = Some(1);
        item.episode_number = Some(1);
        item.episode_number_end = Some(3);

        upsert_media_item(&db, &item).await.unwrap();
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.episode_number_end, Some(3));

        item.episode_number_end = Some(4);
        upsert_media_item(&db, &item).await.unwrap();
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.episode_number_end, Some(4));
    }

    #[tokio::test]
    async fn media_item_upsert_persists_local_nfo_scalars() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("movie-nfo-scalars", "NFO Scalars");
        item.premiere_date = Some("2024-07-02".to_string());
        item.end_date = Some("2024-08-03".to_string());
        item.runtime_ticks = Some(54_000_000_000);
        item.community_rating = Some(8.4);
        item.critic_rating = Some(91.0);
        item.custom_rating = Some("TV-MA".to_string());
        item.original_title = Some("Original NFO Scalars".to_string());
        item.sort_name = Some("NFO Scalars Sort".to_string());
        item.forced_sort_name = Some("Forced NFO Scalars".to_string());
        item.lock_data = Some(true);
        item.locked_fields = vec!["Name".to_string(), "Overview".to_string()];
        item.tagline = Some("Trust no one.".to_string());
        item.collection_name = Some("The Collection".to_string());
        item.original_language = Some("en".to_string());
        item.preferred_metadata_language = Some("ja-JP".to_string());
        item.preferred_metadata_country_code = Some("JP".to_string());
        item.series_status = Some("Ended".to_string());
        item.air_days = vec!["Friday".to_string()];
        item.air_time = Some("23:30".to_string());
        item.home_page_url = Some("https://example.test/movie".to_string());
        item.remote_trailers = vec!["https://www.youtube.com/watch?v=abc".to_string()];
        item.production_locations = vec!["United States".to_string(), "Japan".to_string()];
        item.display_order = Some("dvd".to_string());
        item.aspect_ratio = Some("16:9".to_string());
        item.width = Some(1920);
        item.height = Some(1080);
        item.has_subtitles = true;
        item.airs_before_episode_number = Some(3);
        item.airs_after_season_number = Some(1);
        item.airs_before_season_number = Some(2);
        item.series_name = Some("Example Show".to_string());
        item.video_type = Some("Iso".to_string());
        item.iso_type = Some("BluRay".to_string());
        item.video_3d_format = Some("HalfSideBySide".to_string());

        upsert_media_item(&db, &item).await.unwrap();
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.premiere_date.as_deref(), Some("2024-07-02"));
        assert_eq!(row.end_date.as_deref(), Some("2024-08-03"));
        assert_eq!(row.runtime_ticks, Some(54_000_000_000));
        assert_eq!(row.community_rating, Some(8.4));
        assert_eq!(row.critic_rating, Some(91.0));
        assert_eq!(row.custom_rating.as_deref(), Some("TV-MA"));
        assert_eq!(row.original_title.as_deref(), Some("Original NFO Scalars"));
        assert_eq!(row.sort_name.as_deref(), Some("NFO Scalars Sort"));
        assert_eq!(row.forced_sort_name.as_deref(), Some("Forced NFO Scalars"));
        assert_eq!(row.lock_data, 1);
        assert_eq!(row.locked_fields.as_deref(), Some(r#"["Name","Overview"]"#));
        assert_eq!(row.tagline.as_deref(), Some("Trust no one."));
        assert_eq!(row.collection_name.as_deref(), Some("The Collection"));
        assert_eq!(row.original_language.as_deref(), Some("en"));
        assert_eq!(row.preferred_metadata_language.as_deref(), Some("ja-JP"));
        assert_eq!(row.preferred_metadata_country_code.as_deref(), Some("JP"));
        assert_eq!(row.series_status.as_deref(), Some("Ended"));
        assert_eq!(row.air_days.as_deref(), Some(r#"["Friday"]"#));
        assert_eq!(row.air_time.as_deref(), Some("23:30"));
        assert_eq!(row.aspect_ratio.as_deref(), Some("16:9"));
        assert_eq!(row.width, Some(1920));
        assert_eq!(row.height, Some(1080));
        assert_eq!(row.has_subtitles, 1);
        assert_eq!(row.airs_before_episode_number, Some(3));
        assert_eq!(row.airs_after_season_number, Some(1));
        assert_eq!(row.airs_before_season_number, Some(2));
        assert_eq!(row.series_name.as_deref(), Some("Example Show"));
        assert_eq!(
            row.home_page_url.as_deref(),
            Some("https://example.test/movie")
        );
        assert_eq!(
            row.remote_trailers.as_deref(),
            Some(r#"[{"Url":"https://www.youtube.com/watch?v=abc"}]"#)
        );
        assert_eq!(
            row.production_locations.as_deref(),
            Some(r#"["United States","Japan"]"#)
        );
        assert_eq!(row.display_order.as_deref(), Some("dvd"));
        assert_eq!(row.video_type.as_deref(), Some("Iso"));
        assert_eq!(row.iso_type.as_deref(), Some("BluRay"));
        assert_eq!(row.video_3d_format.as_deref(), Some("HalfSideBySide"));
    }

    #[tokio::test]
    async fn nfo_user_data_import_requires_configured_valid_user() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-nfo-user-data", "NFO User Data");
        upsert_media_item(&db, &item).await.unwrap();
        let metadata = ParsedMetadata {
            has_nfo: true,
            watched: Some(true),
            play_count: Some(3),
            last_played_at: Some(1_700_000_000),
            ..Default::default()
        };

        upsert_media_metadata(&db, &item.id, &metadata)
            .await
            .unwrap();
        assert!(
            UserData::find_by_id(("nfo-user".to_string(), item.id.clone()))
                .one(&db)
                .await
                .unwrap()
                .is_none()
        );

        Users::insert(users::ActiveModel {
            id: Set("nfo-user".to_string()),
            username: Set("nfo-user".to_string()),
            display_name: Set("NFO User".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(&db)
        .await
        .unwrap();
        crate::db::settings::set(&db, "named_config:xbmcmetadata", r#"{"UserId":"nfo-user"}"#)
            .await
            .unwrap();

        upsert_media_metadata(&db, &item.id, &metadata)
            .await
            .unwrap();
        let row = UserData::find_by_id(("nfo-user".to_string(), item.id.clone()))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.played, 1);
        assert_eq!(row.play_count, 3);
        assert_eq!(row.last_played_at, Some(1_700_000_000));
    }

    #[tokio::test]
    async fn local_metadata_refresh_replaces_unlocked_fields_and_relations() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("movie-local-refresh", "Old title");
        item.locked_fields = vec!["Overview".to_string()];
        upsert_media_item(&db, &item).await.unwrap();
        let row = MediaItems::find_by_id(&item.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let metadata = ParsedMetadata {
            has_nfo: true,
            title: Some("NFO title".to_string()),
            overview: Some("NFO overview".to_string()),
            genres: vec!["Drama".to_string(), "Crime".to_string()],
            ..Default::default()
        };

        apply_local_metadata_refresh(&db, &row, &metadata, true)
            .await
            .unwrap();

        let row = MediaItems::find_by_id(&item.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.title, "NFO title");
        assert_eq!(row.overview.as_deref(), Some("Overview"));
        assert_eq!(row.production_year, None);
        assert_eq!(
            media_genres::Entity::find()
                .filter(media_genres::Column::ItemId.eq(&item.id))
                .count(&db)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn nfo_empty_relations_clear_stale_metadata() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-nfo-empty-relations", "NFO Relations");
        upsert_media_item(&db, &item).await.unwrap();

        let populated = ParsedMetadata {
            has_nfo: true,
            genres: vec!["Drama".to_string()],
            tags: vec!["Noir".to_string()],
            studios: vec!["Studio".to_string()],
            people: vec![ParsedPerson {
                name: "Director".to_string(),
                role: Some("Director".to_string()),
                person_type: "Director".to_string(),
            }],
            ..Default::default()
        };
        upsert_media_metadata(&db, &item.id, &populated)
            .await
            .unwrap();

        let empty = ParsedMetadata {
            has_nfo: true,
            ..Default::default()
        };
        upsert_media_metadata(&db, &item.id, &empty).await.unwrap();

        assert_eq!(
            media_genres::Entity::find()
                .filter(media_genres::Column::ItemId.eq(&item.id))
                .count(&db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            media_tags::Entity::find()
                .filter(media_tags::Column::ItemId.eq(&item.id))
                .count(&db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            media_studios::Entity::find()
                .filter(media_studios::Column::ItemId.eq(&item.id))
                .count(&db)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            media_people::Entity::find()
                .filter(media_people::Column::ItemId.eq(&item.id))
                .count(&db)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn named_relation_batch_upsert_reuses_legacy_ids() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-legacy-genre", "Legacy Genre");
        upsert_media_item(&db, &item).await.unwrap();
        Genres::insert(genres::ActiveModel {
            id: Set("legacy-drama-id".to_string()),
            name: Set("Legacy Drama".to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(&db)
        .await
        .unwrap();

        upsert_media_metadata(
            &db,
            &item.id,
            &ParsedMetadata {
                genres: vec!["Legacy Drama".to_string(), "New Genre".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let relations = media_genres::Entity::find()
            .filter(media_genres::Column::ItemId.eq(&item.id))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(relations.len(), 2);
        assert!(
            relations
                .iter()
                .any(|relation| relation.genre_id == "legacy-drama-id")
        );
    }

    #[tokio::test]
    async fn probed_stream_upsert_skips_unchanged_rewrites() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-probe-1", "Probe Movie");
        upsert_media_item(&db, &item).await.unwrap();

        let probe = media_probe("h264");
        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        let mut active: media_streams::ActiveModel = media_stream_row(&db, &item.id).await.into();
        active.created_at = Set(123);
        active.update(&db).await.unwrap();

        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.created_at, 123);

        let changed_probe = media_probe("hevc");
        assert!(
            upsert_probed_media_streams(&db, &item, &changed_probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.codec.as_deref(), Some("hevc"));
    }

    #[tokio::test]
    async fn probed_stream_upsert_persists_embedded_attachments_without_path() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-probe-attachment", "Probe Attachment Movie");
        upsert_media_item(&db, &item).await.unwrap();

        let mut probe = media_probe("h264");
        probe.streams.push(attachment_probe_stream());

        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        let row = MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq(&item.id))
            .filter(media_streams::Column::StreamIndex.eq(5_i64))
            .one(&db)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(row.stream_type, "Attachment");
        assert_eq!(row.codec.as_deref(), Some("ttf"));
        assert_eq!(row.title.as_deref(), Some("Font.ttf"));
        assert_eq!(row.comment.as_deref(), Some("subtitle font"));
        assert_eq!(row.path, None);
        assert_eq!(row.is_external, 0);
    }

    #[tokio::test]
    async fn default_stream_does_not_satisfy_probe_cache() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-default-cache", "Default Cache Movie");
        upsert_media_item(&db, &item).await.unwrap();
        upsert_default_media_stream(&db, &item).await.unwrap();

        assert!(
            cached_media_probe_if_current(
                &db,
                &item.path,
                item.modified_at,
                item.size_bytes,
                false
            )
            .await
            .unwrap()
            .is_none()
        );

        let probe = media_probe("h264");
        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );

        let cached = cached_media_probe_if_current(
            &db,
            &item.path,
            item.modified_at,
            item.size_bytes,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(cached.runtime_ticks, item.runtime_ticks);
        assert_eq!(cached.size_bytes, item.size_bytes);
    }

    #[tokio::test]
    async fn failed_probe_marker_satisfies_probe_cache() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-failed-cache", "Failed Cache Movie");
        upsert_media_item(&db, &item).await.unwrap();
        upsert_default_media_stream(&db, &item).await.unwrap();
        upsert_failed_media_probe(&db, &item).await.unwrap();

        let cached = cached_media_probe_if_current(
            &db,
            &item.path,
            item.modified_at,
            item.size_bytes,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(cached.runtime_ticks, item.runtime_ticks);
        assert_eq!(cached.size_bytes, item.size_bytes);
    }

    fn scanned_movie(id: &str, title: &str) -> ScannedMediaItem {
        ScannedMediaItem {
            id: id.to_string(),
            title: title.to_string(),
            path: format!("/tmp/jellyfin-rs-storage-test/{id}.mkv"),
            library_id: "movies".to_string(),
            parent_id: "movies".to_string(),
            item_type: "Movie".to_string(),
            extra_type: None,
            video_type: None,
            iso_type: None,
            video_3d_format: None,
            is_folder: false,
            container: Some("mkv".to_string()),
            overview: Some("Overview".to_string()),
            official_rating: Some("PG-13".to_string()),
            custom_rating: None,
            extended_video_type: None,
            original_title: None,
            sort_name: None,
            forced_sort_name: None,
            lock_data: None,
            locked_fields: Vec::new(),
            tagline: None,
            collection_name: None,
            original_language: None,
            series_status: None,
            home_page_url: None,
            remote_trailers: Vec::new(),
            production_locations: Vec::new(),
            production_year: Some(2024),
            premiere_date: Some("2024-01-02".to_string()),
            end_date: None,
            runtime_ticks: Some(600_000_000),
            display_order: None,
            size_bytes: Some(1024),
            season_number: None,
            episode_number: None,
            episode_number_end: None,
            community_rating: Some(8.1),
            critic_rating: Some(91.0),
            modified_at: 42,
            created_at: 1,
            ..Default::default()
        }
    }

    fn media_probe(codec: &str) -> MediaProbe {
        MediaProbe {
            runtime_ticks: Some(600_000_000),
            size_bytes: Some(1024),
            container: Some("mkv".to_string()),
            video_3d_format: None,
            audio_metadata: Default::default(),
            chapters: Vec::new(),
            streams: vec![ProbedStream {
                stream_index: 0,
                stream_type: "Video".to_string(),
                codec: Some(codec.to_string()),
                profile: Some("Main".to_string()),
                codec_tag: None,
                language: Some("eng".to_string()),
                title: Some("Main video".to_string()),
                comment: None,
                bit_rate: Some(2_000_000),
                width: Some(1920),
                height: Some(1080),
                aspect_ratio: Some("16:9".to_string()),
                average_frame_rate: Some(24.0),
                real_frame_rate: Some(24.0),
                reference_frame_rate: None,
                channels: None,
                channel_layout: None,
                sample_rate: None,
                bit_depth: Some(8),
                ref_frames: Some(4),
                is_interlaced: false,
                is_avc: Some(true),
                is_anamorphic: Some(false),
                pixel_format: Some("yuv420p".to_string()),
                level: Some(40),
                color_range: None,
                color_space: Some("bt709".to_string()),
                color_transfer: Some("bt709".to_string()),
                color_primaries: Some("bt709".to_string()),
                time_base: Some("1/24000".to_string()),
                codec_time_base: None,
                nal_length_size: None,
                rotation: Some(0),
                video_range: Some("SDR".to_string()),
                video_range_type: None,
                hdr10_plus_present_flag: Some(false),
                is_default: true,
                is_forced: false,
                is_hearing_impaired: false,
                is_original: Some(true),
            }],
        }
    }

    fn attachment_probe_stream() -> ProbedStream {
        ProbedStream {
            stream_index: 5,
            stream_type: "Attachment".to_string(),
            codec: Some("ttf".to_string()),
            profile: None,
            codec_tag: None,
            language: None,
            title: Some("Font.ttf".to_string()),
            comment: Some("subtitle font".to_string()),
            bit_rate: None,
            width: None,
            height: None,
            aspect_ratio: None,
            average_frame_rate: None,
            real_frame_rate: None,
            reference_frame_rate: None,
            channels: None,
            channel_layout: None,
            sample_rate: None,
            bit_depth: None,
            ref_frames: None,
            is_interlaced: false,
            is_avc: None,
            is_anamorphic: None,
            pixel_format: None,
            level: None,
            color_range: None,
            color_space: None,
            color_transfer: None,
            color_primaries: None,
            time_base: None,
            codec_time_base: None,
            nal_length_size: None,
            rotation: None,
            video_range: None,
            video_range_type: None,
            hdr10_plus_present_flag: None,
            is_default: false,
            is_forced: false,
            is_hearing_impaired: false,
            is_original: None,
        }
    }

    async fn media_item_row(db: &sea_orm::DatabaseConnection, path: &str) -> media_items::Model {
        MediaItems::find()
            .filter(media_items::Column::Path.eq(path))
            .one(db)
            .await
            .unwrap()
            .unwrap()
    }

    async fn media_stream_row(
        db: &sea_orm::DatabaseConnection,
        item_id: &str,
    ) -> media_streams::Model {
        MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq(item_id))
            .filter(media_streams::Column::StreamIndex.eq(0_i64))
            .filter(media_streams::Column::IsExternal.eq(0_i64))
            .one(db)
            .await
            .unwrap()
            .unwrap()
    }
}
