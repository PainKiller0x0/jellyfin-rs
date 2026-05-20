use std::path::Path;

use anyhow::Context;
use sea_orm::{ConnectionTrait, Value};

use crate::jellyfin::providers;

/// Extract TMDb ID from `{tmdb-XXXXX}` or `[tmdbid=XXXXX]` in the path
pub fn extract_tmdb_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let Some(tag) = name.split(open).nth(1)?.split(close).next() {
            let tag = tag.to_ascii_lowercase();
            if let Some(id) = tag.strip_prefix("tmdb-") {
                return Some(id.to_string());
            }
            if let Some(id) = tag.strip_prefix("tmdbid=") {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Clean title by removing `{tmdb-XXXXX}` and similar provider ID tags
pub fn clean_provider_tags(title: &str) -> String {
    let mut result = title.to_string();
    for (open, close) in [('{', '}'), ('[', ']'), ('(', ')')] {
        if let (Some(start), Some(end)) = (result.rfind(open), result.rfind(close)) {
            if start < end {
                let tag = &result[start + 1..end].to_ascii_lowercase();
                if tag.starts_with("tmdb-") || tag.starts_with("tmdbid=") {
                    result.replace_range(start..=end, "");
                }
            }
        }
    }
    result.trim().to_string()
}

/// Fetch TMDb metadata for a Series or Movie and store it in the database
pub async fn fetch_and_apply_tmdb_metadata(
    db: &sea_orm::DatabaseConnection,
    item_id: &str,
    item_type: &str,
    path: &Path,
    api_key: &str,
) -> anyhow::Result<()> {
    let Some(tmdb_id) = extract_tmdb_id(path) else {
        return Ok(());
    };

    let client = reqwest::Client::new();
    let metadata = if item_type == "Series" {
        providers::tmdb_tv_details(&client, api_key, &tmdb_id).await
    } else {
        // Try movie details for non-Series items
        providers::tmdb_movie_details(&client, api_key, &tmdb_id).await
    };

    let Ok(metadata) = metadata else {
        return Ok(());
    };

    let backend = db.get_database_backend();
    let overview = metadata
        .get("Overview")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let year = metadata
        .get("ProductionYear")
        .and_then(|v| v.as_i64());
    let genres: Vec<String> = metadata
        .get("Genres")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let studios: Vec<String> = metadata
        .get("Studios")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let people: Vec<(String, String, String)> = metadata
        .get("People")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some((
                        p.get("Name")?.as_str()?.to_string(),
                        p.get("Role")?.as_str()?.to_string(),
                        p.get("Type")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    // Update media item
    if let Some(overview) = overview {
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET overview = ? WHERE id = ?",
                vec![overview.into(), item_id.into()],
            ))
            .await;
    }
    if let Some(year) = year {
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET production_year = ? WHERE id = ?",
                vec![year.into(), item_id.into()],
            ))
            .await;
    }

    // Store provider IDs
    if let Some(provider_ids) = metadata.get("ProviderIds") {
        if let Some(obj) = provider_ids.as_object() {
            for (provider, id) in obj {
                if let Some(id_str) = id.as_str().filter(|s| !s.is_empty()) {
                    let _ = db
                        .execute(crate::db::helpers::portable_statement(
                            backend,
                            "INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id",
                            vec![item_id.into(), provider.as_str().into(), id_str.into()],
                        ))
                        .await;
                }
            }
        }
    }

    // Upsert genres
    let now = crate::util::now_unix();
    for genre_name in &genres {
        let genre_id = crate::util::stable_text_id(&format!("genre:{genre_name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO genres (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![genre_id.clone().into(), genre_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_genres (item_id, genre_id) VALUES (?, ?) ON CONFLICT(item_id, genre_id) DO NOTHING",
                vec![item_id.into(), genre_id.into()],
            ))
            .await;
    }

    // Upsert studios
    for studio_name in &studios {
        let studio_id = crate::util::stable_text_id(&format!("studio:{studio_name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO studios (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![studio_id.clone().into(), studio_name.as_str().into(), now.into()],
            ))
            .await;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_studios (item_id, studio_id) VALUES (?, ?) ON CONFLICT(item_id, studio_id) DO NOTHING",
                vec![item_id.into(), studio_id.into()],
            ))
            .await;
    }

    // Upsert people
    for (name, role, person_type) in &people {
        let person_id = crate::util::stable_text_id(&format!("person:{name}"));
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
                vec![person_id.clone().into(), name.as_str().into(), now.into()],
            ))
            .await;
        let sort_order = people.iter().position(|(n, _, _)| n == name).unwrap_or(0) as i64;
        let _ = db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO NOTHING",
                vec![
                    item_id.into(),
                    person_id.into(),
                    role.as_str().into(),
                    Value::from(person_type.as_str()),
                    sort_order.into(),
                ],
            ))
            .await;
    }

    Ok(())
}
