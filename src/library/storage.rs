use std::collections::HashSet;

use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::{
    db::row_ext::QueryResultExt,
    library::{
        metadata::ParsedMetadata,
        probe::{MediaProbe, ProbedStream},
    },
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
    pub size_bytes: Option<i64>,
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
    allow_size_mismatch: bool,
) -> anyhow::Result<Option<CachedMediaProbe>> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT mi.runtime_ticks, mi.size_bytes
               FROM media_items mi
               WHERE mi.path = ?
                 AND mi.is_folder = 0
                 AND mi.modified_at = ?
                 AND (? = 1 OR COALESCE(mi.size_bytes, -1) = COALESCE(?, -1))
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
            vec![
                path.into(),
                modified_at.into(),
                (if allow_size_mismatch { 1i64 } else { 0i64 }).into(),
                size_bytes.into(),
            ],
        ))
        .await
        .with_context(|| format!("failed to check cached media probe: {path}"))?;
    Ok(row.map(|row| CachedMediaProbe {
        runtime_ticks: row.get_opt_i64("runtime_ticks").ok().flatten(),
        size_bytes: row.get_opt_i64("size_bytes").ok().flatten(),
    }))
}

pub async fn upsert_media_item(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<String> {
    if let Some(id) = unchanged_media_item_id(db, item).await? {
        return Ok(id);
    }

    let row = db
        .query_one(crate::db::helpers::pg_statement(
        r#"WITH upserted AS (
               INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, container, overview, official_rating, extended_video_type, production_year, runtime_ticks, size_bytes, season_number, episode_number, modified_at, created_at, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(path) DO UPDATE SET
                   title = CASE WHEN media_items.item_type = excluded.item_type AND media_items.is_folder = 1 AND (media_items.overview IS NOT NULL OR media_items.premiere_date IS NOT NULL) THEN media_items.title ELSE excluded.title END,
                   library_id = excluded.library_id,
                   parent_id = excluded.parent_id,
                   item_type = excluded.item_type,
                   is_folder = excluded.is_folder,
                   container = excluded.container,
                   overview = COALESCE(excluded.overview, media_items.overview),
                   official_rating = COALESCE(excluded.official_rating, media_items.official_rating),
                   extended_video_type = excluded.extended_video_type,
                   production_year = CASE WHEN media_items.item_type = excluded.item_type AND media_items.premiere_date IS NOT NULL THEN media_items.production_year ELSE COALESCE(excluded.production_year, media_items.production_year) END,
                   runtime_ticks = COALESCE(excluded.runtime_ticks, media_items.runtime_ticks),
                   size_bytes = excluded.size_bytes,
                   season_number = excluded.season_number,
                   episode_number = excluded.episode_number,
                   modified_at = excluded.modified_at,
                   updated_at = excluded.updated_at
               WHERE media_items.title IS DISTINCT FROM CASE WHEN media_items.item_type = excluded.item_type AND media_items.is_folder = 1 AND (media_items.overview IS NOT NULL OR media_items.premiere_date IS NOT NULL) THEN media_items.title ELSE excluded.title END
                  OR media_items.library_id IS DISTINCT FROM excluded.library_id
                  OR media_items.parent_id IS DISTINCT FROM excluded.parent_id
                  OR media_items.item_type IS DISTINCT FROM excluded.item_type
                  OR media_items.is_folder IS DISTINCT FROM excluded.is_folder
                  OR media_items.container IS DISTINCT FROM excluded.container
                  OR media_items.overview IS DISTINCT FROM COALESCE(excluded.overview, media_items.overview)
                  OR media_items.official_rating IS DISTINCT FROM COALESCE(excluded.official_rating, media_items.official_rating)
                  OR media_items.extended_video_type IS DISTINCT FROM excluded.extended_video_type
                  OR media_items.production_year IS DISTINCT FROM CASE WHEN media_items.item_type = excluded.item_type AND media_items.premiere_date IS NOT NULL THEN media_items.production_year ELSE COALESCE(excluded.production_year, media_items.production_year) END
                  OR media_items.runtime_ticks IS DISTINCT FROM COALESCE(excluded.runtime_ticks, media_items.runtime_ticks)
                  OR media_items.size_bytes IS DISTINCT FROM excluded.size_bytes
                  OR media_items.season_number IS DISTINCT FROM excluded.season_number
                  OR media_items.episode_number IS DISTINCT FROM excluded.episode_number
                  OR media_items.modified_at IS DISTINCT FROM excluded.modified_at
               RETURNING id
           )
           SELECT id FROM upserted
           UNION ALL
           SELECT id FROM media_items WHERE path = ? AND NOT EXISTS (SELECT 1 FROM upserted)
           LIMIT 1"#,
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
            item.path.as_str().into(),
        ],
    ))
        .await
        .with_context(|| format!("failed to upsert media item: {}", item.path))?
        .context("media item upsert returned no row")?;
    row.get_str("id")
        .with_context(|| format!("failed to read stored media item id: {}", item.path))
}

