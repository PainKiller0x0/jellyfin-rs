use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, QueryOrder, Value as SeaValue};
use serde_json::json;

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{library_paths, library_paths::Entity as LibraryPaths},
    jellyfin::{
        auth::query_user_id_or_request,
        common::{internal_error, strip_nulls},
        item_queries,
    },
    library::{models::MediaItem, path_utils},
};

/// GET /Items/{item_id}/Intros — get intros (returns empty, not supported)
fn visible_media_item_sql(alias: &str) -> String {
    format!(
        "{alias}.is_public = 1 AND ({alias}.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = {alias}.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = {alias}.parent_id AND parent.is_public = 1))"
    )
}

pub async fn item_intros(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);
    match item_intros_value(&state.db, &user_id, &item_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /Items/{item_id}/LocalTrailers — get local trailers
pub async fn item_local_trailers(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    item_extra_array(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::Trailer,
    )
    .await
}

/// GET /Items/{item_id}/SpecialFeatures — get special features
pub async fn item_special_features(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    item_extra_array(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::SpecialFeature,
    )
    .await
}

/// GET /Items/{item_id}/ThemeSongs — theme songs (empty for video server)
pub async fn item_theme_songs(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    item_theme_result(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::ThemeSong,
    )
    .await
}

/// GET /Items/{item_id}/ThemeVideos — theme videos (empty)
pub async fn item_theme_videos(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    item_theme_result(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::ThemeVideo,
    )
    .await
}

/// GET /Items/{item_id}/ThemeMedia — theme media (empty)
pub async fn item_theme_media(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let songs = item_theme_result_value(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::ThemeSong,
    )
    .await;
    let videos = item_theme_result_value(
        &state,
        &request_user_id,
        &item_id,
        &query,
        ExtraKind::ThemeVideo,
    )
    .await;
    match (songs, videos) {
        (Ok(theme_songs), Ok(theme_videos)) => Json(json!({
            "ThemeVideosResult": theme_videos,
            "ThemeSongsResult": theme_songs,
            "SoundtrackSongsResult": theme_result(Vec::new(), &item_id),
        }))
        .into_response(),
        (Err(error), _) | (_, Err(error)) => internal_error(error),
    }
}

/// GET /MediaSegments/{item_id} — chapter markers (intro/credits segments)
async fn item_extra_array(
    state: &AppState,
    request_user_id: &str,
    item_id: &str,
    query: &HashMap<String, String>,
    kind: ExtraKind,
) -> Response {
    let user_id = query_user_id_or_request(query, request_user_id);
    match item_extras(&state.db, &user_id, item_id, kind).await {
        Ok(items) => Json(media_items_json(items)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_theme_result(
    state: &AppState,
    request_user_id: &str,
    item_id: &str,
    query: &HashMap<String, String>,
    kind: ExtraKind,
) -> Response {
    match item_theme_result_value(state, request_user_id, item_id, query, kind).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_theme_result_value(
    state: &AppState,
    request_user_id: &str,
    item_id: &str,
    query: &HashMap<String, String>,
    kind: ExtraKind,
) -> anyhow::Result<serde_json::Value> {
    let user_id = query_user_id_or_request(query, request_user_id);
    item_extras(&state.db, &user_id, item_id, kind)
        .await
        .map(|items| theme_result(items, item_id))
}

async fn item_extras(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
    kind: ExtraKind,
) -> anyhow::Result<Vec<MediaItem>> {
    let target_visible = visible_media_item_sql("media_items");
    let child_visible = visible_media_item_sql("child");
    let item_visible = visible_media_item_sql("media_items");
    let sql = item_queries::media_item_select_sql(&format!(
        r#"WHERE media_items.id IN (
            WITH RECURSIVE target(id, root_id) AS (
                SELECT media_items.id,
                       CASE
                           WHEN media_items.is_folder = 0 AND media_items.parent_id <> '' THEN media_items.parent_id
                           ELSE media_items.id
                       END
                FROM media_items
                WHERE media_items.id = ? AND {target_visible}
            ),
            tree(id) AS (
                SELECT root_id FROM target
                UNION ALL
                SELECT child.id FROM media_items child
                JOIN tree ON child.parent_id = tree.id AND {child_visible}
            )
            SELECT id FROM tree WHERE id <> (SELECT id FROM target)
        ) AND media_items.is_folder = 0 AND {item_visible}
        ORDER BY media_items.title ASC"#
    ));
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(
            &sql,
            vec![user_id.into(), item_id.into()],
        ))
        .await?;
    let mut items = item_queries::decode_media_items(&rows)?
        .into_iter()
        .filter(|item| kind.matches(item))
        .collect::<Vec<_>>();
    attach_image_tags(db, &mut items).await;
    Ok(items)
}

#[derive(Clone, Copy)]
enum ExtraKind {
    Trailer,
    SpecialFeature,
    ThemeSong,
    ThemeVideo,
}

impl ExtraKind {
    fn matches(self, item: &MediaItem) -> bool {
        let haystack = format!(
            "{} {} {}",
            item.title.to_ascii_lowercase(),
            item.path.replace('\\', "/").to_ascii_lowercase(),
            item.extended_video_type
                .clone()
                .unwrap_or_default()
                .to_ascii_lowercase()
        );
        match self {
            Self::Trailer => item.item_type == "Trailer" || contains_any(&haystack, &["trailer"]),
            Self::SpecialFeature => contains_any(
                &haystack,
                &[
                    "/extras/",
                    "/specials/",
                    "special feature",
                    "behind the scenes",
                    "deleted scene",
                ],
            ),
            Self::ThemeSong => item.item_type == "Audio" && contains_any(&haystack, &["theme"]),
            Self::ThemeVideo => {
                matches!(item.item_type.as_str(), "Video" | "Movie" | "Trailer")
                    && contains_any(&haystack, &["theme"])
            }
        }
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn media_items_json(items: Vec<MediaItem>) -> Vec<serde_json::Value> {
    items
        .into_iter()
        .map(|item| strip_nulls(item.to_jellyfin_json()))
        .collect()
}

fn theme_result(items: Vec<MediaItem>, owner_id: &str) -> serde_json::Value {
    let items = media_items_json(items);
    json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "StartIndex": 0,
        "OwnerId": owner_id,
    })
}

async fn item_intros_value(
    db: &DatabaseConnection,
    user_id: &str,
    item_id: &str,
) -> anyhow::Result<serde_json::Value> {
    let mut items = item_extras(db, user_id, item_id, ExtraKind::Trailer).await?;
    items.extend(item_extras(db, user_id, item_id, ExtraKind::SpecialFeature).await?);
    items.sort_by(|left, right| left.title.cmp(&right.title).then(left.id.cmp(&right.id)));
    items.dedup_by(|left, right| left.id == right.id);
    let total = items.len();
    Ok(json!({
        "Items": media_items_json(items),
        "TotalRecordCount": total,
        "StartIndex": 0,
    }))
}

pub async fn media_segments(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match media_segments_value(&state.db, &item_id, &query).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

async fn media_segments_value(
    db: &DatabaseConnection,
    item_id: &str,
    query: &HashMap<String, String>,
) -> anyhow::Result<Option<serde_json::Value>> {
    let include_types = query_param(query, &["includeSegmentTypes", "IncludeSegmentTypes"])
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(runtime) = public_item_runtime_ticks(db, item_id).await? else {
        return Ok(None);
    };
    let chapters = crate::chapters::get_chapters(db, item_id).await?;
    let mut segments = Vec::new();
    for window in chapters.windows(2) {
        let start = &window[0];
        let end = &window[1];
        let segment_type = match start.marker_type.as_deref() {
            Some("IntroStart") if end.marker_type.as_deref() == Some("IntroEnd") => "Intro",
            _ => continue,
        };
        if !include_types.is_empty()
            && !include_types
                .iter()
                .any(|value| value == &segment_type.to_ascii_lowercase())
        {
            continue;
        }
        segments.push(json!({
            "Id": start.id,
            "ItemId": item_id,
            "Type": segment_type,
            "StartTicks": start.start_position_ticks,
            "EndTicks": end.start_position_ticks,
        }));
    }
    if let Some(credits) = chapters
        .iter()
        .find(|chapter| chapter.marker_type.as_deref() == Some("CreditsStart"))
    {
        if runtime > credits.start_position_ticks
            && (include_types.is_empty() || include_types.iter().any(|value| value == "outro"))
        {
            segments.push(json!({
                "Id": credits.id,
                "ItemId": item_id,
                "Type": "Outro",
                "StartTicks": credits.start_position_ticks,
                "EndTicks": runtime,
            }));
        }
    }
    segments.sort_by_key(|segment| {
        segment
            .get("StartTicks")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    });
    let total = segments.len();
    Ok(Some(json!({
        "Items": segments,
        "TotalRecordCount": total,
        "StartIndex": 0,
    })))
}

async fn public_item_runtime_ticks(
    db: &DatabaseConnection,
    item_id: &str,
) -> anyhow::Result<Option<i64>> {
    let visible = visible_media_item_sql("media_items");
    let sql =
        format!("SELECT runtime_ticks FROM media_items WHERE media_items.id = ? AND {visible}");
    Ok(db
        .query_one_raw(crate::db::helpers::pg_statement(&sql, vec![item_id.into()]))
        .await?
        .map(|row| row.get_i64("runtime_ticks").unwrap_or(0)))
}

/// GET /Items/{item_id}/InstantMix — instant mix from item
pub async fn item_instant_mix(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query_user_id_or_request(&query, &request_user_id);

    match async {
        let seed_ids = seed_ids_for_item(&state.db, &item_id).await?;
        instant_mix_from_seed_ids(&state.db, &user_id, &seed_ids, &query, true).await
    }
    .await
    {
        Ok(items) => instant_mix_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn artist_instant_mix(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = query_param(&query, &["Id", "id", "ItemId", "itemId"]) else {
        return empty_instant_mix_response();
    };
    artist_instant_mix_for_value(&state, &request_user_id, id, &query).await
}

pub async fn artist_instant_mix_by_id(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    artist_instant_mix_for_value(&state, &request_user_id, &item_id, &query).await
}

async fn artist_instant_mix_for_value(
    state: &AppState,
    request_user_id: &str,
    id_or_name: &str,
    query: &HashMap<String, String>,
) -> Response {
    let user_id = query_user_id_or_request(query, request_user_id);

    match async {
        let seed_ids = seed_ids_for_artist(&state.db, id_or_name).await?;
        instant_mix_from_seed_ids(&state.db, &user_id, &seed_ids, query, true).await
    }
    .await
    {
        Ok(items) => instant_mix_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn music_genre_instant_mix(
    State(state): State<Arc<AppState>>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = query_param(&query, &["Id", "id", "Name", "name"]) else {
        return empty_instant_mix_response();
    };
    music_genre_instant_mix_for_value(&state, &request_user_id, id, &query).await
}

pub async fn music_genre_instant_mix_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(request_user_id): Extension<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    music_genre_instant_mix_for_value(&state, &request_user_id, &name, &query).await
}

async fn music_genre_instant_mix_for_value(
    state: &AppState,
    request_user_id: &str,
    id_or_name: &str,
    query: &HashMap<String, String>,
) -> Response {
    let user_id = query_user_id_or_request(query, request_user_id);

    match async {
        let seed_ids = seed_ids_for_music_genre(&state.db, id_or_name, query_limit(query)).await?;
        instant_mix_from_seed_ids(&state.db, &user_id, &seed_ids, query, true).await
    }
    .await
    {
        Ok(items) => instant_mix_response(items),
        Err(error) => internal_error(error),
    }
}

async fn seed_ids_for_item(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Vec<String>> {
    let visible = visible_media_item_sql("media_items");
    let seed_sql = format!(
        "SELECT item_type, is_folder FROM media_items WHERE media_items.id = ? AND {visible}"
    );
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            &seed_sql,
            vec![item_id.into()],
        ))
        .await?;

    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let item_type = row.get_str("item_type").unwrap_or_default();
    let is_folder = row.get_i64("is_folder").unwrap_or_default() != 0;

    if matches!(item_type.as_str(), "Playlist" | "BoxSet") {
        let source_visible = visible_media_item_sql("source");
        let audio_visible = visible_media_item_sql("audio");
        let sql = format!(
            r#"SELECT audio.id
               FROM linked_children lc
               JOIN media_items source ON source.id = lc.item_id
               JOIN media_items audio ON (audio.id = source.id OR audio.parent_id = source.id)
               WHERE lc.parent_id = ? AND {source_visible} AND {audio_visible} AND audio.item_type = 'Audio' AND audio.is_folder = 0
               GROUP BY audio.id
               ORDER BY MIN(audio.title) ASC"#
        );
        return query_string_column(db, &sql, vec![item_id.into()], "id").await;
    }

    if is_folder || item_type == "MusicAlbum" {
        let child_visible = visible_media_item_sql("media_items");
        let sql = format!(
            "SELECT id FROM media_items WHERE parent_id = ? AND item_type = 'Audio' AND is_folder = 0 AND {child_visible} ORDER BY title ASC"
        );
        let children = query_string_column(db, &sql, vec![item_id.into()], "id").await?;
        if !children.is_empty() {
            return Ok(children);
        }
    }

    Ok(vec![item_id.to_string()])
}

async fn seed_ids_for_artist(
    db: &DatabaseConnection,
    id_or_name: &str,
) -> anyhow::Result<Vec<String>> {
    let Some(person_id) = resolve_named_id(db, "people", id_or_name).await? else {
        return Ok(Vec::new());
    };
    let source_visible = visible_media_item_sql("source");
    let audio_visible = visible_media_item_sql("audio");
    let sql = format!(
        r#"SELECT audio.id
           FROM media_people mp
           JOIN media_items source ON source.id = mp.item_id
           JOIN media_items audio ON (audio.id = source.id OR audio.parent_id = source.id)
           WHERE mp.person_id = ?
             AND {source_visible}
             AND {audio_visible}
             AND LOWER(COALESCE(mp.person_type, '')) IN ('artist', 'musicartist', 'albumartist', 'audioalbumartist')
             AND audio.item_type = 'Audio'
             AND audio.is_folder = 0
           GROUP BY audio.id
           ORDER BY MIN(audio.title) ASC"#
    );
    query_string_column(db, &sql, vec![person_id.into()], "id").await
}

async fn seed_ids_for_music_genre(
    db: &DatabaseConnection,
    id_or_name: &str,
    limit: i64,
) -> anyhow::Result<Vec<String>> {
    let Some(genre_id) = resolve_named_id(db, "genres", id_or_name).await? else {
        return Ok(Vec::new());
    };
    let source_visible = visible_media_item_sql("source");
    let audio_visible = visible_media_item_sql("audio");
    let sql = format!(
        r#"SELECT audio.id
           FROM media_genres mg
           JOIN media_items source ON source.id = mg.item_id
           JOIN media_items audio ON (audio.id = source.id OR audio.parent_id = source.id)
           WHERE mg.genre_id = ?
             AND {source_visible}
             AND {audio_visible}
             AND audio.item_type = 'Audio'
             AND audio.is_folder = 0
           GROUP BY audio.id
           ORDER BY MIN(audio.title) ASC
           LIMIT ?"#
    );
    query_string_column(db, &sql, vec![genre_id.into(), limit.into()], "id").await
}

async fn instant_mix_from_seed_ids(
    db: &DatabaseConnection,
    user_id: &str,
    seed_ids: &[String],
    query: &HashMap<String, String>,
    fallback_to_seeds: bool,
) -> anyhow::Result<Vec<MediaItem>> {
    if seed_ids.is_empty() {
        return Ok(Vec::new());
    }

    let limit = query_limit(query);
    let item_types = include_item_types(query);
    let seed_placeholders = placeholders(seed_ids.len());
    let type_placeholders = placeholders(item_types.len());
    let item_visible = visible_media_item_sql("mi");
    let sql = format!(
        r#"SELECT mg_rel.item_id
           FROM media_genres mg_src
           JOIN media_genres mg_rel ON mg_src.genre_id = mg_rel.genre_id
           JOIN media_items mi ON mi.id = mg_rel.item_id
           WHERE mg_src.item_id IN ({seed_placeholders})
             AND mg_rel.item_id NOT IN ({seed_placeholders})
             AND {item_visible}
             AND mi.is_folder = 0
             AND mi.item_type IN ({type_placeholders})
           GROUP BY mg_rel.item_id
           ORDER BY COUNT(*) DESC, MAX(mi.modified_at) DESC
           LIMIT ?"#,
    );
    let mut values = Vec::new();
    push_values(&mut values, seed_ids);
    push_values(&mut values, seed_ids);
    push_values(&mut values, &item_types);
    values.push(limit.into());

    let related_ids = query_string_column(db, &sql, values, "item_id").await?;
    if !related_ids.is_empty() {
        return fetch_items_by_ids(db, user_id, &related_ids, &item_types, limit).await;
    }

    if fallback_to_seeds {
        return fetch_items_by_ids(db, user_id, seed_ids, &item_types, limit).await;
    }
    Ok(Vec::new())
}

async fn fetch_items_by_ids(
    db: &DatabaseConnection,
    user_id: &str,
    ids: &[String],
    item_types: &[String],
    limit: i64,
) -> anyhow::Result<Vec<MediaItem>> {
    let ids = ids
        .iter()
        .take(usize::try_from(limit).unwrap_or(16))
        .cloned()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let id_placeholders = placeholders(ids.len());
    let type_placeholders = placeholders(item_types.len());
    let visible = visible_media_item_sql("media_items");
    let sql = item_queries::media_item_select_sql(&format!(
        "WHERE media_items.id IN ({id_placeholders}) AND media_items.is_folder = 0 AND {visible} AND media_items.item_type IN ({type_placeholders})"
    ));
    let mut values: Vec<SeaValue> = vec![user_id.into()];
    push_values(&mut values, &ids);
    push_values(&mut values, item_types);

    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(&sql, values))
        .await?;
    let mut items = item_queries::decode_media_items(&rows)?;
    items.sort_by_key(|item| {
        ids.iter()
            .position(|id| id == &item.id)
            .unwrap_or(usize::MAX)
    });
    attach_image_tags(db, &mut items).await;
    Ok(items)
}

async fn attach_image_tags(db: &DatabaseConnection, items: &mut [MediaItem]) {
    let _ = item_queries::attach_item_image_tags(db, items).await;
}

async fn resolve_named_id(
    db: &DatabaseConnection,
    table: &str,
    id_or_name: &str,
) -> anyhow::Result<Option<String>> {
    let sql = format!("SELECT id FROM {table} WHERE id = ? OR name = ? LIMIT 1");
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            &sql,
            vec![id_or_name.into(), id_or_name.into()],
        ))
        .await?;
    Ok(row.and_then(|row| row.get_opt_str("id").ok().flatten()))
}

async fn query_string_column(
    db: &DatabaseConnection,
    sql: &str,
    values: Vec<SeaValue>,
    column: &str,
) -> anyhow::Result<Vec<String>> {
    let rows = db
        .query_all_raw(crate::db::helpers::pg_statement(sql, values))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get_opt_str(column).ok().flatten())
        .collect())
}

fn placeholders(len: usize) -> String {
    (0..len).map(|_| "?").collect::<Vec<_>>().join(",")
}

fn push_values(values: &mut Vec<SeaValue>, strings: &[String]) {
    values.extend(strings.iter().map(|value| value.as_str().into()));
}

fn query_param<'a>(query: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| query.get(*key).map(String::as_str))
}

