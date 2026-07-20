use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};

use crate::{
    app::state::{AppState, SERVER_NAME, VERSION},
    jellyfin::common::internal_error,
};

const DEFAULT_PROFILE_ID: &str = "00000000-0000-0000-0000-000000000000";
const MAX_DLNA_PROFILE_ID_LEN: usize = 128;
const MAX_DLNA_PROFILE_JSON_BYTES: usize = 256 * 1024;

pub async fn profile_infos(State(state): State<Arc<AppState>>) -> Response {
    match profile_infos_inner(&state.db).await {
        Ok(profiles) => Json(profiles).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn default_profile() -> impl IntoResponse {
    Json(default_device_profile())
}

pub async fn profile_by_id(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Response {
    match profile_by_id_inner(&state.db, &profile_id).await {
        Ok(Some(profile)) => Json(profile).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn create_profile(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    match save_profile_inner(&state.db, body).await {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::BAD_REQUEST.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_profile(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if body.get("Id").and_then(Value::as_str).is_none() {
        body["Id"] = Value::String(profile_id);
    }
    match save_profile_inner(&state.db, body).await {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::BAD_REQUEST.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Response {
    match delete_profile_inner(&state.db, &profile_id).await {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
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
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL>/Dlna/jellyfin-rs/connectionmanager/connectionmanager.xml</SCPDURL>
        <controlURL>/Dlna/jellyfin-rs/connectionmanager/control</controlURL>
        <eventSubURL>/Dlna/jellyfin-rs/connectionmanager/events</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>
        <SCPDURL>/Dlna/jellyfin-rs/contentdirectory/contentdirectory.xml</SCPDURL>
        <controlURL>/Dlna/jellyfin-rs/contentdirectory/control</controlURL>
        <eventSubURL>/Dlna/jellyfin-rs/contentdirectory/events</eventSubURL>
      </service>
    </serviceList>
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

pub async fn connection_manager_description() -> Response {
    service_description(
        "urn:schemas-upnp-org:service:ConnectionManager:1",
        &[
            ("SourceProtocolInfo", "string"),
            ("SinkProtocolInfo", "string"),
            ("CurrentConnectionIDs", "string"),
        ],
    )
}

pub async fn content_directory_description() -> Response {
    service_description(
        "urn:schemas-upnp-org:service:ContentDirectory:1",
        &[
            ("TransferIDs", "string"),
            ("SystemUpdateID", "ui4"),
            ("ContainerUpdateIDs", "string"),
        ],
    )
}

pub async fn connection_manager_control(headers: HeaderMap, body: Bytes) -> Response {
    let action = soap_action(&headers, &body);
    match action.as_deref() {
        Some("GetProtocolInfo") => soap_response(
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetProtocolInfo",
            "<Source>http-get:*:*:*</Source><Sink></Sink>",
        ),
        Some("GetCurrentConnectionIDs") => soap_response(
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetCurrentConnectionIDs",
            "<ConnectionIDs></ConnectionIDs>",
        ),
        Some("GetCurrentConnectionInfo") => soap_response(
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetCurrentConnectionInfo",
            "<RcsID>-1</RcsID><AVTransportID>-1</AVTransportID><ProtocolInfo></ProtocolInfo><PeerConnectionManager></PeerConnectionManager><PeerConnectionID>-1</PeerConnectionID><Direction>Output</Direction><Status>Unknown</Status>",
        ),
        _ => soap_fault(),
    }
}

pub async fn content_directory_control(headers: HeaderMap, body: Bytes) -> Response {
    let action = soap_action(&headers, &body);
    match action.as_deref() {
        Some("GetSystemUpdateID") => soap_response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "GetSystemUpdateID",
            "<Id>0</Id>",
        ),
        Some("GetSortCapabilities") => soap_response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "GetSortCapabilities",
            "<SortCaps></SortCaps>",
        ),
        Some("GetSearchCapabilities") => soap_response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            "GetSearchCapabilities",
            "<SearchCaps></SearchCaps>",
        ),
        Some("Browse") | Some("Search") => soap_response(
            "urn:schemas-upnp-org:service:ContentDirectory:1",
            action.as_deref().unwrap_or("Browse"),
            "<Result>&lt;DIDL-Lite xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/&quot; xmlns:dc=&quot;http://purl.org/dc/elements/1.1/&quot; xmlns:upnp=&quot;urn:schemas-upnp-org:metadata-1-0/upnp/&quot;&gt;&lt;/DIDL-Lite&gt;</Result><NumberReturned>0</NumberReturned><TotalMatches>0</TotalMatches><UpdateID>0</UpdateID>",
        ),
        _ => soap_fault(),
    }
}

pub fn default_device_profile() -> Value {
    json!({
        "Name": "Default",
        "Id": DEFAULT_PROFILE_ID,
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

async fn profile_infos_inner(db: &sea_orm::DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let mut profiles = vec![profile_info(&default_device_profile(), "System")];
    let settings = crate::db::settings::find_by_prefix(db, "dlna_profile:").await?;
    for setting in settings {
        if let Ok(profile) = serde_json::from_str::<Value>(&setting.value) {
            profiles.push(profile_info(&profile, "User"));
        }
    }
    Ok(profiles)
}

async fn profile_by_id_inner(
    db: &sea_orm::DatabaseConnection,
    profile_id: &str,
) -> anyhow::Result<Option<Value>> {
    if profile_id.eq_ignore_ascii_case("default") || profile_id == DEFAULT_PROFILE_ID {
        return Ok(Some(default_device_profile()));
    }
    let Some(profile_id) = custom_profile_id(profile_id) else {
        return Ok(None);
    };
    let key = profile_key(profile_id);
    Ok(crate::jellyfin::system::app_setting(db, &key, "")
        .await
        .parse::<Value>()
        .ok()
        .filter(Value::is_object))
}

async fn save_profile_inner(
    db: &sea_orm::DatabaseConnection,
    mut profile: Value,
) -> anyhow::Result<bool> {
    let profile_id = {
        let Some(object) = profile.as_object_mut() else {
            return Ok(false);
        };
        let Some(profile_id) = object
            .get("Id")
            .and_then(Value::as_str)
            .and_then(custom_profile_id)
        else {
            return Ok(false);
        };
        object.insert("Id".to_string(), Value::String(profile_id.clone()));
        profile_id
    };
    let profile_json = profile.to_string();
    if profile_json.len() > MAX_DLNA_PROFILE_JSON_BYTES {
        return Ok(false);
    }
    crate::jellyfin::system::set_app_setting(db, &profile_key(&profile_id), &profile_json).await?;
    Ok(true)
}

async fn delete_profile_inner(
    db: &sea_orm::DatabaseConnection,
    profile_id: &str,
) -> anyhow::Result<bool> {
    if profile_id.eq_ignore_ascii_case("default") || profile_id == DEFAULT_PROFILE_ID {
        return Ok(false);
    }
    let Some(profile_id) = custom_profile_id(profile_id) else {
        return Ok(false);
    };
    let result = crate::db::settings::delete(db, &profile_key(profile_id)).await?;
    Ok(result.rows_affected > 0)
}

fn profile_key(profile_id: impl AsRef<str>) -> String {
    let profile_id = profile_id.as_ref();
    format!("dlna_profile:{profile_id}")
}

fn custom_profile_id(profile_id: &str) -> Option<String> {
    let profile_id = profile_id.trim();
    if profile_id.eq_ignore_ascii_case("default") || profile_id == DEFAULT_PROFILE_ID {
        return None;
    }
    if profile_id.is_empty() || profile_id.len() > MAX_DLNA_PROFILE_ID_LEN {
        return None;
    }
    profile_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        .then(|| profile_id.to_string())
}

fn profile_info(profile: &Value, profile_type: &str) -> Value {
    let id = profile
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROFILE_ID);
    let name = profile
        .get("Name")
        .and_then(Value::as_str)
        .unwrap_or("Default");
    json!({
        "Id": id,
        "Name": name,
        "Type": profile_type,
        "FriendlyName": name,
        "Manufacturer": profile.get("Manufacturer").and_then(Value::as_str).unwrap_or("Jellyfin"),
        "ManufacturerUrl": profile.get("ManufacturerUrl").and_then(Value::as_str).unwrap_or("https://jellyfin.org/"),
        "ModelName": profile.get("ModelName").and_then(Value::as_str).unwrap_or("Jellyfin Default Profile"),
        "ModelDescription": profile.get("ModelDescription").and_then(Value::as_str).unwrap_or("DLNA device profile"),
        "ModelNumber": profile.get("ModelNumber").and_then(Value::as_str).unwrap_or(VERSION),
        "ModelUrl": profile.get("ModelUrl").and_then(Value::as_str).unwrap_or("https://jellyfin.org/"),
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
    let fallback_bitrate = profile_bitrate(profile, media_type);

    object.insert("SupportsDirectPlay".to_string(), json!(direct_play));
    object.insert("SupportsDirectStream".to_string(), json!(true));
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
    object
        .entry("Protocol".to_string())
        .or_insert_with(|| json!("File"));
    object.insert("EncoderPath".to_string(), Value::Null);
    object.insert("EncoderProtocol".to_string(), Value::Null);
    object.insert("Type".to_string(), json!("Default"));
    object
        .entry("IsRemote".to_string())
        .or_insert_with(|| json!(false));
    if let Some(bitrate) = fallback_bitrate {
        object.insert(
            "FallbackMaxStreamingBitrate".to_string(),
            Value::from(bitrate),
        );
    }
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

fn service_description(service_type: &str, variables: &[(&str, &str)]) -> Response {
    let variables = variables
        .iter()
        .map(|(name, data_type)| {
            format!(
                r#"<stateVariable sendEvents="yes"><name>{}</name><dataType>{}</dataType></stateVariable>"#,
                escape_xml(name),
                escape_xml(data_type)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <serviceType>{}</serviceType>
  <actionList/>
  <serviceStateTable>{}</serviceStateTable>
</scpd>"#,
        escape_xml(service_type),
        variables
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    (StatusCode::OK, headers, xml).into_response()
}

fn soap_action(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    let header_action = headers
        .get("SOAPACTION")
        .or_else(|| headers.get("SoapAction"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('#').next())
        .map(|value| value.trim_matches(['"', '\'']).to_string())
        .filter(|value| !value.is_empty());
    header_action.or_else(|| {
        let body = String::from_utf8_lossy(body);
        [
            "GetProtocolInfo",
            "GetCurrentConnectionIDs",
            "GetCurrentConnectionInfo",
            "GetSystemUpdateID",
            "GetSortCapabilities",
            "GetSearchCapabilities",
            "Browse",
            "Search",
        ]
        .iter()
        .find(|action| body.contains(&format!(":{action}")) || body.contains(&format!("<{action}")))
        .map(|action| (*action).to_string())
    })
}

fn soap_response(service_type: &str, action: &str, inner: &str) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:{action}Response xmlns:u="{service_type}">{inner}</u:{action}Response>
  </s:Body>
</s:Envelope>"#,
        action = escape_xml(action),
        service_type = escape_xml(service_type),
        inner = inner,
    );
    soap_xml(StatusCode::OK, xml)
}

fn soap_fault() -> Response {
    soap_xml(
        StatusCode::INTERNAL_SERVER_ERROR,
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>Invalid Action</faultstring>
      <detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>401</errorCode><errorDescription>Invalid Action</errorDescription></UPnPError></detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#
            .to_string(),
    )
}

fn soap_xml(status: StatusCode, xml: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/xml; charset=utf-8"),
    );
    (status, headers, xml).into_response()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROFILE_ID, MAX_DLNA_PROFILE_ID_LEN, MAX_DLNA_PROFILE_JSON_BYTES,
        apply_playback_profile, connection_manager_control, connection_manager_description,
        content_directory_control, content_directory_description, delete_profile_inner,
        profile_by_id_inner, profile_infos_inner, save_profile_inner,
    };
    use axum::body::Bytes;
    use axum::http::{HeaderMap, HeaderValue};
    use sea_orm::DatabaseConnection;
    use serde_json::json;

    #[tokio::test]
    async fn dlna_service_descriptions_are_xml() {
        let response = connection_manager_description().await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/xml; charset=utf-8"
        );

        let response = content_directory_description().await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/xml; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn dlna_control_returns_basic_soap_responses() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "SOAPACTION",
            HeaderValue::from_static(
                "\"urn:schemas-upnp-org:service:ConnectionManager:1#GetProtocolInfo\"",
            ),
        );
        let response = connection_manager_control(headers, Bytes::new()).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/xml; charset=utf-8"
        );

        let response = content_directory_control(
            HeaderMap::new(),
            Bytes::from_static(br#"<s:Body><u:Browse /></s:Body>"#),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let response = content_directory_control(HeaderMap::new(), Bytes::new()).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn playback_profile_keeps_http_direct_stream_when_direct_play_profile_misses_container() {
        let mut media_source = json!({
            "Container": "mkv",
            "MediaStreams": [
                { "Type": "Video", "Codec": "hevc", "Index": 0 },
                { "Type": "Audio", "Codec": "aac", "Index": 1 }
            ],
        });
        let profile = json!({
            "DirectPlayProfiles": [
                { "Container": "mp4", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac" }
            ]
        });
        let streams = media_source["MediaStreams"].as_array().unwrap().clone();

        apply_playback_profile(
            &mut media_source,
            &profile,
            &streams,
            &std::collections::HashMap::new(),
        );

        assert_eq!(media_source["SupportsDirectPlay"], false);
        assert_eq!(media_source["SupportsDirectStream"], true);
        assert_eq!(media_source["SupportsTranscoding"], false);
    }

    #[test]
    fn playback_profile_preserves_media_bitrate_and_sets_fallback_limit() {
        let mut media_source = json!({
            "Container": "mkv",
            "Bitrate": 18_000_000,
            "MediaStreams": [
                { "Type": "Video", "Codec": "hevc", "Index": 0 },
                { "Type": "Audio", "Codec": "aac", "Index": 1 }
            ],
        });
        let profile = json!({
            "MaxStaticBitrate": 200_000_000,
            "DirectPlayProfiles": [
                { "Container": "mkv", "Type": "Video", "VideoCodec": "hevc", "AudioCodec": "aac" }
            ]
        });
        let streams = media_source["MediaStreams"].as_array().unwrap().clone();

        apply_playback_profile(
            &mut media_source,
            &profile,
            &streams,
            &std::collections::HashMap::new(),
        );

        assert_eq!(media_source["Bitrate"], 18_000_000);
        assert_eq!(media_source["FallbackMaxStreamingBitrate"], 200_000_000);
    }

    #[test]
    fn playback_profile_preserves_remote_http_media_source_flags() {
        let mut media_source = json!({
            "Container": "mp4",
            "Protocol": "Http",
            "IsRemote": true,
            "Path": "https://example.test/movie.mp4",
            "MediaStreams": [
                { "Type": "Video", "Codec": "h264", "Index": 0 },
                { "Type": "Audio", "Codec": "aac", "Index": 1 }
            ],
        });
        let profile = json!({
            "DirectPlayProfiles": [
                { "Container": "mp4", "Type": "Video", "VideoCodec": "h264", "AudioCodec": "aac" }
            ]
        });
        let streams = media_source["MediaStreams"].as_array().unwrap().clone();

        apply_playback_profile(
            &mut media_source,
            &profile,
            &streams,
            &std::collections::HashMap::new(),
        );

        assert_eq!(media_source["Protocol"], "Http");
        assert_eq!(media_source["IsRemote"], true);
        assert_eq!(media_source["Path"], "https://example.test/movie.mp4");
    }

    #[tokio::test]
    async fn dlna_profiles_can_be_saved_listed_and_deleted() {
        let Some(db) = test_db().await else {
            return;
        };
        let profile = json!({ "Id": "living-room", "Name": "Living Room" });

        assert!(save_profile_inner(&db, profile).await.unwrap());
        assert_eq!(
            profile_by_id_inner(&db, "living-room")
                .await
                .unwrap()
                .unwrap()["Name"],
            "Living Room"
        );
        assert!(
            profile_infos_inner(&db)
                .await
                .unwrap()
                .iter()
                .any(|profile| profile["Id"] == "living-room")
        );
        assert!(delete_profile_inner(&db, "living-room").await.unwrap());
        assert!(
            profile_by_id_inner(&db, "living-room")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn default_dlna_profile_is_read_only() {
        let Some(db) = test_db().await else {
            return;
        };
        assert!(
            !save_profile_inner(&db, json!({ "Id": DEFAULT_PROFILE_ID, "Name": "Changed" }))
                .await
                .unwrap()
        );
        assert!(!delete_profile_inner(&db, DEFAULT_PROFILE_ID).await.unwrap());
        assert_eq!(
            profile_by_id_inner(&db, DEFAULT_PROFILE_ID)
                .await
                .unwrap()
                .unwrap()["Name"],
            "Default"
        );
    }

    #[tokio::test]
    async fn dlna_profile_writes_are_limited() {
        let Some(db) = test_db().await else {
            return;
        };
        assert!(!save_profile_inner(&db, json!(["bad"])).await.unwrap());
        assert!(
            !save_profile_inner(&db, json!({ "Id": "../bad", "Name": "Bad" }))
                .await
                .unwrap()
        );
        assert!(
            !save_profile_inner(
                &db,
                json!({ "Id": "x".repeat(MAX_DLNA_PROFILE_ID_LEN + 1), "Name": "Bad" })
            )
            .await
            .unwrap()
        );
        assert!(
            !save_profile_inner(
                &db,
                json!({ "Id": "big", "Name": "x".repeat(MAX_DLNA_PROFILE_JSON_BYTES) })
            )
            .await
            .unwrap()
        );

        assert!(
            save_profile_inner(&db, json!({ "Id": " ok-id_1.2 ", "Name": "Trimmed" }))
                .await
                .unwrap()
        );
        assert!(profile_by_id_inner(&db, "../bad").await.unwrap().is_none());
        assert!(!delete_profile_inner(&db, "../bad").await.unwrap());
        assert_eq!(
            profile_by_id_inner(&db, "ok-id_1.2")
                .await
                .unwrap()
                .unwrap()["Id"],
            "ok-id_1.2"
        );
    }

    async fn test_db() -> Option<DatabaseConnection> {
        crate::db::test_db().await
    }
}
