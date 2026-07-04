use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
    entities::{
        libraries::{self, Entity as Libraries},
        library_paths::{self, Entity as LibraryPaths},
    },
    jellyfin::common::internal_error,
    jellyfin::system,
    library::{path_utils, scanner::scan_media_library},
    util::{now_unix, stable_text_id},
};

#[derive(Deserialize)]
pub struct VirtualFolderQuery {
    #[serde(rename = "name", alias = "Name")]
    name: Option<String>,
    #[serde(rename = "collectionType", alias = "CollectionType")]
    collection_type: Option<String>,
    #[serde(rename = "paths", alias = "Paths")]
    paths: Option<String>,
}

#[derive(Deserialize)]
pub struct LibraryPathQuery {
    #[serde(rename = "name", alias = "Name")]
    name: String,
    #[serde(rename = "path", alias = "Path")]
    path: String,
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = value.get(*key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    None
}

fn json_path_info_path(value: &Value) -> Option<String> {
    value
        .get("PathInfo")
        .or_else(|| value.get("pathInfo"))
        .and_then(|path_info| json_string(path_info, &["Path", "path"]))
}

fn query_string(query: &HashMap<String, String>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| query.get(*key))
        .cloned()
        .unwrap_or_default()
}

fn parse_json_body(body: Bytes) -> Result<Option<Value>, Response> {
    if body.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&body).map(Some).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": format!("invalid JSON body: {error}") })),
        )
            .into_response()
    })
}

fn virtual_folder_request(
    mut query: VirtualFolderQuery,
    body: Bytes,
) -> Result<VirtualFolderQuery, Response> {
    if let Some(body) = parse_json_body(body)? {
        query.name = json_string(&body, &["Name", "name"]).or(query.name);
        query.collection_type =
            json_string(&body, &["CollectionType", "collectionType"]).or(query.collection_type);
        query.paths = json_string(&body, &["Paths", "paths", "Path", "path"])
            .or_else(|| json_path_info_path(&body))
            .or(query.paths);
    }
    Ok(query)
}

fn library_path_request(
    query: HashMap<String, String>,
    body: Bytes,
) -> Result<LibraryPathQuery, Response> {
    let mut query = LibraryPathQuery {
        name: query_string(&query, &["name", "Name"]),
        path: query_string(&query, &["path", "Path"]),
    };
    if let Some(body) = parse_json_body(body)? {
        if let Some(name) = json_string(&body, &["Name", "name"]) {
            query.name = name;
        }
        if let Some(path) =
            json_string(&body, &["Path", "path"]).or_else(|| json_path_info_path(&body))
        {
            query.path = path;
        }
    }
    Ok(query)
}

fn rename_virtual_folder_request(
    query: &HashMap<String, String>,
    body: Bytes,
) -> Result<(String, String), Response> {
    let mut name = query
        .get("name")
        .or_else(|| query.get("Name"))
        .cloned()
        .unwrap_or_default();
    let mut new_name = query
        .get("newName")
        .or_else(|| query.get("NewName"))
        .cloned()
        .unwrap_or_default();
    if let Some(body) = parse_json_body(body)? {
        if let Some(value) = json_string(&body, &["Name", "name"]) {
            name = value;
        }
        if let Some(value) = json_string(&body, &["NewName", "newName"]) {
            new_name = value;
        }
    }
    Ok((name, new_name))
}

fn update_virtual_folder_path_request(
    query: &HashMap<String, String>,
    body: Bytes,
) -> Result<(String, String, String), Response> {
    let mut name = query
        .get("name")
        .or_else(|| query.get("Name"))
        .cloned()
        .unwrap_or_default();
    let mut path = query
        .get("path")
        .or_else(|| query.get("Path"))
        .cloned()
        .unwrap_or_default();
    let mut new_path = query
        .get("newPath")
        .or_else(|| query.get("NewPath"))
        .cloned()
        .unwrap_or_default();
    if let Some(body) = parse_json_body(body)? {
        if let Some(value) = json_string(&body, &["Name", "name"]) {
            name = value;
        }
        if let Some(value) = json_string(&body, &["Path", "path"]) {
            path = value;
        }
        if let Some(value) =
            json_path_info_path(&body).or_else(|| json_string(&body, &["NewPath", "newPath"]))
        {
            new_path = value;
        }
    }
    Ok((name, path, new_path))
}

