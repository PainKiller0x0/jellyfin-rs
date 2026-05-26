use std::{path::Path, process::Command, time::Duration};

use serde::Deserialize;

#[derive(Default)]
pub struct MediaProbe {
    pub runtime_ticks: Option<i64>,
    pub streams: Vec<ProbedStream>,
}

pub struct ProbedStream {
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
}

pub fn probe_media(path: &Path) -> Option<MediaProbe> {
    let ffprobe =
        std::env::var("JELLYFIN_RS_FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_string());
    let output = Command::new(ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg("-analyzeduration")
        .arg("5000000")  // 5 seconds max analysis
        .arg("-probesize")
        .arg("5000000")  // 5MB max probe size
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
    Some(MediaProbe {
        runtime_ticks: response
            .format
            .and_then(|format| format.duration)
            .and_then(|duration| duration.parse::<f64>().ok())
            .map(|seconds| (seconds * 10_000_000.0) as i64),
        streams: response
            .streams
            .into_iter()
            .filter_map(ProbedStream::from_ffprobe)
            .collect(),
    })
}

impl ProbedStream {
    fn from_ffprobe(stream: FfprobeStream) -> Option<Self> {
        let stream_type = match stream.codec_type.as_deref()? {
            "video" => "Video",
            "audio" => "Audio",
            "subtitle" => "Subtitle",
            _ => return None,
        }
        .to_string();

        Some(Self {
            stream_index: stream.index,
            stream_type,
            codec: stream.codec_name,
            language: stream.tags.as_ref().and_then(|tags| tags.language.clone()),
            title: stream.tags.and_then(|tags| tags.title),
            bit_rate: stream.bit_rate.and_then(|value| value.parse().ok()),
            width: stream.width,
            height: stream.height,
            channels: stream.channels,
            sample_rate: stream.sample_rate.and_then(|value| value.parse().ok()),
        })
    }
}

#[derive(Deserialize)]
struct FfprobeResponse {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    index: i64,
    codec_name: Option<String>,
    codec_type: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    channels: Option<i64>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    tags: Option<FfprobeTags>,
}

#[derive(Deserialize)]
struct FfprobeTags {
    language: Option<String>,
    title: Option<String>,
}
