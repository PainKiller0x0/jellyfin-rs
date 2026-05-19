use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::{
    db::row_ext::QueryResultExt,
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

pub async fn upsert_media_item(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<()> {
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, overview, official_rating, extended_video_type, production_year, runtime_ticks, size_bytes, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET title = excluded.title, library_id = excluded.library_id, parent_id = excluded.parent_id, item_type = excluded.item_type, is_folder = excluded.is_folder, container = excluded.container, overview = excluded.overview, official_rating = excluded.official_rating, extended_video_type = excluded.extended_video_type, production_year = excluded.production_year, runtime_ticks = excluded.runtime_ticks, size_bytes = excluded.size_bytes, modified_at = excluded.modified_at, updated_at = excluded.updated_at"#,
        vec![
            item.id.as_str().into(),
            item.title.as_str().into(),
            item.path.as_str().into(),
            item.library_id.as_str().into(),
            item.parent_id.as_str().into(),
            item.item_type.as_str().into(),
            (if item.is_folder { 1i64 } else { 0i64 }).into(),
            item.container.as_deref().into(),
            item.overview.as_deref().into(),
            item.official_rating.as_deref().into(),
            item.extended_video_type.as_deref().into(),
            item.production_year.into(),
            item.runtime_ticks.into(),
            item.size_bytes.into(),
            item.modified_at.into(),
            item.created_at.into(),
            now_unix().into(),
        ],
    ))
    .await
    .with_context(|| format!("failed to upsert media item: {}", item.path))?;
    Ok(())
}

pub async fn upsert_media_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    let backend = db.get_database_backend();
    for (provider, provider_item_id) in &metadata.provider_ids {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO provider_ids (item_id, provider, provider_item_id) VALUES (?, ?, ?) ON CONFLICT(item_id, provider) DO UPDATE SET provider_item_id = excluded.provider_item_id"#,
            vec![item_id.into(), provider.as_str().into(), provider_item_id.as_str().into()],
        ))
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
    db: &DatabaseConnection,
    item_id: &str,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    values: &[String],
) -> anyhow::Result<()> {
    if values.is_empty() {
        return Ok(());
    }

    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        &format!("DELETE FROM {relation_table} WHERE item_id = ?"),
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;

    for value in values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let id = stable_text_id(&format!("{table}:{}", value.to_ascii_lowercase()));
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"),
            vec![id.clone().into(), value.into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert {table}: {value}"))?;
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("INSERT INTO {relation_table} (item_id, {relation_column}) VALUES (?, ?) ON CONFLICT(item_id, {relation_column}) DO NOTHING"),
            vec![item_id.into(), id.into()],
        ))
        .await
        .with_context(|| format!("failed to link {table} to item: {item_id}"))?;
    }
    Ok(())
}

async fn upsert_people(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    if metadata.people.is_empty() {
        return Ok(());
    }

    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_people WHERE item_id = ?",
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    for (sort_order, person) in metadata.people.iter().enumerate() {
        let id = stable_text_id(&format!(
            "people:{}",
            person.name.trim().to_ascii_lowercase()
        ));
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
            vec![id.clone().into(), person.name.trim().into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert person: {}", person.name))?;
        db.execute(crate::db::helpers::portable_statement(
            backend,
            "INSERT INTO media_people (item_id, person_id, role, person_type, sort_order) VALUES (?, ?, ?, ?, ?) ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET role = excluded.role, sort_order = excluded.sort_order",
            vec![item_id.into(), id.into(), person.role.as_deref().into(), person.person_type.as_str().into(), i64::try_from(sort_order).unwrap_or(i64::MAX).into()],
        ))
        .await
        .with_context(|| format!("failed to link person to item: {item_id}"))?;
    }
    Ok(())
}

pub async fn upsert_default_media_stream(
    db: &DatabaseConnection,
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
    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, created_at) VALUES (?, ?, 0, ?, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type"#,
        vec![
            stable_text_id(&format!("stream:{}:0", item.id)).into(),
            item.id.as_str().into(),
            stream_type.into(),
            now_unix().into(),
        ],
    ))
    .await
    .with_context(|| format!("failed to upsert default media stream: {}", item.path))?;
    Ok(())
}

pub async fn upsert_probed_media_streams(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    if item.is_folder || probe.streams.is_empty() {
        return Ok(false);
    }

    let backend = db.get_database_backend();
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM media_streams WHERE item_id = ? AND is_external = 0",
        vec![item.id.as_str().into()],
    ))
    .await
    .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        db.execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, language, title, bit_rate, width, height, channels, sample_rate, is_external, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type, codec = excluded.codec, language = excluded.language, title = excluded.title, bit_rate = excluded.bit_rate, width = excluded.width, height = excluded.height, channels = excluded.channels, sample_rate = excluded.sample_rate, is_external = 0"#,
            vec![
                stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index)).into(),
                item.id.as_str().into(),
                stream.stream_index.into(),
                stream.stream_type.as_str().into(),
                stream.codec.as_deref().into(),
                stream.language.as_deref().into(),
                stream.title.as_deref().into(),
                stream.bit_rate.into(),
                stream.width.into(),
                stream.height.into(),
                stream.channels.into(),
                stream.sample_rate.into(),
                now_unix().into(),
            ],
        ))
        .await
        .with_context(|| format!("failed to upsert probed stream for item: {}", item.id))?;
    }

    Ok(true)
}

pub async fn remove_missing_media_items(
    db: &DatabaseConnection,
    seen_paths: &[String],
) -> anyhow::Result<()> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id, path FROM media_items",
            vec![],
        ))
        .await
        .context("failed to list media items for cleanup")?;
    for row in &rows {
        let id: String = row.get_str("id")?;
        let path: String = row.get_str("path")?;
        if !seen_paths.iter().any(|seen| seen == &path) && !std::path::Path::new(&path).exists() {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                "DELETE FROM media_items WHERE id = ?",
                vec![id.into()],
            ))
            .await
            .with_context(|| format!("failed to delete missing media item: {path}"))?;
        }
    }
    Ok(())
}
