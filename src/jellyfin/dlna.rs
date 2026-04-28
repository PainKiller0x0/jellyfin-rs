use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

use crate::app::state::{AppState, SERVER_NAME, VERSION};

pub async fn profile_infos() -> impl IntoResponse {
    Json(json!([
        {
            "Id": "default",
            "Name": "Default",
            "Type": "System",
            "FriendlyName": "Default",
            "Manufacturer": "Jellyfin",
            "ManufacturerUrl": "https://jellyfin.org/",
            "ModelName": "Jellyfin Default Profile",
            "ModelDescription": "Default DLNA device profile",
            "ModelNumber": VERSION,
            "ModelUrl": "https://jellyfin.org/",
        }
    ]))
}

pub async fn default_profile() -> impl IntoResponse {
    Json(default_device_profile())
}

pub async fn profile_by_id(Path(_profile_id): Path<String>) -> impl IntoResponse {
    Json(default_device_profile())
}

pub async fn device_description(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1:8096");
    let scheme = query.get("scheme").map(String::as_str).unwrap_or("http");
    let base_url = format!("{scheme}://{host}");
    let server_name =
        crate::jellyfin::system::app_setting(&state.db, "ServerName", SERVER_NAME).await;
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <URLBase>{base_url}</URLBase>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
    <friendlyName>{server_name}</friendlyName>
    <manufacturer>Jellyfin</manufacturer>
    <manufacturerURL>https://jellyfin.org/</manufacturerURL>
    <modelDescription>Jellyfin compatible media server</modelDescription>
    <modelName>jellyfin-rs</modelName>
    <modelNumber>{VERSION}</modelNumber>
    <modelURL>https://github.com/dydydd/jellyfin-rs</modelURL>
    <serialNumber>jellyfin-rs</serialNumber>
    <UDN>uuid:jellyfin-rs</UDN>
    <presentationURL>{base_url}/web/index.html</presentationURL>
  </device>
</root>"#,
        base_url = escape_xml(&base_url),
        server_name = escape_xml(&server_name)
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    (StatusCode::OK, headers, xml).into_response()
}