pub async fn virtual_folders(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => Json(folders).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn virtual_folders_query(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => Json(query_result(folders)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn media_folders(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => Json(query_result(folders)).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn available_options() -> Response {
    Json(library_options_result()).into_response()
}

pub async fn create_virtual_folder(
    State(state): State<Arc<AppState>>,
    Query(query): Query<VirtualFolderQuery>,
    body: Bytes,
) -> Response {
    let query = match virtual_folder_request(query, body) {
        Ok(query) => query,
        Err(response) => return response,
    };
    match create_virtual_folder_inner(&state.db, query).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// DELETE /Library/VirtualFolders — delete a virtual folder and all its media
pub async fn delete_virtual_folder(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query_string(&query, &["name", "Name"]);
    let name = name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name is required" })),
        )
            .into_response();
    }

    match delete_virtual_folder_inner(&state.db, name).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn add_virtual_folder_path(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let query = match library_path_request(query, body) {
        Ok(query) => query,
        Err(response) => return response,
    };
    match upsert_library_path(&state.db, &query.name, &query.path, None).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn delete_virtual_folder_path(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query_string(&query, &["name", "Name"]);
    let path = query_string(&query, &["path", "Path"]);
    if name.trim().is_empty() || path.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name and path are required" })),
        )
            .into_response();
    }

    match delete_virtual_folder_path_inner(&state.db, &name, &path).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn refresh_library(State(state): State<Arc<AppState>>) -> Response {
    tokio::spawn(async move {
        let start = now_unix();
        let result = scan_media_library(&state).await;
        let end = now_unix();
        let (status, message) = match &result {
            Ok(count) => ("Completed", Some(format!("Scanned {count} items"))),
            Err(error) => ("Failed", Some(format!("{error:#}"))),
        };
        system::upsert_task_result(
            &state,
            "scan-library",
            status,
            start,
            end,
            message.as_deref(),
        )
        .await;
        system::log_activity(&state, "Library scan", "LibraryScan", None, None).await;
    });
    Json(json!({ "Scanning": true })).into_response()
}

async fn virtual_folders_inner(db: &DatabaseConnection) -> anyhow::Result<Vec<Value>> {
    let backend = db.get_database_backend();
    let rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            r#"SELECT libraries.id, libraries.name, libraries.collection_type, library_paths.path FROM libraries LEFT JOIN library_paths ON library_paths.library_id = libraries.id ORDER BY libraries.name ASC, library_paths.path ASC"#,
            vec![],
        ))
        .await
        .context("failed to list virtual folders")?;

    let mut folders = Vec::<VirtualFolder>::new();
    for row in &rows {
        let id: String = row.get_str("id")?;
        let name: String = row.get_str("name")?;
        let collection_type: String = row.get_str("collection_type")?;
        let path: Option<String> = row.get_opt_str("path")?;

        if let Some(folder) = folders.iter_mut().find(|folder| folder.id == id) {
            if let Some(path) = path {
                folder.paths.push(path);
            }
            continue;
        }

        folders.push(VirtualFolder {
            id,
            name,
            collection_type,
            paths: path.into_iter().collect(),
        });
    }

    Ok(folders
        .into_iter()
        .map(|folder| {
            json!({
                "Name": folder.name,
                "Id": folder.id,
                "ItemId": folder.id,
                "CollectionType": folder.collection_type,
                "Locations": folder.paths,
            })
        })
        .collect())
}

async fn create_virtual_folder_inner(
    db: &DatabaseConnection,
    query: VirtualFolderQuery,
) -> anyhow::Result<()> {
    let name = query.name.as_deref().unwrap_or_default().trim();
    if name.is_empty() {
        bail!("name is required");
    }

    let collection_type = query.collection_type.as_deref().unwrap_or("movies").trim();
    let library_id = library_id_for_name(name);
    let now = now_unix();

    upsert_library(db, &library_id, name, collection_type, now).await?;

    if let Some(paths) = query.paths {
        for path in paths.split('|').flat_map(|value| value.split(',')) {
            let path = path.trim();
            if !path.is_empty() {
                upsert_library_path(db, name, path, Some(collection_type)).await?;
            }
        }
    }

    Ok(())
}

async fn upsert_library(
    db: &DatabaseConnection,
    id: &str,
    name: &str,
    collection_type: &str,
    now: i64,
) -> anyhow::Result<()> {
    match Libraries::find_by_id(id).one(db).await? {
        Some(model) => {
            let mut active: libraries::ActiveModel = model.into();
            active.name = Set(name.to_string());
            active.collection_type = Set(collection_type.to_string());
            active.updated_at = Set(now);
            active.update(db).await?;
        }
        None => {
            let active = libraries::ActiveModel {
                id: Set(id.to_string()),
                name: Set(name.to_string()),
                collection_type: Set(collection_type.to_string()),
                created_at: Set(now),
                updated_at: Set(now),
            };
            Libraries::insert(active).exec(db).await?;
        }
    }
    Ok(())
}

async fn upsert_library_path(
    db: &DatabaseConnection,
    name: &str,
    path: &str,
    collection_type: Option<&str>,
) -> anyhow::Result<()> {
    let name = name.trim();
    let path = path_utils::validate_library_path(path)?;
    if name.is_empty() {
        bail!("name is required");
    }
    if path.is_empty() {
        bail!("path is required");
    }

    let library_id = library_id_for_name(name);
    let collection_type = collection_type.unwrap_or_else(|| collection_type_for_name(name));
    let now = now_unix();

    upsert_library(db, &library_id, name, collection_type, now).await?;

    let path_id = stable_text_id(&format!("library-path:{path}"));
    let existing = LibraryPaths::find_by_id(&path_id).one(db).await?;
    if let Some(model) = existing {
        let mut active: library_paths::ActiveModel = model.into();
        active.library_id = Set(library_id);
        active.update(db).await?;
    } else {
        let active = library_paths::ActiveModel {
            id: Set(path_id),
            library_id: Set(library_id),
            path: Set(path),
            created_at: Set(now),
        };
        LibraryPaths::insert(active).exec(db).await?;
    }

    Ok(())
}

async fn delete_virtual_folder_inner(db: &DatabaseConnection, name: &str) -> anyhow::Result<bool> {
    let library_id = library_id_for_name(name);
    let backend = db.get_database_backend();
    if Libraries::find_by_id(&library_id).one(db).await?.is_none() {
        return Ok(false);
    }

    let item_rows = db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT id FROM media_items WHERE library_id = ?",
            vec![library_id.clone().into()],
        ))
        .await
        .context("failed to list library media items")?;
    let item_ids: Vec<String> = item_rows
        .iter()
        .filter_map(|row| row.get_str("id").ok())
        .collect();

    if !item_ids.is_empty() {
        let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let params: Vec<sea_orm::Value> = item_ids.iter().map(|id| id.as_str().into()).collect();
        for table in [
            "media_genres",
            "media_tags",
            "media_studios",
            "media_people",
            "media_game_genres",
            "media_streams",
            "user_data",
            "image_assets",
            "provider_ids",
            "chapters",
            "trickplay_images",
        ] {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                &format!("DELETE FROM {table} WHERE item_id IN ({placeholders})"),
                params.clone(),
            ))
            .await
            .with_context(|| format!("failed to delete {table} for library: {name}"))?;
        }
        for column in ["item_id", "parent_id"] {
            db.execute(crate::db::helpers::portable_statement(
                backend,
                &format!("DELETE FROM linked_children WHERE {column} IN ({placeholders})"),
                params.clone(),
            ))
            .await
            .context("failed to delete linked children for library")?;
        }
        db.execute(crate::db::helpers::portable_statement(
            backend,
            &format!("DELETE FROM media_items WHERE id IN ({placeholders})"),
            params,
        ))
        .await
        .context("failed to delete library media items")?;
    }

    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM library_paths WHERE library_id = ?",
        vec![library_id.clone().into()],
    ))
    .await
    .context("failed to delete library paths")?;
    db.execute(crate::db::helpers::portable_statement(
        backend,
        "DELETE FROM libraries WHERE id = ?",
        vec![library_id.into()],
    ))
    .await
    .context("failed to delete library")?;
    Ok(true)
}

