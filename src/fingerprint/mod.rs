use std::path::Path;
use std::process::Command;

/// Extract audio fingerprint using fpcalc CLI tool.
/// Returns the fingerprint bytes and duration in seconds.
#[allow(dead_code)]
pub fn extract_fingerprint(
    media_path: &Path,
    fpcalc_path: &str,
    max_duration_secs: Option<i64>,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let mut cmd = Command::new(fpcalc_path);
    cmd.arg("-raw");
    if let Some(dur) = max_duration_secs {
        cmd.arg("-length").arg(dur.to_string());
    }
    cmd.arg(media_path);

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run fpcalc: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("fpcalc failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut fingerprint = Vec::new();
    let mut duration = 0.0f64;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("FINGERPRINT=") {
            // Parse space-separated integers
            for num_str in rest.split_whitespace() {
                if let Ok(num) = num_str.parse::<i32>() {
                    fingerprint.extend_from_slice(&num.to_le_bytes());
                }
            }
        } else if let Some(rest) = line.strip_prefix("DURATION=") {
            duration = rest.parse().unwrap_or(0.0);
        }
    }

    Ok((fingerprint, duration))
}

/// Compare two fingerprints and return a similarity score (0.0 to 1.0).
/// Uses a simple Hamming-distance-like comparison on the raw integer arrays.
#[allow(dead_code)]
pub fn compare_fingerprints(fp1: &[u8], fp2: &[u8]) -> f64 {
    if fp1.is_empty() || fp2.is_empty() {
        return 0.0;
    }

    let len = fp1.len().min(fp2.len());
    let mut matching = 0;
    let total = len / 4; // each i32 is 4 bytes

    if total == 0 {
        return 0.0;
    }

    for i in 0..total {
        let offset = i * 4;
        let a = i32::from_le_bytes([
            fp1[offset],
            fp1[offset + 1],
            fp1[offset + 2],
            fp1[offset + 3],
        ]);
        let b = i32::from_le_bytes([
            fp2[offset],
            fp2[offset + 1],
            fp2[offset + 2],
            fp2[offset + 3],
        ]);

        // Count matching bits
        let xor = (a ^ b) as u32;
        let differing_bits = xor.count_ones();
        if differing_bits <= 8 {
            matching += 1;
        }
    }

    matching as f64 / total as f64
}
