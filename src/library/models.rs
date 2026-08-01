use std::{cmp::Reverse, sync::OnceLock};

use serde_json::{Map, Value as JsonValue, json};

use crate::{
    db::row_ext::QueryResultExt,
    library::photo::PhotoMetadata,
    util::{unix_to_jellyfin_date, yyyy_mm_dd_to_jellyfin_date},
};

#[derive(Clone, Debug, Default)]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    pub path: String,
    pub library_id: String,
    pub collection_type: String,
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
    pub lock_data: bool,
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
    pub remote_trailers: Vec<JsonValue>,
    pub production_locations: Vec<String>,
    pub production_year: Option<i64>,
    pub premiere_date: Option<String>,
    pub end_date: Option<String>,
    pub runtime_ticks: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub has_subtitles: bool,
    pub photo_metadata: PhotoMetadata,
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
    pub created_at: i64,
    pub modified_at: i64,
    pub is_public: bool,
    pub is_favorite: bool,
    pub played: bool,
    pub playback_position_ticks: i64,
    pub played_percentage: Option<f64>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub image_tags: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubtitlePlaybackMode {
    Default,
    Smart,
    Always,
    OnlyForced,
    None,
}

impl SubtitlePlaybackMode {
    pub fn from_jellyfin_value(value: Option<&str>) -> Self {
        match value
            .unwrap_or("Default")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "none" => Self::None,
            "smart" => Self::Smart,
            "always" => Self::Always,
            "onlyforced" | "only forced" | "forced" => Self::OnlyForced,
            _ => Self::Default,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaStreamSelectionPreferences {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    pub subtitle_mode: SubtitlePlaybackMode,
    pub play_default_audio_track: bool,
    pub remember_audio_selections: bool,
    pub remember_subtitle_selections: bool,
    pub remembered_audio_stream_index: Option<i64>,
    pub remembered_subtitle_stream_index: Option<i64>,
}

impl Default for MediaStreamSelectionPreferences {
    fn default() -> Self {
        Self {
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            subtitle_mode: SubtitlePlaybackMode::Default,
            play_default_audio_track: true,
            remember_audio_selections: true,
            remember_subtitle_selections: true,
            remembered_audio_stream_index: None,
            remembered_subtitle_stream_index: None,
        }
    }
}

impl MediaItem {
    pub fn from_query_result(row: &sea_orm::QueryResult) -> Result<Self, sea_orm::DbErr> {
        Ok(Self {
            id: row.get_str("id")?,
            title: row.get_str("title")?,
            path: row.get_str("path")?,
            library_id: row.get_str("library_id")?,
            collection_type: row.get_str("collection_type").unwrap_or_default(),
            parent_id: row.get_str("parent_id")?,
            item_type: row.get_str("item_type")?,
            extra_type: row.get_opt_str("extra_type").ok().flatten(),
            video_type: row.get_opt_str("video_type").ok().flatten(),
            iso_type: row.get_opt_str("iso_type").ok().flatten(),
            video_3d_format: row.get_opt_str("video_3d_format").ok().flatten(),
            is_folder: row.get_bool_from_i64("is_folder")?,
            container: row.get_opt_str("container")?,
            overview: row.get_opt_str("overview")?,
            official_rating: row.get_opt_str("official_rating")?,
            custom_rating: row.get_opt_str("custom_rating").ok().flatten(),
            extended_video_type: row.get_opt_str("extended_video_type")?,
            original_title: row.get_opt_str("original_title").ok().flatten(),
            sort_name: row.get_opt_str("sort_name").ok().flatten(),
            forced_sort_name: row.get_opt_str("forced_sort_name").ok().flatten(),
            lock_data: row.get_bool_from_i64("lock_data").unwrap_or(false),
            locked_fields: row
                .get_opt_str("locked_fields")
                .ok()
                .flatten()
                .map(|value| parse_locked_fields(&value))
                .unwrap_or_default(),
            tagline: row.get_opt_str("tagline").ok().flatten(),
            collection_name: row.get_opt_str("collection_name").ok().flatten(),
            original_language: row.get_opt_str("original_language").ok().flatten(),
            preferred_metadata_language: row
                .get_opt_str("preferred_metadata_language")
                .ok()
                .flatten(),
            preferred_metadata_country_code: row
                .get_opt_str("preferred_metadata_country_code")
                .ok()
                .flatten(),
            series_status: row.get_opt_str("series_status").ok().flatten(),
            air_days: row
                .get_opt_str("air_days")
                .ok()
                .flatten()
                .map(|value| parse_string_vec(&value))
                .unwrap_or_default(),
            air_time: row.get_opt_str("air_time").ok().flatten(),
            home_page_url: row.get_opt_str("home_page_url").ok().flatten(),
            remote_trailers: row
                .get_opt_str("remote_trailers")
                .ok()
                .flatten()
                .and_then(|value| serde_json::from_str::<Vec<JsonValue>>(&value).ok())
                .unwrap_or_default(),
            production_locations: row
                .get_opt_str("production_locations")
                .ok()
                .flatten()
                .map(|value| parse_string_vec(&value))
                .unwrap_or_default(),
            production_year: row.get_opt_i64("production_year")?,
            premiere_date: row.get_opt_str("premiere_date").ok().flatten(),
            end_date: row.get_opt_str("end_date").ok().flatten(),
            runtime_ticks: row.get_opt_i64("runtime_ticks")?,
            aspect_ratio: row.get_opt_str("aspect_ratio").ok().flatten(),
            width: row.get_opt_i64("width").ok().flatten(),
            height: row.get_opt_i64("height").ok().flatten(),
            has_subtitles: row.get_bool_from_i64("has_subtitles").unwrap_or(false),
            photo_metadata: PhotoMetadata::from_storage(
                row.get_opt_str("photo_metadata").ok().flatten().as_deref(),
            ),
            display_order: row.get_opt_str("display_order").ok().flatten(),
            size_bytes: row.get_opt_i64("size_bytes")?,
            season_number: row.get_opt_i64("season_number").ok().flatten(),
            episode_number: row.get_opt_i64("episode_number").ok().flatten(),
            episode_number_end: row.get_opt_i64("episode_number_end").ok().flatten(),
            airs_before_episode_number: row
                .get_opt_i64("airs_before_episode_number")
                .ok()
                .flatten(),
            airs_after_season_number: row.get_opt_i64("airs_after_season_number").ok().flatten(),
            airs_before_season_number: row.get_opt_i64("airs_before_season_number").ok().flatten(),
            series_name: row.get_opt_str("series_name").ok().flatten(),
            community_rating: row.get_f64("community_rating").ok().flatten(),
            critic_rating: row.get_f64("critic_rating").ok().flatten(),
            created_at: row.get_i64("created_at")?,
            modified_at: row.get_i64("modified_at")?,
            is_public: row.get_bool_from_i64("is_public").unwrap_or(true),
            is_favorite: row.get_bool_from_i64("is_favorite")?,
            played: row.get_bool_from_i64("played")?,
            playback_position_ticks: row.get_i64("playback_position_ticks").unwrap_or(0),
            played_percentage: row.get_f64("played_percentage").ok().flatten(),
            play_count: row.get_i64("play_count").unwrap_or(0),
            last_played_at: row.get_opt_i64("last_played_at").ok().flatten(),
            image_tags: None,
        })
    }

    pub fn to_jellyfin_json(&self) -> JsonValue {
        let media_sources = if self.is_folder && self.video_type.is_none() {
            json!([])
        } else {
            json!([media_source_json(self)])
        };

        let media_type = match self.item_type.as_str() {
            "Audio" | "AudioBook" => Some("Audio"),
            "Movie" | "Episode" | "Video" | "Trailer" | "MusicVideo" => Some("Video"),
            "Photo" => Some("Photo"),
            "Series" | "Season" => Some("Unknown"),
            _ => None,
        };

        let location_type = if !self.path.is_empty() {
            Some("FileSystem")
        } else {
            None
        };

        let mut map = Map::new();
        map.insert("Name".into(), JsonValue::String(self.title.clone()));
        map.insert("Id".into(), JsonValue::String(self.id.clone()));
        map.insert("Type".into(), JsonValue::String(self.item_type.clone()));
        map.insert(
            "ServerId".into(),
            JsonValue::String("jellyfin-rs".to_string()),
        );
        map.insert(
            "Etag".into(),
            JsonValue::String(format!("{}-{}", self.id, self.modified_at)),
        );
        map.insert("Path".into(), JsonValue::String(self.path.clone()));
        map.insert(
            "LibraryId".into(),
            JsonValue::String(self.library_id.clone()),
        );
        map.insert("ParentId".into(), JsonValue::String(self.parent_id.clone()));
        map.insert("RunTimeTicks".into(), opt_i64(self.runtime_ticks));
        map.insert("Container".into(), opt_str(&self.container));
        map.insert("MediaType".into(), opt_str_val(media_type));
        map.insert("LocationType".into(), opt_str_val(location_type));
        map.insert("IsFolder".into(), JsonValue::Bool(self.is_folder));
        map.insert("Overview".into(), opt_str(&self.overview));
        map.insert("OfficialRating".into(), opt_str(&self.official_rating));
        map.insert("CustomRating".into(), opt_str(&self.custom_rating));
        map.insert(
            "ExtendedVideoType".into(),
            opt_str(&self.extended_video_type),
        );
        map.insert("OriginalTitle".into(), opt_str(&self.original_title));
        map.insert("CollectionName".into(), opt_str(&self.collection_name));
        map.insert("HomePageUrl".into(), opt_str(&self.home_page_url));
        map.insert("Status".into(), opt_str(&self.series_status));
        map.insert("AirDays".into(), json!(self.air_days));
        map.insert("AirTime".into(), opt_str(&self.air_time));
        map.insert("ProductionYear".into(), opt_i64(self.production_year));
        let premiere_date = self
            .premiere_date
            .as_deref()
            .and_then(yyyy_mm_dd_to_jellyfin_date);
        map.insert("PremiereDate".into(), opt_str(&premiere_date));
        let end_date = self
            .end_date
            .as_deref()
            .and_then(yyyy_mm_dd_to_jellyfin_date);
        map.insert("EndDate".into(), opt_str(&end_date));
        let index_number = match self.item_type.as_str() {
            "Season" => self.season_number,
            "Episode" | "Audio" | "AudioBook" | "Book" => self.episode_number,
            _ => None,
        };
        let parent_index_number = match self.item_type.as_str() {
            "Episode" | "Audio" | "AudioBook" | "Book" => self.season_number,
            _ => None,
        };
        map.insert("IndexNumber".into(), opt_i64(index_number));
        map.insert("ParentIndexNumber".into(), opt_i64(parent_index_number));
        map.insert(
            "IndexNumberEnd".into(),
            opt_i64(
                (self.item_type == "Episode")
                    .then_some(self.episode_number_end)
                    .flatten(),
            ),
        );
        map.insert(
            "AirsBeforeEpisodeNumber".into(),
            opt_i64(self.airs_before_episode_number),
        );
        map.insert(
            "AirsAfterSeasonNumber".into(),
            opt_i64(self.airs_after_season_number),
        );
        map.insert(
            "AirsBeforeSeasonNumber".into(),
            opt_i64(self.airs_before_season_number),
        );
        map.insert("SeriesName".into(), opt_str(&self.series_name));
        map.insert("Number".into(), JsonValue::Null);
        map.insert(
            "SortName".into(),
            JsonValue::String(
                self.sort_name
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| self.title.clone()),
            ),
        );
        map.insert("ForcedSortName".into(), opt_str(&self.forced_sort_name));
        map.insert("ProviderIds".into(), json!({}));
        map.insert("LockData".into(), JsonValue::Bool(self.lock_data));
        map.insert("LockedFields".into(), json!(self.locked_fields));
        map.insert("CanDelete".into(), JsonValue::Bool(true));
        map.insert("CanDownload".into(), JsonValue::Bool(true));
        map.insert("HasSubtitles".into(), JsonValue::Bool(self.has_subtitles));
        if self.item_type == "Photo" {
            let photo = &self.photo_metadata;
            map.insert("CameraMake".into(), opt_str(&photo.camera_make));
            map.insert("CameraModel".into(), opt_str(&photo.camera_model));
            map.insert("Software".into(), opt_str(&photo.software));
            map.insert("ExposureTime".into(), opt_f64(photo.exposure_time));
            map.insert("FocalLength".into(), opt_f64(photo.focal_length));
            map.insert("ImageOrientation".into(), opt_str(&photo.image_orientation));
            map.insert("Aperture".into(), opt_f64(photo.aperture));
            map.insert("ShutterSpeed".into(), opt_f64(photo.shutter_speed));
            map.insert("Latitude".into(), opt_f64(photo.latitude));
            map.insert("Longitude".into(), opt_f64(photo.longitude));
            map.insert("Altitude".into(), opt_f64(photo.altitude));
            map.insert("IsoSpeedRating".into(), opt_i64(photo.iso_speed_rating));
            if let Some(date_taken) = photo.date_taken_unix {
                map.insert(
                    "PremiereDate".into(),
                    JsonValue::String(unix_to_jellyfin_date(date_taken)),
                );
            }
        }
        map.insert("HasLyrics".into(), JsonValue::Null);
        map.insert(
            "SupportsResume".into(),
            JsonValue::Bool(supports_resume(self)),
        );
        map.insert("SupportsSync".into(), JsonValue::Bool(false));
        map.insert("DisplaySpecialsWithSeasons".into(), JsonValue::Bool(false));
        map.insert("PlayAccess".into(), JsonValue::String("Full".to_string()));
        map.insert("Size".into(), opt_i64(self.size_bytes));
        map.insert("Genres".into(), json!([]));
        map.insert("GenreItems".into(), json!([]));
        map.insert("Tags".into(), json!([]));
        map.insert("TagItems".into(), json!([]));
        map.insert(
            "Taglines".into(),
            self.tagline
                .as_ref()
                .filter(|tagline| !tagline.trim().is_empty())
                .map(|tagline| json!([tagline]))
                .unwrap_or_else(|| json!([])),
        );
        map.insert("Studios".into(), json!([]));
        map.insert("People".into(), json!([]));
        map.insert(
            "ProductionLocations".into(),
            json!(self.production_locations),
        );
        map.insert("VideoType".into(), opt_str(&resolved_video_type(self)));
        map.insert("IsoType".into(), opt_str(&resolved_iso_type(self)));
        map.insert("Video3DFormat".into(), opt_str(&self.video_3d_format));
        map.insert("AspectRatio".into(), opt_str(&self.aspect_ratio));
        map.insert("Width".into(), opt_i64(self.width));
        map.insert("Height".into(), opt_i64(self.height));
        map.insert(
            "IsHD".into(),
            self.height
                .map(|height| JsonValue::Bool(height >= 720))
                .unwrap_or(JsonValue::Null),
        );
        map.insert(
            "CollectionType".into(),
            if self.collection_type.is_empty() {
                JsonValue::Null
            } else {
                JsonValue::String(self.collection_type.clone())
            },
        );
        map.insert(
            "DisplayPreferencesId".into(),
            JsonValue::String(self.id.clone()),
        );
        map.insert(
            "PreferredMetadataLanguage".into(),
            opt_str(&self.preferred_metadata_language),
        );
        map.insert(
            "PreferredMetadataCountryCode".into(),
            opt_str(&self.preferred_metadata_country_code),
        );
        map.insert("UserData".into(), build_user_data(self));
        map.insert(
            "DateCreated".into(),
            JsonValue::String(unix_to_jellyfin_date(self.created_at)),
        );
        map.insert(
            "DateLastMediaAdded".into(),
            JsonValue::String(unix_to_jellyfin_date(self.modified_at)),
        );
        let image_tags = self.image_tags.clone().unwrap_or_else(|| json!({}));
        if let Some(primary_image_tag) = image_tags
            .get("Primary")
            .and_then(JsonValue::as_str)
            .filter(|tag| !tag.is_empty())
        {
            map.insert(
                "PrimaryImageTag".into(),
                JsonValue::String(primary_image_tag.to_string()),
            );
        }
        map.insert("ImageTags".into(), image_tags);

        // Build BackdropImageTags from image_tags
        let backdrop_tags: Vec<JsonValue> = self
            .image_tags
            .as_ref()
            .and_then(|tags| tags.get("Backdrop"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|etag| vec![JsonValue::String(etag.to_string())])
            .unwrap_or_default();
        map.insert("BackdropImageTags".into(), JsonValue::Array(backdrop_tags));
        map.insert("ScreenshotImageTags".into(), json!([]));
        let primary_image_aspect_ratio = if self.item_type == "Photo" {
            self.width
                .zip(self.height)
                .and_then(|(mut width, mut height)| {
                    if matches!(
                        self.photo_metadata.image_orientation.as_deref(),
                        Some("LeftBottom" | "LeftTop" | "RightBottom" | "RightTop")
                    ) {
                        std::mem::swap(&mut width, &mut height);
                    }
                    (width > 0 && height > 0).then_some(width as f64 / height as f64)
                })
        } else {
            None
        };
        map.insert(
            "PrimaryImageAspectRatio".into(),
            opt_f64(primary_image_aspect_ratio),
        );
        map.insert("ImageBlurHashes".into(), json!({}));
        map.insert("ParentLogoItemId".into(), JsonValue::Null);
        map.insert("ParentLogoImageTag".into(), JsonValue::Null);
        map.insert("ParentBackdropItemId".into(), JsonValue::Null);
        map.insert("ParentBackdropImageTags".into(), json!([]));
        map.insert("ParentThumbItemId".into(), JsonValue::Null);
        map.insert("ParentThumbImageTag".into(), JsonValue::Null);
        map.insert("ParentArtItemId".into(), JsonValue::Null);
        map.insert("ParentArtImageTag".into(), JsonValue::Null);
        map.insert("ParentPrimaryImageItemId".into(), JsonValue::Null);
        map.insert("ParentPrimaryImageTag".into(), JsonValue::Null);
        map.insert("MediaSources".into(), media_sources);
        map.insert("MediaStreams".into(), JsonValue::Null);
        map.insert(
            "MediaSourceCount".into(),
            JsonValue::Number(serde_json::Number::from(
                if self.is_folder && self.video_type.is_none() {
                    0
                } else {
                    1
                },
            )),
        );
        map.insert("Chapters".into(), json!([]));
        map.insert("Trickplay".into(), json!({}));
        map.insert("LocalTrailerCount".into(), JsonValue::Number(0.into()));
        map.insert("RemoteTrailers".into(), json!(self.remote_trailers));
        map.insert("SpecialFeatureCount".into(), JsonValue::Number(0.into()));
        map.insert("ExtraType".into(), opt_str(&self.extra_type));
        map.insert(
            "IsPlaceHolder".into(),
            JsonValue::Bool(crate::library::classify::is_video_stub(
                std::path::Path::new(&self.path),
            )),
        );
        map.insert("ChildCount".into(), JsonValue::Number(0.into()));
        map.insert("RecursiveItemCount".into(), JsonValue::Number(0.into()));
        map.insert("CumulativeRunTimeTicks".into(), JsonValue::Null);
        map.insert("PartCount".into(), JsonValue::Null);
        map.insert("EnableMediaSourceDisplay".into(), JsonValue::Bool(false));
        map.insert("IsMovie".into(), JsonValue::Null);
        map.insert("IsSeries".into(), JsonValue::Null);
        map.insert("IsSports".into(), JsonValue::Null);
        map.insert("IsLive".into(), JsonValue::Null);
        map.insert("IsNews".into(), JsonValue::Null);
        map.insert("IsKids".into(), JsonValue::Null);
        map.insert("IsPremiere".into(), JsonValue::Null);
        map.insert("PlaylistItemId".into(), JsonValue::Null);
        map.insert("SourceType".into(), JsonValue::Null);
        map.insert("CommunityRating".into(), opt_f64(self.community_rating));
        map.insert("CriticRating".into(), opt_f64(self.critic_rating));
        map.insert("ExternalUrls".into(), json!([]));
        map.insert(
            "Album".into(),
            if matches!(
                self.item_type.as_str(),
                "Audio" | "AudioBook" | "MusicVideo"
            ) {
                opt_str(&self.collection_name)
            } else {
                JsonValue::Null
            },
        );
        map.insert(
            "AlbumId".into(),
            if self.item_type == "Audio" && !self.parent_id.is_empty() {
                JsonValue::String(self.parent_id.clone())
            } else {
                JsonValue::Null
            },
        );
        map.insert("AlbumPrimaryImageTag".into(), JsonValue::Null);
        map.insert("AlbumArtist".into(), JsonValue::Null);
        map.insert("AlbumArtists".into(), json!([]));
        map.insert("Artists".into(), json!([]));
        map.insert("ArtistItems".into(), json!([]));
        map.insert("DisplayOrder".into(), opt_str(&self.display_order));
        map.insert("OriginalLanguage".into(), opt_str(&self.original_language));
        map.insert("EpisodeCount".into(), JsonValue::Null);
        map.insert("SeasonCount".into(), JsonValue::Null);
        map.insert("MovieCount".into(), JsonValue::Null);
        map.insert("SeriesCount".into(), JsonValue::Null);
        map.insert("SongCount".into(), JsonValue::Null);
        map.insert("AlbumCount".into(), JsonValue::Null);
        map.insert("ArtistCount".into(), JsonValue::Null);
        map.insert("MusicVideoCount".into(), JsonValue::Null);
        map.insert("TrailerCount".into(), JsonValue::Null);
        map.insert("ProgramCount".into(), JsonValue::Null);
        JsonValue::Object(map)
    }
}

// Helper functions for Map-based JSON construction

fn opt_i64(val: Option<i64>) -> JsonValue {
    val.map(|n| JsonValue::Number(serde_json::Number::from(n)))
        .unwrap_or(JsonValue::Null)
}

fn opt_f64(val: Option<f64>) -> JsonValue {
    val.and_then(|n| serde_json::Number::from_f64(n).map(JsonValue::Number))
        .unwrap_or(JsonValue::Null)
}

fn opt_bool(val: Option<bool>) -> JsonValue {
    val.map(JsonValue::Bool).unwrap_or(JsonValue::Null)
}

fn opt_str(val: &Option<String>) -> JsonValue {
    val.as_ref()
        .map(|s| JsonValue::String(s.clone()))
        .unwrap_or(JsonValue::Null)
}

fn opt_str_val(val: Option<&str>) -> JsonValue {
    val.map(|s| JsonValue::String(s.to_string()))
        .unwrap_or(JsonValue::Null)
}

fn resolved_video_type(item: &MediaItem) -> Option<String> {
    item.video_type
        .clone()
        .or_else(|| inferred_video_type(&item.path, &item.item_type))
}

fn resolved_iso_type(item: &MediaItem) -> Option<String> {
    item.iso_type
        .clone()
        .or_else(|| inferred_iso_type(&item.path))
}

fn inferred_video_type(path: &str, item_type: &str) -> Option<String> {
    if !matches!(item_type, "Video" | "Movie" | "Episode") {
        return None;
    }
    let path = std::path::Path::new(path);
    crate::library::classify::file_video_type(path)
        .unwrap_or("VideoFile")
        .to_string()
        .into()
}

fn inferred_iso_type(path: &str) -> Option<String> {
    crate::library::classify::iso_type_from_name(std::path::Path::new(path))
        .map(ToString::to_string)
}

fn parse_locked_fields(value: &str) -> Vec<String> {
    parse_string_vec(value)
}

fn parse_string_vec(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value)
        .unwrap_or_else(|_| {
            value
                .split(['|', ',', ';'])
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .into_iter()
        .map(|field| field.trim().to_string())
        .filter(|field| !field.is_empty())
        .collect()
}

fn build_user_data(item: &MediaItem) -> JsonValue {
    let mut map = Map::new();
    map.insert("ItemId".into(), JsonValue::String(item.id.clone()));
    map.insert("Key".into(), JsonValue::String(item.id.clone()));
    map.insert("Rating".into(), JsonValue::Null);
    map.insert("Played".into(), JsonValue::Bool(item.played));
    map.insert(
        "PlayCount".into(),
        JsonValue::Number(serde_json::Number::from(item.play_count)),
    );
    map.insert(
        "LastPlayedDate".into(),
        item.last_played_at
            .map(|ts| JsonValue::String(unix_to_jellyfin_date(ts)))
            .unwrap_or(JsonValue::Null),
    );
    let playback_position_ticks = if item.played {
        0
    } else {
        item.playback_position_ticks.max(0)
    };
    map.insert(
        "PlaybackPositionTicks".into(),
        JsonValue::Number(serde_json::Number::from(playback_position_ticks)),
    );
    let played_pct = if item.played {
        JsonValue::Null
    } else {
        item.played_percentage
            .or_else(|| {
                item.runtime_ticks
                    .filter(|runtime| *runtime > 0 && playback_position_ticks > 0)
                    .map(|runtime| {
                        (playback_position_ticks as f64 / runtime as f64 * 100.0).min(100.0)
                    })
            })
            .and_then(|f| serde_json::Number::from_f64(f).map(JsonValue::Number))
            .unwrap_or(JsonValue::Null)
    };
    map.insert("PlayedPercentage".into(), played_pct);
    map.insert("IsFavorite".into(), JsonValue::Bool(item.is_favorite));
    map.insert("Likes".into(), JsonValue::Null);
    map.insert("UnplayedItemCount".into(), JsonValue::Null);
    JsonValue::Object(map)
}

fn supports_resume(item: &MediaItem) -> bool {
    (!item.is_folder || item.video_type.is_some())
        && matches!(
            item.item_type.as_str(),
            "Audio"
                | "AudioBook"
                | "Book"
                | "Movie"
                | "Episode"
                | "Video"
                | "Trailer"
                | "MusicVideo"
        )
}

pub fn media_source_json(item: &MediaItem) -> JsonValue {
    media_source_json_with_streams(item, Vec::new())
}

pub fn media_source_json_with_streams(
    item: &MediaItem,
    media_streams: Vec<JsonValue>,
) -> JsonValue {
    let (media_streams, media_attachments) =
        split_media_streams_and_attachments(&item.id, &item.id, media_streams);
    let (default_audio_stream_index, default_subtitle_stream_index) =
        default_media_stream_indexes(&media_streams);
    let bitrate = media_source_bitrate(item.size_bytes, item.runtime_ticks, &media_streams);
    let remote_target = remote_strm_target(&item.path);
    let direct_stream_url = match remote_target.clone() {
        Some(target) => target,
        None => match item.item_type.as_str() {
            "Audio" => format!("/Audio/{}/universal", item.id),
            _ => format!("/Videos/{}/stream", item.id),
        },
    };
    let (protocol, path, is_remote) =
        media_source_protocol_path_with_target(&item.path, remote_target.as_deref());
    let video_type = opt_str(&resolved_video_type(item));
    let iso_type = opt_str(&resolved_iso_type(item));

    let mut map = Map::new();
    map.insert("Id".into(), JsonValue::String(item.id.clone()));
    map.insert("Name".into(), JsonValue::String(item.title.clone()));
    map.insert("Type".into(), JsonValue::String("Default".to_string()));
    map.insert("Protocol".into(), JsonValue::String(protocol));
    map.insert("Path".into(), JsonValue::String(path));
    map.insert("Container".into(), opt_str(&item.container));
    map.insert("Size".into(), opt_i64(item.size_bytes));
    map.insert("RunTimeTicks".into(), opt_i64(item.runtime_ticks));
    map.insert("VideoType".into(), video_type);
    map.insert("IsoType".into(), iso_type);
    map.insert("Video3DFormat".into(), opt_str(&item.video_3d_format));
    map.insert("Timestamp".into(), JsonValue::Null);
    map.insert("Bitrate".into(), opt_i64(bitrate));
    map.insert("FallbackMaxStreamingBitrate".into(), JsonValue::Null);
    map.insert("SupportsDirectPlay".into(), JsonValue::Bool(true));
    map.insert("SupportsDirectStream".into(), JsonValue::Bool(true));
    map.insert("SupportsTranscoding".into(), JsonValue::Bool(false));
    map.insert("AddApiKeyToDirectStreamUrl".into(), JsonValue::Bool(false));
    map.insert("SupportsProbing".into(), JsonValue::Bool(true));
    map.insert("IsInfiniteStream".into(), JsonValue::Bool(false));
    map.insert("IsRemote".into(), JsonValue::Bool(is_remote));
    map.insert("RequiresOpening".into(), JsonValue::Bool(false));
    map.insert("RequiresClosing".into(), JsonValue::Bool(false));
    map.insert("RequiresLooping".into(), JsonValue::Bool(false));
    map.insert("ReadAtNativeFramerate".into(), JsonValue::Bool(false));
    map.insert("IgnoreDts".into(), JsonValue::Bool(false));
    map.insert("IgnoreIndex".into(), JsonValue::Bool(false));
    map.insert("GenPtsInput".into(), JsonValue::Bool(false));
    map.insert("SupportsSegmentSeeking".into(), JsonValue::Bool(true));
    map.insert("HasSegments".into(), JsonValue::Bool(false));
    map.insert("MediaStreams".into(), JsonValue::Array(media_streams));
    map.insert(
        "MediaAttachments".into(),
        JsonValue::Array(media_attachments),
    );
    map.insert(
        "DefaultAudioStreamIndex".into(),
        opt_i64(default_audio_stream_index),
    );
    map.insert(
        "DefaultSubtitleStreamIndex".into(),
        opt_i64(default_subtitle_stream_index),
    );
    map.insert("Formats".into(), JsonValue::Array(vec![]));
    map.insert("RequiredHttpHeaders".into(), JsonValue::Object(Map::new()));
    map.insert("TranscodingUrl".into(), JsonValue::Null);
    map.insert("TranscodingContainer".into(), JsonValue::Null);
    map.insert("TranscodingSubProtocol".into(), JsonValue::Null);
    map.insert(
        "DirectStreamUrl".into(),
        JsonValue::String(direct_stream_url),
    );
    map.insert("EncoderPath".into(), JsonValue::Null);
    map.insert("EncoderProtocol".into(), JsonValue::Null);
    map.insert("BufferMs".into(), JsonValue::Null);
    map.insert("AnalyzeDurationMs".into(), JsonValue::Null);
    map.insert(
        "UseMostCompatibleTranscodingProfile".into(),
        JsonValue::Bool(false),
    );
    map.insert("LiveStreamId".into(), JsonValue::Null);
    map.insert("OpenToken".into(), JsonValue::Null);
    map.insert("ETag".into(), JsonValue::String(item.id.clone()));
    JsonValue::Object(map)
}

fn media_source_protocol_path_with_target(
    path: &str,
    remote_target: Option<&str>,
) -> (String, String, bool) {
    match remote_target {
        Some(target) => ("Http".to_string(), target.to_string(), true),
        None => ("File".to_string(), path.to_string(), false),
    }
}

fn remote_strm_target(path: &str) -> Option<String> {
    let path = std::path::Path::new(path);
    crate::strm::is_strm_path(path)
        .then(|| crate::strm::read_strm_target(path).ok())
        .flatten()
        .filter(|target| crate::strm::is_remote_url(target))
        .map(|target| rewrite_public_strm_target(&target))
}

static STRM_PUBLIC_BASE: OnceLock<Option<String>> = OnceLock::new();

fn rewrite_public_strm_target(target: &str) -> String {
    let public_base = STRM_PUBLIC_BASE.get_or_init(|| {
        std::env::var("JELLYFIN_RS_STRM_PUBLIC_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
    });
    rewrite_public_strm_target_with_base(target, public_base.as_deref())
}

fn rewrite_public_strm_target_with_base(target: &str, public_base: Option<&str>) -> String {
    let Some(public_base) = public_base else {
        return target.to_string();
    };
    for internal_base in ["http://127.0.0.1:8024", "http://localhost:8024"] {
        if let Some(path_and_query) = target.strip_prefix(internal_base) {
            return format!("{public_base}{path_and_query}");
        }
    }
    target.to_string()
}

fn split_media_streams_and_attachments(
    item_id: &str,
    media_source_id: &str,
    streams: Vec<JsonValue>,
) -> (Vec<JsonValue>, Vec<JsonValue>) {
    let mut media_streams = Vec::new();
    let mut attachments = Vec::new();
    for stream in streams {
        if stream
            .get("Type")
            .and_then(JsonValue::as_str)
            .is_some_and(|stream_type| stream_type.eq_ignore_ascii_case("Attachment"))
        {
            if let Some(attachment) = media_attachment_json(item_id, media_source_id, &stream) {
                attachments.push(attachment);
            }
        } else {
            media_streams.push(stream);
        }
    }
    (media_streams, attachments)
}

fn default_media_stream_indexes(media_streams: &[JsonValue]) -> (Option<i64>, Option<i64>) {
    media_stream_indexes_for_preferences(media_streams, &MediaStreamSelectionPreferences::default())
}

pub fn apply_media_source_user_preferences(
    media_source: &mut JsonValue,
    preferences: &MediaStreamSelectionPreferences,
) {
    let Some(object) = media_source.as_object_mut() else {
        return;
    };
    let Some(media_streams) = object
        .get_mut("MediaStreams")
        .and_then(JsonValue::as_array_mut)
    else {
        return;
    };

    let (default_audio_stream_index, default_subtitle_stream_index) =
        media_stream_indexes_for_preferences(media_streams, preferences);
    apply_subtitle_stream_scores(media_streams, preferences, default_audio_stream_index);
    object.insert(
        "DefaultAudioStreamIndex".to_string(),
        opt_i64(default_audio_stream_index),
    );
    object.insert(
        "DefaultSubtitleStreamIndex".to_string(),
        opt_i64(default_subtitle_stream_index),
    );
}

fn media_stream_indexes_for_preferences(
    media_streams: &[JsonValue],
    preferences: &MediaStreamSelectionPreferences,
) -> (Option<i64>, Option<i64>) {
    let default_audio_stream_index = default_audio_stream_index(media_streams, preferences);
    let audio_language = default_audio_stream_index
        .and_then(|index| stream_by_type_and_index(media_streams, "Audio", index))
        .and_then(|stream| json_str(stream, "Language"));
    let default_subtitle_stream_index =
        default_subtitle_stream_index(media_streams, preferences, audio_language);
    (default_audio_stream_index, default_subtitle_stream_index)
}

fn default_audio_stream_index(
    media_streams: &[JsonValue],
    preferences: &MediaStreamSelectionPreferences,
) -> Option<i64> {
    if preferences.remember_audio_selections {
        if let Some(index) = preferences
            .remembered_audio_stream_index
            .filter(|index| stream_by_type_and_index(media_streams, "Audio", *index).is_some())
        {
            return Some(index);
        }
    }

    let mut audio_streams = media_streams
        .iter()
        .filter(|stream| stream_type_is(stream, "Audio"))
        .collect::<Vec<_>>();
    audio_streams.sort_by_key(|stream| {
        (
            Reverse(stream_score(stream, &preferences.audio_languages)),
            json_i64(stream, "Index").unwrap_or(i64::MAX),
        )
    });
    if preferences.play_default_audio_track {
        if let Some(index) = audio_streams
            .iter()
            .find(|stream| json_bool(stream, "IsDefault"))
            .and_then(|stream| json_i64(stream, "Index"))
        {
            return Some(index);
        }
    }
    audio_streams
        .first()
        .and_then(|stream| json_i64(stream, "Index"))
}

fn default_subtitle_stream_index(
    media_streams: &[JsonValue],
    preferences: &MediaStreamSelectionPreferences,
    audio_track_language: Option<&str>,
) -> Option<i64> {
    if preferences.subtitle_mode == SubtitlePlaybackMode::None {
        return None;
    }

    if preferences.remember_subtitle_selections {
        if let Some(index) = preferences.remembered_subtitle_stream_index {
            if index == -1 || stream_by_type_and_index(media_streams, "Subtitle", index).is_some() {
                return Some(index);
            }
        }
    }

    let mut subtitle_streams = media_streams
        .iter()
        .filter(|stream| stream_type_is(stream, "Subtitle"))
        .collect::<Vec<_>>();
    subtitle_streams.sort_by_key(|stream| {
        (
            Reverse(json_bool(stream, "IsExternal")),
            Reverse(json_bool(stream, "IsDefault")),
            Reverse(
                !json_bool(stream, "IsForced")
                    && matches_preferred_language(
                        json_str(stream, "Language"),
                        &preferences.subtitle_languages,
                    ),
            ),
            Reverse(
                json_bool(stream, "IsForced")
                    && matches_preferred_language(
                        json_str(stream, "Language"),
                        &preferences.subtitle_languages,
                    ),
            ),
            Reverse(
                json_bool(stream, "IsForced")
                    && is_language_undefined(json_str(stream, "Language")),
            ),
            Reverse(json_bool(stream, "IsForced")),
            json_i64(stream, "Index").unwrap_or(i64::MAX),
        )
    });

    let stream = match preferences.subtitle_mode {
        SubtitlePlaybackMode::None => None,
        SubtitlePlaybackMode::Default => subtitle_streams.into_iter().find(|stream| {
            json_bool(stream, "IsExternal")
                || json_bool(stream, "IsDefault")
                || json_bool(stream, "IsForced")
        }),
        SubtitlePlaybackMode::Smart => {
            if !language_list_contains(&preferences.subtitle_languages, audio_track_language) {
                subtitle_streams.into_iter().find(|stream| {
                    matches_preferred_language(
                        json_str(stream, "Language"),
                        &preferences.subtitle_languages,
                    )
                })
            } else {
                only_forced_subtitle_stream(subtitle_streams, &preferences.subtitle_languages)
            }
        }
        SubtitlePlaybackMode::Always => subtitle_streams
            .iter()
            .copied()
            .find(|stream| {
                !json_bool(stream, "IsForced")
                    && matches_preferred_language(
                        json_str(stream, "Language"),
                        &preferences.subtitle_languages,
                    )
            })
            .or_else(|| {
                only_forced_subtitle_stream(subtitle_streams, &preferences.subtitle_languages)
            }),
        SubtitlePlaybackMode::OnlyForced => {
            only_forced_subtitle_stream(subtitle_streams, &preferences.subtitle_languages)
        }
    };

    stream.and_then(|stream| json_i64(stream, "Index"))
}

fn only_forced_subtitle_stream<'a>(
    subtitle_streams: Vec<&'a JsonValue>,
    preferred_languages: &[String],
) -> Option<&'a JsonValue> {
    subtitle_streams.into_iter().find(|stream| {
        json_bool(stream, "IsForced")
            && (matches_preferred_language(json_str(stream, "Language"), preferred_languages)
                || is_language_undefined(json_str(stream, "Language")))
    })
}

fn apply_subtitle_stream_scores(
    media_streams: &mut [JsonValue],
    preferences: &MediaStreamSelectionPreferences,
    audio_track_language: Option<i64>,
) {
    if preferences.subtitle_mode == SubtitlePlaybackMode::None {
        return;
    }
    let audio_language = audio_track_language
        .and_then(|index| stream_by_type_and_index(media_streams, "Audio", index))
        .and_then(|stream| json_str(stream, "Language"))
        .map(ToString::to_string);
    for stream in media_streams {
        if !stream_type_is(stream, "Subtitle") {
            continue;
        }
        if !subtitle_stream_matches_mode(
            stream,
            &preferences.subtitle_languages,
            &preferences.subtitle_mode,
            audio_language.as_deref(),
        ) {
            continue;
        }
        if let Some(object) = stream.as_object_mut() {
            object.insert(
                "Score".to_string(),
                JsonValue::Number(serde_json::Number::from(stream_score(
                    &JsonValue::Object(object.clone()),
                    &preferences.subtitle_languages,
                ))),
            );
        }
    }
}

fn subtitle_stream_matches_mode(
    stream: &JsonValue,
    preferred_languages: &[String],
    mode: &SubtitlePlaybackMode,
    audio_track_language: Option<&str>,
) -> bool {
    match mode {
        SubtitlePlaybackMode::None => false,
        SubtitlePlaybackMode::Default => {
            json_bool(stream, "IsExternal")
                || json_bool(stream, "IsDefault")
                || json_bool(stream, "IsForced")
        }
        SubtitlePlaybackMode::Smart => {
            if !language_list_contains(preferred_languages, audio_track_language) {
                matches_preferred_language(json_str(stream, "Language"), preferred_languages)
            } else {
                json_bool(stream, "IsForced")
                    && (matches_preferred_language(
                        json_str(stream, "Language"),
                        preferred_languages,
                    ) || is_language_undefined(json_str(stream, "Language")))
            }
        }
        SubtitlePlaybackMode::Always => {
            (!json_bool(stream, "IsForced")
                && matches_preferred_language(json_str(stream, "Language"), preferred_languages))
                || (json_bool(stream, "IsForced")
                    && (matches_preferred_language(
                        json_str(stream, "Language"),
                        preferred_languages,
                    ) || is_language_undefined(json_str(stream, "Language"))))
        }
        SubtitlePlaybackMode::OnlyForced => {
            json_bool(stream, "IsForced")
                && (matches_preferred_language(json_str(stream, "Language"), preferred_languages)
                    || is_language_undefined(json_str(stream, "Language")))
        }
    }
}

fn stream_type_is(stream: &JsonValue, expected: &str) -> bool {
    stream
        .get("Type")
        .and_then(JsonValue::as_str)
        .is_some_and(|stream_type| stream_type.eq_ignore_ascii_case(expected))
}

fn stream_by_type_and_index<'a>(
    media_streams: &'a [JsonValue],
    stream_type: &str,
    index: i64,
) -> Option<&'a JsonValue> {
    media_streams.iter().find(|stream| {
        stream_type_is(stream, stream_type) && json_i64(stream, "Index") == Some(index)
    })
}