async fn delete_virtual_folder_path_inner(
    db: &DatabaseConnection,
    name: &str,
    path: &str,
) -> anyhow::Result<bool> {
    let path = path_utils::canonicalize_path(path)?;
    let result = LibraryPaths::delete_many()
        .filter(library_paths::Column::LibraryId.eq(library_id_for_name(name)))
        .filter(library_paths::Column::Path.eq(path))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

async fn rename_virtual_folder_inner(
    db: &DatabaseConnection,
    name: &str,
    new_name: &str,
) -> anyhow::Result<bool> {
    let result = db
        .execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "UPDATE libraries SET name = ?, updated_at = ? WHERE id = ?",
            vec![
                new_name.into(),
                now_unix().into(),
                library_id_for_name(name).into(),
            ],
        ))
        .await?;
    Ok(result.rows_affected() > 0)
}

async fn update_virtual_folder_path_inner(
    db: &DatabaseConnection,
    name: &str,
    path: &str,
    target_path: &str,
) -> anyhow::Result<bool> {
    let path = path_utils::canonicalize_path(path)?;
    let target_path = path_utils::validate_library_path(target_path)?;
    let result = db
        .execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "UPDATE library_paths SET path = ?, library_id = ? WHERE library_id = ? AND path = ?",
            vec![
                target_path.into(),
                library_id_for_name(name).into(),
                library_id_for_name(name).into(),
                path.into(),
            ],
        ))
        .await?;
    Ok(result.rows_affected() > 0)
}

