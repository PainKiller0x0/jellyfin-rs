use serde_json::{Value, json};

use crate::{
    db::row_ext::QueryResultExt,
    library::naming::parse_media_name,
    util::unix_to_jellyfin_date,
};

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
    pub fn from_query_result(row: &sea_orm::QueryResult) -> Result<Self, sea_orm::DbErr> {
        Ok(Self {
            id: row.get_str("id")?,
            title: row.get_str("title")?,
            path: row.get_str("path")?,
            library_id: row.get_str("library_id")?,
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
            created_at: row.get_i64("created_at")?,
            modified_at: row.get_i64("modified_at")?,
            is_favorite: row.get_bool_from_i64("is_favorite")?,
            played: row.get_bool_from_i64("played")?,
            playback_position_ticks: row.get_i64("playback_position_ticks").unwrap_or(0),
            played_percentage: row.get_f64("played_percentage").ok().flatten(),
            play_count: row.get_i64("play_count").unwrap_or(0),
            last_played_at: row.get_opt_i64("last_played_at").ok().flatten(),
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
    pub fn from_query_result(row: &sea_orm::QueryResult) -> Result<Self, sea_orm::DbErr> {
        Ok(Self {
            stream_index: row.get_i64("stream_index")?,
            stream_type: row.get_str("stream_type")?,
            codec: row.get_opt_str("codec")?,
            language: row.get_opt_str("language")?,
            title: row.get_opt_str("title")?,
            bit_rate: row.get_opt_i64("bit_rate")?,
            width: row.get_opt_i64("width")?,
            height: row.get_opt_i64("height")?,
            channels: row.get_opt_i64("channels")?,
            sample_rate: row.get_opt_i64("sample_rate")?,
            is_external: row.get_bool_from_i64("is_external")?,
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