fn json_bool(value: &JsonValue, key: &str) -> bool {
    value
        .get(key)
        .and_then(JsonValue::as_bool)
        .unwrap_or_default()
}

fn json_str<'a>(value: &'a JsonValue, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn json_i64(value: &JsonValue, key: &str) -> Option<i64> {
    value.get(key).and_then(JsonValue::as_i64)
}

fn matches_preferred_language(language: Option<&str>, preferred_languages: &[String]) -> bool {
    preferred_languages.is_empty()
        || language
            .is_some_and(|language| language_list_contains(preferred_languages, Some(language)))
}

fn language_list_contains(preferred_languages: &[String], language: Option<&str>) -> bool {
    let Some(language) = language
        .map(str::trim)
        .filter(|language| !language.is_empty())
    else {
        return false;
    };
    preferred_languages
        .iter()
        .any(|preferred| preferred.eq_ignore_ascii_case(language))
}

fn is_language_undefined(language: Option<&str>) -> bool {
    let Some(language) = language
        .map(str::trim)
        .filter(|language| !language.is_empty())
    else {
        return true;
    };
    matches!(
        language.to_ascii_lowercase().as_str(),
        "und" | "unknown" | "undetermined" | "mul" | "zxx"
    )
}

fn stream_score(stream: &JsonValue, language_preferences: &[String]) -> i64 {
    let index = json_str(stream, "Language")
        .and_then(|language| {
            language_preferences
                .iter()
                .position(|preferred| preferred.eq_ignore_ascii_case(language))
        })
        .map(|index| index as i64);
    let mut score = index.map_or(1, |index| 101 - index);
    score = (score * 10) + if json_bool(stream, "IsForced") { 2 } else { 1 };
    score = (score * 10) + if json_bool(stream, "IsDefault") { 2 } else { 1 };
    score = (score * 10)
        + if json_bool(stream, "SupportsExternalStream") {
            2
        } else {
            1
        };
    score = (score * 10)
        + if json_bool(stream, "IsTextSubtitleStream") {
            2
        } else {
            1
        };
    score = (score * 10)
        + if json_bool(stream, "IsExternal") {
            2
        } else {
            1
        };
    score
}

