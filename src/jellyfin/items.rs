use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    jellyfin::{
        common::internal_error,
        item_queries::{latest_media_items, library_views, list_media_items, resume_media_items},
    },
    library::{models::MediaItem, scanner::scan_media_library},
    util::now_unix,
};

mod discovery;
mod item_operations;
mod remote_metadata;

pub use crate::jellyfin::item_queries::find_media_item;
pub use discovery::{search_hints, shows_missing, shows_next_up, similar_items};
pub use item_operations::{delete_info, delete_items, update_item};
pub use remote_metadata::{apply_remote_search, remote_search};

pub async fn views(State(state): State<Arc<AppState>>) -> Response {
    match library_views(&state.db).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn user_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match list_media_items(&state.db, &user_id, &query).await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn latest_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match latest_media_items(&state.db, &user_id).await {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|item| item.to_jellyfin_json())
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn resume_items(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> Response {
    match resume_media_items(&state.db, &user_id).await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn show_seasons(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match child_items_by_type(&state.db, &user_id, &show_id, "Season").await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub async fn show_episodes(
    State(state): State<Arc<AppState>>,
    Path(show_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    let result = if let Some(season_id) = query.get("SeasonId") {
        child_items_by_type(&state.db, &user_id, season_id, "Episode").await
    } else {
        descendant_episodes(&state.db, &user_id, &show_id).await
    };
    match result {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

pub(super) fn media_list_response(items: Vec<MediaItem>) -> Response {
    let total = items.len();
    Json(json!({ "Items": items.into_iter().map(|item| item.to_jellyfin_json()).collect::<Vec<_>>(), "TotalRecordCount": total })).into_response()
}

async fn child_items_by_type(
    db: &DatabaseConnection,
    user_id: &str,
    parent_id: &str,
    item_type: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &crate::jellyfin::item_queries::media_item_select_sql(
                "WHERE media_items.parent_id = ? AND media_items.item_type = ? ORDER BY media_items.title ASC",
            ),
            vec![user_id.into(), parent_id.into(), item_type.into()],
        ))
        .await
        .with_context(|| format!("failed to list {item_type} children for: {parent_id}"))?;
    rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show child items")
}

async fn descendant_episodes(
    db: &DatabaseConnection,
    user_id: &str,
    show_id: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &format!(
                r#"WITH RECURSIVE tree(id) AS (SELECT ? UNION ALL SELECT media_items.id FROM media_items JOIN tree ON media_items.parent_id = tree.id) {} WHERE media_items.id IN (SELECT id FROM tree WHERE id <> ?) AND media_items.item_type = 'Episode' ORDER BY media_items.title ASC"#,
                crate::jellyfin::item_queries::media_item_select_sql("").trim()
            ),
            vec![show_id.into(), user_id.into(), show_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list episodes for show: {show_id}"))?;
    rows.iter()
        .map(MediaItem::from_query_result)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to decode show episodes")
}

pub async fn item_by_id(
    State(state): State<Arc<AppState>>,
    Path((user_id, item_id)): Path<(String, String)>,
) -> Response {
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, item).await {
            Ok(item) => Json(item).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn item_by_id_public(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    let user_id = state.user_id.to_string();
    match find_media_item(&state.db, &user_id, &item_id).await {
        Ok(Some(item)) => match item_json_with_provider_ids(&state.db, item).await {
            Ok(item) => Json(item).into_response(),
            Err(error) => internal_error(error),
        },
        Ok(None) => Json(json!({ "Name": item_id, "Id": item_id, "Type": "Folder", "UserData": { "Played": false, "IsFavorite": false } })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn items_root(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let user_id = query
        .get("UserId")
        .cloned()
        .unwrap_or_else(|| state.user_id.to_string());
    match list_media_items(&state.db, &user_id, &query).await {
        Ok(items) => media_list_response(items),
        Err(error) => internal_error(error),
    }
}

async fn item_json_with_provider_ids(db: &DatabaseConnection, item: MediaItem) -> anyhow::Result<Value> {
    let mut value = item.to_jellyfin_json();
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT provider, provider_item_id FROM provider_ids WHERE item_id = ?",
            vec![item.id.clone().into()],
        ))
        .await
        .with_context(|| format!("failed to list provider ids for item: {}", item.id))?;
    let provider_ids = rows
        .iter()
        .map(|row| -> anyhow::Result<(String, Value)> {
            Ok((
                row.get_str("provider")?,
                Value::String(row.get_str("provider_item_id")?),
            ))
        })
        .collect::<anyhow::Result<serde_json::Map<String, Value>>>()?;
    value["ProviderIds"] = Value::Object(provider_ids);
    value["ImageTags"] = crate::jellyfin::images::item_image_tags(db, &item.id)
        .await
        .context("failed to load image tags")?;
    value["GenreItems"] =
        Value::Array(relation_values(db, "genres", "media_genres", "genre_id", &item.id).await?);
    value["TagItems"] =
        Value::Array(relation_values(db, "tags", "media_tags", "tag_id", &item.id).await?);
    value["Studios"] =
        Value::Array(relation_values(db, "studios", "media_studios", "studio_id", &item.id).await?);
    value["People"] = Value::Array(people_values(db, &item.id).await?);
    Ok(value)
}

async fn relation_values(
    db: &DatabaseConnection,
    table: &str,
    relation_table: &str,
    relation_column: &str,
    item_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let sql = format!(
        "SELECT {table}.id, {table}.name FROM {table} JOIN {relation_table} ON {relation_table}.{relation_column} = {table}.id WHERE {relation_table}.item_id = ? ORDER BY {table}.name ASC"
    );
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            &sql,
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list {table} for item: {item_id}"))?;
    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Id": row.get_str("id")?,
                "Name": row.get_str("name")?,
            }))
        })
        .collect()
}

async fn people_values(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT people.id, people.name, media_people.role, media_people.person_type FROM people JOIN media_people ON media_people.person_id = people.id WHERE media_people.item_id = ? ORDER BY media_people.sort_order ASC, people.name ASC",
            vec![item_id.into()],
        ))
        .await
        .with_context(|| format!("failed to list people for item: {item_id}"))?;
    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Id": row.get_str("id")?,
                "Name": row.get_str("name")?,
                "Role": row.get_opt_str("role")?,
                "Type": row.get_opt_str("person_type")?,
            }))
        })
        .collect()
}