async fn update_library_options_inner(
    db: &DatabaseConnection,
    body: &Value,
) -> anyhow::Result<bool> {
    let library_id = json_string(body, &["Id", "id"])
        .or_else(|| json_string(body, &["Name", "name"]).map(|name| library_id_for_name(&name)))
        .unwrap_or_default();
    if library_id.trim().is_empty() {
        bail!("Id or Name is required");
    }
    if Libraries::find_by_id(&library_id).one(db).await?.is_none() {
        return Ok(false);
    }
    let options = body
        .get("LibraryOptions")
        .or_else(|| body.get("libraryOptions"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let value = serde_json::to_string(&options).context("failed to serialize library options")?;
    system::set_app_setting(db, &library_options_key(&library_id), &value).await?;
    Ok(true)
}

fn library_options_key(library_id: &str) -> String {
    format!("LibraryOptions.{library_id}")
}

fn library_id_for_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "movies" => "movies".to_string(),
        "tv shows" | "tvshows" => "tvshows".to_string(),
        "music" => "music".to_string(),
        _ => stable_text_id(&format!("library:{}", name.trim().to_ascii_lowercase())),
    }
}

pub async fn physical_paths(State(state): State<Arc<AppState>>) -> Response {
    match LibraryPaths::find()
        .order_by_asc(library_paths::Column::Path)
        .all(&state.db)
        .await
    {
        Ok(models) => {
            Json(models.iter().map(|m| m.path.as_str()).collect::<Vec<_>>()).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

fn collection_type_for_name(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "tv shows" | "tvshows" => "tvshows",
        "music" => "music",
        _ => "movies",
    }
}

/// POST /Library/VirtualFolders/Name — rename a virtual folder
pub async fn rename_virtual_folder(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let (name, new_name) = match rename_virtual_folder_request(&query, body) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let name = name.trim();
    let new_name = new_name.trim();

    if name.is_empty() || new_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name and newName are required" })),
        )
            .into_response();
    }

    match rename_virtual_folder_inner(&state.db, name, new_name).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => internal_error(error),
    }
}