fn query_limit(query: &HashMap<String, String>) -> i64 {
    query_param(query, &["Limit", "limit"])
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(16)
        .clamp(1, 200)
}

fn include_item_types(query: &HashMap<String, String>) -> Vec<String> {
    let types = query_param(query, &["IncludeItemTypes", "includeItemTypes"])
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if types.is_empty() {
        vec!["Audio".to_string()]
    } else {
        types
    }
}

fn instant_mix_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(json!({
        "Items": items.into_iter().map(|item| strip_nulls(item.to_jellyfin_json())).collect::<Vec<_>>(),
        "TotalRecordCount": total,
        "StartIndex": 0
    }))
    .into_response()
}

fn empty_instant_mix_response() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 })).into_response()
}

/// GET /Items/{id}/CriticReviews — critic reviews
pub async fn item_critic_reviews() -> Response {
    Json(json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 })).into_response()
}

/// GET /Items/{id}/ThumbnailSet — trickplay thumbnail set
pub async fn thumbnail_set(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let width = query
        .get("Width")
        .or_else(|| query.get("width"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(320);
    match trickplay_info(&state.db, &item_id, width).await {
        Ok(Some(info)) => Json(json!({
            "AspectRatio": 16.0 / 9.0,
            "Thumbnails": (0..info.tile_count).map(|index| json!({
                "PositionTicks": index * info.interval_ticks,
                "ImageTag": format!("{}-{}-{}", item_id, info.width, index),
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn trickplay_playlist(
    State(state): State<Arc<AppState>>,
    Path((item_id, width)): Path<(String, i64)>,
) -> Response {
    match trickplay_info(&state.db, &item_id, width).await {
        Ok(Some(info)) => {
            if !info.has_allowed_path(&state.db).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            if tokio::fs::metadata(&info.path).await.is_err() {
                return StatusCode::NOT_FOUND.into_response();
            }
            let interval_seconds = (info.interval_ticks as f64 / 10_000_000.0).max(0.001)
                * info.tile_count.max(1) as f64;
            let mut body = "#EXTM3U\n#EXT-X-VERSION:3\n".to_string();
            body.push_str(&format!(
                "#EXTINF:{interval_seconds:.3},\n/Videos/{}/Trickplay/{}/0.jpg\n",
                item_id, info.width
            ));
            body.push_str("#EXT-X-ENDLIST\n");
            ([(header::CONTENT_TYPE, "application/x-mpegURL")], body).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn trickplay_tile(
    State(state): State<Arc<AppState>>,
    Path((item_id, width, index)): Path<(String, i64, String)>,
) -> Response {
    let Some(index) = parse_trickplay_index(&index) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match trickplay_info(&state.db, &item_id, width).await {
        Ok(Some(info)) if index == 0 => {
            if !info.has_allowed_path(&state.db).await {
                return StatusCode::NOT_FOUND.into_response();
            }
            match tokio::fs::read(&info.path).await {
                Ok(bytes) => {
                    let mut headers = HeaderMap::new();
                    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
                    headers.insert(
                        header::CONTENT_LENGTH,
                        HeaderValue::from_str(&bytes.len().to_string())
                            .unwrap_or_else(|_| HeaderValue::from_static("0")),
                    );
                    (headers, bytes).into_response()
                }
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

struct TrickplayInfo {
    width: i64,
    tile_count: i64,
    interval_ticks: i64,
    path: String,
}

impl TrickplayInfo {
    async fn has_allowed_path(&self, db: &DatabaseConnection) -> bool {
        let mut roots = vec![
            std::path::PathBuf::from("data")
                .join("trickplay")
                .to_string_lossy()
                .to_string(),
            std::path::PathBuf::from("data")
                .join("images")
                .to_string_lossy()
                .to_string(),
        ];
        match library_roots(db).await {
            Ok(library_roots) => roots.extend(library_roots),
            Err(error) => tracing::warn!("failed to read library roots for trickplay: {error:#}"),
        }
        path_utils::path_within_roots(&self.path, &roots)
    }
}

async fn trickplay_info(
    db: &DatabaseConnection,
    item_id: &str,
    width: i64,
) -> anyhow::Result<Option<TrickplayInfo>> {
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            "SELECT tp.width, tp.tile_count, tp.interval_ticks, tp.path FROM trickplay_images tp JOIN media_items mi ON mi.id = tp.item_id WHERE tp.item_id = ? AND tp.width = ? AND mi.is_public = 1 AND (mi.parent_id = '' OR EXISTS (SELECT 1 FROM libraries library_parent WHERE library_parent.id = mi.parent_id) OR EXISTS (SELECT 1 FROM media_items parent WHERE parent.id = mi.parent_id AND parent.is_public = 1))",
            vec![item_id.into(), width.into()],
        ))
        .await?;
    Ok(row.map(|row| TrickplayInfo {
        width: row.get_i64("width").unwrap_or(width),
        tile_count: row.get_i64("tile_count").unwrap_or_default(),
        interval_ticks: row.get_i64("interval_ticks").unwrap_or_default(),
        path: row.get_str("path").unwrap_or_default(),
    }))
}

fn parse_trickplay_index(value: &str) -> Option<i64> {
    value.trim_end_matches(".jpg").parse().ok()
}

async fn library_roots(db: &DatabaseConnection) -> anyhow::Result<Vec<String>> {
    let paths = LibraryPaths::find()
        .order_by_asc(library_paths::Column::Path)
        .all(db)
        .await?;
    Ok(paths.into_iter().map(|path| path.path).collect())
}

/// GET /Items/{item_id}/RemoteSearch/Subtitles/{language} — search remote subtitles
pub async fn remote_subtitle_search(
    State(state): State<Arc<AppState>>,
    Path((item_id, _param)): Path<(String, String)>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    match public_item_exists(&state.db, &item_id).await {
        Ok(true) => Json(json!([])).into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// POST /Items/{item_id}/RemoteSearch/Subtitles/{subtitle_id} — download remote subtitle
pub async fn download_remote_subtitle(
    State(_state): State<Arc<AppState>>,
    Path((_item_id, _param)): Path<(String, String)>,
) -> Response {
    // Not implemented - would need subtitle provider integration
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Genres/{name}/Images/{image_type} — genre image
pub async fn genre_image(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type)): Path<(String, String)>,
) -> Response {
    // Genre images not stored
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Studios/{name}/Images/{image_type} — studio image
pub async fn studio_image(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type)): Path<(String, String)>,
) -> Response {
    // Studio images not stored
    StatusCode::NOT_FOUND.into_response()
}

/// GET /Users/{id}/Items/{id}/Intros — per-user intros
pub async fn user_item_intros(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match item_intros_value(&state.db, &user_id, &item_id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /Users/{id}/Items/{id}/LocalTrailers — per-user local trailers
pub async fn user_item_local_trailers(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match item_extras(&state.db, &user_id, &item_id, ExtraKind::Trailer).await {
        Ok(items) => Json(media_items_json(items)).into_response(),
        Err(error) => internal_error(error),
    }
}

/// GET /Users/{id}/Items/{id}/SpecialFeatures — per-user special features
pub async fn user_item_special_features(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match item_extras(&state.db, &user_id, &item_id, ExtraKind::SpecialFeature).await {
        Ok(items) => Json(media_items_json(items)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn genre_image_with_index(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type, _index)): Path<(String, String, String)>,
) -> Response {
    StatusCode::NOT_FOUND.into_response()
}

pub async fn studio_image_with_index(
    State(_state): State<Arc<AppState>>,
    Path((_name, _image_type, _index)): Path<(String, String, String)>,
) -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn public_item_exists(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<bool> {
    Ok(item_queries::find_media_item(db, "", item_id)
        .await?
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::{
        ExtraKind, TrickplayInfo, empty_instant_mix_response, include_item_types,
        instant_mix_from_seed_ids, instant_mix_response, item_critic_reviews, item_extras,
        item_intros_value, media_segments_value, parse_trickplay_index, public_item_exists,
        query_limit, remote_subtitle_search, seed_ids_for_artist, seed_ids_for_item,
        seed_ids_for_music_genre, studio_image_with_index, trickplay_info,
    };
    use crate::entities::{
        chapters::{self, Entity as Chapters},
        genres::{self, Entity as Genres},
        libraries::{self, Entity as Libraries},
        library_paths::{self, Entity as LibraryPaths},
        linked_children::{self, Entity as LinkedChildren},
        media_genres::{self, Entity as MediaGenres},
        media_items::{self, Entity as MediaItems},
        media_people::{self, Entity as MediaPeople},
        people::{self, Entity as People},
        trickplay_images::{self, Entity as TrickplayImages},
    };
    use axum::{
        body::to_bytes, extract::Path, extract::Query, extract::State, response::IntoResponse,
    };
    use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{RwLock, broadcast};
    use uuid::Uuid;

    fn item(title: &str, path: &str, item_type: &str) -> crate::library::models::MediaItem {
        crate::library::models::MediaItem {
            id: "i1".to_string(),
            title: title.to_string(),
            path: path.to_string(),
            library_id: "movies".to_string(),
            collection_type: "movies".to_string(),
            parent_id: "m1".to_string(),
            item_type: item_type.to_string(),
            is_folder: false,
            container: None,
            overview: None,
            official_rating: None,
            extended_video_type: None,
            production_year: None,
            premiere_date: None,
            runtime_ticks: None,
            size_bytes: None,
            season_number: None,
            episode_number: None,
            community_rating: None,
            critic_rating: None,
            created_at: 0,
            modified_at: 0,
            is_public: true,
            is_favorite: false,
            played: false,
            playback_position_ticks: 0,
            played_percentage: None,
            play_count: 0,
            last_played_at: None,
            image_tags: None,
        }
    }

    #[test]
    fn instant_mix_query_defaults_to_audio() {
        let query = HashMap::new();
        assert_eq!(include_item_types(&query), vec!["Audio"]);
        assert_eq!(query_limit(&query), 16);
    }

    #[test]
    fn instant_mix_query_accepts_emby_and_jellyfin_casing() {
        let mut query = HashMap::new();
        query.insert(
            "includeItemTypes".to_string(),
            "Audio,MusicAlbum".to_string(),
        );
        query.insert("limit".to_string(), "500".to_string());
        assert_eq!(include_item_types(&query), vec!["Audio", "MusicAlbum"]);
        assert_eq!(query_limit(&query), 200);
    }

    #[tokio::test]
    async fn instant_mix_responses_include_start_index() {
        let response =
            instant_mix_response(vec![item("Song", "Music/song.mp3", "Audio")]).into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["TotalRecordCount"], 1);
        assert_eq!(value["StartIndex"], 0);

        let response = empty_instant_mix_response().into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["TotalRecordCount"], 0);
        assert_eq!(value["StartIndex"], 0);
    }

    #[tokio::test]
    async fn instant_mix_ignores_items_under_private_parents() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };

        for (id, title, parent_id, item_type, is_folder, is_public) in [
            ("song1", "Alpha", "", "Audio", 0_i64, 1_i64),
            ("song2", "Beta", "", "Audio", 0_i64, 1_i64),
            ("playlist", "Playlist", "", "Playlist", 1_i64, 1_i64),
            (
                "private-parent",
                "Private Parent",
                "",
                "MusicAlbum",
                1_i64,
                0_i64,
            ),
            (
                "hidden-song",
                "Hidden",
                "private-parent",
                "Audio",
                0_i64,
                1_i64,
            ),
        ] {
            insert_media_item(
                &db,
                id,
                title,
                &format!("D:/{id}.mp3"),
                "",
                parent_id,
                item_type,
                is_folder,
                is_public,
                None,
            )
            .await;
        }

        insert_linked_child(&db, "playlist", "song1", 0).await;
        insert_linked_child(&db, "playlist", "hidden-song", 1).await;
        insert_person(&db, "artist1", "Artist").await;
        for item_id in ["song1", "hidden-song"] {
            insert_media_person(&db, item_id, "artist1", "Artist").await;
        }
        insert_genre(&db, "genre1", "Genre").await;
        for item_id in ["song1", "song2", "hidden-song"] {
            insert_media_genre(&db, item_id, "genre1").await;
        }

        assert_eq!(
            seed_ids_for_item(&db, "hidden-song").await.unwrap().len(),
            0
        );
        assert_eq!(
            seed_ids_for_item(&db, "playlist").await.unwrap(),
            vec!["song1".to_string()]
        );
        assert_eq!(
            seed_ids_for_artist(&db, "artist1").await.unwrap(),
            vec!["song1".to_string()]
        );
        let genre_seed_ids = seed_ids_for_music_genre(&db, "genre1", 16).await.unwrap();
        assert!(genre_seed_ids.contains(&"song1".to_string()));
        assert!(genre_seed_ids.contains(&"song2".to_string()));
        assert!(!genre_seed_ids.contains(&"hidden-song".to_string()));

        let related =
            instant_mix_from_seed_ids(&db, "u1", &["song1".to_string()], &HashMap::new(), false)
                .await
                .unwrap();
        assert_eq!(
            related
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["song2"]
        );
    }

    #[tokio::test]
    async fn indexed_studio_image_path_returns_not_found() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let response = studio_image_with_index(
            State(Arc::new(test_state(db))),
            Path(("studio".to_string(), "Primary".to_string(), "0".to_string())),
        )
        .await
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn extra_kind_matches_local_extra_paths() {
        assert!(ExtraKind::Trailer.matches(&item(
            "Trailer",
            "Movie/trailers/trailer.mp4",
            "Video",
        )));
        assert!(ExtraKind::SpecialFeature.matches(&item(
            "Behind the Scenes",
            "Movie/extras/behind.mp4",
            "Video",
        )));
        assert!(ExtraKind::ThemeSong.matches(&item("Theme", "Movie/theme.mp3", "Audio",)));
    }

    #[tokio::test]
    async fn item_extras_find_sibling_extras_for_movie_file() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_library(&db, "movies", "Movies", "movies").await;
        for (id, title, path, parent_id, item_type, is_folder) in [
            ("movie1", "Movie", "D:/Movie", "movies", "Movie", 1_i64),
            (
                "video1",
                "Movie",
                "D:/Movie/Movie.mp4",
                "movie1",
                "Video",
                0_i64,
            ),
            (
                "extra1",
                "Behind the Scenes",
                "D:/Movie/extras/Behind the Scenes.mp4",
                "movie1",
                "Video",
                0_i64,
            ),
            (
                "trailer1",
                "Trailer",
                "D:/Movie/trailers/Trailer.mp4",
                "movie1",
                "Video",
                0_i64,
            ),
        ] {
            insert_media_item(
                &db, id, title, path, "movies", parent_id, item_type, is_folder, 1, None,
            )
            .await;
        }

        let items = item_extras(&db, "u1", "video1", ExtraKind::SpecialFeature)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "extra1");

        let intros = item_intros_value(&db, "u1", "video1").await.unwrap();
        assert_eq!(intros["TotalRecordCount"], 2);
        assert_eq!(intros["StartIndex"], 0);
        let ids: Vec<_> = intros["Items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["Id"].as_str())
            .collect();
        assert!(ids.contains(&"extra1"));
        assert!(ids.contains(&"trailer1"));

        update_item_public(&db, "video1", 0).await;
        assert!(
            item_extras(&db, "u1", "video1", ExtraKind::SpecialFeature)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            item_intros_value(&db, "u1", "video1").await.unwrap()["TotalRecordCount"],
            0
        );

        for (id, title, path, parent_id, item_type, is_folder, is_public) in [
            (
                "hidden-movie",
                "Hidden Movie",
                "D:/Hidden",
                "movies",
                "Movie",
                1_i64,
                0_i64,
            ),
            (
                "hidden-video",
                "Hidden Movie",
                "D:/Hidden/Movie.mp4",
                "hidden-movie",
                "Video",
                0_i64,
                1_i64,
            ),
            (
                "hidden-extra",
                "Hidden Behind the Scenes",
                "D:/Hidden/extras/Behind the Scenes.mp4",
                "hidden-movie",
                "Video",
                0_i64,
                1_i64,
            ),
        ] {
            insert_media_item(
                &db, id, title, path, "movies", parent_id, item_type, is_folder, is_public, None,
            )
            .await;
        }
        assert!(
            item_extras(&db, "u1", "hidden-video", ExtraKind::SpecialFeature)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            item_intros_value(&db, "u1", "hidden-video").await.unwrap()["TotalRecordCount"],
            0
        );
    }

    #[tokio::test]
    async fn trickplay_info_reads_existing_record() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(
            &db,
            "item1",
            "Video",
            "D:/video.mkv",
            "",
            "",
            "Video",
            0,
            1,
            None,
        )
        .await;
        insert_trickplay(&db, "tp1", "item1", 320, 3, 5_000_000, "D:/tiles.jpg").await;

        let info = trickplay_info(&db, "item1", 320).await.unwrap().unwrap();
        assert_eq!(info.tile_count, 3);
        assert_eq!(info.interval_ticks, 5_000_000);
        assert_eq!(info.path, "D:/tiles.jpg");
        assert!(trickplay_info(&db, "item1", 640).await.unwrap().is_none());
        assert_eq!(parse_trickplay_index("2.jpg"), Some(2));

        update_item_public(&db, "item1", 0).await;
        assert!(trickplay_info(&db, "item1", 320).await.unwrap().is_none());

        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            "D:/private-parent",
            "",
            "",
            "Movie",
            1,
            0,
            None,
        )
        .await;
        insert_media_item(
            &db,
            "public-child",
            "Public Child",
            "D:/public-child.mkv",
            "",
            "private-parent",
            "Video",
            0,
            1,
            None,
        )
        .await;
        insert_trickplay(
            &db,
            "tp2",
            "public-child",
            320,
            1,
            5_000_000,
            "D:/hidden-tiles.jpg",
        )
        .await;
        assert!(
            trickplay_info(&db, "public-child", 320)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn trickplay_paths_must_stay_inside_library_or_data_roots() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let temp =
            std::env::temp_dir().join(format!("jellyfin-rs-trickplay-{}", uuid::Uuid::new_v4()));
        let library = temp.join("library");
        let outside = temp.join("outside");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let allowed_tile = library.join("tiles.jpg");
        let rejected_tile = outside.join("secret.jpg");
        std::fs::write(&allowed_tile, b"tile").unwrap();
        std::fs::write(&rejected_tile, b"secret").unwrap();

        insert_library(&db, "lib", "Library", "movies").await;
        insert_library_path(&db, "path1", "lib", &library.to_string_lossy()).await;

        assert!(
            (TrickplayInfo {
                width: 320,
                tile_count: 1,
                interval_ticks: 5_000_000,
                path: allowed_tile.to_string_lossy().to_string(),
            })
            .has_allowed_path(&db)
            .await
        );
        assert!(
            !(TrickplayInfo {
                width: 320,
                tile_count: 1,
                interval_ticks: 5_000_000,
                path: rejected_tile.to_string_lossy().to_string(),
            })
            .has_allowed_path(&db)
            .await
        );

        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn media_segments_use_query_result_shape_and_filter() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        insert_media_item(
            &db,
            "ep1",
            "Episode",
            "D:/ep1.mkv",
            "",
            "",
            "Episode",
            0,
            1,
            Some(100),
        )
        .await;
        for (id, ticks, marker) in [
            ("intro-start", 10_i64, "IntroStart"),
            ("intro-end", 20_i64, "IntroEnd"),
            ("credits", 80_i64, "CreditsStart"),
        ] {
            insert_chapter(&db, id, "ep1", ticks, marker, marker).await;
        }

        let value = media_segments_value(&db, "ep1", &HashMap::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["TotalRecordCount"], 2);
        assert_eq!(value["StartIndex"], 0);
        assert_eq!(
            value["Items"][0],
            json!({
                "Id": "intro-start",
                "ItemId": "ep1",
                "Type": "Intro",
                "StartTicks": 10,
                "EndTicks": 20,
            })
        );
        assert_eq!(
            value["Items"][1],
            json!({
                "Id": "credits",
                "ItemId": "ep1",
                "Type": "Outro",
                "StartTicks": 80,
                "EndTicks": 100,
            })
        );

        let mut query = HashMap::new();
        query.insert("includeSegmentTypes".to_string(), "Intro".to_string());
        let filtered = media_segments_value(&db, "ep1", &query)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(filtered["TotalRecordCount"], 1);
        assert_eq!(filtered["Items"][0]["Type"], "Intro");
        update_item_public(&db, "ep1", 0).await;
        assert!(
            media_segments_value(&db, "ep1", &HashMap::new())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            media_segments_value(&db, "missing", &HashMap::new())
                .await
                .unwrap()
                .is_none()
        );

        insert_media_item(
            &db,
            "private-parent",
            "Private Parent",
            "D:/private-parent",
            "",
            "",
            "Series",
            1,
            0,
            None,
        )
        .await;
        insert_media_item(
            &db,
            "hidden-episode",
            "Hidden Episode",
            "D:/hidden-episode.mkv",
            "",
            "private-parent",
            "Episode",
            0,
            1,
            Some(100),
        )
        .await;
        insert_chapter(
            &db,
            "hidden-intro",
            "hidden-episode",
            10,
            "IntroStart",
            "IntroStart",
        )
        .await;
        assert!(
            media_segments_value(&db, "hidden-episode", &HashMap::new())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn remote_subtitle_search_requires_public_item() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        for (id, parent_id, is_public) in [
            ("public", "", 1_i64),
            ("private", "", 0_i64),
            ("private-parent", "", 0_i64),
            ("public-child", "private-parent", 1_i64),
        ] {
            insert_media_item(
                &db,
                id,
                id,
                &format!("D:/{id}.mkv"),
                "",
                parent_id,
                "Video",
                0,
                is_public,
                None,
            )
            .await;
        }

        assert!(public_item_exists(&db, "public").await.unwrap());
        assert!(!public_item_exists(&db, "private").await.unwrap());
        assert!(!public_item_exists(&db, "public-child").await.unwrap());
        assert!(!public_item_exists(&db, "missing").await.unwrap());

        let state = Arc::new(test_state(db));
        assert_eq!(
            remote_subtitle_search(
                State(state.clone()),
                Path(("public".to_string(), "eng".to_string())),
                Query(HashMap::new()),
            )
            .await
            .into_response()
            .status(),
            axum::http::StatusCode::OK
        );
        assert_eq!(
            remote_subtitle_search(
                State(state.clone()),
                Path(("private".to_string(), "eng".to_string())),
                Query(HashMap::new()),
            )
            .await
            .into_response()
            .status(),
            axum::http::StatusCode::NOT_FOUND
        );
        assert_eq!(
            remote_subtitle_search(
                State(state),
                Path(("missing".to_string(), "eng".to_string())),
                Query(HashMap::new()),
            )
            .await
            .into_response()
            .status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn critic_reviews_empty_result_has_start_index() {
        let response = item_critic_reviews().await.into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["TotalRecordCount"], 0);
        assert_eq!(value["StartIndex"], 0);
        assert!(value["Items"].as_array().unwrap().is_empty());
    }

    async fn insert_library(db: &DatabaseConnection, id: &str, name: &str, collection_type: &str) {
        Libraries::insert(libraries::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            collection_type: Set(collection_type.to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_library_path(db: &DatabaseConnection, id: &str, library_id: &str, path: &str) {
        LibraryPaths::insert(library_paths::ActiveModel {
            id: Set(id.to_string()),
            library_id: Set(library_id.to_string()),
            path: Set(path.to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_media_item(
        db: &DatabaseConnection,
        id: &str,
        title: &str,
        path: &str,
        library_id: &str,
        parent_id: &str,
        item_type: &str,
        is_folder: i64,
        is_public: i64,
        runtime_ticks: Option<i64>,
    ) {
        MediaItems::insert(media_items::ActiveModel {
            id: Set(id.to_string()),
            title: Set(title.to_string()),
            path: Set(path.to_string()),
            library_id: Set(library_id.to_string()),
            parent_id: Set(parent_id.to_string()),
            item_type: Set(item_type.to_string()),
            is_folder: Set(is_folder),
            is_public: Set(is_public),
            runtime_ticks: Set(runtime_ticks),
            modified_at: Set(1),
            created_at: Set(1),
            updated_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn update_item_public(db: &DatabaseConnection, id: &str, is_public: i64) {
        let mut active: media_items::ActiveModel = MediaItems::find_by_id(id.to_string())
            .one(db)
            .await
            .unwrap()
            .unwrap()
            .into();
        active.is_public = Set(is_public);
        active.update(db).await.unwrap();
    }

    async fn insert_linked_child(
        db: &DatabaseConnection,
        parent_id: &str,
        item_id: &str,
        sort_order: i64,
    ) {
        LinkedChildren::insert(linked_children::ActiveModel {
            parent_id: Set(parent_id.to_string()),
            item_id: Set(item_id.to_string()),
            sort_order: Set(sort_order),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_person(db: &DatabaseConnection, id: &str, name: &str) {
        People::insert(people::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            created_at: Set(1),
            ..Default::default()
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_media_person(
        db: &DatabaseConnection,
        item_id: &str,
        person_id: &str,
        person_type: &str,
    ) {
        MediaPeople::insert(media_people::ActiveModel {
            item_id: Set(item_id.to_string()),
            person_id: Set(person_id.to_string()),
            person_type: Set(person_type.to_string()),
            role: Set(None),
            sort_order: Set(0),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_genre(db: &DatabaseConnection, id: &str, name: &str) {
        Genres::insert(genres::ActiveModel {
            id: Set(id.to_string()),
            name: Set(name.to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_media_genre(db: &DatabaseConnection, item_id: &str, genre_id: &str) {
        MediaGenres::insert(media_genres::ActiveModel {
            item_id: Set(item_id.to_string()),
            genre_id: Set(genre_id.to_string()),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_trickplay(
        db: &DatabaseConnection,
        id: &str,
        item_id: &str,
        width: i64,
        tile_count: i64,
        interval_ticks: i64,
        path: &str,
    ) {
        TrickplayImages::insert(trickplay_images::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            width: Set(width),
            tile_count: Set(tile_count),
            interval_ticks: Set(interval_ticks),
            path: Set(path.to_string()),
            created_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    async fn insert_chapter(
        db: &DatabaseConnection,
        id: &str,
        item_id: &str,
        start_position_ticks: i64,
        name: &str,
        marker_type: &str,
    ) {
        Chapters::insert(chapters::ActiveModel {
            id: Set(id.to_string()),
            item_id: Set(item_id.to_string()),
            start_position_ticks: Set(start_position_ticks),
            name: Set(name.to_string()),
            marker_type: Set(Some(marker_type.to_string())),
            source: Set("test".to_string()),
            created_at: Set(1),
            updated_at: Set(1),
        })
        .exec_without_returning(db)
        .await
        .unwrap();
    }

    fn test_state(db: DatabaseConnection) -> crate::app::state::AppState {
        let (ws_event_tx, _) = broadcast::channel(4);
        crate::app::state::AppState {
            user_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"test"),
            access_token: "test-token".to_string(),
            db,
            media_dirs: Vec::new(),
            http_client: reqwest::Client::new(),
            tmdb_api_key: RwLock::new(None),
            tmdb_proxy_url: Arc::new(RwLock::new(None)),
            tmdb_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            douban_cookie: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            playback_sessions: RwLock::new(HashMap::new()),
            session_capabilities: RwLock::new(HashMap::new()),
            admin_http_log_seq: std::sync::atomic::AtomicU64::new(0),
            admin_http_logs: RwLock::new(std::collections::VecDeque::new()),
            playback_distribution: RwLock::new(crate::app::state::PlaybackDistribution::default()),
            ws_event_tx,
            sa_config: crate::config::StrmAssistantConfig::default(),
            intro_detector: Arc::new(crate::intro_skip::detector::IntroDetector::default()),
            queue_manager: Arc::new(crate::queue::QueueManager::default()),
        }
    }
}
