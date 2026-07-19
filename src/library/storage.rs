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
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub modified_at: i64,
    pub created_at: i64,
}

pub struct CachedMediaProbe {
    pub runtime_ticks: Option<i64>,
}

impl ScannedMediaItem {
    #[allow(clippy::too_many_arguments)]
    pub fn folder_with_type(
        id: String,
        library_id: String,
        parent_id: String,
        path: String,
        title: String,
        item_type: &str,
        modified_at: i64,
        production_year: Option<i64>,
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
            production_year,
            runtime_ticks: None,
            size_bytes: None,
            season_number: None,
            episode_number: None,
            modified_at,
            created_at: now_unix(),
        }
    }
}

pub async fn cached_media_probe_if_current(
    db: &DatabaseConnection,
    path: &str,
    modified_at: i64,
    size_bytes: Option<i64>,
) -> anyhow::Result<Option<CachedMediaProbe>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT mi.runtime_ticks
               FROM media_items mi
               WHERE mi.path = ?
                 AND mi.is_folder = 0
                 AND mi.modified_at = ?
                 AND COALESCE(mi.size_bytes, -1) = COALESCE(?, -1)
                 AND EXISTS (
                     SELECT 1
                     FROM media_streams ms
                     WHERE ms.item_id = mi.id
                       AND ms.is_external = 0
                       AND (
                           ms.profile IS NOT NULL
                           OR ms.pixel_format IS NOT NULL
                           OR ms.average_frame_rate IS NOT NULL
                           OR ms.aspect_ratio IS NOT NULL
                           OR ms.channel_layout IS NOT NULL
                           OR ms.color_transfer IS NOT NULL
                           OR ms.bit_depth IS NOT NULL
                           OR ms.is_default <> 0
                       )
                 )
               LIMIT 1"#,
            vec![path.into(), modified_at.into(), size_bytes.into()],
        ))
        .await
        .with_context(|| format!("failed to check cached media probe: {path}"))?;
    Ok(row.map(|row| CachedMediaProbe {
        runtime_ticks: row.get_opt_i64("runtime_ticks").ok().flatten(),
    }))
}

pub async fn upsert_media_item(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<String> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
        r#"INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, overview, official_rating, extended_video_type, production_year, runtime_ticks, size_bytes, season_number, episode_number, modified_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET title = CASE WHEN media_items.item_type = excluded.item_type AND (media_items.overview IS NOT NULL OR media_items.premiere_date IS NOT NULL) THEN media_items.title ELSE excluded.title END, library_id = excluded.library_id, parent_id = excluded.parent_id, item_type = excluded.item_type, is_folder = excluded.is_folder, container = excluded.container, overview = COALESCE(excluded.overview, media_items.overview), official_rating = COALESCE(excluded.official_rating, media_items.official_rating), extended_video_type = excluded.extended_video_type, production_year = CASE WHEN media_items.item_type = excluded.item_type AND media_items.premiere_date IS NOT NULL THEN media_items.production_year ELSE COALESCE(excluded.production_year, media_items.production_year) END, runtime_ticks = COALESCE(excluded.runtime_ticks, media_items.runtime_ticks), size_bytes = excluded.size_bytes, season_number = excluded.season_number, episode_number = excluded.episode_number, modified_at = excluded.modified_at, updated_at = excluded.updated_at RETURNING id"#,
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
            item.season_number.into(),
            item.episode_number.into(),
            item.modified_at.into(),
            item.created_at.into(),
            now_unix().into(),
        ],
    ))
        .await
        .with_context(|| format!("failed to upsert media item: {}", item.path))?
        .context("media item upsert returned no row")?;
    row.get_str("id")
        .with_context(|| format!("failed to read stored media item id: {}", item.path))
}