pub async fn item_subtitles(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
) -> Response {
    match subtitle_list_inner(&state.db, &item_id).await {
        Ok(items) => {
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

async fn subtitle_list_inner(db: &DatabaseConnection, item_id: &str) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT stream_index, codec, language, title, is_external FROM media_streams WHERE item_id = ? AND stream_type = 'Subtitle' ORDER BY stream_index ASC",
            vec![item_id.into()],
        ))
        .await
        .context("failed to list subtitles")?;

    rows.iter()
        .map(|row| -> anyhow::Result<Value> {
            Ok(json!({
                "Index": row.get_i64("stream_index")?,
                "Codec": row.get_opt_str("codec")?,
                "Language": row.get_opt_str("language")?,
                "DisplayTitle": row.get_opt_str("title")?,
                "IsExternal": row.get_i64("is_external").unwrap_or_default() != 0,
            }))
        })
        .collect()
}

pub async fn metadata_reset(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    let item_ids: Vec<&str> = body
        .get("Ids")
        .and_then(Value::as_str)
        .map(|ids| {
            ids.split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if item_ids.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let now = now_unix();
    let backend = state.db.get_database_backend();
    for item_id in &item_ids {
        let _ = state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                "UPDATE media_items SET overview = NULL, production_year = NULL, updated_at = ? WHERE id = ?",
                vec![now.into(), (*item_id).into()],
            ))
            .await;

        for table in [
            "media_people",
            "media_genres",
            "media_tags",
            "media_studios",
            "provider_ids",
        ] {
            let _ = state
                .db
                .execute(crate::db::helpers::portable_statement(
                    backend,
                    &format!("DELETE FROM {table} WHERE item_id = ?"),
                    vec![(*item_id).into()],
                ))
                .await;
        }

        crate::jellyfin::system::log_activity(
            &state,
            "Metadata reset",
            "MetadataReset",
            None,
            Some(item_id),
        )
        .await;
    }

    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
) -> Response {
    match display_preferences_inner(&state.db, &prefs_id).await {
        Ok(Some(prefs)) => Json(prefs).into_response(),
        Ok(None) => Json(json!({ "Id": prefs_id, "CustomPrefs": {} })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn update_display_preferences(
    State(state): State<Arc<AppState>>,
    Path(prefs_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let now = now_unix();
    let prefs_json = body.to_string();
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let default_user_id = state.user_id.to_string();
    let user_id = body
        .get("UserId")
        .and_then(Value::as_str)
        .unwrap_or(&default_user_id);
    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO display_preferences (id, user_id, preferences_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET preferences_json = excluded.preferences_json, user_id = excluded.user_id, updated_at = excluded.updated_at"#,
            vec![id.into(), user_id.into(), prefs_json.into(), now.into(), now.into()],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

async fn display_preferences_inner(
    db: &DatabaseConnection,
    prefs_id: &str,
) -> anyhow::Result<Option<Value>> {
    let id = crate::util::stable_text_id(&format!("display-prefs:{prefs_id}"));
    let backend = db.get_database_backend();
    let row = db
        .query_one(crate::db::helpers::portable_statement(
            backend,
            "SELECT preferences_json FROM display_preferences WHERE id = ?",
            vec![id.into()],
        ))
        .await
        .context("failed to load display preferences")?;
    match row {
        Some(row) => {
            let json_str: String = row.get_str("preferences_json")?;
            Ok(Some(serde_json::from_str(&json_str)?))
        }
        None => Ok(None),
    }
}

pub async fn item_counts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match item_counts_inner(&state.db, &query).await {
        Ok(counts) => Json(counts).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn item_counts_inner(
    db: &DatabaseConnection,
    _query: &HashMap<String, String>,
) -> anyhow::Result<Value> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT library_id, item_type, COUNT(*) AS count FROM media_items WHERE is_folder = 0 GROUP BY library_id, item_type"#,
            vec![],
        ))
        .await
        .context("failed to count items")?;

    let mut counts = serde_json::Map::new();
    for row in &rows {
        let library_id: String = row.get_str("library_id")?;
        let item_type: String = row.get_str("item_type")?;
        let count: i64 = row.get_i64("count")?;
        if let Some(obj) = counts
            .entry(library_id)
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            let existing = obj.get(&item_type).and_then(Value::as_i64).unwrap_or(0);
            obj.insert(item_type, json!(existing + count));
        }
    }

    Ok(Value::Object(counts))
}

pub async fn scan_handler(State(state): State<Arc<AppState>>) -> Response {
    let start = now_unix();
    let result = scan_media_library(&state).await;
    let end = now_unix();
    let (status, message) = match &result {
        Ok(count) => ("Completed", Some(format!("Scanned {count} items"))),
        Err(error) => ("Failed", Some(format!("{error:#}"))),
    };
    crate::jellyfin::system::upsert_task_result(
        &state,
        "scan-library",
        status,
        start,
        end,
        message.as_deref(),
    )
    .await;
    crate::jellyfin::system::log_activity(&state, "Library scan", "LibraryScan", None, None).await;
    match result {
        Ok(scanned) => Json(json!({ "Scanned": scanned })).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn external_id_infos() -> Response {
    Json(json!([
        {
            "Name": "TheMovieDb",
            "Key": "Tmdb",
            "Website": "https://www.themoviedb.org/",
            "UrlFormatString": "https://www.themoviedb.org/movie/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "TheTVDB",
            "Key": "Tvdb",
            "Website": "https://thetvdb.com/",
            "UrlFormatString": "https://thetvdb.com/?id={0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "IMDb",
            "Key": "IMDB",
            "Website": "https://www.imdb.com/",
            "UrlFormatString": "https://www.imdb.com/title/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album",
            "Key": "MusicBrainzAlbum",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Album Artist",
            "Key": "MusicBrainzAlbumArtist",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/artist/{0}",
            "IsSupportedAsIdentifier": true
        },
        {
            "Name": "MusicBrainz Release Group",
            "Key": "MusicBrainzReleaseGroup",
            "Website": "https://musicbrainz.org/",
            "UrlFormatString": "https://musicbrainz.org/release-group/{0}",
            "IsSupportedAsIdentifier": true
        }
    ]))
    .into_response()
}