/// POST /Library/VirtualFolders/LibraryOptions — update library options
pub async fn update_library_options(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Response {
    match update_library_options_inner(&state.db, &body).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) if error.to_string().contains("required") => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": error.to_string() })),
        )
            .into_response(),
        Err(error) => internal_error(error),
    }
}

/// POST /Library/VirtualFolders/Paths/Update — update a library path
pub async fn update_virtual_folder_path(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let (name, path, new_path) = match update_virtual_folder_path_request(&query, body) {
        Ok(values) => values,
        Err(response) => return response,
    };
    let name = name.trim();
    let path = path.trim();
    let new_path = new_path.trim();

    if name.is_empty() || path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name and path are required" })),
        )
            .into_response();
    }

    let target_path = if !new_path.is_empty() { new_path } else { path };
    match update_virtual_folder_path_inner(&state.db, name, path, target_path).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error)
            if error.to_string().contains("required") || error.to_string().contains("exist") =>
        {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "Error": error.to_string() })),
            )
                .into_response()
        }
        Err(error) => internal_error(error),
    }
}

/// POST /Library/SelectableMediaFolders — get selectable media folders
pub async fn selectable_media_folders(State(state): State<Arc<AppState>>) -> Response {
    // Return the same as virtual_folders but in a different format
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => Json(folders).into_response(),
        Err(error) => internal_error(error),
    }
}

/// Library change notification handlers — trigger a background scan
pub async fn library_notify(State(state): State<Arc<AppState>>) -> Response {
    // These endpoints notify the server that media has changed externally
    // We trigger a background scan in response
    tokio::spawn(async move {
        let _ = scan_media_library(&state).await;
    });
    StatusCode::NO_CONTENT.into_response()
}

struct VirtualFolder {
    id: String,
    name: String,
    collection_type: String,
    paths: Vec<String>,
}

fn library_options_result() -> Value {
    let local = option_info("Nfo", true);
    let tmdb = option_info("TheMovieDb", true);
    let image_types = vec![
        "Primary",
        "Art",
        "Backdrop",
        "Banner",
        "Logo",
        "Thumb",
        "Disc",
        "Box",
        "Screenshot",
        "Menu",
        "Chapter",
    ];
    let type_options = [
        ("Movie", true),
        ("Series", true),
        ("Season", true),
        ("Episode", true),
        ("MusicAlbum", true),
        ("Audio", true),
        ("MusicArtist", true),
        ("BoxSet", true),
        ("Playlist", false),
        ("Photo", false),
    ]
    .into_iter()
    .map(|(item_type, metadata)| {
        json!({
            "Type": item_type,
            "MetadataFetchers": if metadata { vec![tmdb.clone()] } else { Vec::new() },
            "ImageFetchers": if metadata { vec![tmdb.clone()] } else { Vec::new() },
            "SimilarItemProviders": [],
            "SupportedImageTypes": image_types,
            "DefaultImageOptions": []
        })
    })
    .collect::<Vec<_>>();

    json!({
        "MetadataSavers": [local],
        "MetadataReaders": [option_info("Nfo", true)],
        "SubtitleFetchers": [],
        "LyricFetchers": [],
        "MediaSegmentProviders": [],
        "TypeOptions": type_options
    })
}