pub async fn upsert_media_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    for (provider, provider_item_id) in &metadata.provider_ids {
        db.execute(crate::db::helpers::pg_statement(
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
    db.execute(crate::db::helpers::pg_statement(
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
        db.execute(crate::db::helpers::pg_statement(
            &format!("INSERT INTO {table} (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING"),
            vec![id.clone().into(), value.into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert {table}: {value}"))?;
        db.execute(crate::db::helpers::pg_statement(
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
    db.execute(crate::db::helpers::pg_statement(
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
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
            vec![id.clone().into(), person.name.trim().into(), now_unix().into()],
        ))
        .await
        .with_context(|| format!("failed to upsert person: {}", person.name))?;
        db.execute(crate::db::helpers::pg_statement(
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
    let stream_id = stable_text_id(&format!("stream:{}:0", item.id));
    delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, created_at) VALUES (?, ?, 0, ?, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type"#,
        vec![
            stream_id.into(),
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
    db.execute(crate::db::helpers::pg_statement(
        "DELETE FROM media_streams WHERE item_id = ? AND is_external = 0",
        vec![item.id.as_str().into()],
    ))
    .await
    .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
        delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
        db.execute(crate::db::helpers::pg_statement(
            r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, codec, profile, codec_tag, language, title, comment, bit_rate, width, height, aspect_ratio, average_frame_rate, real_frame_rate, reference_frame_rate, channels, channel_layout, sample_rate, bit_depth, ref_frames, is_interlaced, is_avc, is_anamorphic, pixel_format, level, color_range, color_space, color_transfer, color_primaries, time_base, codec_time_base, nal_length_size, rotation, video_range, video_range_type, hdr10_plus_present_flag, is_default, is_forced, is_hearing_impaired, is_original, is_external, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?) ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type, codec = excluded.codec, profile = excluded.profile, codec_tag = excluded.codec_tag, language = excluded.language, title = excluded.title, comment = excluded.comment, bit_rate = excluded.bit_rate, width = excluded.width, height = excluded.height, aspect_ratio = excluded.aspect_ratio, average_frame_rate = excluded.average_frame_rate, real_frame_rate = excluded.real_frame_rate, reference_frame_rate = excluded.reference_frame_rate, channels = excluded.channels, channel_layout = excluded.channel_layout, sample_rate = excluded.sample_rate, bit_depth = excluded.bit_depth, ref_frames = excluded.ref_frames, is_interlaced = excluded.is_interlaced, is_avc = excluded.is_avc, is_anamorphic = excluded.is_anamorphic, pixel_format = excluded.pixel_format, level = excluded.level, color_range = excluded.color_range, color_space = excluded.color_space, color_transfer = excluded.color_transfer, color_primaries = excluded.color_primaries, time_base = excluded.time_base, codec_time_base = excluded.codec_time_base, nal_length_size = excluded.nal_length_size, rotation = excluded.rotation, video_range = excluded.video_range, video_range_type = excluded.video_range_type, hdr10_plus_present_flag = excluded.hdr10_plus_present_flag, is_default = excluded.is_default, is_forced = excluded.is_forced, is_hearing_impaired = excluded.is_hearing_impaired, is_original = excluded.is_original, is_external = 0"#,
            vec![
                stream_id.into(),
                item.id.as_str().into(),
                stream.stream_index.into(),
                stream.stream_type.as_str().into(),
                stream.codec.as_deref().into(),
                stream.profile.as_deref().into(),
                stream.codec_tag.as_deref().into(),
                stream.language.as_deref().into(),
                stream.title.as_deref().into(),
                stream.comment.as_deref().into(),
                stream.bit_rate.into(),
                stream.width.into(),
                stream.height.into(),
                stream.aspect_ratio.as_deref().into(),
                stream.average_frame_rate.into(),
                stream.real_frame_rate.into(),
                stream.reference_frame_rate.into(),
                stream.channels.into(),
                stream.channel_layout.as_deref().into(),
                stream.sample_rate.into(),
                stream.bit_depth.into(),
                stream.ref_frames.into(),
                (stream.is_interlaced as i64).into(),
                stream.is_avc.map(|value| value as i64).into(),
                stream.is_anamorphic.map(|value| value as i64).into(),
                stream.pixel_format.as_deref().into(),
                stream.level.into(),
                stream.color_range.as_deref().into(),
                stream.color_space.as_deref().into(),
                stream.color_transfer.as_deref().into(),
                stream.color_primaries.as_deref().into(),
                stream.time_base.as_deref().into(),
                stream.codec_time_base.as_deref().into(),
                stream.nal_length_size.as_deref().into(),
                stream.rotation.into(),
                stream.video_range.as_deref().into(),
                stream.video_range_type.as_deref().into(),
                stream.hdr10_plus_present_flag.map(|value| value as i64).into(),
                (stream.is_default as i64).into(),
                (stream.is_forced as i64).into(),
                (stream.is_hearing_impaired as i64).into(),
                stream.is_original.map(|value| value as i64).into(),
                now_unix().into(),
            ],
        ))
        .await
        .with_context(|| format!("failed to upsert probed stream for item: {}", item.id))?;
    }

    Ok(true)
}

async fn delete_stale_generated_stream_id(
    db: &DatabaseConnection,
    stream_id: &str,
    item_id: &str,
) -> anyhow::Result<()> {
    db.execute(crate::db::helpers::pg_statement(
        "DELETE FROM media_streams WHERE id = ? AND item_id <> ? AND is_external = 0",
        vec![stream_id.into(), item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear stale generated stream id: {stream_id}"))?;
    Ok(())
}

pub async fn remove_missing_media_items(
    db: &DatabaseConnection,
    seen_paths: &[String],
) -> anyhow::Result<()> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT id, path FROM media_items",
            vec![],
        ))
        .await
        .context("failed to list media items for cleanup")?;
    for row in &rows {
        let id: String = row.get_str("id")?;
        let path: String = row.get_str("path")?;
        if !seen_paths.iter().any(|seen| seen == &path) && !std::path::Path::new(&path).exists() {
            db.execute(crate::db::helpers::pg_statement(
                "DELETE FROM media_items WHERE id = ?",
                vec![id.into()],
            ))
            .await
            .with_context(|| format!("failed to delete missing media item: {path}"))?;
        }
    }
    Ok(())
}