fn media_source_bitrate(
    size_bytes: Option<i64>,
    runtime_ticks: Option<i64>,
    media_streams: &[JsonValue],
) -> Option<i64> {
    if let (Some(size_bytes), Some(runtime_ticks)) = (size_bytes, runtime_ticks) {
        if size_bytes > 0 && runtime_ticks > 0 {
            return size_bytes
                .checked_mul(8)
                .and_then(|bits| bits.checked_mul(10_000_000))
                .map(|bits_per_ticks| bits_per_ticks / runtime_ticks)
                .filter(|bitrate| *bitrate > 0);
        }
    }

    let stream_bitrate = media_streams
        .iter()
        .filter(|stream| {
            stream
                .get("Type")
                .and_then(JsonValue::as_str)
                .is_some_and(|stream_type| {
                    stream_type.eq_ignore_ascii_case("Video")
                        || stream_type.eq_ignore_ascii_case("Audio")
                })
        })
        .filter_map(|stream| stream.get("BitRate").and_then(JsonValue::as_i64))
        .filter(|bitrate| *bitrate > 0)
        .sum::<i64>();

    (stream_bitrate > 0).then_some(stream_bitrate)
}

fn media_attachment_json(
    item_id: &str,
    media_source_id: &str,
    stream: &JsonValue,
) -> Option<JsonValue> {
    let index = stream
        .get("Index")
        .and_then(JsonValue::as_i64)
        .unwrap_or_default();
    let codec = stream.get("Codec").cloned().unwrap_or(JsonValue::Null);
    let mime_type = attachment_mime_type(codec.as_str().unwrap_or_default());
    Some(json!({
        "Codec": codec,
        "CodecTag": stream.get("CodecTag").cloned().unwrap_or(JsonValue::Null),
        "Comment": stream.get("Comment").cloned().unwrap_or(JsonValue::Null),
        "Index": index,
        "FileName": stream.get("Title").cloned().unwrap_or(JsonValue::Null),
        "MimeType": mime_type,
        "DeliveryUrl": format!("/Videos/{item_id}/{media_source_id}/Attachments/{index}")
    }))
}

