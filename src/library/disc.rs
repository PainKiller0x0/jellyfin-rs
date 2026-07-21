use std::path::{Path, PathBuf};

use bdinfo_rs_core::{
    bdrom::{
        disc::{BdRom, ScanMode},
        order::PlaylistFilter,
    },
    stream::TsStreamType,
    vfs::fs::FsDir,
};
use walkdir::WalkDir;

pub struct DiscProbePlan {
    pub files: Vec<PathBuf>,
    pub runtime_ticks: Option<i64>,
    pub chapter_ticks: Vec<i64>,
    pub streams: Vec<DiscStream>,
}

pub struct DiscStream {
    pub stream_type: String,
    pub codec: String,
    pub language: Option<String>,
    pub bit_rate: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub average_frame_rate: Option<f64>,
    pub channels: Option<i64>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub is_interlaced: bool,
}

pub fn probe_plan(path: &Path, video_type: &str) -> Option<DiscProbePlan> {
    match video_type {
        "Dvd" => dvd_probe_plan(path),
        "BluRay" => bluray_probe_plan(path),
        _ => None,
    }
}

fn dvd_probe_plan(path: &Path) -> Option<DiscProbePlan> {
    let mut vobs = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.into_path();
            let name = path.file_name()?.to_str()?;
            (path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vob"))
                && !name.eq_ignore_ascii_case("VIDEO_TS.VOB")
                && !name.to_ascii_uppercase().ends_with("_0.VOB"))
            .then_some(path)
        })
        .collect::<Vec<_>>();
    vobs.sort();
    let first = vobs.first()?;

    // Jellyfin keeps every segment number represented by a >=900 MiB VOB,
    // falling back to the first segment number when no large VOB exists.
    let mut title_segments = vobs
        .iter()
        .filter(|path| {
            path.metadata()
                .ok()
                .is_some_and(|metadata| metadata.len() >= 900 * 1024 * 1024)
        })
        .filter_map(|path| dvd_segment_number(path))
        .collect::<Vec<_>>();
    title_segments.sort();
    title_segments.dedup();
    if title_segments.is_empty() {
        title_segments.push(dvd_segment_number(first)?);
    }
    vobs.retain(|path| {
        dvd_segment_number(path)
            .is_some_and(|segment| title_segments.iter().any(|title| title == &segment))
    });

    (!vobs.is_empty()).then_some(DiscProbePlan {
        files: vobs,
        runtime_ticks: None,
        chapter_ticks: Vec::new(),
        streams: Vec::new(),
    })
}

fn dvd_segment_number(path: &Path) -> Option<String> {
    path.file_stem()?
        .to_str()?
        .rsplit('_')
        .next()
        .map(ToString::to_string)
}

fn bluray_probe_plan(path: &Path) -> Option<DiscProbePlan> {
    let root = FsDir::new(path);
    // Jellyfin's BDInfo examiner calls BDROM.Scan(), which performs the full
    // packet scan and supplies measured stream bitrates.
    let report = BdRom::open_resilient(&root, ScanMode::Full).ok()?;
    let order = report
        .bdrom
        .presentation_order(&PlaylistFilter::everything());
    let playlist = order
        .first()
        .and_then(|index| report.bdrom.playlists.get(*index))?;
    let stream_dir = bluray_stream_directory(path)?;
    let files = playlist
        .clips
        .iter()
        .filter(|clip| clip.angle_index == 0)
        .filter_map(|clip| case_insensitive_child_file(&stream_dir, &clip.name))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return None;
    }

    Some(DiscProbePlan {
        files,
        runtime_ticks: seconds_to_ticks(playlist.total_length),
        chapter_ticks: playlist
            .chapters
            .iter()
            .filter_map(|chapter| seconds_to_ticks(chapter.time_in))
            .collect(),
        streams: playlist
            .streams
            .iter()
            .filter(|stream| stream.angle_index == 0)
            .filter_map(disc_stream)
            .collect(),
    })
}

fn disc_stream(stream: &bdinfo_rs_core::bdrom::disc::StreamSummary) -> Option<DiscStream> {
    let (stream_type, codec) =
        disc_stream_type_and_codec(stream.stream_type, &stream.codec_short_name)?;
    let height = (stream.height > 0).then_some(i64::from(stream.height));
    let width = height.and_then(bluray_width_for_height);
    let channel_layout =
        (!stream.channel_description.is_empty()).then(|| stream.channel_description.clone());
    Some(DiscStream {
        stream_type: stream_type.to_string(),
        codec,
        language: (!stream.language_code.is_empty()).then(|| stream.language_code.clone()),
        bit_rate: (stream.bitrate > 0).then_some(stream.bitrate),
        width,
        height,
        average_frame_rate: frame_rate_from_description(&stream.description),
        channels: channel_layout.as_deref().and_then(channels_from_layout),
        channel_layout,
        sample_rate: (stream.sample_rate > 0).then_some(i64::from(stream.sample_rate)),
        bit_depth: (stream.bit_depth > 0).then_some(i64::from(stream.bit_depth)),
        is_interlaced: ["480i", "576i", "1080i"]
            .iter()
            .any(|format| stream.description.to_ascii_lowercase().contains(format)),
    })
}

