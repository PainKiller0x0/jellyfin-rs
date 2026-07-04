use serde_json::{Map, Value as JsonValue, json};

use crate::{db::row_ext::QueryResultExt, util::unix_to_jellyfin_date};

#[derive(Clone, Debug)]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    pub path: String,
    pub library_id: String,
    pub collection_type: String,
    pub parent_id: String,
    pub item_type: String,
    pub is_folder: bool,
    pub container: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
    pub extended_video_type: Option<String>,
    pub production_year: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub size_bytes: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
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
            is_folder: row.get_bool_from_i64("is_folder")?,
            container: row.get_opt_str("container")?,
            overview: row.get_opt_str("overview")?,
            official_rating: row.get_opt_str("official_rating")?,
            extended_video_type: row.get_opt_str("extended_video_type")?,
            production_year: row.get_opt_i64("production_year")?,
            runtime_ticks: row.get_opt_i64("runtime_ticks")?,
            size_bytes: row.get_opt_i64("size_bytes")?,
            season_number: row.get_opt_i64("season_number").ok().flatten(),
            episode_number: row.get_opt_i64("episode_number").ok().flatten(),
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
        let media_sources = if self.is_folder {
            json!([])
        } else {
            json!([media_source_json(self)])
        };

        let media_type = match self.item_type.as_str() {
            "Audio" => Some("Audio"),
            "Movie" | "Episode" | "Series" | "Season" | "Video" => Some("Video"),
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
        map.insert("ServerId".into(), JsonValue::Null);
        map.insert("Etag".into(), JsonValue::Null);
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
        map.insert("CustomRating".into(), JsonValue::Null);
        map.insert(
            "ExtendedVideoType".into(),
            opt_str(&self.extended_video_type),
        );
        map.insert("OriginalTitle".into(), JsonValue::Null);
        map.insert("ProductionYear".into(), opt_i64(self.production_year));
        let premiere_date = self
            .production_year
            .map(|y| format!("{y}-01-01T00:00:00.0000000Z"));
        map.insert("PremiereDate".into(), opt_str(&premiere_date));
        map.insert("EndDate".into(), JsonValue::Null);
        map.insert("IndexNumber".into(), opt_i64(self.episode_number));
        map.insert("ParentIndexNumber".into(), opt_i64(self.season_number));
        map.insert("IndexNumberEnd".into(), JsonValue::Null);
        map.insert("Number".into(), JsonValue::Null);
        map.insert("SortName".into(), JsonValue::String(self.title.clone()));
        map.insert("ForcedSortName".into(), JsonValue::Null);
        map.insert("ProviderIds".into(), json!({}));
        map.insert("LockData".into(), JsonValue::Bool(false));
        map.insert("LockedFields".into(), json!([]));
        map.insert("CanDelete".into(), JsonValue::Bool(true));
        map.insert("CanDownload".into(), JsonValue::Bool(true));
        map.insert("HasSubtitles".into(), JsonValue::Null);
        map.insert("HasLyrics".into(), JsonValue::Null);
        map.insert("PlayAccess".into(), JsonValue::String("Full".to_string()));
        map.insert("Size".into(), opt_i64(self.size_bytes));
        map.insert("Genres".into(), json!([]));
        map.insert("GenreItems".into(), json!([]));
        map.insert("Tags".into(), json!([]));
        map.insert("TagItems".into(), json!([]));
        map.insert("Taglines".into(), json!([]));
        map.insert("Studios".into(), json!([]));
        map.insert("People".into(), json!([]));
        map.insert("ProductionLocations".into(), json!([]));
        map.insert("VideoType".into(), JsonValue::Null);
        map.insert("IsoType".into(), JsonValue::Null);
        map.insert("Video3DFormat".into(), JsonValue::Null);
        map.insert("AspectRatio".into(), JsonValue::Null);
        map.insert("Width".into(), JsonValue::Null);
        map.insert("Height".into(), JsonValue::Null);
        map.insert("IsHD".into(), JsonValue::Null);
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
        map.insert("PreferredMetadataLanguage".into(), JsonValue::Null);
        map.insert("PreferredMetadataCountryCode".into(), JsonValue::Null);
        map.insert("UserData".into(), build_user_data(self));
        map.insert(
            "DateCreated".into(),
            JsonValue::String(unix_to_jellyfin_date(self.created_at)),
        );
        map.insert(
            "DateLastMediaAdded".into(),
            JsonValue::String(unix_to_jellyfin_date(self.modified_at)),
        );
        map.insert(
            "ImageTags".into(),
            self.image_tags.clone().unwrap_or_else(|| json!({})),
        );

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
        map.insert("PrimaryImageAspectRatio".into(), JsonValue::Null);
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
        map.insert("MediaSourceCount".into(), JsonValue::Null);
        map.insert("Chapters".into(), json!([]));
        map.insert("Trickplay".into(), json!({}));
        map.insert("LocalTrailerCount".into(), JsonValue::Null);
        map.insert("RemoteTrailers".into(), json!([]));
        map.insert("SpecialFeatureCount".into(), JsonValue::Null);
        map.insert("ExtraType".into(), JsonValue::Null);
        map.insert("IsPlaceHolder".into(), JsonValue::Null);
        map.insert("ChildCount".into(), JsonValue::Null);
        map.insert("RecursiveItemCount".into(), JsonValue::Null);
        map.insert("CumulativeRunTimeTicks".into(), JsonValue::Null);
        map.insert("PartCount".into(), JsonValue::Null);
        map.insert("EnableMediaSourceDisplay".into(), JsonValue::Null);
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
        map.insert("Album".into(), JsonValue::Null);
        map.insert("AlbumId".into(), JsonValue::Null);
        map.insert("AlbumPrimaryImageTag".into(), JsonValue::Null);
        map.insert("AlbumArtist".into(), JsonValue::Null);
        map.insert("AlbumArtists".into(), json!([]));
        map.insert("Artists".into(), json!([]));
        map.insert("ArtistItems".into(), json!([]));
        map.insert("DisplayOrder".into(), JsonValue::Null);
        map.insert("OriginalLanguage".into(), JsonValue::Null);
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

fn opt_str(val: &Option<String>) -> JsonValue {
    val.as_ref()
        .map(|s| JsonValue::String(s.clone()))
        .unwrap_or(JsonValue::Null)
}

fn opt_str_val(val: Option<&str>) -> JsonValue {
    val.map(|s| JsonValue::String(s.to_string()))
        .unwrap_or(JsonValue::Null)
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
    map.insert(
        "PlaybackPositionTicks".into(),
        JsonValue::Number(serde_json::Number::from(item.playback_position_ticks)),
    );
    let played_pct = if item.played {
        JsonValue::Number(serde_json::Number::from_f64(100.0).unwrap())
    } else {
        item.played_percentage
            .and_then(|f| serde_json::Number::from_f64(f).map(JsonValue::Number))
            .unwrap_or(JsonValue::Null)
    };
    map.insert("PlayedPercentage".into(), played_pct);
    map.insert("IsFavorite".into(), JsonValue::Bool(item.is_favorite));
    map.insert("Likes".into(), JsonValue::Null);
    map.insert("UnplayedItemCount".into(), JsonValue::Null);
    JsonValue::Object(map)
}

pub fn media_source_json(item: &MediaItem) -> JsonValue {
    media_source_json_with_streams(item, Vec::new())
}

pub fn media_source_json_with_streams(
    item: &MediaItem,
    media_streams: Vec<JsonValue>,
) -> JsonValue {
    let container = item.container.as_deref().unwrap_or("bin");
    let stream_path = match item.item_type.as_str() {
        "Audio" => format!("/Audio/{}/universal", item.id),
        _ => format!("/Videos/{}/stream.{container}", item.id),
    };

    let video_type =
        if item.item_type == "Video" || item.item_type == "Movie" || item.item_type == "Episode" {
            JsonValue::String("VideoFile".to_string())
        } else {
            JsonValue::Null
        };

    let mut map = Map::new();
    map.insert("Id".into(), JsonValue::String(item.id.clone()));
    map.insert("Name".into(), JsonValue::String(item.title.clone()));
    map.insert("Type".into(), JsonValue::String("Default".to_string()));
    map.insert("Protocol".into(), JsonValue::String("File".to_string()));
    map.insert("Path".into(), JsonValue::String(item.path.clone()));
    map.insert("Container".into(), opt_str(&item.container));
    map.insert("Size".into(), opt_i64(item.size_bytes));
    map.insert("RunTimeTicks".into(), opt_i64(item.runtime_ticks));
    map.insert("VideoType".into(), video_type);
    map.insert("IsoType".into(), JsonValue::Null);
    map.insert("Video3DFormat".into(), JsonValue::Null);
    map.insert("Timestamp".into(), JsonValue::Null);
    map.insert("Bitrate".into(), JsonValue::Null);
    map.insert("FallbackMaxStreamingBitrate".into(), JsonValue::Null);
    map.insert("SupportsDirectPlay".into(), JsonValue::Bool(true));
    map.insert("SupportsDirectStream".into(), JsonValue::Bool(true));
    map.insert("SupportsTranscoding".into(), JsonValue::Bool(false));
    map.insert("SupportsProbing".into(), JsonValue::Bool(true));
    map.insert("IsInfiniteStream".into(), JsonValue::Bool(false));
    map.insert("IsRemote".into(), JsonValue::Bool(false));
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
    map.insert("MediaAttachments".into(), JsonValue::Array(vec![]));
    map.insert("DefaultAudioStreamIndex".into(), JsonValue::Null);
    map.insert("DefaultSubtitleStreamIndex".into(), JsonValue::Null);
    map.insert("Formats".into(), JsonValue::Array(vec![]));
    map.insert("RequiredHttpHeaders".into(), JsonValue::Object(Map::new()));
    map.insert("TranscodingUrl".into(), JsonValue::Null);
    map.insert("TranscodingContainer".into(), JsonValue::Null);
    map.insert("TranscodingSubProtocol".into(), JsonValue::Null);
    map.insert("DirectStreamUrl".into(), JsonValue::String(stream_path));
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

pub struct MediaStreamRow {
    pub stream_index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
    pub path: Option<String>,
    pub is_external: bool,
}

impl MediaStreamRow {
    pub fn to_jellyfin_json(&self, item_id: &str) -> JsonValue {
        let codec = self.codec.as_deref().unwrap_or_default();
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
        map.insert("CodecTag".into(), JsonValue::Null);
        map.insert("Language".into(), opt_str(&self.language));
        map.insert("Title".into(), opt_str(&self.title));
        map.insert("Comment".into(), JsonValue::Null);
        map.insert("DisplayTitle".into(), JsonValue::String(display_title));
        map.insert(
            "Path".into(),
            if self.is_external {
                opt_str(&self.path)
            } else {
                JsonValue::Null
            },
        );
        map.insert("BitRate".into(), opt_i64(self.bit_rate));
        map.insert("Width".into(), opt_i64(self.width));
        map.insert("Height".into(), opt_i64(self.height));
        map.insert("AspectRatio".into(), JsonValue::Null);
        map.insert("AverageFrameRate".into(), JsonValue::Null);
        map.insert("RealFrameRate".into(), JsonValue::Null);
        map.insert("ReferenceFrameRate".into(), JsonValue::Null);
        map.insert("BitDepth".into(), JsonValue::Null);
        map.insert("RefFrames".into(), JsonValue::Null);
        map.insert("IsInterlaced".into(), JsonValue::Bool(false));
        map.insert("IsAVC".into(), JsonValue::Null);
        map.insert("IsAnamorphic".into(), JsonValue::Null);
        map.insert("Rotation".into(), JsonValue::Null);
        map.insert("VideoRange".into(), JsonValue::Null);
        map.insert("VideoRangeType".into(), JsonValue::Null);
        map.insert("Profile".into(), JsonValue::Null);
        map.insert("Level".into(), JsonValue::Null);
        map.insert("PixelFormat".into(), JsonValue::Null);
        map.insert("ColorRange".into(), JsonValue::Null);
        map.insert("ColorSpace".into(), JsonValue::Null);
        map.insert("ColorTransfer".into(), JsonValue::Null);
        map.insert("ColorPrimaries".into(), JsonValue::Null);
        map.insert("DvVersionMajor".into(), JsonValue::Null);
        map.insert("DvVersionMinor".into(), JsonValue::Null);
        map.insert("DvProfile".into(), JsonValue::Null);
        map.insert("DvLevel".into(), JsonValue::Null);
        map.insert("RpuPresentFlag".into(), JsonValue::Null);
        map.insert("ElPresentFlag".into(), JsonValue::Null);
        map.insert("BlPresentFlag".into(), JsonValue::Null);
        map.insert("DvBlSignalCompatibilityId".into(), JsonValue::Null);
        map.insert("VideoDoViTitle".into(), JsonValue::Null);
        map.insert("Hdr10PlusPresentFlag".into(), JsonValue::Null);
        map.insert("Channels".into(), opt_i64(self.channels));
        map.insert("ChannelLayout".into(), JsonValue::Null);
        map.insert("SampleRate".into(), opt_i64(self.sample_rate));
        map.insert(
            "AudioSpatialFormat".into(),
            JsonValue::String("None".to_string()),
        );
        map.insert("DeliveryMethod".into(), opt_str_val(delivery_method));
        map.insert("DeliveryUrl".into(), opt_str_val(delivery_url.as_deref()));
        map.insert("IsExternal".into(), JsonValue::Bool(self.is_external));
        map.insert("IsExternalUrl".into(), JsonValue::Null);
        map.insert("IsDefault".into(), JsonValue::Bool(false));
        map.insert("IsForced".into(), JsonValue::Bool(false));
        map.insert("IsHearingImpaired".into(), JsonValue::Bool(false));
        map.insert("IsOriginal".into(), JsonValue::Null);
        map.insert("SupportsExternalStream".into(), JsonValue::Bool(true));
        map.insert("IsTextSubtitleStream".into(), JsonValue::Null);
        map.insert("IsPgsSubtitleStream".into(), JsonValue::Null);
        map.insert("IsExtractableSubtitleStream".into(), JsonValue::Null);
        map.insert("TimeBase".into(), JsonValue::Null);
        map.insert("CodecTimeBase".into(), JsonValue::Null);
        map.insert("NalLengthSize".into(), JsonValue::Null);
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

fn compute_stream_display_title(stream: &MediaStreamRow) -> String {
    if stream.stream_type == "Video" {
        let mut parts = Vec::new();
        if let Some(ref title) = stream.title {
            parts.push(title.clone());
        }
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
            parts.push(label.to_string());
        }
        if let Some(ref codec) = stream.codec {
            parts.push(codec.to_uppercase());
        }
        if parts.is_empty() {
            "Unknown".to_string()
        } else {
            parts.join(" - ")
        }
    } else if stream.stream_type == "Audio" {
        let mut parts = Vec::new();
        if let Some(ref title) = stream.title {
            parts.push(title.clone());
        }
        if let Some(ref codec) = stream.codec {
            parts.push(codec.to_uppercase());
        }
        if let Some(channels) = stream.channels {
            let ch_label = match channels {
                8 => "7.1",
                6 => "5.1",
                2 => "Stereo",
                1 => "Mono",
                _ => &format!("{channels} ch"),
            };
            parts.push(ch_label.to_string());
        }
        if let Some(ref lang) = stream.language {
            parts.push(lang.to_string());
        }
        if parts.is_empty() {
            "Unknown".to_string()
        } else {
            parts.join(" - ")
        }
    } else if stream.stream_type == "Subtitle" {
        let mut parts = Vec::new();
        if let Some(ref title) = stream.title {
            parts.push(title.clone());
        }
        if let Some(ref lang) = stream.language {
            parts.push(lang.to_string());
        }
        let codec = stream.codec.as_deref().unwrap_or("");
        if stream.is_external {
            parts.push(format!("({} External)", codec));
        } else {
            parts.push(format!("({} Embedded)", codec));
        }
        if parts.is_empty() {
            "Unknown".to_string()
        } else {
            parts.join(" - ")
        }
    } else {
        stream
            .title
            .clone()
            .unwrap_or_else(|| "Unknown".to_string())
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
    let mut map = Map::new();
    map.insert("Id".into(), JsonValue::String(id.to_string()));
    map.insert("Name".into(), JsonValue::String(title.to_string()));
    map.insert("Type".into(), JsonValue::String("Default".to_string()));
    map.insert("Protocol".into(), JsonValue::String("File".to_string()));
    map.insert("Path".into(), JsonValue::String(path.to_string()));
    map.insert("Container".into(), JsonValue::String(container.to_string()));
    map.insert("Size".into(), opt_i64(size));
    map.insert("RunTimeTicks".into(), opt_i64(runtime_ticks));
    map.insert(
        "VideoType".into(),
        JsonValue::String("VideoFile".to_string()),
    );
    map.insert("IsoType".into(), JsonValue::Null);
    map.insert("Video3DFormat".into(), JsonValue::Null);
    map.insert("Timestamp".into(), JsonValue::Null);
    map.insert("Bitrate".into(), JsonValue::Null);
    map.insert("FallbackMaxStreamingBitrate".into(), JsonValue::Null);
    map.insert("SupportsDirectPlay".into(), JsonValue::Bool(true));
    map.insert("SupportsDirectStream".into(), JsonValue::Bool(true));
    map.insert("SupportsTranscoding".into(), JsonValue::Bool(false));
    map.insert("SupportsProbing".into(), JsonValue::Bool(true));
    map.insert("IsInfiniteStream".into(), JsonValue::Bool(false));
    map.insert("IsRemote".into(), JsonValue::Bool(false));
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
    map.insert("MediaAttachments".into(), JsonValue::Array(vec![]));
    map.insert("DefaultAudioStreamIndex".into(), JsonValue::Null);
    map.insert("DefaultSubtitleStreamIndex".into(), JsonValue::Null);
    map.insert("Formats".into(), JsonValue::Array(vec![]));
    map.insert("RequiredHttpHeaders".into(), JsonValue::Object(Map::new()));
    map.insert("TranscodingUrl".into(), JsonValue::Null);
    map.insert("TranscodingContainer".into(), JsonValue::Null);
    map.insert("TranscodingSubProtocol".into(), JsonValue::Null);
    map.insert(
        "DirectStreamUrl".into(),
        JsonValue::String(format!("/Videos/{id}/stream.{container}")),
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
    map.insert("ETag".into(), JsonValue::String(id.to_string()));
    JsonValue::Object(map)
}