pub(crate) fn attachment_mime_type(codec: &str) -> &'static str {
    match codec.to_ascii_lowercase().as_str() {
        "ttf" | "truetype" => "font/ttf",
        "otf" | "opentype" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "mjpeg" | "mjpg" => "image/jpeg",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

pub struct MediaStreamRow {
    pub stream_index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub codec_tag: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub comment: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub aspect_ratio: Option<String>,
    pub average_frame_rate: Option<f64>,
    pub real_frame_rate: Option<f64>,
    pub reference_frame_rate: Option<f64>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub ref_frames: Option<i64>,
    pub is_interlaced: bool,
    pub is_avc: Option<bool>,
    pub is_anamorphic: Option<bool>,
    pub pixel_format: Option<String>,
    pub level: Option<i64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub time_base: Option<String>,
    pub codec_time_base: Option<String>,
    pub nal_length_size: Option<String>,
    pub rotation: Option<i64>,
    pub video_range: Option<String>,
    pub video_range_type: Option<String>,
    pub hdr10_plus_present_flag: Option<bool>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    pub is_original: Option<bool>,
    pub path: Option<String>,
    pub is_external: bool,
}

impl MediaStreamRow {
    pub fn to_jellyfin_json(&self, item_id: &str) -> JsonValue {
        let codec = self.codec.as_deref().unwrap_or_default();
        let is_subtitle = self.stream_type == "Subtitle";
        let is_text_subtitle = is_subtitle && is_text_subtitle_codec(codec);
        let is_pgs_subtitle = is_subtitle && is_pgs_subtitle_codec(codec);
        let delivery_url = if self.stream_type == "Subtitle" && self.is_external {
            Some(format!(
                "/Videos/{item_id}/{item_id}/Subtitles/{}/Stream.{}",
                self.stream_index,
                if codec.is_empty() { "srt" } else { codec }
            ))
        } else {
            None
        };

        let delivery_method = if self.stream_type == "Subtitle" {
            if self.is_external {
                Some("External")
            } else {
                Some("Embed")
            }
        } else {
            None
        };

        let display_title = compute_stream_display_title(self);

        let mut map = Map::new();
        map.insert(
            "Index".into(),
            JsonValue::Number(serde_json::Number::from(self.stream_index)),
        );
        map.insert("Type".into(), JsonValue::String(self.stream_type.clone()));
        map.insert("Codec".into(), opt_str(&self.codec));
        map.insert("CodecTag".into(), opt_str(&self.codec_tag));
        map.insert("Language".into(), opt_str(&self.language));
        map.insert("Title".into(), opt_str(&self.title));
        map.insert("Comment".into(), opt_str(&self.comment));
        map.insert("DisplayTitle".into(), JsonValue::String(display_title));
        map.insert("DisplayLanguage".into(), opt_str(&self.language));
        map.insert("Extradata".into(), JsonValue::String(String::new()));
        map.insert(
            "Path".into(),
            if self.is_external || self.stream_type.eq_ignore_ascii_case("Attachment") {
                opt_str(&self.path)
            } else {
                JsonValue::Null
            },
        );
        map.insert("BitRate".into(), opt_i64(self.bit_rate));
        map.insert("Width".into(), opt_i64(self.width));
        map.insert("Height".into(), opt_i64(self.height));
        map.insert("AspectRatio".into(), opt_str(&self.aspect_ratio));
        map.insert("AverageFrameRate".into(), opt_f64(self.average_frame_rate));
        map.insert("RealFrameRate".into(), opt_f64(self.real_frame_rate));
        map.insert(
            "ReferenceFrameRate".into(),
            opt_f64(self.reference_frame_rate),
        );
        map.insert("BitDepth".into(), opt_i64(self.bit_depth));
        map.insert("RefFrames".into(), opt_i64(self.ref_frames));
        map.insert("IsInterlaced".into(), JsonValue::Bool(self.is_interlaced));
        map.insert("IsAVC".into(), opt_bool(self.is_avc));
        map.insert("IsAnamorphic".into(), opt_bool(self.is_anamorphic));
        map.insert("Rotation".into(), opt_i64(self.rotation));
        map.insert("VideoRange".into(), opt_str(&self.video_range));
        map.insert("VideoRangeType".into(), opt_str(&self.video_range_type));
        map.insert("Profile".into(), opt_str(&self.profile));
        map.insert("Level".into(), opt_i64(self.level));
        map.insert("PixelFormat".into(), opt_str(&self.pixel_format));
        map.insert("ColorRange".into(), opt_str(&self.color_range));
        map.insert("ColorSpace".into(), opt_str(&self.color_space));
        map.insert("ColorTransfer".into(), opt_str(&self.color_transfer));
        map.insert("ColorPrimaries".into(), opt_str(&self.color_primaries));
        map.insert("DvVersionMajor".into(), JsonValue::Null);
        map.insert("DvVersionMinor".into(), JsonValue::Null);
        map.insert("DvProfile".into(), JsonValue::Null);
        map.insert("DvLevel".into(), JsonValue::Null);
        map.insert("RpuPresentFlag".into(), JsonValue::Null);
        map.insert("ElPresentFlag".into(), JsonValue::Null);
        map.insert("BlPresentFlag".into(), JsonValue::Null);
        map.insert("DvBlSignalCompatibilityId".into(), JsonValue::Null);
        map.insert("VideoDoViTitle".into(), JsonValue::Null);
        map.insert(
            "Hdr10PlusPresentFlag".into(),
            opt_bool(self.hdr10_plus_present_flag),
        );
        map.insert("Channels".into(), opt_i64(self.channels));
        map.insert("ChannelLayout".into(), opt_str(&self.channel_layout));
        map.insert("SampleRate".into(), opt_i64(self.sample_rate));
        map.insert(
            "AudioSpatialFormat".into(),
            JsonValue::String(audio_spatial_format(self.profile.as_deref()).to_string()),
        );
        map.insert("DeliveryMethod".into(), opt_str_val(delivery_method));
        map.insert("DeliveryUrl".into(), opt_str_val(delivery_url.as_deref()));
        map.insert("IsExternal".into(), JsonValue::Bool(self.is_external));
        map.insert("IsExternalUrl".into(), JsonValue::Null);
        map.insert("IsDefault".into(), JsonValue::Bool(self.is_default));
        map.insert("IsForced".into(), JsonValue::Bool(self.is_forced));
        map.insert(
            "IsHearingImpaired".into(),
            JsonValue::Bool(self.is_hearing_impaired),
        );
        map.insert("IsOriginal".into(), opt_bool(self.is_original));
        map.insert("SupportsExternalStream".into(), JsonValue::Bool(true));
        map.insert(
            "IsTextSubtitleStream".into(),
            subtitle_flag(is_subtitle, is_text_subtitle),
        );
        map.insert(
            "IsPgsSubtitleStream".into(),
            subtitle_flag(is_subtitle, is_pgs_subtitle),
        );
        map.insert(
            "IsExtractableSubtitleStream".into(),
            subtitle_flag(is_subtitle, !self.is_external),
        );
        map.insert("TimeBase".into(), opt_str(&self.time_base));
        map.insert("CodecTimeBase".into(), opt_str(&self.codec_time_base));
        map.insert("NalLengthSize".into(), opt_str(&self.nal_length_size));
        map.insert("PacketLength".into(), JsonValue::Null);
        map.insert("Score".into(), JsonValue::Null);
        map.insert("LocalizedUndefined".into(), JsonValue::Null);
        map.insert("LocalizedDefault".into(), JsonValue::Null);
        map.insert("LocalizedForced".into(), JsonValue::Null);
        map.insert("LocalizedExternal".into(), JsonValue::Null);
        map.insert("LocalizedHearingImpaired".into(), JsonValue::Null);
        map.insert("LocalizedLanguage".into(), JsonValue::Null);
        map.insert("LocalizedOriginal".into(), JsonValue::Null);
        JsonValue::Object(map)
    }
}

fn subtitle_flag(is_subtitle: bool, value: bool) -> JsonValue {
    if is_subtitle {
        JsonValue::Bool(value)
    } else {
        JsonValue::Null
    }
}

fn is_text_subtitle_codec(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "ass" | "ssa" | "srt" | "subrip" | "text" | "mov_text" | "webvtt" | "vtt" | "smi" | "sami"
    )
}