fn option_info(name: &str, default_enabled: bool) -> Value {
    json!({ "Name": name, "DefaultEnabled": default_enabled })
}

fn query_result(items: Vec<Value>) -> Value {
    json!({
        "Items": items,
        "TotalRecordCount": items.len(),
        "StartIndex": 0
    })
}

#[cfg(test)]
mod tests {
    use super::{
        VirtualFolderQuery, delete_virtual_folder_inner, delete_virtual_folder_path_inner,
        library_options_result, library_path_request, query_result, query_string,
        rename_virtual_folder_inner, rename_virtual_folder_request, update_library_options_inner,
        update_virtual_folder_path_inner, update_virtual_folder_path_request,
        virtual_folder_request,
    };
    use crate::db::row_ext::QueryResultExt;
    use axum::body::Bytes;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use std::collections::HashMap;

    #[test]
    fn library_options_result_has_type_options() {
        let result = library_options_result();
        assert!(
            result["MetadataReaders"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(result["TypeOptions"].as_array().is_some_and(|v| {
            v.iter().any(|item| item["Type"] == "Movie")
                && v.iter().any(|item| item["Type"] == "Series")
                && v.iter().any(|item| item["Type"] == "Audio")
        }));
    }

    #[test]
    fn library_query_results_include_start_index() {
        let result = query_result(vec![serde_json::json!({ "Name": "Movies" })]);
        assert_eq!(result["TotalRecordCount"], 1);
        assert_eq!(result["StartIndex"], 0);
        assert_eq!(result["Items"][0]["Name"], "Movies");
    }

    #[test]
    fn virtual_folder_requests_accept_json_body_shapes() {
        let request = virtual_folder_request(
            VirtualFolderQuery {
                name: None,
                collection_type: None,
                paths: None,
            },
            Bytes::from_static(
                br#"{"Name":"Movies","CollectionType":"movies","PathInfo":{"Path":"D:/Media"}}"#,
            ),
        )
        .unwrap();
        assert_eq!(request.name.as_deref(), Some("Movies"));
        assert_eq!(request.collection_type.as_deref(), Some("movies"));
        assert_eq!(request.paths.as_deref(), Some("D:/Media"));

        let path = library_path_request(
            HashMap::new(),
            Bytes::from_static(br#"{"Name":"Movies","PathInfo":{"Path":"D:/Media"}}"#),
        )
        .unwrap();
        assert_eq!(path.name, "Movies");
        assert_eq!(path.path, "D:/Media");
    }

    #[test]
    fn virtual_folder_change_requests_accept_json_body_shapes() {
        assert_eq!(
            query_string(
                &HashMap::from([("Name".to_string(), "Movies".to_string())]),
                &["name", "Name"],
            ),
            "Movies"
        );

        let (name, new_name) = rename_virtual_folder_request(
            &HashMap::new(),
            Bytes::from_static(br#"{"Name":"Movies","NewName":"Films"}"#),
        )
        .unwrap();
        assert_eq!(name, "Movies");
        assert_eq!(new_name, "Films");

        let (name, path, new_path) = update_virtual_folder_path_request(
            &HashMap::from([("Path".to_string(), "D:/Old".to_string())]),
            Bytes::from_static(br#"{"Name":"Movies","PathInfo":{"Path":"D:/New"}}"#),
        )
        .unwrap();
        assert_eq!(name, "Movies");
        assert_eq!(path, "D:/Old");
        assert_eq!(new_path, "D:/New");
    }

    #[tokio::test]
    async fn virtual_folder_changes_report_missing_targets() {
        let db = test_db().await;
        let dir = temp_media_dir("missing-targets");
        std::fs::create_dir_all(&dir).unwrap();

        assert!(
            !rename_virtual_folder_inner(&db, "Missing", "New")
                .await
                .unwrap()
        );
        assert!(
            !delete_virtual_folder_path_inner(&db, "Movies", &dir.to_string_lossy())
                .await
                .unwrap()
        );
        assert!(!delete_virtual_folder_inner(&db, "Missing").await.unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn library_options_are_persisted_for_existing_library() {
        let db = test_db().await;
        insert_library(&db, "movies", "Movies").await;

        assert!(
            update_library_options_inner(
                &db,
                &serde_json::json!({
                    "Name": "Movies",
                    "LibraryOptions": { "EnableRealtimeMonitor": false }
                }),
            )
            .await
            .unwrap()
        );
        let saved = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT value FROM app_settings WHERE key = ?",
                vec!["LibraryOptions.movies".into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .get_str("value")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&saved).unwrap()["EnableRealtimeMonitor"],
            false
        );

        assert!(
            !update_library_options_inner(
                &db,
                &serde_json::json!({
                    "Name": "Missing",
                    "LibraryOptions": {}
                }),
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn virtual_folder_path_update_validates_and_updates_path() {
        let db = test_db().await;
        let old_dir = temp_media_dir("old");
        let new_dir = temp_media_dir("new");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        insert_library(&db, "movies", "Movies").await;
        insert_library_path(&db, "movies", &old_dir.to_string_lossy()).await;

        assert!(
            update_virtual_folder_path_inner(
                &db,
                "Movies",
                &old_dir.to_string_lossy(),
                &new_dir.to_string_lossy(),
            )
            .await
            .unwrap()
        );
        assert!(
            !delete_virtual_folder_path_inner(&db, "Movies", &old_dir.to_string_lossy())
                .await
                .unwrap()
        );
        assert!(
            delete_virtual_folder_path_inner(&db, "Movies", &new_dir.to_string_lossy())
                .await
                .unwrap()
        );

        std::fs::remove_dir_all(&old_dir).unwrap();
        std::fs::remove_dir_all(&new_dir).unwrap();
    }

    #[tokio::test]
    async fn delete_virtual_folder_removes_items_and_library() {
        let db = test_db().await;
        let dir = temp_media_dir("delete");
        std::fs::create_dir_all(&dir).unwrap();
        insert_library(&db, "movies", "Movies").await;
        insert_library_path(&db, "movies", &dir.to_string_lossy()).await;
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO media_items (id, title, path, library_id, parent_id, item_type, is_folder, modified_at, created_at, updated_at) VALUES (?, ?, ?, 'movies', '', 'Movie', 0, 1, 1, 1)",
            vec![
                "m1".into(),
                "Movie".into(),
                dir.join("movie.mkv").to_string_lossy().to_string().into(),
            ],
        ))
        .await
        .unwrap();

        assert!(delete_virtual_folder_inner(&db, "Movies").await.unwrap());
        let remaining = db
            .query_one(crate::db::helpers::portable_statement(
                db.get_database_backend(),
                "SELECT id FROM media_items WHERE id = ?",
                vec!["m1".into()],
            ))
            .await
            .unwrap();
        assert!(remaining.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }
    async fn test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&db, "sqlite::memory:").await.unwrap();
        db
    }

    async fn insert_library(db: &DatabaseConnection, id: &str, name: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', 1, 1)",
            vec![id.into(), name.into()],
        ))
        .await
        .unwrap();
    }

    async fn insert_library_path(db: &DatabaseConnection, library_id: &str, path: &str) {
        db.execute(crate::db::helpers::portable_statement(
            db.get_database_backend(),
            "INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, 1)",
            vec![
                crate::util::stable_text_id(&format!("library-path:{path}")).into(),
                library_id.into(),
                crate::library::path_utils::canonicalize_path(path)
                    .unwrap()
                    .into(),
            ],
        ))
        .await
        .unwrap();
    }

    fn temp_media_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "jellyfin-rs-library-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }
}
