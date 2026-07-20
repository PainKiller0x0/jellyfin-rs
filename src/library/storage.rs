use std::{collections::HashSet, time::Duration};

use anyhow::{Context, bail};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, sea_query::OnConflict,
};

use crate::{
    entities::{
        genres::{self, Entity as Genres},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        media_streams::{self, Entity as MediaStreams},
        media_studios::{self, Entity as MediaStudios},
        media_tags::{self, Entity as MediaTags},
        people::{self, Entity as People},
        studios::{self, Entity as Studios},
        tags::{self, Entity as Tags},
    },
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

const MISSING_CLEANUP_BATCH_SIZE: u64 = 500;
const MISSING_CLEANUP_PAUSE: Duration = Duration::from_millis(2);

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
    let mut item_query = MediaItems::find()
        .filter(media_items::Column::Path.eq(path))
        .filter(media_items::Column::IsFolder.eq(0))
        .filter(media_items::Column::ModifiedAt.eq(modified_at));
    if !allow_size_mismatch {
        item_query = match size_bytes {
            Some(size_bytes) => item_query.filter(media_items::Column::SizeBytes.eq(size_bytes)),
            None => item_query.filter(media_items::Column::SizeBytes.is_null()),
        };
    }

    let Some(item) = item_query
        .one(db)
        .await
        .with_context(|| format!("failed to check cached media probe: {path}"))?
    else {
        return Ok(None);
    };

    let has_probe_stream = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::IsExternal.eq(0))
        .filter(
            Condition::any()
                .add(media_streams::Column::Codec.is_not_null())
                .add(media_streams::Column::Profile.is_not_null())
                .add(media_streams::Column::BitRate.is_not_null())
                .add(media_streams::Column::Width.is_not_null())
                .add(media_streams::Column::Height.is_not_null())
                .add(media_streams::Column::PixelFormat.is_not_null())
                .add(media_streams::Column::AverageFrameRate.is_not_null())
                .add(media_streams::Column::AspectRatio.is_not_null())
                .add(media_streams::Column::Channels.is_not_null())
                .add(media_streams::Column::SampleRate.is_not_null())
                .add(media_streams::Column::ChannelLayout.is_not_null())
                .add(media_streams::Column::ColorTransfer.is_not_null())
                .add(media_streams::Column::BitDepth.is_not_null()),
        )
        .one(db)
        .await
        .with_context(|| format!("failed to check cached media stream probe: {path}"))?
        .is_some();

    if !has_probe_stream {
        return Ok(None);
    }

    Ok(Some(CachedMediaProbe {
        runtime_ticks: item.runtime_ticks,
        size_bytes: item.size_bytes,
    }))
}