fn is_pgs_subtitle_codec(codec: &str) -> bool {
    let codec = codec.to_ascii_lowercase();
    codec == "pgs" || codec == "hdmv_pgs_subtitle" || codec.contains("pgs")
}

fn compute_stream_display_title(stream: &MediaStreamRow) -> String {
    if stream.stream_type == "Video" {
        let mut attrs = Vec::new();
        if let (Some(_w), Some(h)) = (stream.width, stream.height) {
            let label = if h >= 2160 {
                "4K"
            } else if h >= 1080 {
                "1080p"
            } else if h >= 720 {
                "720p"
            } else {
                "SD"
            };
            attrs.push(label.to_string());
        }
        if let Some(ref codec) = stream.codec {
            attrs.push(codec.to_uppercase());
        }
        if stream
            .video_range
            .as_deref()
            .is_some_and(|range| !range.eq_ignore_ascii_case("SDR"))
        {
            if let Some(ref range_type) = stream.video_range_type {
                attrs.push(range_type.clone());
            } else if let Some(ref range) = stream.video_range {
                attrs.push(range.clone());
            }
        }
        titled_display(stream.title.as_deref(), &attrs, " ")
    } else if stream.stream_type == "Audio" {
        let mut attrs = Vec::new();
        if let Some(ref lang) = stream.language {
            attrs.push(lang.to_string());
        }
        if let Some(ref profile) = stream.profile {
            if !profile.eq_ignore_ascii_case("lc") {
                attrs.push(profile.clone());
            }
        }
        if attrs.is_empty() {
            if let Some(ref codec) = stream.codec {
                attrs.push(codec.to_uppercase());
            }
        }
        if let Some(ref layout) = stream.channel_layout {
            attrs.push(layout.clone());
        } else if let Some(channels) = stream.channels {
            let ch_label = match channels {
                8 => "7.1",
                6 => "5.1",
                2 => "Stereo",
                1 => "Mono",
                _ => &format!("{channels} ch"),
            };
            attrs.push(ch_label.to_string());
        }
        if stream.is_default {
            attrs.push("Default".to_string());
        }
        if stream.is_external {
            attrs.push("External".to_string());
        }
        if stream.is_original == Some(true) {
            attrs.push("Original".to_string());
        }
        titled_display(stream.title.as_deref(), &attrs, " - ")
    } else if stream.stream_type == "Subtitle" {
        let mut attrs = Vec::new();
        if let Some(ref lang) = stream.language {
            attrs.push(lang.to_string());
        } else {
            attrs.push("Und".to_string());
        }
        if stream.is_hearing_impaired {
            attrs.push("Hearing Impaired".to_string());
        }
        if stream.is_default {
            attrs.push("Default".to_string());
        }
        if stream.is_forced {
            attrs.push("Forced".to_string());
        }
        if let Some(ref codec) = stream.codec {
            attrs.push(codec.to_uppercase());
        }
        if stream.is_external {
            attrs.push("External".to_string());
        }
        titled_display(stream.title.as_deref(), &attrs, " - ")
    } else {
        stream
            .title
            .clone()
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

fn titled_display(title: Option<&str>, attrs: &[String], separator: &str) -> String {
    if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
        let mut result = title.to_string();
        for attr in attrs {
            if !result
                .to_ascii_lowercase()
                .contains(&attr.to_ascii_lowercase())
            {
                result.push_str(" - ");
                result.push_str(attr);
            }
        }
        return result;
    }
    if attrs.is_empty() {
        "Unknown".to_string()
    } else {
        attrs.join(separator)
    }
}

fn audio_spatial_format(profile: Option<&str>) -> &'static str {
    let Some(profile) = profile else {
        return "None";
    };
    if profile.to_ascii_lowercase().contains("dolby atmos") {
        "DolbyAtmos"
    } else if profile.to_ascii_lowercase().contains("dts:x") {
        "DTSX"
    } else {
        "None"
    }
}

