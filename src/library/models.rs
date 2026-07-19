use serde_json::{Map, Value as JsonValue, json};

use crate::{
    db::row_ext::QueryResultExt,
    util::{unix_to_jellyfin_date, yyyy_mm_dd_to_jellyfin_date},
};

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
    pub premiere_date: Option<String>,
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
            premiere_date: row.get_opt_str("premiere_date").ok().flatten(),
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
            "Movie" | "Episode" | "Video" => Some("Video"),
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
            .premiere_date
            .as_deref()
            .and_then(yyyy_mm_dd_to_jellyfin_date);
        map.insert("PremiereDate".into(), opt_str(&premiere_date));
        map.insert("EndDate".into(), JsonValue::Null);
        let index_number = if self.item_type == "Season" {
            self.season_number
        } else {
            self.episode_number
        };
        let parent_index_number = if self.item_type == "Episode" {
            self.season_number
        } else {
            None
        };
        map.insert("IndexNumber".into(), opt_i64(index_number));
        map.insert("ParentIndexNumber".into(), opt_i64(parent_index_number));
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

pub fn media_source_json(item: &MediaItem) -> JsonValue {
    media_source_json_with_streams(item, Vec::new())
}

pub fn media_source_json_with_streams(
    item: &MediaItem,
    media_streams: Vec<JsonValue>,
) -> JsonValue {
    let (media_streams, media_attachments) =
        split_media_streams_and_attachments(&item.id, &item.id, media_streams);
    let bitrate = media_source_bitrate(item.size_bytes, item.runtime_ticks, &media_streams);
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
    map.insert("Bitrate".into(), opt_i64(bitrate));
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
    map.insert(
        "MediaAttachments".into(),
        JsonValue::Array(media_attachments),
    );
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
    stream
        .get("Path")
        .and_then(JsonValue::as_str)
        .filter(|path| !path.trim().is_empty())?;
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

fn attachment_mime_type(codec: &str) -> &'static str {
    match codec.to_ascii_lowercase().as_str() {
        "ttf" | "truetype" => "font/ttf",
        "otf" | "opentype" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
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
    let bitrate = media_source_bitrate(size, runtime_ticks, &media_streams);
    let mut map = Map::new();
    map.insert("Id".into(), JsonValue::String(media_source_id.to_string()));
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
    map.insert("Bitrate".into(), opt_i64(bitrate));
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
    map.insert(
        "MediaAttachments".into(),
        JsonValue::Array(media_attachments),
    );
    map.insert("DefaultAudioStreamIndex".into(), JsonValue::Null);
    map.insert("DefaultSubtitleStreamIndex".into(), JsonValue::Null);
    map.insert("Formats".into(), JsonValue::Array(vec![]));
    map.insert("RequiredHttpHeaders".into(), JsonValue::Object(Map::new()));
    map.insert("TranscodingUrl".into(), JsonValue::Null);
    map.insert("TranscodingContainer".into(), JsonValue::Null);
    map.insert("TranscodingSubProtocol".into(), JsonValue::Null);
    map.insert(
        "DirectStreamUrl".into(),
        JsonValue::String(format!(
            "/Videos/{item_id}/{media_source_id}/stream.{container}"
        )),
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
        MediaItem, MediaStreamRow, child_video_source_json, media_source_json_with_streams,
    };
    use serde_json::json;

    #[test]
    fn media_sources_split_attachments_from_playable_streams() {
        let source = media_source_json_with_streams(&video_item(), sample_streams());

        assert_eq!(source["MediaStreams"].as_array().unwrap().len(), 1);
        assert_eq!(source["MediaStreams"][0]["Type"], "Video");
        assert_eq!(source["MediaAttachments"].as_array().unwrap().len(), 1);
        assert_eq!(source["MediaAttachments"][0]["Index"], 5);
        assert_eq!(source["MediaAttachments"][0]["FileName"], "Font.ttf");
        assert_eq!(source["MediaAttachments"][0]["MimeType"], "font/ttf");
        assert_eq!(
            source["MediaAttachments"][0]["DeliveryUrl"],
            "/Videos/movie/movie/Attachments/5"
        );
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
        assert_eq!(source["MediaAttachments"].as_array().unwrap().len(), 1);
        assert_eq!(
            source["MediaAttachments"][0]["DeliveryUrl"],
            "/Videos/part1/part1/Attachments/5"
        );
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
    fn subtitle_streams_report_codec_flags() {
        let text = subtitle_stream("subrip", false).to_jellyfin_json("movie");
        assert_eq!(text["IsTextSubtitleStream"], true);
        assert_eq!(text["IsPgsSubtitleStream"], false);
        assert_eq!(text["IsExtractableSubtitleStream"], true);

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

    fn video_item() -> MediaItem {
        MediaItem {
            id: "movie".to_string(),
            title: "Movie".to_string(),
            path: "D:/Movies/movie.mkv".to_string(),
            library_id: "movies".to_string(),
            collection_type: "movies".to_string(),
            parent_id: String::new(),
            item_type: "Video".to_string(),
            is_folder: false,
            container: Some("mkv".to_string()),
            overview: None,
            official_rating: None,
            extended_video_type: None,
            production_year: None,
            premiere_date: None,
            runtime_ticks: Some(456),
            size_bytes: Some(123),
            season_number: None,
            episode_number: None,
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
        }
    }
}