pub async fn upsert_media_item(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<String> {
    if let Some(id) = unchanged_media_item_id(db, item).await? {
        return Ok(id);
    }

    if let Some(existing) = MediaItems::find()
        .filter(media_items::Column::Path.eq(&item.path))
        .one(db)
        .await
        .with_context(|| format!("failed to read existing media item: {}", item.path))?
    {
        let id = existing.id.clone();
        let preserve_folder_title = existing.item_type == item.item_type
            && existing.is_folder != 0
            && (existing.overview.is_some() || existing.premiere_date.is_some());
        let preserve_production_year =
            existing.item_type == item.item_type && existing.premiere_date.is_some();

        let mut active: media_items::ActiveModel = existing.clone().into();
        active.title = Set(if preserve_folder_title {
            existing.title
        } else {
            item.title.clone()
        });
        active.library_id = Set(item.library_id.clone());
        active.parent_id = Set(item.parent_id.clone());
        active.item_type = Set(item.item_type.clone());
        active.is_folder = Set(item.is_folder as i64);
        active.container = Set(item.container.clone());
        active.overview = Set(item.overview.clone().or(existing.overview));
        active.official_rating = Set(item.official_rating.clone().or(existing.official_rating));
        active.extended_video_type = Set(item.extended_video_type.clone());
        active.production_year = Set(if preserve_production_year {
            existing.production_year
        } else {
            item.production_year.or(existing.production_year)
        });
        active.runtime_ticks = Set(item.runtime_ticks.or(existing.runtime_ticks));
        active.size_bytes = Set(item.size_bytes);
        active.season_number = Set(item.season_number);
        active.episode_number = Set(item.episode_number);
        active.modified_at = Set(item.modified_at);
        active.updated_at = Set(now_unix());
        active
            .update(db)
            .await
            .with_context(|| format!("failed to update media item: {}", item.path))?;
        return Ok(id);
    }

    MediaItems::insert(media_items::ActiveModel {
        id: Set(item.id.clone()),
        title: Set(item.title.clone()),
        path: Set(item.path.clone()),
        library_id: Set(item.library_id.clone()),
        parent_id: Set(item.parent_id.clone()),
        item_type: Set(item.item_type.clone()),
        is_folder: Set(item.is_folder as i64),
        is_public: Set(1),
        container: Set(item.container.clone()),
        overview: Set(item.overview.clone()),
        official_rating: Set(item.official_rating.clone()),
        extended_video_type: Set(item.extended_video_type.clone()),
        production_year: Set(item.production_year),
        runtime_ticks: Set(item.runtime_ticks),
        size_bytes: Set(item.size_bytes),
        season_number: Set(item.season_number),
        episode_number: Set(item.episode_number),
        modified_at: Set(item.modified_at),
        created_at: Set(item.created_at),
        updated_at: Set(now_unix()),
        ..Default::default()
    })
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to insert media item: {}", item.path))?;
    Ok(item.id.clone())
}

async fn unchanged_media_item_id(
    db: &DatabaseConnection,
    item: &ScannedMediaItem,
) -> anyhow::Result<Option<String>> {
    let Some(model) = MediaItems::find()
        .filter(media_items::Column::Path.eq(&item.path))
        .one(db)
        .await
        .with_context(|| format!("failed to read existing media item: {}", item.path))?
    else {
        return Ok(None);
    };

    let id = model.id.clone();
    if media_item_scan_matches(&model, item)? {
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

fn media_item_scan_matches(
    existing: &media_items::Model,
    item: &ScannedMediaItem,
) -> anyhow::Result<bool> {
    let existing_is_folder = existing.is_folder != 0;
    let preserve_folder_title = existing.item_type == item.item_type
        && existing_is_folder
        && (existing.overview.is_some() || existing.premiere_date.is_some());

    let title_matches = preserve_folder_title || existing.title == item.title;
    let overview_matches = option_str_eq(
        existing.overview.as_deref(),
        item.overview.as_deref().or(existing.overview.as_deref()),
    );
    let official_rating_matches = option_str_eq(
        existing.official_rating.as_deref(),
        item.official_rating
            .as_deref()
            .or(existing.official_rating.as_deref()),
    );
    let preserve_production_year =
        existing.item_type == item.item_type && existing.premiere_date.is_some();
    let target_production_year = if preserve_production_year {
        existing.production_year
    } else {
        item.production_year.or(existing.production_year)
    };
    let target_runtime_ticks = item.runtime_ticks.or(existing.runtime_ticks);

    Ok(title_matches
        && existing.library_id == item.library_id
        && existing.parent_id == item.parent_id
        && existing.item_type == item.item_type
        && existing_is_folder == item.is_folder
        && option_str_eq(existing.container.as_deref(), item.container.as_deref())
        && overview_matches
        && official_rating_matches
        && option_str_eq(
            existing.extended_video_type.as_deref(),
            item.extended_video_type.as_deref(),
        )
        && existing.production_year == target_production_year
        && existing.runtime_ticks == target_runtime_ticks
        && existing.size_bytes == item.size_bytes
        && existing.season_number == item.season_number
        && existing.episode_number == item.episode_number
        && existing.modified_at == item.modified_at)
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
    let kind = NamedRelationKind::from_tables(table, relation_table, relation_column)?;
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
    if relation_ids_match(db, item_id, kind, &desired_ids).await? {
        return Ok(());
    }

    clear_named_relations(db, item_id, kind)
        .await
        .with_context(|| format!("failed to clear {relation_table} for item: {item_id}"))?;

    for (id, value) in relation_values {
        upsert_named_value(db, kind, &id, value)
            .await
            .with_context(|| format!("failed to upsert {table}: {value}"))?;
        link_named_relation(db, kind, item_id, &id)
            .await
            .with_context(|| format!("failed to link {table} to item: {item_id}"))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum NamedRelationKind {
    Genre,
    Tag,
    Studio,
}

impl NamedRelationKind {
    fn from_tables(
        table: &str,
        relation_table: &str,
        relation_column: &str,
    ) -> anyhow::Result<Self> {
        match (table, relation_table, relation_column) {
            ("genres", "media_genres", "genre_id") => Ok(Self::Genre),
            ("tags", "media_tags", "tag_id") => Ok(Self::Tag),
            ("studios", "media_studios", "studio_id") => Ok(Self::Studio),
            _ => bail!(
                "unsupported named relation mapping: {table}/{relation_table}/{relation_column}"
            ),
        }
    }
}

async fn clear_named_relations(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
) -> anyhow::Result<()> {
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::delete_many()
                .filter(media_genres::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        NamedRelationKind::Tag => {
            MediaTags::delete_many()
                .filter(media_tags::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::delete_many()
                .filter(media_studios::Column::ItemId.eq(item_id))
                .exec(db)
                .await?;
        }
    }
    Ok(())
}

async fn upsert_named_value(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    id: &str,
    value: &str,
) -> anyhow::Result<()> {
    let now = now_unix();
    match kind {
        NamedRelationKind::Genre => {
            Genres::insert(genres::ActiveModel {
                id: Set(id.to_string()),
                name: Set(value.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(genres::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Tag => {
            Tags::insert(tags::ActiveModel {
                id: Set(id.to_string()),
                name: Set(value.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(tags::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Studio => {
            Studios::insert(studios::ActiveModel {
                id: Set(id.to_string()),
                name: Set(value.to_string()),
                created_at: Set(now),
            })
            .on_conflict(
                OnConflict::column(studios::Column::Name)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
    }
    Ok(())
}

async fn link_named_relation(
    db: &DatabaseConnection,
    kind: NamedRelationKind,
    item_id: &str,
    relation_id: &str,
) -> anyhow::Result<()> {
    match kind {
        NamedRelationKind::Genre => {
            MediaGenres::insert(media_genres::ActiveModel {
                item_id: Set(item_id.to_string()),
                genre_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([media_genres::Column::ItemId, media_genres::Column::GenreId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Tag => {
            MediaTags::insert(media_tags::ActiveModel {
                item_id: Set(item_id.to_string()),
                tag_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([media_tags::Column::ItemId, media_tags::Column::TagId])
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
        NamedRelationKind::Studio => {
            MediaStudios::insert(media_studios::ActiveModel {
                item_id: Set(item_id.to_string()),
                studio_id: Set(relation_id.to_string()),
            })
            .on_conflict(
                OnConflict::columns([
                    media_studios::Column::ItemId,
                    media_studios::Column::StudioId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
        }
    }
    Ok(())
}

async fn relation_ids_match(
    db: &DatabaseConnection,
    item_id: &str,
    kind: NamedRelationKind,
    desired_ids: &HashSet<&str>,
) -> anyhow::Result<bool> {
    let existing_ids = match kind {
        NamedRelationKind::Genre => MediaGenres::find()
            .filter(media_genres::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.genre_id)
            .collect::<Vec<_>>(),
        NamedRelationKind::Tag => MediaTags::find()
            .filter(media_tags::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.tag_id)
            .collect::<Vec<_>>(),
        NamedRelationKind::Studio => MediaStudios::find()
            .filter(media_studios::Column::ItemId.eq(item_id))
            .all(db)
            .await?
            .into_iter()
            .map(|relation| relation.studio_id)
            .collect::<Vec<_>>(),
    };
    if existing_ids.len() != desired_ids.len() {
        return Ok(false);
    }
    for relation_id in existing_ids {
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

    MediaPeople::delete_many()
        .filter(media_people::Column::ItemId.eq(item_id))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear people for item: {item_id}"))?;
    for person in people {
        People::insert(people::ActiveModel {
            id: Set(person.relation.person_id.clone()),
            name: Set(person.name.clone()),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(people::Column::Name)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to upsert person: {}", person.name))?;
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person.relation.person_id.clone()),
            role: Set(person.relation.role.clone()),
            person_type: Set(person.relation.person_type.clone()),
            sort_order: Set(person.relation.sort_order),
        })
        .on_conflict(
            OnConflict::columns([
                media_people::Column::ItemId,
                media_people::Column::PersonId,
                media_people::Column::PersonType,
            ])
            .update_columns([media_people::Column::Role, media_people::Column::SortOrder])
            .to_owned(),
        )
        .exec_without_returning(db)
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
    let existing_people = MediaPeople::find()
        .filter(media_people::Column::ItemId.eq(item_id))
        .order_by_asc(media_people::Column::SortOrder)
        .order_by_asc(media_people::Column::PersonId)
        .order_by_asc(media_people::Column::PersonType)
        .all(db)
        .await
        .with_context(|| format!("failed to read people for item: {item_id}"))?;
    if existing_people.len() != people.len() {
        return Ok(false);
    }
    for (existing_person, person) in existing_people.iter().zip(people.iter()) {
        let existing = PersonRelation {
            person_id: existing_person.person_id.clone(),
            role: existing_person.role.clone(),
            person_type: existing_person.person_type.clone(),
            sort_order: existing_person.sort_order,
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
    if let Some(existing) = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::StreamIndex.eq(0))
        .one(db)
        .await
        .with_context(|| format!("failed to read default media stream: {}", item.path))?
    {
        let mut active: media_streams::ActiveModel = existing.into();
        active.stream_type = Set(stream_type.to_string());
        active.is_external = Set(0);
        active
            .update(db)
            .await
            .with_context(|| format!("failed to update default media stream: {}", item.path))?;
    } else {
        MediaStreams::insert(media_streams::ActiveModel {
            id: Set(stream_id),
            item_id: Set(item.id.clone()),
            stream_index: Set(0),
            stream_type: Set(stream_type.to_string()),
            created_at: Set(now_unix()),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .with_context(|| format!("failed to insert default media stream: {}", item.path))?;
    }
    Ok(())
}

async fn default_media_stream_matches(
    db: &DatabaseConnection,
    item_id: &str,
    stream_type: &str,
) -> anyhow::Result<bool> {
    let stream = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::StreamIndex.eq(0))
        .one(db)
        .await
        .with_context(|| format!("failed to read default media stream: {item_id}"))?;
    let Some(stream) = stream else {
        return Ok(false);
    };
    Ok(stream.stream_type == stream_type && stream.is_external == 0)
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

    MediaStreams::delete_many()
        .filter(media_streams::Column::ItemId.eq(&item.id))
        .filter(media_streams::Column::IsExternal.eq(0))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear probed media streams: {}", item.id))?;

    for stream in &probe.streams {
        let stream_id = stable_text_id(&format!("stream:{}:{}", item.id, stream.stream_index));
        if let Some(existing) = MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq(&item.id))
            .filter(media_streams::Column::StreamIndex.eq(stream.stream_index))
            .one(db)
            .await
            .with_context(|| format!("failed to read probed stream for item: {}", item.id))?
        {
            let mut active: media_streams::ActiveModel = existing.into();
            apply_probed_stream(&mut active, stream);
            active
                .update(db)
                .await
                .with_context(|| format!("failed to update probed stream for item: {}", item.id))?;
        } else {
            let mut active = media_streams::ActiveModel {
                id: Set(stream_id),
                item_id: Set(item.id.clone()),
                stream_index: Set(stream.stream_index),
                created_at: Set(now_unix()),
                ..Default::default()
            };
            apply_probed_stream(&mut active, stream);
            MediaStreams::insert(active)
                .exec_without_returning(db)
                .await
                .with_context(|| format!("failed to insert probed stream for item: {}", item.id))?;
        }
    }

    Ok(true)
}

fn apply_probed_stream(active: &mut media_streams::ActiveModel, stream: &ProbedStream) {
    active.stream_type = Set(stream.stream_type.clone());
    active.codec = Set(stream.codec.clone());
    active.profile = Set(stream.profile.clone());
    active.codec_tag = Set(stream.codec_tag.clone());
    active.language = Set(stream.language.clone());
    active.title = Set(stream.title.clone());
    active.comment = Set(stream.comment.clone());
    active.bit_rate = Set(stream.bit_rate);
    active.width = Set(stream.width);
    active.height = Set(stream.height);
    active.aspect_ratio = Set(stream.aspect_ratio.clone());
    active.average_frame_rate = Set(stream.average_frame_rate);
    active.real_frame_rate = Set(stream.real_frame_rate);
    active.reference_frame_rate = Set(stream.reference_frame_rate);
    active.channels = Set(stream.channels);
    active.channel_layout = Set(stream.channel_layout.clone());
    active.sample_rate = Set(stream.sample_rate);
    active.bit_depth = Set(stream.bit_depth);
    active.ref_frames = Set(stream.ref_frames);
    active.is_interlaced = Set(stream.is_interlaced as i64);
    active.is_avc = Set(stream.is_avc.map(|value| value as i64));
    active.is_anamorphic = Set(stream.is_anamorphic.map(|value| value as i64));
    active.pixel_format = Set(stream.pixel_format.clone());
    active.level = Set(stream.level);
    active.color_range = Set(stream.color_range.clone());
    active.color_space = Set(stream.color_space.clone());
    active.color_transfer = Set(stream.color_transfer.clone());
    active.color_primaries = Set(stream.color_primaries.clone());
    active.time_base = Set(stream.time_base.clone());
    active.codec_time_base = Set(stream.codec_time_base.clone());
    active.nal_length_size = Set(stream.nal_length_size.clone());
    active.rotation = Set(stream.rotation);
    active.video_range = Set(stream.video_range.clone());
    active.video_range_type = Set(stream.video_range_type.clone());
    active.hdr10_plus_present_flag = Set(stream.hdr10_plus_present_flag.map(|value| value as i64));
    active.is_default = Set(stream.is_default as i64);
    active.is_forced = Set(stream.is_forced as i64);
    active.is_hearing_impaired = Set(stream.is_hearing_impaired as i64);
    active.is_original = Set(stream.is_original.map(|value| value as i64));
    active.is_external = Set(0);
}

async fn probed_media_streams_match(
    db: &DatabaseConnection,
    item_id: &str,
    probe: &MediaProbe,
) -> anyhow::Result<bool> {
    let streams = MediaStreams::find()
        .filter(media_streams::Column::ItemId.eq(item_id))
        .filter(media_streams::Column::IsExternal.eq(0))
        .order_by_asc(media_streams::Column::StreamIndex)
        .all(db)
        .await
        .with_context(|| format!("failed to read probed media streams: {item_id}"))?;
    if streams.len() != probe.streams.len() {
        return Ok(false);
    }
    for (existing, stream) in streams.iter().zip(probe.streams.iter()) {
        if !probed_stream_matches(existing, stream)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn probed_stream_matches(
    existing: &media_streams::Model,
    stream: &ProbedStream,
) -> anyhow::Result<bool> {
    Ok(existing.stream_index == stream.stream_index
        && existing.stream_type == stream.stream_type
        && option_str_eq(existing.codec.as_deref(), stream.codec.as_deref())
        && option_str_eq(existing.profile.as_deref(), stream.profile.as_deref())
        && option_str_eq(existing.codec_tag.as_deref(), stream.codec_tag.as_deref())
        && option_str_eq(existing.language.as_deref(), stream.language.as_deref())
        && option_str_eq(existing.title.as_deref(), stream.title.as_deref())
        && option_str_eq(existing.comment.as_deref(), stream.comment.as_deref())
        && existing.bit_rate == stream.bit_rate
        && existing.width == stream.width
        && existing.height == stream.height
        && option_str_eq(
            existing.aspect_ratio.as_deref(),
            stream.aspect_ratio.as_deref(),
        )
        && option_f64_eq(existing.average_frame_rate, stream.average_frame_rate)
        && option_f64_eq(existing.real_frame_rate, stream.real_frame_rate)
        && option_f64_eq(existing.reference_frame_rate, stream.reference_frame_rate)
        && existing.channels == stream.channels
        && option_str_eq(
            existing.channel_layout.as_deref(),
            stream.channel_layout.as_deref(),
        )
        && existing.sample_rate == stream.sample_rate
        && existing.bit_depth == stream.bit_depth
        && existing.ref_frames == stream.ref_frames
        && i64_bool(existing.is_interlaced) == stream.is_interlaced
        && opt_i64_bool(existing.is_avc) == stream.is_avc
        && opt_i64_bool(existing.is_anamorphic) == stream.is_anamorphic
        && option_str_eq(
            existing.pixel_format.as_deref(),
            stream.pixel_format.as_deref(),
        )
        && existing.level == stream.level
        && option_str_eq(
            existing.color_range.as_deref(),
            stream.color_range.as_deref(),
        )
        && option_str_eq(
            existing.color_space.as_deref(),
            stream.color_space.as_deref(),
        )
        && option_str_eq(
            existing.color_transfer.as_deref(),
            stream.color_transfer.as_deref(),
        )
        && option_str_eq(
            existing.color_primaries.as_deref(),
            stream.color_primaries.as_deref(),
        )
        && option_str_eq(existing.time_base.as_deref(), stream.time_base.as_deref())
        && option_str_eq(
            existing.codec_time_base.as_deref(),
            stream.codec_time_base.as_deref(),
        )
        && option_str_eq(
            existing.nal_length_size.as_deref(),
            stream.nal_length_size.as_deref(),
        )
        && existing.rotation == stream.rotation
        && option_str_eq(
            existing.video_range.as_deref(),
            stream.video_range.as_deref(),
        )
        && option_str_eq(
            existing.video_range_type.as_deref(),
            stream.video_range_type.as_deref(),
        )
        && opt_i64_bool(existing.hdr10_plus_present_flag) == stream.hdr10_plus_present_flag
        && i64_bool(existing.is_default) == stream.is_default
        && i64_bool(existing.is_forced) == stream.is_forced
        && i64_bool(existing.is_hearing_impaired) == stream.is_hearing_impaired
        && opt_i64_bool(existing.is_original) == stream.is_original)
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
    MediaStreams::delete_many()
        .filter(media_streams::Column::Id.eq(stream_id))
        .filter(media_streams::Column::ItemId.ne(item_id))
        .filter(media_streams::Column::IsExternal.eq(0))
        .exec(db)
        .await
        .with_context(|| format!("failed to clear stale generated stream id: {stream_id}"))?;
    Ok(())
}

pub async fn remove_missing_media_items(
    db: &DatabaseConnection,
    library_ids: &[String],
    seen_paths: &[String],
) -> anyhow::Result<()> {
    if library_ids.is_empty() {
        return Ok(());
    }

    let seen_paths = seen_paths
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let library_ids = library_ids
        .iter()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if library_ids.is_empty() {
        return Ok(());
    }

    let mut last_id = None::<String>;
    loop {
        let mut query = MediaItems::find()
            .filter(media_items::Column::LibraryId.is_in(library_ids.clone()))
            .order_by_asc(media_items::Column::Id)
            .limit(MISSING_CLEANUP_BATCH_SIZE);
        if let Some(last_id) = last_id.as_deref() {
            query = query.filter(media_items::Column::Id.gt(last_id));
        }

        let items = query
            .all(db)
            .await
            .context("failed to list media items for cleanup")?;
        if items.is_empty() {
            break;
        }

        last_id = items.last().map(|item| item.id.clone());
        for item in items {
            let id = item.id.clone();
            let path = item.path.clone();
            if seen_paths.contains(path.as_str()) || cleanup_path_exists(&path).await {
                continue;
            }
            MediaItems::delete_by_id(id)
                .exec(db)
                .await
                .with_context(|| format!("failed to delete missing media item: {path}"))?;
        }

        tokio::task::yield_now().await;
        tokio::time::sleep(MISSING_CLEANUP_PAUSE).await;
    }
    Ok(())
}

async fn cleanup_path_exists(path: &str) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::debug!("skipping missing-media cleanup for unreadable path {path}: {error}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ScannedMediaItem, cached_media_probe_if_current, upsert_default_media_stream,
        upsert_media_item, upsert_probed_media_streams,
    };
    use crate::entities::{
        media_items::{self, Entity as MediaItems},
        media_streams::{self, Entity as MediaStreams},
    };
    use crate::library::probe::{MediaProbe, ProbedStream};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

    #[tokio::test]
    async fn media_item_upsert_skips_unchanged_conflict_update() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let mut item = scanned_movie("movie-1", "Movie");

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");

        let mut active: media_items::ActiveModel = MediaItems::find_by_id(item.id.clone())
            .one(&db)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.updated_at = Set(123);
        active.update(&db).await.unwrap();

        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.updated_at, 123);

        item.title = "Renamed Movie".to_string();
        let stored_id = upsert_media_item(&db, &item).await.unwrap();
        assert_eq!(stored_id, "movie-1");
        let row = media_item_row(&db, &item.path).await;
        assert_eq!(row.title, "Renamed Movie");
        assert_ne!(row.updated_at, 123);
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
        let mut active: media_streams::ActiveModel = media_stream_row(&db, &item.id).await.into();
        active.created_at = Set(123);
        active.update(&db).await.unwrap();

        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.created_at, 123);

        let changed_probe = media_probe("hevc");
        assert!(
            upsert_probed_media_streams(&db, &item, &changed_probe)
                .await
                .unwrap()
        );
        let row = media_stream_row(&db, &item.id).await;
        assert_eq!(row.codec.as_deref(), Some("hevc"));
    }

    #[tokio::test]
    async fn default_stream_does_not_satisfy_probe_cache() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let item = scanned_movie("movie-default-cache", "Default Cache Movie");
        upsert_media_item(&db, &item).await.unwrap();
        upsert_default_media_stream(&db, &item).await.unwrap();

        assert!(
            cached_media_probe_if_current(
                &db,
                &item.path,
                item.modified_at,
                item.size_bytes,
                false
            )
            .await
            .unwrap()
            .is_none()
        );

        let probe = media_probe("h264");
        assert!(
            upsert_probed_media_streams(&db, &item, &probe)
                .await
                .unwrap()
        );

        let cached = cached_media_probe_if_current(
            &db,
            &item.path,
            item.modified_at,
            item.size_bytes,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(cached.runtime_ticks, item.runtime_ticks);
        assert_eq!(cached.size_bytes, item.size_bytes);
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

    async fn media_item_row(db: &sea_orm::DatabaseConnection, path: &str) -> media_items::Model {
        MediaItems::find()
            .filter(media_items::Column::Path.eq(path))
            .one(db)
            .await
            .unwrap()
            .unwrap()
    }

    async fn media_stream_row(
        db: &sea_orm::DatabaseConnection,
        item_id: &str,
    ) -> media_streams::Model {
        MediaStreams::find()
            .filter(media_streams::Column::ItemId.eq(item_id))
            .filter(media_streams::Column::StreamIndex.eq(0))
            .filter(media_streams::Column::IsExternal.eq(0))
            .one(db)
            .await
            .unwrap()
            .unwrap()
    }
}