/// Build a MediaSourceInfo JSON for a child video file (used in multi-version movies/episodes).
/// This avoids the json! macro recursion limit by using Map construction.
pub fn child_video_source_json(
    id: &str,
    title: &str,
    path: &str,
    container: &str,
    size: Option<i64>,
    runtime_ticks: Option<i64>,
    media_streams: Vec<JsonValue>,
) -> JsonValue {
    child_video_source_json_for_item(
        id,
        id,
        title,
        path,
        container,
        size,
        runtime_ticks,
        media_streams,
    )
}

pub fn child_video_source_json_for_item(
    item_id: &str,
    media_source_id: &str,
    title: &str,
    path: &str,
    container: &str,
    size: Option<i64>,
    runtime_ticks: Option<i64>,
    media_streams: Vec<JsonValue>,
) -> JsonValue {
    let (media_streams, media_attachments) =
        split_media_streams_and_attachments(item_id, media_source_id, media_streams);
    let (default_audio_stream_index, default_subtitle_stream_index) =
        default_media_stream_indexes(&media_streams);
    let bitrate = media_source_bitrate(size, runtime_ticks, &media_streams);
    let remote_target = remote_strm_target(path);
    let direct_stream_url = remote_target
        .clone()
        .unwrap_or_else(|| format!("/Videos/{item_id}/{media_source_id}/stream"));
    let (protocol, path, is_remote) =
        media_source_protocol_path_with_target(path, remote_target.as_deref());
    let video_type = crate::library::classify::file_video_type(std::path::Path::new(&path))
        .unwrap_or("VideoFile")
        .to_string();
    let iso_type = crate::library::classify::iso_type_from_name(std::path::Path::new(&path))
        .map(ToString::to_string);
    let video_3d_format = crate::library::naming::parse_video_3d_format(&path);
    let mut map = Map::new();
    map.insert("Id".into(), JsonValue::String(media_source_id.to_string()));
    map.insert("Name".into(), JsonValue::String(title.to_string()));
    map.insert("Type".into(), JsonValue::String("Default".to_string()));
    map.insert("Protocol".into(), JsonValue::String(protocol));
    map.insert("Path".into(), JsonValue::String(path));
    map.insert("Container".into(), JsonValue::String(container.to_string()));
    map.insert("Size".into(), opt_i64(size));
    map.insert("RunTimeTicks".into(), opt_i64(runtime_ticks));
    map.insert("VideoType".into(), JsonValue::String(video_type));
    map.insert("IsoType".into(), opt_str(&iso_type));
    map.insert("Video3DFormat".into(), opt_str(&video_3d_format));
    map.insert("Timestamp".into(), JsonValue::Null);
    map.insert("Bitrate".into(), opt_i64(bitrate));
    map.insert("FallbackMaxStreamingBitrate".into(), JsonValue::Null);
    map.insert("SupportsDirectPlay".into(), JsonValue::Bool(true));
    map.insert("SupportsDirectStream".into(), JsonValue::Bool(true));
    map.insert("SupportsTranscoding".into(), JsonValue::Bool(false));
    map.insert("AddApiKeyToDirectStreamUrl".into(), JsonValue::Bool(false));
    map.insert("SupportsProbing".into(), JsonValue::Bool(true));
    map.insert("IsInfiniteStream".into(), JsonValue::Bool(false));
    map.insert("IsRemote".into(), JsonValue::Bool(is_remote));
    map.insert("RequiresOpening".into(), JsonValue::Bool(false));
    map.insert("RequiresClosing".into(), JsonValue::Bool(false));
    map.insert("RequiresLooping".into(), JsonValue::Bool(false));
    map.insert("ReadAtNativeFramerate".into(), JsonValue::Bool(false));
    map.insert("IgnoreDts".into(), JsonValue::Bool(false));
    map.insert("IgnoreIndex".into(), JsonValue::Bool(false));
    map.insert("GenPtsInput".into(), JsonValue::Bool(false));
    map.insert("SupportsSegmentSeeking".into(), JsonValue::Bool(true));
    map.insert("HasSegments".into(), JsonValue::Bool(false));
    map.insert("MediaStreams".into(), JsonValue::Array(media_streams));
    map.insert(
        "MediaAttachments".into(),
        JsonValue::Array(media_attachments),
    );
    map.insert(
        "DefaultAudioStreamIndex".into(),
        opt_i64(default_audio_stream_index),
    );
    map.insert(
        "DefaultSubtitleStreamIndex".into(),
        opt_i64(default_subtitle_stream_index),
    );
    map.insert("Formats".into(), JsonValue::Array(vec![]));
    map.insert("RequiredHttpHeaders".into(), JsonValue::Object(Map::new()));
    map.insert("TranscodingUrl".into(), JsonValue::Null);
    map.insert("TranscodingContainer".into(), JsonValue::Null);
    map.insert("TranscodingSubProtocol".into(), JsonValue::Null);
    map.insert(
        "DirectStreamUrl".into(),
        JsonValue::String(direct_stream_url),
    );
    map.insert("EncoderPath".into(), JsonValue::Null);
    map.insert("EncoderProtocol".into(), JsonValue::Null);
    map.insert("BufferMs".into(), JsonValue::Null);
    map.insert("AnalyzeDurationMs".into(), JsonValue::Null);
    map.insert(
        "UseMostCompatibleTranscodingProfile".into(),
        JsonValue::Bool(false),
    );
    map.insert("LiveStreamId".into(), JsonValue::Null);
    map.insert("OpenToken".into(), JsonValue::Null);
    map.insert(
        "ETag".into(),
        JsonValue::String(media_source_id.to_string()),
    );
    JsonValue::Object(map)
}