async fn unchanged_media_item_id(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<Option<String>> {
    let Some(row) = db
        .query_one(crate::db::helpers::pg_statement(
            r#"SELECT id, title, library_id, parent_id, item_type, is_folder, container, overview,
                      official_rating, extended_video_type, production_year, premiere_date,
                      runtime_ticks, size_bytes, season_number, episode_number, modified_at
               FROM media_items
               WHERE path = ?"#,
            vec![item.path.as_str().into()],
        ))
        .await
        .with_context(|| format!("failed to read existing media item: {}", item.path))?
    else {
        return Ok(None);
    };

    let id = row.get_str("id")?;
    if media_item_scan_matches(&row, item)? {
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

fn media_item_scan_matches(
    row: &sea_orm::QueryResult,
    item: &ScannedMediaItem,
) -> anyhow::Result<bool> {
    let existing_item_type = row.get_str("item_type")?;
    let existing_is_folder = row.get_i64("is_folder")? != 0;
    let existing_overview = row.get_opt_str("overview")?;
    let existing_premiere_date = row.get_opt_str("premiere_date")?;
    let preserve_folder_title = existing_item_type == item.item_type
        && existing_is_folder
        && (existing_overview.is_some() || existing_premiere_date.is_some());

    let title_matches = preserve_folder_title || row.get_str("title")? == item.title;
    let overview_matches = option_str_eq(
        existing_overview.as_deref(),
        item.overview.as_deref().or(existing_overview.as_deref()),
    );
    let official_rating = row.get_opt_str("official_rating")?;
    let official_rating_matches = option_str_eq(
        official_rating.as_deref(),
        item.official_rating
            .as_deref()
            .or(official_rating.as_deref()),
    );
    let existing_production_year = row.get_opt_i64("production_year")?;
    let preserve_production_year =
        existing_item_type == item.item_type && existing_premiere_date.is_some();
    let target_production_year = if preserve_production_year {
        existing_production_year
    } else {
        item.production_year.or(existing_production_year)
    };
    let existing_runtime_ticks = row.get_opt_i64("runtime_ticks")?;
    let target_runtime_ticks = item.runtime_ticks.or(existing_runtime_ticks);

    Ok(title_matches
        && row.get_str("library_id")? == item.library_id
        && row.get_str("parent_id")? == item.parent_id
        && existing_item_type == item.item_type
        && existing_is_folder == item.is_folder
        && option_str_eq(
            row.get_opt_str("container")?.as_deref(),
            item.container.as_deref(),
        )
        && overview_matches
        && official_rating_matches
        && option_str_eq(
            row.get_opt_str("extended_video_type")?.as_deref(),
            item.extended_video_type.as_deref(),
        )
        && existing_production_year == target_production_year
        && existing_runtime_ticks == target_runtime_ticks
        && row.get_opt_i64("size_bytes")? == item.size_bytes
        && row.get_opt_i64("season_number")? == item.season_number
        && row.get_opt_i64("episode_number")? == item.episode_number
        && row.get_i64("modified_at")? == item.modified_at)
}

fn option_str_eq(left: Option<&str>, right: Option<&str>) -> bool {
    left == right
}

pub async fn upsert_media_metadata(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    for (provider, provider_item_id) in &metadata.provider_ids {
        crate::db::provider_ids::upsert(db, item_id, provider, provider_item_id)
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
    let relation_values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            (
                stable_text_id(&format!("{table}:{}", value.to_ascii_lowercase())),
                value,
            )
        })
        .collect::<Vec<_>>();
    let desired_ids = relation_values
        .iter()
        .map(|(id, _value)| id.as_str())
        .collect::<HashSet<_>>();
    if relation_ids_match(db, item_id, relation_table, relation_column, &desired_ids).await? {
        return Ok(());
    }

    db.execute(crate::db::helpers::pg_statement(
        &format!("DELETE FROM {relation_table} WHERE item_id = ?"),
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;

    for (id, value) in relation_values {
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

async fn relation_ids_match(
    db: &DatabaseConnection,
    item_id: &str,
    relation_table: &str,
    relation_column: &str,
    desired_ids: &HashSet<&str>,
) -> anyhow::Result<bool> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            &format!(
                "SELECT {relation_column} AS relation_id FROM {relation_table} WHERE item_id = ?"
            ),
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to read {relation_table} for item: {item_id}"))?;
    if rows.len() != desired_ids.len() {
        return Ok(false);
    }
    for row in rows {
        let relation_id = row.get_str("relation_id")?;
        if !desired_ids.contains(relation_id.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn upsert_people(
    db: &DatabaseConnection,
    item_id: &str,
    metadata: &ParsedMetadata,
) -> anyhow::Result<()> {
    if metadata.people.is_empty() {
        return Ok(());
    }
    let people = metadata
        .people
        .iter()
        .enumerate()
        .map(|(sort_order, person)| DesiredPerson {
            relation: PersonRelation {
                person_id: stable_text_id(&format!(
                    "people:{}",
                    person.name.trim().to_ascii_lowercase()
                )),
                role: person.role.clone(),
                person_type: person.person_type.clone(),
                sort_order: i64::try_from(sort_order).unwrap_or(i64::MAX),
            },
            name: person.name.trim().to_string(),
        })
        .collect::<Vec<_>>();
    if people_match(db, item_id, &people).await? {
        return Ok(());
    }

    db.execute(crate::db::helpers::pg_statement(
        "DELETE FROM media_people WHERE item_id = ?",
        vec![item_id.into()],
    ))
    .await
    .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    for person in people {
        db.execute(crate::db::helpers::pg_statement(
            "INSERT INTO people (id, name, created_at) VALUES (?, ?, ?) ON CONFLICT(name) DO NOTHING",
            vec![
                person.relation.person_id.clone().into(),
                person.name.as_str().into(),
                now_unix().into(),
            ],
        ))
        .await
        .with_context(|| format!("failed to upsert person: {}", person.name))?;
        db.execute(crate::db::helpers::pg_statement(
            r#"INSERT INTO media_people (item_id, person_id, role, person_type, sort_order)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(item_id, person_id, person_type) DO UPDATE SET
                   role = excluded.role,
                   sort_order = excluded.sort_order
               WHERE media_people.role IS DISTINCT FROM excluded.role
                  OR media_people.sort_order IS DISTINCT FROM excluded.sort_order"#,
            vec![
                item_id.into(),
                person.relation.person_id.as_str().into(),
                person.relation.role.as_deref().into(),
                person.relation.person_type.as_str().into(),
                person.relation.sort_order.into(),
            ],
        ))
        .await
        .with_context(|| format!("failed to link person to item: {item_id}"))?;
    }
    Ok(())
}

struct DesiredPerson {
    relation: PersonRelation,
    name: String,
}

#[derive(Debug, PartialEq, Eq)]
struct PersonRelation {
    person_id: String,
    role: Option<String>,
    person_type: String,
    sort_order: i64,
}

async fn people_match(
    db: &DatabaseConnection,
    item_id: &str,
    people: &[DesiredPerson],
) -> anyhow::Result<bool> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT person_id, role, person_type, sort_order
               FROM media_people
               WHERE item_id = ?
               ORDER BY sort_order, person_id, person_type"#,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to read people for item: {item_id}"))?;
    if rows.len() != people.len() {
        return Ok(false);
    }
    for (row, person) in rows.iter().zip(people.iter()) {
        let existing = PersonRelation {
            person_id: row.get_str("person_id")?,
            role: row.get_opt_str("role")?,
            person_type: row.get_str("person_type")?,
            sort_order: row.get_i64("sort_order")?,
        };
        if existing != person.relation {
            return Ok(false);
        }
    }
    Ok(true)
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
    if default_media_stream_matches(db, &item.id, stream_type).await? {
        return Ok(());
    }
    db.execute(crate::db::helpers::pg_statement(
        r#"INSERT INTO media_streams (id, item_id, stream_index, stream_type, created_at)
           VALUES (?, ?, 0, ?, ?)
           ON CONFLICT(item_id, stream_index) DO UPDATE SET stream_type = excluded.stream_type
           WHERE media_streams.stream_type IS DISTINCT FROM excluded.stream_type"#,
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

async fn default_media_stream_matches(
    db: &DatabaseConnection,
    item_id: &str,
    stream_type: &str,
) -> anyhow::Result<bool> {
    let row = db
        .query_one(crate::db::helpers::pg_statement(
            "SELECT stream_type, is_external FROM media_streams WHERE item_id = ? AND stream_index = 0",
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to read default media stream: {item_id}"))?;
    let Some(row) = row else {
        return Ok(false);
    };
    Ok(row.get_str("stream_type")? == stream_type && row.get_i64("is_external")? == 0)
}

pub async fn upsert_probed_media_streams(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    if item.is_folder || probe.streams.is_empty() {
        return Ok(false);
    }
    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
        delete_stale_generated_stream_id(db, &stream_id, &item.id).await?;
    }
    if probed_media_streams_match(db, &item.id, probe).await? {
        return Ok(true);
    }

    db.execute(crate::db::helpers::pg_statement(
        "DELETE FROM media_streams WHERE item_id = ? AND is_external = 0",
        vec![item.id.as_str().into()],
    ))
    .await
    .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
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

async fn probed_media_streams_match(
    db: &DatabaseConnection,
    item_id: &str,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    let rows = db
        .query_all(crate::db::helpers::pg_statement(
            r#"SELECT stream_index, stream_type, codec, profile, codec_tag, language, title,
                      comment, bit_rate, width, height, aspect_ratio, average_frame_rate,
                      real_frame_rate, reference_frame_rate, channels, channel_layout, sample_rate,
                      bit_depth, ref_frames, is_interlaced, is_avc, is_anamorphic, pixel_format,
                      level, color_range, color_space, color_transfer, color_primaries, time_base,
                      codec_time_base, nal_length_size, rotation, video_range, video_range_type,
                      hdr10_plus_present_flag, is_default, is_forced, is_hearing_impaired,
                      is_original
               FROM media_streams
               WHERE item_id = ? AND is_external = 0
               ORDER BY stream_index"#,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to read probed media streams: {item_id}"))?;
    if rows.len() != probe.streams.len() {
        return Ok(false);
    }
    for (row, stream) in rows.iter().zip(probe.streams.iter()) {
        if !probed_stream_matches(row, stream)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn probed_stream_matches(
    row: &sea_orm::QueryResult,
    stream: &ProbedStream,
) -> anyhow::Result<bool> {
    Ok(row.get_i64("stream_index")? == stream.stream_index
        && row.get_str("stream_type")? == stream.stream_type
        && option_str_eq(
            row.get_opt_str("codec")?.as_deref(),
            stream.codec.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("profile")?.as_deref(),
            stream.profile.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("codec_tag")?.as_deref(),
            stream.codec_tag.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("language")?.as_deref(),
            stream.language.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("title")?.as_deref(),
            stream.title.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("comment")?.as_deref(),
            stream.comment.as_deref(),
        )
        && row.get_opt_i64("bit_rate")? == stream.bit_rate
        && row.get_opt_i64("width")? == stream.width
        && row.get_opt_i64("height")? == stream.height
        && option_str_eq(
            row.get_opt_str("aspect_ratio")?.as_deref(),
            stream.aspect_ratio.as_deref(),
        )
        && option_f64_eq(
            row.get_f64("average_frame_rate")?,
            stream.average_frame_rate,
        )
        && option_f64_eq(row.get_f64("real_frame_rate")?, stream.real_frame_rate)
        && option_f64_eq(
            row.get_f64("reference_frame_rate")?,
            stream.reference_frame_rate,
        )
        && row.get_opt_i64("channels")? == stream.channels
        && option_str_eq(
            row.get_opt_str("channel_layout")?.as_deref(),
            stream.channel_layout.as_deref(),
        )
        && row.get_opt_i64("sample_rate")? == stream.sample_rate
        && row.get_opt_i64("bit_depth")? == stream.bit_depth
        && row.get_opt_i64("ref_frames")? == stream.ref_frames
        && i64_bool(row.get_i64("is_interlaced")?) == stream.is_interlaced
        && opt_i64_bool(row.get_opt_i64("is_avc")?) == stream.is_avc
        && opt_i64_bool(row.get_opt_i64("is_anamorphic")?) == stream.is_anamorphic
        && option_str_eq(
            row.get_opt_str("pixel_format")?.as_deref(),
            stream.pixel_format.as_deref(),
        )
        && row.get_opt_i64("level")? == stream.level
        && option_str_eq(
            row.get_opt_str("color_range")?.as_deref(),
            stream.color_range.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("color_space")?.as_deref(),
            stream.color_space.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("color_transfer")?.as_deref(),
            stream.color_transfer.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("color_primaries")?.as_deref(),
            stream.color_primaries.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("time_base")?.as_deref(),
            stream.time_base.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("codec_time_base")?.as_deref(),
            stream.codec_time_base.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("nal_length_size")?.as_deref(),
            stream.nal_length_size.as_deref(),
        )
        && row.get_opt_i64("rotation")? == stream.rotation
        && option_str_eq(
            row.get_opt_str("video_range")?.as_deref(),
            stream.video_range.as_deref(),
        )
        && option_str_eq(
            row.get_opt_str("video_range_type")?.as_deref(),
            stream.video_range_type.as_deref(),
        )
        && opt_i64_bool(row.get_opt_i64("hdr10_plus_present_flag")?)
            == stream.hdr10_plus_present_flag
        && i64_bool(row.get_i64("is_default")?) == stream.is_default
        && i64_bool(row.get_i64("is_forced")?) == stream.is_forced
        && i64_bool(row.get_i64("is_hearing_impaired")?) == stream.is_hearing_impaired
        && opt_i64_bool(row.get_opt_i64("is_original")?) == stream.is_original)
}

fn i64_bool(value: i64) -> bool {
    value != 0
}

fn opt_i64_bool(value: Option<i64>) -> Option<bool> {
    value.map(i64_bool)
}

fn option_f64_eq(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right || (left.is_nan() && right.is_nan()),
        (None, None) => true,
        _ => false,
    }
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
    let seen_paths = seen_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
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
        if !seen_paths.contains(path.as_str()) && !std::path::Path::new(&path).exists() {
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

#[cfg(test)]
mod tests {
    use super::{ScannedMediaItem, upsert_media_item, upsert_probed_media_streams};
    use crate::db::row_ext::QueryResultExt;
    use crate::library::probe::{MediaProbe, ProbedStream};
    use sea_orm::ConnectionTrait;

    #[tokio::test]
    async fn media_item_upsert_skips_unchanged_conflict_update() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("movie-1", "Movie");

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");

        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_items SET updated_at = 123 WHERE id = ?",
            vec![item.id.as_str().into()],
        ))
        .await
        .unwrap();

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.get_i64("updated_at").unwrap(), 123);

        item.title = "Renamed Movie".to_string();
        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.get_str("title").unwrap(), "Renamed Movie");
        assert_ne!(row.get_i64("updated_at").unwrap(), 123);
    }

    #[tokio::test]
    async fn probed_stream_upsert_skips_unchanged_rewrites() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-probe-1", "Probe Movie");
        upsert_media_item(&db, &item).await.unwrap();

        let probe = media_probe("h264");
        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        db.execute(crate::db::helpers::pg_statement(
            "UPDATE media_streams SET created_at = 123 WHERE item_id = ? AND stream_index = 0 AND is_external = 0",
            vec![item.id.as_str().into()],
        ))
        .await
        .unwrap();

        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.get_i64("created_at").unwrap(), 123);

        let changed_probe = media_probe("hevc");
        assert!(
            upsert_probed_media_streams(&db, &item, &changed_probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.get_opt_str("codec").unwrap().as_deref(), Some("hevc"));
    }

    fn scanned_movie(id: &str, title: &str) -> ScannedMediaItem {
        ScannedMediaItem {
            id: id.to_string(),
            title: title.to_string(),
            path: format!("/tmp/jellyfin-rs-storage-test/{id}.mkv"),
            library_id: "movies".to_string(),
            parent_id: "movies".to_string(),
            item_type: "Movie".to_string(),
            is_folder: false,
            container: Some("mkv".to_string()),
            overview: Some("Overview".to_string()),
            official_rating: Some("PG-13".to_string()),
            extended_video_type: None,
            production_year: Some(2024),
            runtime_ticks: Some(600_000_000),
            size_bytes: Some(1024),
            season_number: None,
            episode_number: None,
            modified_at: 42,
            created_at: 1,
        }
    }

    fn media_probe(codec: &str) -> MediaProbe {
        MediaProbe {
            runtime_ticks: Some(600_000_000),
            size_bytes: Some(1024),
            streams: vec![ProbedStream {
                stream_index: 0,
                stream_type: "Video".to_string(),
                codec: Some(codec.to_string()),
                profile: Some("Main".to_string()),
                codec_tag: None,
                language: Some("eng".to_string()),
                title: Some("Main video".to_string()),
                comment: None,
                bit_rate: Some(2_000_000),
                width: Some(1920),
                height: Some(1080),
                aspect_ratio: Some("16:9".to_string()),
                average_frame_rate: Some(24.0),
                real_frame_rate: Some(24.0),
                reference_frame_rate: None,
                channels: None,
                channel_layout: None,
                sample_rate: None,
                bit_depth: Some(8),
                ref_frames: Some(4),
                is_interlaced: false,
                is_avc: Some(true),
                is_anamorphic: Some(false),
                pixel_format: Some("yuv420p".to_string()),
                level: Some(40),
                color_range: None,
                color_space: Some("bt709".to_string()),
                color_transfer: Some("bt709".to_string()),
                color_primaries: Some("bt709".to_string()),
                time_base: Some("1/24000".to_string()),
                codec_time_base: None,
                nal_length_size: None,
                rotation: Some(0),
                video_range: Some("SDR".to_string()),
                video_range_type: None,
                hdr10_plus_present_flag: Some(false),
                is_default: true,
                is_forced: false,
                is_hearing_impaired: false,
                is_original: Some(true),
            }],
        }
    }

    async fn media_item_row(db: &sea_orm::DatabaseConnection, path: &str) -> sea_orm::QueryResult {
        db.query_one(crate::db::helpers::pg_statement(
            "SELECT title, updated_at FROM media_items WHERE path = ?",
            vec![path.into()],
        ))
        .await
        .unwrap()
        .unwrap()
    }

    async fn media_stream_row(
        db: &sea_orm::DatabaseConnection,
        item_id: &str,
    ) -> sea_orm::QueryResult {
        db.query_one(crate::db::helpers::pg_statement(
            "SELECT codec, created_at FROM media_streams WHERE item_id = ? AND stream_index = 0 AND is_external = 0",
            vec![item_id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
    }
}
