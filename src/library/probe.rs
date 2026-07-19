use std::{collections::HashMap, path::Path, process::Command, time::Duration};

use serde::{Deserialize, Deserializer};

#[derive(Default)]
pub struct MediaProbe {
    pub runtime_ticks: Option<i64>,
    pub size_bytes: Option<i64>,
    pub streams: Vec<ProbedStream>,
}

pub struct ProbedStream {
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
}

pub fn probe_media(path: &Path) -> Option<MediaProbe> {
    let ffprobe =
        std::env::var("JELLYFIN_RS_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_string());
    let analyze_duration = std::env::var("JELLYFIN_RS_FFPROBE_ANALYZE_DURATION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "30000000".to_string());
    let probe_size = std::env::var("JELLYFIN_RS_FFPROBE_PROBE_SIZE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "100000000".to_string());

    let mut command = Command::new(ffprobe);
    command
        .arg("-v")
        .arg("warning")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg("-show_frames")
        .arg("-read_intervals")
        .arg("%+#1");
    if analyze_duration != "0" {
        command.arg("-analyzeduration").arg(analyze_duration);
    }
    if probe_size != "0" {
        command.arg("-probesize").arg(probe_size);
    }

    let output = command
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            // Wait with timeout (30 seconds)
            let timeout = Duration::from_secs(30);
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let stdout = child.wait_with_output().ok()?.stdout;
                        if !status.success() {
                            return None;
                        }
                        return Some(stdout);
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            let _ = child.kill();
                            let _ = child.wait();
                            tracing::warn!("ffprobe timed out for: {}", path.display());
                            return None;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => return None,
                }
            }
        })?;

    let response = serde_json::from_slice::<FfprobeResponse>(&output).ok()?;
    Some(media_probe_from_ffprobe_response(response))
}

fn media_probe_from_ffprobe_response(response: FfprobeResponse) -> MediaProbe {
    let FfprobeResponse {
        streams,
        frames,
        format,
    } = response;
    let frames_by_stream = frames
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|frame| frame.stream_index.map(|index| (index, frame)))
        .collect::<HashMap<_, _>>();
    let hdr10_plus_streams = frames
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|frame| {
            frame
                .side_data_list
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|side_data| {
                    side_data.side_data_type.as_deref().is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("HDR Dynamic Metadata SMPTE2094-40 (HDR10+)")
                    })
                })
        })
        .filter_map(|frame| frame.stream_index)
        .collect::<std::collections::HashSet<_>>();
    let runtime_ticks = format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(|duration| duration.parse::<f64>().ok())
        .map(|seconds| (seconds * 10_000_000.0) as i64);
    let size_bytes = format
        .as_ref()
        .and_then(|format| format.size.as_deref())
        .and_then(parse_i64)
        .filter(|size| *size > 0);

    MediaProbe {
        runtime_ticks,
        size_bytes,
        streams: streams
            .into_iter()
            .filter_map(|stream| {
                let frame = frames_by_stream.get(&stream.index).copied();
                let has_hdr10_plus = hdr10_plus_streams.contains(&stream.index);
                ProbedStream::from_ffprobe(stream, frame, has_hdr10_plus)
            })
            .collect(),
    }
}

