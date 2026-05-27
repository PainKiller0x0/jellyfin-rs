use std::path::Path;
use std::process::Command;

/// Generate a trickplay tile strip using ffmpeg.
/// Extracts frames at regular intervals and arranges them in a horizontal strip.
pub fn generate_trickplay_tiles(
    media_path: &Path,
    output_dir: &Path,
    ffmpeg_path: &str,
    width: u32,
    interval_secs: u64,
) -> anyhow::Result<TrickplayResult> {
    // Get video duration first
    let duration = get_video_duration(media_path, ffmpeg_path)?;
    if duration <= 0.0 {
        anyhow::bail!("video has zero duration");
    }

    let height = (width * 9) / 16; // 16:9 aspect ratio
    let frame_count = (duration / interval_secs as f64).ceil() as u32;
    let tiles_per_row = 10; // 10 tiles per row in the strip

    std::fs::create_dir_all(output_dir)?;

    let mut tile_paths = Vec::new();
    let mut row = 0u32;
    let mut col = 0u32;
    let mut current_tile_frames = Vec::new();

    for i in 0..frame_count {
        let timestamp = i as f64 * interval_secs as f64;
        let frame_path = output_dir.join(format!("frame_{i:04}.jpg"));

        // Extract single frame
        let status = Command::new(ffmpeg_path)
            .args([
                "-ss",
                &timestamp.to_string(),
                "-i",
                &media_path.to_string_lossy(),
                "-vframes",
                "1",
                "-vf",
                &format!("scale={width}:{height}"),
                "-y",
                &frame_path.to_string_lossy(),
            ])
            .output()?;

        if status.status.success() && frame_path.exists() {
            current_tile_frames.push(frame_path.clone());
        }

        col += 1;
        if col >= tiles_per_row || i == frame_count - 1 {
            // Create a tile strip for this row
            if !current_tile_frames.is_empty() {
                let tile_path = output_dir.join(format!("tiles_{row:04}.jpg"));
                create_tile_strip(&current_tile_frames, &tile_path, width, height)?;
                tile_paths.push(tile_path);

                // Clean up individual frames
                for f in &current_tile_frames {
                    let _ = std::fs::remove_file(f);
                }
            }
            current_tile_frames.clear();
            col = 0;
            row += 1;
        }
    }

    Ok(TrickplayResult {
        tile_count: frame_count,
        interval_ticks: interval_secs as i64 * 10_000_000,
        width,
        height,
        tile_paths,
    })
}

pub struct TrickplayResult {
    pub tile_count: u32,
    pub interval_ticks: i64,
    pub width: u32,
    pub height: u32,
    pub tile_paths: Vec<std::path::PathBuf>,
}

fn get_video_duration(path: &Path, ffprobe_path: &str) -> anyhow::Result<f64> {
    let output = Command::new(ffprobe_path)
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &path.to_string_lossy(),
        ])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .trim()
        .parse::<f64>()
        .map_err(|e| anyhow::anyhow!("failed to parse duration: {e}"))
}

fn create_tile_strip(
    frames: &[std::path::PathBuf],
    output: &Path,
    tile_width: u32,
    tile_height: u32,
) -> anyhow::Result<()> {
    // Use ffmpeg's tile filter to create a horizontal strip
    let n = frames.len();
    if n == 0 {
        return Ok(());
    }

    let mut cmd = Command::new("ffmpeg");
    for f in frames {
        cmd.args(["-i", &f.to_string_lossy()]);
    }

    let filter = if n == 1 {
        format!("scale={tile_width}:{tile_height}")
    } else {
        format!(
            "{}scale={tile_width}:{tile_height},tile={n}x1",
            (0..n)
                .map(|i| format!("[{i}:v]"))
                .collect::<String>()
        )
    };

    cmd.args(["-filter_complex", &filter, "-y", &output.to_string_lossy()]);
    let status = cmd.output()?;
    if !status.status.success() {
        anyhow::bail!("ffmpeg tile strip creation failed");
    }
    Ok(())
}
