use std::{fs, path::Path, time::UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::util::{media_title, stable_item_id, system_time_to_unix};

#[derive(Debug, Clone)]
pub struct ResolvedPathInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size_bytes: Option<i64>,
    pub modified_at: i64,
}

pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let separator = if trimmed.contains('/') {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    };
    let other = if separator == '/' { '\\' } else { '/' };
    let mut normalized = trimmed.replace(other, &separator.to_string());
    while normalized.len() > 1 && normalized.ends_with(separator) {
        normalized.pop();
    }
    normalized
}

pub fn canonicalize_path(path: &str) -> anyhow::Result<String> {
    let normalized = normalize_path(path);
    if normalized.is_empty() {
        bail!("path is required");
    }
    match fs::canonicalize(&normalized) {
        Ok(canonical) => Ok(strip_windows_extended_prefix(
            &canonical.to_string_lossy(),
        )),
        Err(_) => {
            // canonicalize fails on virtual filesystems (e.g. CloudDrive).
            // Fall back to the normalized path if the directory exists.
            let p = Path::new(&normalized);
            if !p.try_exists().unwrap_or(false) || !p.is_dir() {
                bail!("path does not exist: {normalized}");
            }
            Ok(normalized)
        }
    }
}

pub fn validate_path(
    path: &str,
    is_file: Option<bool>,
    validate_writable: bool,
) -> anyhow::Result<()> {
    let path = Path::new(path.trim());
    if path.as_os_str().is_empty() {
        bail!("path is required");
    }
    match is_file {
        Some(true) if !path.is_file() => bail!("file not found"),
        Some(false) if !path.is_dir() => bail!("directory not found"),
        None if !path.exists() => bail!("path not found"),
        _ => {}
    }
    if validate_writable {
        validate_path_writable(path)?;
    }
    Ok(())
}

pub fn validate_library_path(path: &str) -> anyhow::Result<String> {
    let canonical = canonicalize_path(path)?;
    validate_path(&canonical, Some(false), false)?;
    Ok(canonical)
}

pub fn parent_path(path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let path = Path::new(&normalized);
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_string_lossy().to_string())
}

pub fn resolve_path_info(path: &Path) -> anyhow::Result<ResolvedPathInfo> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to read path: {}", path.display()))?;
    let is_directory = metadata.is_dir();
    Ok(ResolvedPathInfo {
        id: stable_item_id(path),
        name: media_title(path),
        path: path.to_string_lossy().to_string(),
        is_directory,
        size_bytes: (!is_directory).then(|| i64::try_from(metadata.len()).unwrap_or(i64::MAX)),
        modified_at: system_time_to_unix(metadata.modified().unwrap_or(UNIX_EPOCH)),
    })
}

pub fn directory_entries(
    path: &str,
    include_files: bool,
    include_directories: bool,
) -> anyhow::Result<Vec<Value>> {
    let path = canonicalize_path(path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&path).with_context(|| format!("failed to read directory: {path}"))? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let is_directory = metadata.is_dir();
        if (is_directory && !include_directories) || (!is_directory && !include_files) {
            continue;
        }
        entries.push(json!({
            "Name": entry.file_name().to_string_lossy(),
            "Path": entry.path().to_string_lossy(),
            "Type": if is_directory { "Directory" } else { "File" },
        }));
    }
    entries.sort_by(|a, b| {
        a.get("Path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(
                &b.get("Path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            )
    });
    Ok(entries)
}

pub fn drive_entries() -> Vec<Value> {
    #[cfg(windows)]
    {
        ('A'..='Z')
            .filter_map(|letter| {
                let path = format!("{letter}:\\");
                Path::new(&path).exists().then(|| {
                    json!({
                        "Name": path,
                        "Path": path,
                        "Type": "Directory",
                    })
                })
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![json!({ "Name": "/", "Path": "/", "Type": "Directory" })]
    }
}

fn validate_path_writable(path: &Path) -> anyhow::Result<()> {
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or_else(|| Path::new("."))
    };
    let probe = directory.join(format!(".jellyfin-rs-write-test-{}", uuid::Uuid::new_v4()));
    fs::write(&probe, b"")
        .with_context(|| format!("path is not writable: {}", directory.display()))?;
    let _ = fs::remove_file(probe);
    Ok(())
}

fn strip_windows_extended_prefix(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .unwrap_or(path)
        .strip_prefix("UNC\\")
        .map(|path| format!(r"\\{path}"))
        .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators() {
        assert_eq!(normalize_path("a\\b/c"), "a/b/c");
    }
}