impl ProbedStream {
    fn from_ffprobe(
        stream: FfprobeStream,
        frame: Option<&FfprobeFrame>,
        has_hdr10_plus: bool,
    ) -> Option<Self> {
        if stream
            .disposition
            .as_ref()
            .is_some_and(|disposition| disposition.attached_pic.unwrap_or(0) != 0)
        {
            return None;
        }

        let stream_type = match stream.codec_type.as_deref()? {
            "video" => "Video",
            "audio" => "Audio",
            "subtitle" => "Subtitle",
            _ => return None,
        }
        .to_string();
        let language = tag_value(stream.tags.as_ref(), "language");
        let mut title = tag_value(stream.tags.as_ref(), "title");
        let comment = tag_value(stream.tags.as_ref(), "comment");
        if title.is_none() {
            let handler = tag_value(stream.tags.as_ref(), "handler_name");
            let default_handler = match stream_type.as_str() {
                "Audio" => "SoundHandler",
                "Subtitle" => "SubtitleHandler",
                _ => "",
            };
            if handler
                .as_deref()
                .is_some_and(|value| !value.eq_ignore_ascii_case(default_handler))
            {
                title = handler;
            }
        }

        let bit_rate = stream
            .bit_rate
            .as_deref()
            .and_then(parse_i64)
            .or_else(|| tag_value(stream.tags.as_ref(), "BPS").and_then(|value| parse_i64(&value)))
            .or_else(|| bitrate_from_duration_tags(stream.tags.as_ref()));
        let channel_layout = stream
            .channel_layout
            .as_deref()
            .map(parse_channel_layout)
            .filter(|value| !value.is_empty());
        let pixel_format = stream.pixel_format.clone().or_else(|| {
            frame
                .and_then(|frame| frame.pixel_format.as_ref())
                .map(ToString::to_string)
        });
        let bit_depth = parse_bit_depth(
            stream.bits_per_sample,
            stream.bits_per_raw_sample,
            &pixel_format,
        );
        let color_range = stream
            .color_range
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_range.clone()));
        let color_space = stream
            .color_space
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_space.clone()));
        let color_transfer = stream
            .color_transfer
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_transfer.clone()));
        let color_primaries = stream
            .color_primaries
            .clone()
            .or_else(|| frame.and_then(|frame| frame.color_primaries.clone()));
        let is_interlaced = stream
            .field_order
            .as_deref()
            .is_some_and(|field_order| !field_order.eq_ignore_ascii_case("progressive"))
            || frame.and_then(|frame| frame.interlaced_frame).unwrap_or(0) != 0;
        let aspect_ratio = aspect_ratio(
            stream.display_aspect_ratio.as_deref(),
            stream.width,
            stream.height,
        );
        let is_anamorphic = is_anamorphic(
            stream.sample_aspect_ratio.as_deref(),
            stream.display_aspect_ratio.as_deref(),
            stream.width,
            stream.height,
        );
        let (video_range, video_range_type) =
            video_range(&stream_type, color_transfer.as_deref(), has_hdr10_plus);
        let disposition = stream.disposition.as_ref();

        Some(Self {
            stream_index: stream.index,
            stream_type,
            codec: stream.codec_name,
            profile: stream.profile,
            codec_tag: stream
                .codec_tag_string
                .and_then(|tag| (!tag.trim().is_empty() && !tag.contains("[0]")).then_some(tag)),
            language,
            title,
            comment,
            bit_rate,
            width: stream.width,
            height: stream.height,
            aspect_ratio,
            average_frame_rate: parse_frame_rate(stream.avg_frame_rate.as_deref()),
            real_frame_rate: parse_frame_rate(stream.real_frame_rate.as_deref()),
            reference_frame_rate: parse_frame_rate(stream.real_frame_rate.as_deref()),
            channels: stream.channels,
            channel_layout,
            sample_rate: stream.sample_rate.as_deref().and_then(parse_i64),
            bit_depth,
            ref_frames: stream.refs.filter(|refs| *refs > 0),
            is_interlaced,
            is_avc: stream.is_avc,
            is_anamorphic,
            pixel_format,
            level: stream.level,
            color_range,
            color_space,
            color_transfer,
            color_primaries,
            time_base: stream.time_base,
            codec_time_base: stream.codec_time_base,
            nal_length_size: stream.nal_length_size,
            rotation: frame
                .and_then(|frame| frame.side_data_list.as_deref())
                .and_then(|side_data| side_data.iter().find_map(|data| data.rotation)),
            video_range,
            video_range_type,
            hdr10_plus_present_flag: has_hdr10_plus.then_some(true),
            is_default: disposition.is_some_and(|d| d.default.unwrap_or(0) != 0),
            is_forced: disposition.is_some_and(|d| d.forced.unwrap_or(0) != 0),
            is_hearing_impaired: disposition.is_some_and(|d| d.hearing_impaired.unwrap_or(0) != 0),
            is_original: disposition
                .and_then(|d| d.original)
                .map(|original| original != 0),
        })
    }
}