#[cfg(test)]
mod tests {
    use super::{
        MediaItem, MediaStreamRow, MediaStreamSelectionPreferences, SubtitlePlaybackMode,
        apply_media_source_user_preferences, attachment_mime_type, child_video_source_json,
        media_source_json_with_streams, rewrite_public_strm_target_with_base,
    };
    use serde_json::json;

    #[test]
    fn public_strm_base_rewrites_only_local_smartstrm_targets() {
        assert_eq!(
            rewrite_public_strm_target_with_base(
                "http://127.0.0.1:8024/smartstrm_fid/demo/movie.mkv?sign=x",
                Some("https://smartstrm.example.test"),
            ),
            "https://smartstrm.example.test/smartstrm_fid/demo/movie.mkv?sign=x"
        );
        assert_eq!(
            rewrite_public_strm_target_with_base(
                "https://other.example.test/movie.mkv",
                Some("https://smartstrm.example.test"),
            ),
            "https://other.example.test/movie.mkv"
        );
        assert_eq!(
            rewrite_public_strm_target_with_base(
                "http://127.0.0.1:8024/movie.mkv",
                None,
            ),
            "http://127.0.0.1:8024/movie.mkv"
        );
    }

    #[test]
    fn media_sources_split_attachments_from_playable_streams() {
        let source = media_source_json_with_streams(&video_item(), sample_streams());

        assert_eq!(source["MediaStreams"].as_array().unwrap().len(), 1);
        assert_eq!(source["MediaStreams"][0]["Type"], "Video");
        assert_eq!(source["MediaAttachments"].as_array().unwrap().len(), 2);
        assert_eq!(source["MediaAttachments"][0]["Index"], 5);
        assert_eq!(source["MediaAttachments"][0]["FileName"], "Font.ttf");
        assert_eq!(source["MediaAttachments"][0]["MimeType"], "font/ttf");
        assert_eq!(
            source["MediaAttachments"][0]["DeliveryUrl"],
            "/Videos/movie/movie/Attachments/5"
        );
        assert_eq!(source["MediaAttachments"][1]["Index"], 6);
        assert_eq!(source["MediaAttachments"][1]["FileName"], "Embedded.otf");
        assert_eq!(source["MediaAttachments"][1]["MimeType"], "font/otf");
    }

    #[test]
    fn child_video_sources_include_media_attachments() {
        let source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            Some(123),
            Some(456),
            sample_streams(),
        );