fn disc_stream_type_and_codec(
    stream_type: TsStreamType,
    codec_short_name: &str,
) -> Option<(&'static str, String)> {
    use TsStreamType::*;
    let kind = match stream_type {
        Mpeg1Video | Mpeg2Video | AvcVideo | MvcVideo | HevcVideo | Vc1Video => "Video",
        Mpeg1Audio
        | Mpeg2Audio
        | Mpeg2AacAudio
        | Mpeg4AacAudio
        | LpcmAudio
        | Ac3Audio
        | Ac3PlusAudio
        | Ac3PlusSecondaryAudio
        | Ac3TrueHdAudio
        | DtsAudio
        | DtsHdAudio
        | DtsHdSecondaryAudio
        | DtsHdMasterAudio => "Audio",
        PresentationGraphics | InteractiveGraphics | Subtitle => "Subtitle",
        Unknown => return None,
    };
    let codec = match stream_type {
        Mpeg1Video => "mpeg1video",
        Mpeg2Video => "mpeg2video",
        Vc1Video => "vc1",
        Ac3PlusAudio | Ac3PlusSecondaryAudio => "eac3",
        DtsAudio | DtsHdAudio | DtsHdMasterAudio | DtsHdSecondaryAudio => "dts",
        PresentationGraphics => "pgssub",
        _ => codec_short_name,
    };
    Some((kind, codec.to_string()))
}

fn bluray_width_for_height(height: i64) -> Option<i64> {
    match height {
        2160 => Some(3840),
        1080 => Some(1920),
        720 => Some(1280),
        576 | 480 => Some(720),
        _ => None,
    }
}

fn channels_from_layout(layout: &str) -> Option<i64> {
    let layout = layout.split('-').next()?.trim();
    let (main, lfe) = layout.split_once('.')?;
    Some(
        main.parse::<i64>()
            .ok()?
            .saturating_add(lfe.parse::<i64>().ok()?),
    )
}

fn frame_rate_from_description(description: &str) -> Option<f64> {
    let marker = description.find(" fps")?;
    let token = description[..marker].split_whitespace().next_back()?;
    token.parse::<f64>().ok()
}

fn seconds_to_ticks(seconds: f64) -> Option<i64> {
    (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 10_000_000.0).round() as i64)
}

fn bluray_stream_directory(path: &Path) -> Option<PathBuf> {
    let bdmv = if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("bdmv"))
    {
        path.to_path_buf()
    } else {
        case_insensitive_child_directory(path, "BDMV")?
    };
    case_insensitive_child_directory(&bdmv, "STREAM")
}

fn case_insensitive_child_directory(parent: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|candidate| candidate.to_str())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        })
}

fn case_insensitive_child_file(parent: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|candidate| candidate.to_str())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn dvd_plan_omits_menus_and_uses_jellyfin_segment_selection() {
        let root = test_dir("dvd-plan");
        let video_ts = root.join("VIDEO_TS");
        fs::create_dir_all(&video_ts).unwrap();
        for name in [
            "VIDEO_TS.VOB",
            "VTS_01_0.VOB",
            "VTS_01_1.VOB",
            "VTS_01_2.VOB",
            "VTS_02_1.VOB",
        ] {
            fs::write(video_ts.join(name), []).unwrap();
        }

        let plan = dvd_probe_plan(&root).unwrap();
        let names = plan
            .files
            .iter()
            .filter_map(|path| path.file_name()?.to_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["VTS_01_1.VOB", "VTS_02_1.VOB"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bdinfo_codecs_are_normalized_like_jellyfin() {
        assert_eq!(
            disc_stream_type_and_codec(TsStreamType::Mpeg2Video, "MPEG-2"),
            Some(("Video", "mpeg2video".to_string()))
        );
        assert_eq!(
            disc_stream_type_and_codec(TsStreamType::Ac3PlusAudio, "AC3+"),
            Some(("Audio", "eac3".to_string()))
        );
        assert_eq!(
            disc_stream_type_and_codec(TsStreamType::PresentationGraphics, "PGS"),
            Some(("Subtitle", "pgssub".to_string()))
        );
        assert_eq!(channels_from_layout("5.1"), Some(6));
    }

    fn test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jellyfin-rs-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