pub fn default_device_profile() -> Value {
    json!({
        "Name": "Default",
        "Id": "00000000-0000-0000-0000-000000000000",
        "MaxStreamingBitrate": 120000000,
        "MaxStaticBitrate": 120000000,
        "MusicStreamingTranscodingBitrate": 192000,
        "MaxStaticMusicBitrate": 120000000,
        "DirectPlayProfiles": [
            { "Container": "mp4,m4v", "Type": "Video", "VideoCodec": "h264,hevc,mpeg4", "AudioCodec": "aac,mp3,ac3,eac3,flac" },
            { "Container": "mkv", "Type": "Video", "VideoCodec": "h264,hevc,vp8,vp9,av1,mpeg2video,mpeg4", "AudioCodec": "aac,mp3,ac3,eac3,dts,flac,opus,vorbis" },
            { "Container": "webm", "Type": "Video", "VideoCodec": "vp8,vp9,av1", "AudioCodec": "vorbis,opus" },
            { "Container": "mov", "Type": "Video", "VideoCodec": "h264,hevc,mpeg4", "AudioCodec": "aac,mp3,ac3" },
            { "Container": "avi", "Type": "Video", "VideoCodec": "mpeg4,mjpeg", "AudioCodec": "mp3,ac3" },
            { "Container": "mp3", "Type": "Audio", "AudioCodec": "mp3" },
            { "Container": "m4a,aac", "Type": "Audio", "AudioCodec": "aac" },
            { "Container": "flac", "Type": "Audio", "AudioCodec": "flac" },
            { "Container": "ogg,opus", "Type": "Audio", "AudioCodec": "opus,vorbis" },
            { "Container": "wav", "Type": "Audio", "AudioCodec": "pcm" },
            { "Container": "jpg,jpeg,png,webp", "Type": "Photo" }
        ],
        "TranscodingProfiles": [
            { "Container": "ts", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac,mp3,ac3", "Protocol": "hls", "EstimateContentLength": false, "EnableMpegtsM2TsMode": false, "TranscodeSeekInfo": "Auto", "CopyTimestamps": false, "Context": "Streaming", "EnableSubtitlesInManifest": true, "MaxAudioChannels": "6", "MinSegments": 1, "SegmentLength": 3, "BreakOnNonKeyFrames": false, "EnableAudioVbrEncoding": true, "Conditions": [] },
            { "Container": "mp3", "Type": "Audio", "AudioCodec": "mp3", "Protocol": "http", "EstimateContentLength": false, "EnableMpegtsM2TsMode": false, "TranscodeSeekInfo": "Auto", "CopyTimestamps": false, "Context": "Streaming", "MaxAudioChannels": "2", "MinSegments": 0, "SegmentLength": 0, "BreakOnNonKeyFrames": false, "EnableAudioVbrEncoding": true, "Conditions": [] }
        ],
        "ContainerProfiles": [],
        "CodecProfiles": [],
        "SubtitleProfiles": [
            { "Format": "srt", "Method": "External" },
            { "Format": "vtt", "Method": "External" },
            { "Format": "ass", "Method": "External" },
            { "Format": "ssa", "Method": "External" }
        ]
    })
}

pub fn request_device_profile(body: Option<&Value>) -> Value {
    body.and_then(|value| value.get("DeviceProfile"))
        .cloned()
        .filter(|value| value.is_object())
        .unwrap_or_else(default_device_profile)
}

pub fn apply_playback_profile(
    media_source: &mut Value,
    profile: &Value,
    media_streams: &[Value],
    query: &HashMap<String, String>,
) {
    let Some(object) = media_source.as_object_mut() else {
        return;
    };
    let container = object
        .get("Container")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let media_type = if media_streams.iter().any(|stream| {
        stream
            .get("Type")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("Video"))
    }) {
        "Video"
    } else {
        "Audio"
    };
    let direct_play = supports_direct_play(profile, media_type, container, media_streams);
    let bitrate = profile_bitrate(profile, media_type);

    object.insert("SupportsDirectPlay".to_string(), json!(direct_play));
    object.insert("SupportsDirectStream".to_string(), json!(direct_play));
    object.insert("SupportsTranscoding".to_string(), json!(false));
    object.insert(
        "DefaultAudioStreamIndex".to_string(),
        first_stream_index(media_streams, "Audio"),
    );
    object.insert("DefaultSubtitleStreamIndex".to_string(), Value::Null);
    object.insert("TranscodingUrl".to_string(), Value::Null);
    object.insert("TranscodingSubProtocol".to_string(), Value::Null);
    object.insert("TranscodingContainer".to_string(), Value::Null);
    object.insert("AnalyzeDurationMs".to_string(), Value::Null);
    object.insert("ReadAtNativeFramerate".to_string(), json!(false));
    object.insert("RequiredHttpHeaders".to_string(), Value::Object(Map::new()));
    object.insert("BufferMs".to_string(), Value::Null);
    object.insert("RequiresOpening".to_string(), json!(false));
    object.insert("RequiresClosing".to_string(), json!(false));
    object.insert("RequiresLooping".to_string(), json!(false));
    object.insert("SupportsProbing".to_string(), json!(true));
    object.insert("VideoType".to_string(), json!("VideoFile"));
    object.insert("IsoType".to_string(), Value::Null);
    object.insert("Protocol".to_string(), json!("File"));
    object.insert("EncoderPath".to_string(), Value::Null);
    object.insert("EncoderProtocol".to_string(), Value::Null);
    object.insert("Type".to_string(), json!("Default"));
    object.insert("IsRemote".to_string(), json!(false));
    object.insert(
        "Bitrate".to_string(),
        bitrate.map_or(Value::Null, Value::from),
    );
    object.insert(
        "DeviceProfileId".to_string(),
        query
            .get("DeviceProfileId")
            .or_else(|| query.get("deviceProfileId"))
            .cloned()
            .or_else(|| {
                profile
                    .get("Id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .map_or(Value::Null, Value::from),
    );
}

fn supports_direct_play(
    profile: &Value,
    media_type: &str,
    container: &str,
    media_streams: &[Value],
) -> bool {
    let Some(profiles) = profile.get("DirectPlayProfiles").and_then(Value::as_array) else {
        return true;
    };
    profiles.iter().any(|play_profile| {
        let profile_type = play_profile
            .get("Type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !profile_type.eq_ignore_ascii_case(media_type) {
            return false;
        }
        if !contains_csv(play_profile.get("Container"), container) {
            return false;
        }
        media_streams
            .iter()
            .all(|stream| stream_supported(play_profile, stream))
    })
}

fn stream_supported(play_profile: &Value, stream: &Value) -> bool {
    let stream_type = stream
        .get("Type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let codec = stream
        .get("Codec")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if codec.is_empty() || stream_type.eq_ignore_ascii_case("Subtitle") {
        return true;
    }
    if stream_type.eq_ignore_ascii_case("Video") {
        return contains_csv(play_profile.get("VideoCodec"), codec);
    }
    if stream_type.eq_ignore_ascii_case("Audio") {
        return contains_csv(play_profile.get("AudioCodec"), codec);
    }
    true
}

fn contains_csv(value: Option<&Value>, needle: &str) -> bool {
    value
        .and_then(Value::as_str)
        .map(|values| {
            values
                .split(',')
                .map(str::trim)
                .any(|value| value.eq_ignore_ascii_case(needle))
        })
        .unwrap_or(true)
}

fn first_stream_index(media_streams: &[Value], stream_type: &str) -> Value {
    media_streams
        .iter()
        .find(|stream| {
            stream
                .get("Type")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case(stream_type))
        })
        .and_then(|stream| stream.get("Index"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn profile_bitrate(profile: &Value, media_type: &str) -> Option<i64> {
    let key = if media_type.eq_ignore_ascii_case("Audio") {
        "MaxStaticMusicBitrate"
    } else {
        "MaxStaticBitrate"
    };
    profile.get(key).and_then(Value::as_i64)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