        assert_eq!(source["MediaStreams"].as_array().unwrap().len(), 1);
        assert_eq!(source["MediaAttachments"].as_array().unwrap().len(), 2);
        assert_eq!(
            source["MediaAttachments"][0]["DeliveryUrl"],
            "/Videos/part1/part1/Attachments/5"
        );
    }

    #[test]
    fn attachment_mime_type_reports_common_fonts_and_images() {
        assert_eq!(attachment_mime_type("ttf"), "font/ttf");
        assert_eq!(attachment_mime_type("otf"), "font/otf");
        assert_eq!(attachment_mime_type("mjpeg"), "image/jpeg");
        assert_eq!(attachment_mime_type("png"), "image/png");
    }

    #[test]
    fn media_sources_report_default_stream_indexes_like_jellyfin_default_mode() {
        let source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            None,
            None,
            vec![
                json!({ "Index": 0, "Type": "Video", "Codec": "h264" }),
                json!({ "Index": 1, "Type": "Audio", "Language": "fre", "IsDefault": false }),
                json!({ "Index": 2, "Type": "Audio", "Language": "jpn", "IsDefault": true }),
                json!({ "Index": 3, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": false, "IsExternal": false }),
                json!({ "Index": 4, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": true, "IsExternal": false }),
                json!({ "Index": 5, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": false, "IsExternal": true }),
            ],
        );

        assert_eq!(source["DefaultAudioStreamIndex"], 2);
        assert_eq!(source["DefaultSubtitleStreamIndex"], 5);
    }

    #[test]
    fn media_sources_leave_unflagged_subtitles_disabled_by_default() {
        let source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            None,
            None,
            vec![
                json!({ "Index": 0, "Type": "Video", "Codec": "h264" }),
                json!({ "Index": 2, "Type": "Audio", "Language": "eng", "IsDefault": false }),
                json!({ "Index": 3, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": false, "IsExternal": false }),
            ],
        );

        assert_eq!(source["DefaultAudioStreamIndex"], 2);
        assert_eq!(
            source["DefaultSubtitleStreamIndex"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn media_sources_remember_valid_audio_and_subtitle_stream_indexes() {
        let mut source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            None,
            None,
            vec![
                json!({ "Index": 0, "Type": "Video", "Codec": "h264" }),
                json!({ "Index": 1, "Type": "Audio", "Language": "eng", "IsDefault": true }),
                json!({ "Index": 2, "Type": "Audio", "Language": "jpn", "IsDefault": false }),
                json!({ "Index": 3, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": true, "IsExternal": false }),
                json!({ "Index": 4, "Type": "Subtitle", "Language": "jpn", "IsForced": false, "IsDefault": false, "IsExternal": true }),
            ],
        );
        let preferences = MediaStreamSelectionPreferences {
            remembered_audio_stream_index: Some(2),
            remembered_subtitle_stream_index: Some(4),
            ..Default::default()
        };

        apply_media_source_user_preferences(&mut source, &preferences);

        assert_eq!(source["DefaultAudioStreamIndex"], 2);
        assert_eq!(source["DefaultSubtitleStreamIndex"], 4);
    }

    #[test]
    fn media_sources_apply_jellyfin_smart_and_always_subtitle_modes() {
        let mut smart_source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            None,
            None,
            vec![
                json!({ "Index": 0, "Type": "Video", "Codec": "h264" }),
                json!({ "Index": 1, "Type": "Audio", "Language": "eng", "IsDefault": true }),
                json!({ "Index": 2, "Type": "Subtitle", "Language": "eng", "IsForced": false, "IsDefault": false, "IsExternal": true, "IsTextSubtitleStream": true, "SupportsExternalStream": true }),
                json!({ "Index": 3, "Type": "Subtitle", "Language": "eng", "IsForced": true, "IsDefault": false, "IsExternal": false, "IsTextSubtitleStream": false, "SupportsExternalStream": true }),
            ],
        );
        let smart_preferences = MediaStreamSelectionPreferences {
            subtitle_languages: vec!["eng".to_string()],
            subtitle_mode: SubtitlePlaybackMode::Smart,
            ..Default::default()
        };

        apply_media_source_user_preferences(&mut smart_source, &smart_preferences);

        assert_eq!(smart_source["DefaultSubtitleStreamIndex"], 3);
        assert!(smart_source["MediaStreams"][3]["Score"].as_i64().is_some());

        let mut always_source = smart_source.clone();
        let always_preferences = MediaStreamSelectionPreferences {
            subtitle_languages: vec!["eng".to_string()],
            subtitle_mode: SubtitlePlaybackMode::Always,
            ..Default::default()
        };

        apply_media_source_user_preferences(&mut always_source, &always_preferences);

        assert_eq!(always_source["DefaultSubtitleStreamIndex"], 2);
    }

    #[test]
    fn media_sources_report_bitrate_from_file_size_and_runtime() {
        let source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            Some(1_000),
            Some(10_000_000),
            vec![json!({ "Index": 0, "Type": "Video", "BitRate": 1_000 })],
        );

        assert_eq!(source["Bitrate"], 8_000);
    }

    #[test]
    fn media_sources_fall_back_to_stream_bitrate() {
        let source = child_video_source_json(
            "part1",
            "Part 1",
            "D:/Movies/part1.mkv",
            "mkv",
            None,
            None,
            vec![
                json!({ "Index": 0, "Type": "Video", "BitRate": 5_000 }),
                json!({ "Index": 1, "Type": "Audio", "BitRate": 1_000 }),
                json!({ "Index": 2, "Type": "Subtitle", "BitRate": 500 }),
            ],
        );

        assert_eq!(source["Bitrate"], 6_000);
    }

    #[test]
    fn remote_strm_media_source_uses_http_protocol_and_direct_url() {
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-strm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("movie.strm");
        std::fs::write(&path, "https://example.test/movie.mp4?token=1").unwrap();
        let mut item = video_item();
        item.path = path.to_string_lossy().to_string();
        item.container = Some("mp4".to_string());

        let source = media_source_json_with_streams(&item, sample_streams());

        assert_eq!(source["Protocol"], "Http");
        assert_eq!(source["Path"], "https://example.test/movie.mp4?token=1");
        assert_eq!(source["IsRemote"], true);
        assert_eq!(source["AddApiKeyToDirectStreamUrl"], false);
        assert_eq!(
            source["DirectStreamUrl"],
            "https://example.test/movie.mp4?token=1"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn child_remote_strm_media_source_uses_http_protocol_and_direct_url() {
        let root =
            std::env::temp_dir().join(format!("jellyfin-rs-strm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("part.strm");
        std::fs::write(&path, "https://example.test/part.mkv").unwrap();

        let source = child_video_source_json(
            "part1",
            "Part 1",
            &path.to_string_lossy(),
            "mkv",
            None,
            None,
            vec![],
        );

        assert_eq!(source["Protocol"], "Http");
        assert_eq!(source["Path"], "https://example.test/part.mkv");
        assert_eq!(source["IsRemote"], true);
        assert_eq!(source["AddApiKeyToDirectStreamUrl"], false);
        assert_eq!(source["DirectStreamUrl"], "https://example.test/part.mkv");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn media_item_json_uses_emby_base_item_defaults() {
        let mut item = video_item();
        item.original_title = Some("Original Movie".to_string());
        item.sort_name = Some("Movie Sort".to_string());
        item.forced_sort_name = Some("Forced Movie".to_string());
        item.lock_data = true;
        item.locked_fields = vec!["Name".to_string(), "Overview".to_string()];
        item.custom_rating = Some("TV-MA".to_string());
        item.tagline = Some("Trust no one.".to_string());
        item.collection_name = Some("The Collection".to_string());
        item.original_language = Some("en".to_string());
        item.preferred_metadata_language = Some("ja-JP".to_string());
        item.preferred_metadata_country_code = Some("JP".to_string());
        item.series_status = Some("Ended".to_string());
        item.air_days = vec!["Friday".to_string()];
        item.air_time = Some("23:30".to_string());
        item.aspect_ratio = Some("16:9".to_string());
        item.width = Some(1920);
        item.height = Some(1080);
        item.has_subtitles = true;
        item.home_page_url = Some("https://example.test/movie".to_string());
        item.remote_trailers = vec![json!({
            "Name": "Official Trailer",
            "Url": "https://www.youtube.com/watch?v=abc"
        })];
        item.production_locations = vec!["United States".to_string(), "Japan".to_string()];
        item.end_date = Some("2024-08-03".to_string());
        item.display_order = Some("dvd".to_string());
        let value = item.to_jellyfin_json();

        assert_eq!(value["ServerId"], "jellyfin-rs");
        assert_eq!(value["Etag"], "movie-1");
        assert_eq!(value["OriginalTitle"], "Original Movie");
        assert_eq!(value["SortName"], "Movie Sort");
        assert_eq!(value["ForcedSortName"], "Forced Movie");
        assert_eq!(value["LockData"], true);
        assert_eq!(value["LockedFields"], json!(["Name", "Overview"]));
        assert_eq!(value["CustomRating"], "TV-MA");
        assert_eq!(value["Taglines"], json!(["Trust no one."]));
        assert_eq!(value["CollectionName"], "The Collection");
        assert_eq!(value["OriginalLanguage"], "en");
        assert_eq!(value["PreferredMetadataLanguage"], "ja-JP");
        assert_eq!(value["PreferredMetadataCountryCode"], "JP");
        assert_eq!(value["AirDays"], json!(["Friday"]));
        assert_eq!(value["AirTime"], "23:30");
        assert_eq!(value["AspectRatio"], "16:9");
        assert_eq!(value["Width"], 1920);
        assert_eq!(value["Height"], 1080);
        assert_eq!(value["IsHD"], true);
        assert_eq!(value["HasSubtitles"], true);
        assert_eq!(value["Status"], "Ended");
        assert_eq!(value["HomePageUrl"], "https://example.test/movie");
        assert_eq!(value["RemoteTrailers"][0]["Name"], "Official Trailer");
        assert_eq!(
            value["ProductionLocations"],
            json!(["United States", "Japan"])
        );
        assert_eq!(value["EndDate"], "2024-08-03T00:00:00.0000000Z");
        assert_eq!(value["DisplayOrder"], "dvd");
        assert_eq!(value["MediaSourceCount"], 1);
        assert_eq!(value["PartCount"], serde_json::Value::Null);
        assert_eq!(value["ChildCount"], 0);
        assert_eq!(value["RecursiveItemCount"], 0);
        assert_eq!(value["LocalTrailerCount"], 0);
        assert_eq!(value["SupportsResume"], true);
        assert_eq!(value["SupportsSync"], false);
        assert_eq!(value["DisplaySpecialsWithSeasons"], false);
        assert_eq!(value["EnableMediaSourceDisplay"], false);
        assert_eq!(
            value["MediaSources"][0]["DirectStreamUrl"],
            "/Videos/movie/stream"
        );
    }

    #[test]
    fn video_type_and_iso_type_are_reported_on_item_and_media_source() {
        let mut item = video_item();
        item.path = "D:/Movies/Movie.bluray.iso".to_string();
        item.video_type = Some("Iso".to_string());
        item.iso_type = Some("BluRay".to_string());

        let value = item.to_jellyfin_json();

        assert_eq!(value["VideoType"], "Iso");
        assert_eq!(value["IsoType"], "BluRay");
        assert_eq!(value["MediaSources"][0]["VideoType"], "Iso");
        assert_eq!(value["MediaSources"][0]["IsoType"], "BluRay");
    }

    #[test]
    fn video_3d_format_is_reported_on_item_and_media_source() {
        let mut item = video_item();
        item.video_3d_format = Some("HalfSideBySide".to_string());

        let value = item.to_jellyfin_json();

        assert_eq!(value["Video3DFormat"], "HalfSideBySide");
        assert_eq!(value["MediaSources"][0]["Video3DFormat"], "HalfSideBySide");
    }

    #[test]
    fn disc_folder_video_items_keep_a_media_source() {
        let mut item = video_item();
        item.is_folder = true;
        item.video_type = Some("Dvd".to_string());

        let value = item.to_jellyfin_json();

        assert_eq!(value["VideoType"], "Dvd");
        assert_eq!(value["MediaSourceCount"], 1);
        assert_eq!(value["MediaSources"][0]["VideoType"], "Dvd");
        assert_eq!(value["SupportsResume"], true);
    }

    #[test]
    fn episode_json_reports_index_number_end() {
        let mut item = video_item();
        item.item_type = "Episode".to_string();
        item.season_number = Some(1);
        item.episode_number = Some(1);
        item.episode_number_end = Some(3);

        let value = item.to_jellyfin_json();

        assert_eq!(value["IndexNumber"], 1);
        assert_eq!(value["ParentIndexNumber"], 1);
        assert_eq!(value["IndexNumberEnd"], 3);
    }

    #[test]
    fn subtitle_streams_report_codec_flags() {
        let text = subtitle_stream("subrip", false).to_jellyfin_json("movie");
        assert_eq!(text["IsTextSubtitleStream"], true);
        assert_eq!(text["IsPgsSubtitleStream"], false);
        assert_eq!(text["IsExtractableSubtitleStream"], true);
        assert_eq!(text["DisplayLanguage"], "eng");
        assert_eq!(text["Extradata"], "");

        let pgs = subtitle_stream("hdmv_pgs_subtitle", false).to_jellyfin_json("movie");
        assert_eq!(pgs["IsTextSubtitleStream"], false);
        assert_eq!(pgs["IsPgsSubtitleStream"], true);
        assert_eq!(pgs["IsExtractableSubtitleStream"], true);

        let external = subtitle_stream("srt", true).to_jellyfin_json("movie");
        assert_eq!(external["DeliveryMethod"], "External");
        assert_eq!(external["IsTextSubtitleStream"], true);
        assert_eq!(external["IsExtractableSubtitleStream"], false);
    }

    fn sample_streams() -> Vec<serde_json::Value> {
        vec![
            json!({ "Index": 0, "Type": "Video", "Codec": "h264" }),
            json!({
                "Index": 5,
                "Type": "Attachment",
                "Codec": "ttf",
                "Title": "Font.ttf",
                "Comment": "subtitle font",
                "Path": "D:/Movies/Font.ttf"
            }),
            json!({
                "Index": 6,
                "Type": "Attachment",
                "Codec": "otf",
                "Title": "Embedded.otf"
            }),
        ]
    }

    fn subtitle_stream(codec: &str, is_external: bool) -> MediaStreamRow {
        MediaStreamRow {
            stream_index: 2,
            stream_type: "Subtitle".to_string(),
            codec: Some(codec.to_string()),
            profile: None,
            codec_tag: None,
            language: Some("eng".to_string()),
            title: None,
            comment: None,
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
            path: is_external.then(|| "D:/Movie/movie.eng.srt".to_string()),
            is_external,
        }
    }

    #[test]
    fn audio_dto_exposes_embedded_album_and_track_numbers() {
        let mut item = video_item();
        item.item_type = "Audio".to_string();
        item.parent_id = "album-1".to_string();
        item.collection_name = Some("Album One".to_string());
        item.episode_number = Some(7);
        item.season_number = Some(2);

        let value = item.to_jellyfin_json();

        assert_eq!(value["Album"], "Album One");
        assert_eq!(value["AlbumId"], "album-1");
        assert_eq!(value["IndexNumber"], 7);
        assert_eq!(value["ParentIndexNumber"], 2);
    }

    fn video_item() -> MediaItem {
        MediaItem {
            id: "movie".to_string(),
            title: "Movie".to_string(),
            path: "D:/Movies/movie.mkv".to_string(),
            library_id: "movies".to_string(),
            collection_type: "movies".to_string(),
            parent_id: String::new(),
            item_type: "Video".to_string(),
            extra_type: None,
            video_type: None,
            iso_type: None,
            video_3d_format: None,
            is_folder: false,
            container: Some("mkv".to_string()),
            overview: None,
            official_rating: None,
            custom_rating: None,
            extended_video_type: None,
            original_title: None,
            sort_name: None,
            forced_sort_name: None,
            lock_data: false,
            locked_fields: Vec::new(),
            tagline: None,
            collection_name: None,
            original_language: None,
            series_status: None,
            home_page_url: None,
            remote_trailers: Vec::new(),
            production_locations: Vec::new(),
            production_year: None,
            premiere_date: None,
            end_date: None,
            runtime_ticks: Some(456),
            display_order: None,
            size_bytes: Some(123),
            season_number: None,
            episode_number: None,
            episode_number_end: None,
            community_rating: None,
            critic_rating: None,
            created_at: 1,
            modified_at: 1,
            is_public: true,
            is_favorite: false,
            played: false,
            playback_position_ticks: 0,
            played_percentage: None,
            play_count: 0,
            last_played_at: None,
            image_tags: None,
            ..Default::default()
        }
    }
}