#[derive(Deserialize)]
struct FfprobeResponse {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    #[serde(default)]
    frames: Option<Vec<FfprobeFrame>>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    size: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    index: i64,
    profile: Option<String>,
    codec_name: Option<String>,
    codec_type: Option<String>,
    codec_tag_string: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    display_aspect_ratio: Option<String>,
    sample_aspect_ratio: Option<String>,
    avg_frame_rate: Option<String>,
    #[serde(rename = "r_frame_rate")]
    real_frame_rate: Option<String>,
    channels: Option<i64>,
    channel_layout: Option<String>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    bits_per_sample: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    bits_per_raw_sample: Option<i64>,
    refs: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    is_avc: Option<bool>,
    #[serde(rename = "pix_fmt")]
    pixel_format: Option<String>,
    level: Option<i64>,
    field_order: Option<String>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    time_base: Option<String>,
    codec_time_base: Option<String>,
    nal_length_size: Option<String>,
    disposition: Option<FfprobeDisposition>,
    tags: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct FfprobeDisposition {
    attached_pic: Option<i64>,
    default: Option<i64>,
    forced: Option<i64>,
    hearing_impaired: Option<i64>,
    original: Option<i64>,
}

#[derive(Deserialize)]
struct FfprobeFrame {
    stream_index: Option<i64>,
    #[serde(rename = "pix_fmt")]
    pixel_format: Option<String>,
    interlaced_frame: Option<i64>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    side_data_list: Option<Vec<FfprobeSideData>>,
}

#[derive(Deserialize)]
struct FfprobeSideData {
    side_data_type: Option<String>,
    rotation: Option<i64>,
}

fn tag_value(tags: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    tags.and_then(|tags| {
        tags.iter()
            .find(|(candidate, value)| {
                candidate.eq_ignore_ascii_case(key) && !value.trim().is_empty()
            })
            .map(|(_, value)| value.clone())
    })
}

fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok()
}

fn deserialize_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(value) => parse_i64(&value),
        serde_json::Value::Bool(value) => Some(if value { 1 } else { 0 }),
        _ => None,
    })
}

fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(match value {
        serde_json::Value::Bool(value) => Some(value),
        serde_json::Value::Number(number) => number.as_i64().map(|value| value != 0),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

fn parse_frame_rate(value: Option<&str>) -> Option<f64> {
    let value = value?.trim();
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    if denominator == 0.0 {
        return None;
    }
    let rate = numerator / denominator;
    (rate > 0.0).then_some(rate)
}

fn parse_channel_layout(value: &str) -> String {
    value
        .split_once('(')
        .map(|(layout, _)| layout)
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn parse_bit_depth(
    bits_per_sample: Option<i64>,
    bits_per_raw_sample: Option<i64>,
    pixel_format: &Option<String>,
) -> Option<i64> {
    bits_per_sample
        .filter(|value| *value > 0)
        .or_else(|| bits_per_raw_sample.filter(|value| *value > 0))
        .or_else(|| {
            let fmt = pixel_format.as_deref()?.to_ascii_lowercase();
            for depth in [16_i64, 14, 12, 10, 9] {
                if fmt.contains(&format!("p{depth}")) || fmt.contains(&format!("p{depth}le")) {
                    return Some(depth);
                }
            }
            fmt.starts_with("yuv").then_some(8)
        })
}

fn aspect_ratio(
    display_aspect_ratio: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
) -> Option<String> {
    let ratio = display_aspect_ratio
        .filter(|value| valid_ratio(value))
        .map(ToString::to_string);
    if ratio.is_some() {
        return ratio;
    }
    let (Some(width), Some(height)) = (width, height) else {
        return None;
    };
    if width <= 0 || height <= 0 {
        return None;
    }
    let ratio = width as f64 / height as f64;
    if is_close(ratio, 1.777_777_778, 0.03) {
        Some("16:9".to_string())
    } else if is_close(ratio, 1.333_333_333, 0.05) {
        Some("4:3".to_string())
    } else if is_close(ratio, 1.41, 0.005) {
        Some("1.41:1".to_string())
    } else if is_close(ratio, 1.5, 0.005) {
        Some("1.5:1".to_string())
    } else if is_close(ratio, 1.6, 0.005) {
        Some("1.6:1".to_string())
    } else if is_close(ratio, 1.666_666_667, 0.005) {
        Some("5:3".to_string())
    } else if is_close(ratio, 1.85, 0.02) {
        Some("1.85:1".to_string())
    } else if is_close(ratio, 2.35, 0.025) {
        Some("2.35:1".to_string())
    } else if is_close(ratio, 2.4, 0.025) {
        Some("2.40:1".to_string())
    } else {
        None
    }
}

fn valid_ratio(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    width.parse::<i64>().is_ok_and(|value| value > 0)
        && height.parse::<i64>().is_ok_and(|value| value > 0)
}

fn is_anamorphic(
    sample_aspect_ratio: Option<&str>,
    display_aspect_ratio: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
) -> Option<bool> {
    if let Some(sar) = sample_aspect_ratio {
        if near_square_sar(sar) {
            return Some(false);
        }
        if sar != "0:1" {
            return Some(true);
        }
    }
    let Some(dar) = display_aspect_ratio.filter(|value| valid_ratio(value)) else {
        return sample_aspect_ratio.or(display_aspect_ratio).map(|_| false);
    };
    let derived = aspect_ratio(None, width, height);
    Some(derived.as_deref().is_some_and(|value| value != dar))
}

fn near_square_sar(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    let (Ok(width), Ok(height)) = (width.parse::<f64>(), height.parse::<f64>()) else {
        return false;
    };
    if height == 0.0 {
        return false;
    }
    is_close(width / height, 1.0, 0.001)
}

fn is_close(left: f64, right: f64, variance: f64) -> bool {
    (left - right).abs() <= variance
}

fn bitrate_from_duration_tags(tags: Option<&HashMap<String, String>>) -> Option<i64> {
    let seconds = tag_value(tags, "DURATION").and_then(|value| parse_duration_seconds(&value));
    let bytes = tag_value(tags, "NUMBER_OF_BYTES").and_then(|value| parse_i64(&value));
    let (Some(seconds), Some(bytes)) = (seconds, bytes) else {
        return None;
    };
    (seconds >= 1.0).then_some((bytes as f64 * 8.0 / seconds) as i64)
}

fn parse_duration_seconds(value: &str) -> Option<f64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn video_range(
    stream_type: &str,
    color_transfer: Option<&str>,
    has_hdr10_plus: bool,
) -> (Option<String>, Option<String>) {
    if stream_type != "Video" {
        return (None, None);
    }
    if color_transfer.is_some_and(|value| value.eq_ignore_ascii_case("smpte2084")) {
        return (
            Some("HDR".to_string()),
            Some(if has_hdr10_plus { "HDR10Plus" } else { "HDR10" }.to_string()),
        );
    }
    if color_transfer.is_some_and(|value| value.eq_ignore_ascii_case("arib-std-b67")) {
        return (Some("HDR".to_string()), Some("HLG".to_string()));
    }
    (Some("SDR".to_string()), Some("SDR".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_probe_extracts_format_size() {
        let response: FfprobeResponse = serde_json::from_str(
            r#"{
                "format": {
                    "duration": "12.500000",
                    "size": "987654321"
                },
                "streams": []
            }"#,
        )
        .unwrap();

        let probe = media_probe_from_ffprobe_response(response);

        assert_eq!(probe.runtime_ticks, Some(125_000_000));
        assert_eq!(probe.size_bytes, Some(987_654_321));
    }

    #[test]
    fn media_probe_ignores_missing_or_invalid_format_size() {
        for size in ["", "0", "-1", "unknown"] {
            let response: FfprobeResponse = serde_json::from_str(&format!(
                r#"{{
                    "format": {{
                        "duration": "1.000000",
                        "size": "{size}"
                    }},
                    "streams": []
                }}"#
            ))
            .unwrap();

            let probe = media_probe_from_ffprobe_response(response);

            assert_eq!(probe.size_bytes, None);
        }
    }
}
