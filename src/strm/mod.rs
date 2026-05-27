use std::path::{Path, PathBuf};

pub fn is_strm_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"))
}

pub fn resolve_strm_path(strm_path: &Path) -> anyhow::Result<PathBuf> {
    let content = std::fs::read_to_string(strm_path)
        .map_err(|e| anyhow::anyhow!("failed to read STRM file {}: {e}", strm_path.display()))?;
    let target = content.trim();
    if target.is_empty() {
        anyhow::bail!("empty STRM file: {}", strm_path.display());
    }
    // Support URLs (http/https) — return as-is, caller handles streaming
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(PathBuf::from(target));
    }
    // Support relative paths (relative to the STRM file location)
    let resolved = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        strm_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    };
    // Normalize the path (handle .., ., etc.)
    let resolved = normalize_path(&resolved);
    Ok(resolved)
}

/// Returns the actual media path for an item, resolving STRM files if needed.
/// Returns None if the STRM target doesn't exist or can't be read.
pub fn resolve_media_path(item_path: &Path) -> Option<PathBuf> {
    if is_strm_path(item_path) {
        resolve_strm_path(item_path).ok().filter(|p| p.exists())
    } else if item_path.exists() {
        Some(item_path.to_path_buf())
    } else {
        None
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    // Simple normalization: resolve canonical if possible, otherwise return as-is
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
