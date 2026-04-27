use serde_json::{Value, json};
use sqlx::Row;

use crate::{library::naming::parse_media_name, util::unix_to_jellyfin_date};

#[derive(Clone, Debug)]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    pub path: String,
    pub library_id: String,
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
    pub created_at: i64,
    pub modified_at: i64,
    pub is_favorite: bool,
    pub played: bool,
    pub playback_position_ticks: i64,
    pub played_percentage: Option<f64>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

impl MediaItem {
    pub fn from_row(row: sqlx::any::AnyRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            path: row.try_get("path")?,
            library_id: row.try_get("library_id")?,
            parent_id: row.try_get("parent_id")?,
            item_type: row.try_get("item_type")?,
            is_folder: row.try_get::<i64, _>("is_folder").unwrap_or_default() != 0,
            container: row.try_get("container")?,
            overview: row.try_get("overview")?,
            official_rating: row.try_get("official_rating")?,
            extended_video_type: row.try_get("extended_video_type")?,
            production_year: row.try_get("production_year")?,
            runtime_ticks: row.try_get("runtime_ticks")?,
            size_bytes: row.try_get("size_bytes")?,
            created_at: row.try_get("created_at")?,
            modified_at: row.try_get("modified_at")?,
            is_favorite: row.try_get::<i64, _>("is_favorite").unwrap_or_default() != 0,
            played: row.try_get::<i64, _>("played").unwrap_or_default() != 0,
            playback_position_ticks: row.try_get("playback_position_ticks").unwrap_or_default(),
            played_percentage: row.try_get("played_percentage").ok(),
            play_count: row.try_get("play_count").unwrap_or_default(),
            last_played_at: row.try_get("last_played_at").ok().flatten(),
        })
    }

    pub fn to_jellyfin_json(&self) -> Value {
        let parsed_name = parse_media_name(std::path::Path::new(&self.path), &self.library_id);
        let media_sources = if self.is_folder {
            json!([])
        } else {
            json!([media_source_json(self)])
        };

        json!({
            "Name": self.title,
            "Id": self.id,
            "Type": self.item_type,
            "Path": self.path,
            "LibraryId": self.library_id,
            "ParentId": self.parent_id,
            "RunTimeTicks": self.runtime_ticks,
            "Container": self.container,
            "Overview": self.overview,
            "OfficialRating": self.official_rating,
            "ExtendedVideoType": self.extended_video_type,
            "ProductionYear": self.production_year,
            "PremiereDate": self.production_year.map(|year| format!("{year}-01-01T00:00:00.0000000Z")),
            "IndexNumber": parsed_name.episode_number,
            "ParentIndexNumber": parsed_name.season_number.or_else(|| season_number(&self.title)),
            "IndexNumberEnd": parsed_name.ending_episode_number,
            "OriginalTitle": parsed_name.version,
            "SortName": self.title,
            "ProviderIds": {},
            "LockData": false,
            "CanDelete": true,
            "CanDownload": true,
            "Size": self.size_bytes,
            "UserData": {
                "Played": self.played,
                "IsFavorite": self.is_favorite,
                "PlaybackPositionTicks": self.playback_position_ticks,
                "PlayedPercentage": self.played_percentage,
            },
            "DateCreated": unix_to_jellyfin_date(self.created_at),
            "DateLastMediaAdded": unix_to_jellyfin_date(self.modified_at),
            "ImageTags": {},
            "MediaSources": media_sources,
        })
    }
}

fn season_number(value: &str) -> Option<i64> {
    let lower = value.to_ascii_lowercase();
    let digits = lower
        .strip_prefix("season")
        .or_else(|| lower.strip_prefix('s'))?
        .trim_matches(|c: char| !c.is_ascii_digit());
    digits.parse().ok()
}

pub fn media_source_json(item: &MediaItem) -> Value {
    media_source_json_with_streams(item, Vec::new())
}

pub fn media_source_json_with_streams(item: &MediaItem, media_streams: Vec<Value>) -> Value {
    let container = item.container.as_deref().unwrap_or("bin");
    let stream_path = match item.item_type.as_str() {
        "Audio" => format!("/Audio/{}/universal", item.id),
        _ => format!("/Videos/{}/stream.{container}", item.id),
    };

    json!({
        "Id": item.id,
        "Name": item.title,
        "Size": item.size_bytes,
        "Path": item.path,
        "Container": item.container,
        "RunTimeTicks": item.runtime_ticks,
        "SupportsDirectPlay": true,
        "SupportsDirectStream": true,
        "SupportsTranscoding": false,
        "DirectStreamUrl": stream_path,
        "TranscodingUrl": null,
        "MediaStreams": media_streams,
        "ETag": item.id,
    })
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
    pub is_external: bool,
}

impl MediaStreamRow {
    pub fn from_row(row: sqlx::any::AnyRow) -> sqlx::Result<Self> {
        Ok(Self {
            stream_index: row.try_get("stream_index")?,
            stream_type: row.try_get("stream_type")?,
            codec: row.try_get("codec")?,
            language: row.try_get("language")?,
            title: row.try_get("title")?,
            bit_rate: row.try_get("bit_rate")?,
            width: row.try_get("width")?,
            height: row.try_get("height")?,
            channels: row.try_get("channels")?,
            sample_rate: row.try_get("sample_rate")?,
            is_external: row.try_get::<i64, _>("is_external").unwrap_or_default() != 0,
        })
    }

    pub fn to_jellyfin_json(&self, item_id: &str) -> Value {
        let codec = self.codec.as_deref().unwrap_or_default();
        let delivery_url = if self.stream_type == "Subtitle" && self.is_external {
            Some(format!(
                "/Videos/{item_id}/Subtitles/{}/Stream.{}",
                self.stream_index,
                if codec.is_empty() { "srt" } else { codec }
            ))
        } else {
            None
        };

        json!({
            "Index": self.stream_index,
            "Type": self.stream_type,
            "Codec": self.codec,
            "Language": self.language,
            "DisplayLanguage": self.language,
            "Title": self.title,
            "DisplayTitle": self.title,
            "BitRate": self.bit_rate,
            "Width": self.width,
            "Height": self.height,
            "Channels": self.channels,
            "SampleRate": self.sample_rate,
            "DeliveryUrl": delivery_url,
            "IsExternal": self.is_external,
        })
    }
}
