use anyhow::Context;
use sqlx::{AnyPool, Row};

use crate::{
    library::{metadata::ParsedMetadata, probe::MediaProbe},
    util::{now_unix, stable_text_id},
};

pub struct ScannedMediaItem {
    pub id: String,
    pub title: String,
    pub path: String,
    pub library_id: String,
    pub parent_id: String,
    pub item_type: String,
    pub is_folder: bool,
    pub container: Option<String>,
    pub overview: Option<String>,
    pub official_rating: Option<String>,
    pub extended_video_type: Option<String>,
    pub production_year: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub size_bytes: Option<i64>,
    pub modified_at: i64,
    pub created_at: i64,
}

impl ScannedMediaItem {
    pub fn folder_with_type(
        id: String,
        library_id: String,
        parent_id: String,
        path: String,
        title: String,
        item_type: &str,
        modified_at: i64,
    ) -> Self {
        Self {
            id,
            title,
            path,
            library_id,
            parent_id,
            item_type: item_type.to_string(),
            is_folder: true,
            container: None,
            overview: None,
            official_rating: None,
            extended_video_type: None,
            production_year: None,
            runtime_ticks: None,
            size_bytes: None,
            modified_at,
            created_at: now_unix(),
        }
    }
}

pub async fn upsert_media_item(db: &AnyPool, item: &ScannedMediaItem) -> anyhow::Result<()> {
    sqlx::query(r#"INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, overview, official_rating, extended_video_type, production_year, runtime_ticks, size_bytes, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET title = excluded.title, library_id = excluded.library_id, parent_id = excluded.parent_id, item_type = excluded.item_type, is_folder = excluded.is_folder, container = excluded.container, overview = excluded.overview, official_rating = excluded.official_rating, extended_video_type = excluded.extended_video_type, production_year = excluded.production_year, runtime_ticks = excluded.runtime_ticks, size_bytes = excluded.size_bytes, modified_at = excluded.modified_at, updated_at = excluded.updated_at"#)
        .bind(&item.id).bind(&item.title).bind(&item.path).bind(&item.library_id).bind(&item.parent_id).bind(&item.item_type).bind(if item.is_folder { 1 } else { 0 }).bind(&item.container).bind(&item.overview).bind(&item.official_rating).bind(&item.extended_video_type).bind(item.production_year).bind(item.runtime_ticks).bind(item.size_bytes).bind(item.modified_at).bind(item.created_at).bind(now_unix())
        .execute(db).await.with_context(|| format!("failed to upsert media item: {}", item.path))?;
    Ok(())
}

pub async fn upsert_media_metadata(
    db: &AnyPool,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    for (provider, provider_item_id) in &metadata.provider_ids {
        sqlx::query(r#"INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id"#)
            .bind(item_id)
            .bind(provider)
            .bind(provider_item_id)
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert provider id for item: {item_id}"))?;
    }

    upsert_named_relations(
        db,
        item_id,
        "genres",
        "media_genres",
        "genre_id",
        &metadata.genres,
    )
    .await?;
    upsert_named_relations(db, item_id, "tags", "media_tags", "tag_id", &metadata.tags).await?;
    upsert_named_relations(
        db,
        item_id,
        "studios",
        "media_studios",
        "studio_id",
        &metadata.studios,
    )
    .await?;
    upsert_people(db, item_id, metadata).await?;
    Ok(())
}

async fn upsert_named_relations(
    db: &AnyPool,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    values: &[String],
) -> anyhow::Result<()> {
    if values.is_empty() {
        return Ok(());
    }

    sqlx::query(&format!("DELETE FROM {relation_table} WHERE item_id = ?"))
        .bind(item_id)
        .execute(db)
        .await
        .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;

    for value in values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let id = stable_text_id(&format!("{table}:{}", value.to_ascii_lowercase()));
        sqlx::query(&format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"))
            .bind(&id)
            .bind(value)
            .bind(now_unix())
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert {table}: {value}"))?;
        sqlx::query(&format!("INSERT INTO {relation_table} (item_id, {relation_column}) VALUES (?, ?) ON CONFLICT(item_id, {relation_column}) DO NOTHING"))
            .bind(item_id)
            .bind(id)
            .execute(db)
            .await
            .with_context(|| format!("failed to link {table} to item: {item_id}"))?;
    }
    Ok(())
}

async fn upsert_people(
    db: &AnyPool,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    if metadata.people.is_empty() {
        return Ok(());
    }

    sqlx::query("DELETE FROM media_people WHERE item_id = ?")
        .bind(item_id)
        .execute(db)
        .await
        .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    for (sort_order, person) in metadata.people.iter().enumerate() {
        let id = stable_text_id(&format!(
            "people:{}",
            person.name.trim().to_ascii_lowercase()
        ));
        sqlx::query("INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING")
            .bind(&id)
            .bind(person.name.trim())
            .bind(now_unix())
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert person: {}", person.name))?;
        sqlx::query("INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = excluded.role, sort_order = excluded.sort_order")
            .bind(item_id)
            .bind(id)
            .bind(&person.role)
            .bind(&person.person_type)
            .bind(i64::try_from(sort_order).unwrap_or(i64::MAX))
            .execute(db)
            .await
            .with_context(|| format!("failed to link person to item: {item_id}"))?;
    }
    Ok(())
}

pub async fn upsert_default_media_stream(
    db: &AnyPool,
    item: &ScannedMediaItem,
) -> anyhow::Result<()> {
    if item.is_folder {
        return Ok(());
    }
    let stream_type = if item.item_type == "Audio" {
        "Audio"
    } else {
        "Video"
    };
    sqlx::query(r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, created_at) VALUES (?, ?, 0, ?, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type"#)
        .bind(stable_text_id(&format!("stream:{}:0", item.id))).bind(&item.id).bind(stream_type).bind(now_unix()).execute(db).await
        .with_context(|| format!("failed to upsert default media stream: {}", item.path))?;
    Ok(())
}

pub async fn upsert_probed_media_streams(
    db: &AnyPool,
    item: &ScannedMediaItem,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    if item.is_folder || probe.streams.is_empty() {
        return Ok(false);
    }

    sqlx::query("DELETE FROM media_streams WHERE item_id = ? AND is_external = 0")
        .bind(&item.id)
        .execute(db)
        .await
        .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        sqlx::query(r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, bit_rate, width, height, channels, sample_rate, is_external, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type, codec = excluded.codec, language = excluded.language, title = excluded.title, bit_rate = excluded.bit_rate, width = excluded.width, height = excluded.height, channels = excluded.channels, sample_rate = excluded.sample_rate, is_external = 0"#)
            .bind(stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index)))
            .bind(&item.id)
            .bind(stream.stream_index)
            .bind(&stream.stream_type)
            .bind(&stream.codec)
            .bind(&stream.language)
            .bind(&stream.title)
            .bind(stream.bit_rate)
            .bind(stream.width)
            .bind(stream.height)
            .bind(stream.channels)
            .bind(stream.sample_rate)
            .bind(now_unix())
            .execute(db)
            .await
            .with_context(|| format!("failed to upsert probed stream for item: {}", item.id))?;
    }

    Ok(true)
}

pub async fn remove_missing_media_items(db: &AnyPool, seen_paths: &[String]) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, path FROM media_items")
        .fetch_all(db)
        .await
        .context("failed to list media items for cleanup")?;
    for row in rows {
        let id: String = row.try_get("id")?;
        let path: String = row.try_get("path")?;
        if !seen_paths.iter().any(|seen| seen == &path) && !std::path::Path::new(&path).exists() {
            sqlx::query("DELETE FROM media_items WHERE id = ?")
                .bind(id)
                .execute(db)
                .await
                .with_context(|| format!("failed to delete missing media item: {path}"))?;
        }
    }
    Ok(())
}
