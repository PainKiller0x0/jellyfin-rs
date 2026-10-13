use std::collections::HashMap;

use sea_orm::DatabaseConnection;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryRuntimeOptions {
    pub enabled: bool,
    pub enable_photos: bool,
    pub enable_realtime_monitor: bool,
}

impl Default for LibraryRuntimeOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_photos: true,
            // Existing jellyfin-rs libraries were always monitored. Keep that
            // behavior until a library explicitly disables realtime monitoring.
            enable_realtime_monitor: true,
        }
    }
}

impl LibraryRuntimeOptions {
    fn from_json(value: &Value) -> Self {
        let defaults = Self::default();
        Self {
            enabled: bool_field(value, "Enabled", "enabled").unwrap_or(defaults.enabled),
            enable_photos: bool_field(value, "EnablePhotos", "enablePhotos")
                .unwrap_or(defaults.enable_photos),
            enable_realtime_monitor: bool_field(
                value,
                "EnableRealtimeMonitor",
                "enableRealtimeMonitor",
            )
            .unwrap_or(defaults.enable_realtime_monitor),
        }
    }
}

pub async fn load_runtime_options(
    db: &DatabaseConnection,
) -> anyhow::Result<HashMap<String, LibraryRuntimeOptions>> {
    Ok(crate::db::settings::find_by_prefix(db, "LibraryOptions.")
        .await?
        .into_iter()
        .filter_map(|setting| {
            let library_id = setting.key.strip_prefix("LibraryOptions.")?.to_string();
            let value = serde_json::from_str::<Value>(&setting.value).ok()?;
            value
                .is_object()
                .then(|| (library_id, LibraryRuntimeOptions::from_json(&value)))
        })
        .collect())
}

pub async fn raw_options(db: &DatabaseConnection) -> anyhow::Result<HashMap<String, Value>> {
    Ok(crate::db::settings::find_by_prefix(db, "LibraryOptions.")
        .await?
        .into_iter()
        .filter_map(|setting| {
            let library_id = setting.key.strip_prefix("LibraryOptions.")?.to_string();
            let value = serde_json::from_str::<Value>(&setting.value).ok()?;
            value.is_object().then_some((library_id, value))
        })
        .collect())
}

pub fn default_options_json() -> Value {
    serde_json::json!({
        "Enabled": true,
        "EnablePhotos": true,
        "EnableRealtimeMonitor": true,
        "EnableAutomaticSeriesGrouping": true,
        "PreferredMetadataLanguage": "zh-CN",
        "MetadataCountryCode": "CN",
        "SaveLocalMetadata": false,
        "AutomaticallyAddToCollection": false,
        "EnableChapterImageExtraction": false,
        "ExtractChapterImagesDuringLibraryScan": false,
        "EnableTrickplayImageExtraction": false,
        "ExtractTrickplayImagesDuringLibraryScan": false,
        "TypeOptions": []
    })
}

pub fn options_with_defaults(value: Option<Value>) -> Value {
    let mut defaults = default_options_json();
    if let (Some(defaults), Some(overrides)) = (
        defaults.as_object_mut(),
        value.and_then(|v| v.as_object().cloned()),
    ) {
        defaults.extend(overrides);
    }
    defaults
}

fn bool_field(value: &Value, primary: &str, alias: &str) -> Option<bool> {
    value
        .get(primary)
        .or_else(|| value.get(alias))
        .and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::{LibraryRuntimeOptions, options_with_defaults};

    #[test]
    fn runtime_options_keep_legacy_monitoring_default() {
        let options = LibraryRuntimeOptions::from_json(&serde_json::json!({}));
        assert!(options.enabled);
        assert!(options.enable_photos);
        assert!(options.enable_realtime_monitor);
    }

    #[test]
    fn explicit_runtime_options_are_respected() {
        let options = LibraryRuntimeOptions::from_json(&serde_json::json!({
            "Enabled": false,
            "EnablePhotos": false,
            "EnableRealtimeMonitor": false
        }));
        assert!(!options.enabled);
        assert!(!options.enable_photos);
        assert!(!options.enable_realtime_monitor);
    }

    #[test]
    fn stored_options_override_defaults_without_dropping_new_fields() {
        let options = options_with_defaults(Some(serde_json::json!({
            "EnableRealtimeMonitor": false
        })));
        assert_eq!(options["EnableRealtimeMonitor"], false);
        assert_eq!(options["Enabled"], true);
        assert_eq!(options["PreferredMetadataLanguage"], "zh-CN");
    }
}
