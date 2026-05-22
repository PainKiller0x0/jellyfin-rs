use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Json,
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
    #[serde(rename = "name")]
    name: Option<String>,
    #[serde(rename = "collectionType")]
    collection_type: Option<String>,
    #[serde(rename = "paths")]
    paths: Option<String>,
}

#[derive(Deserialize)]
pub struct LibraryPathQuery {
    #[serde(rename = "name")]
    name: String,
    #[serde(rename = "path")]
    path: String,
}

pub async fn virtual_folders(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => Json(folders).into_response(),
        Err(error) => internal_error(error),
    }
}

pub async fn virtual_folders_query(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => {
            let total = folders.len();
            Json(json!({ "Items": folders, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn media_folders(State(state): State<Arc<AppState>>) -> Response {
    match virtual_folders_inner(&state.db).await {
        Ok(folders) => {
            let total = folders.len();
            Json(json!({ "Items": folders, "TotalRecordCount": total })).into_response()
        }
        Err(error) => internal_error(error),
    }
}

pub async fn create_virtual_folder(
    State(state): State<Arc<AppState>>,
    Query(query): Query<VirtualFolderQuery>,
) -> Response {
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

/// DELETE /Library/VirtualFolders — delete a virtual folder
pub async fn delete_virtual_folder(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query.get("name").map(String::as_str).unwrap_or_default().trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name is required" })),
        )
            .into_response();
    }

    let library_id = library_id_for_name(name);
    let backend = state.db.get_database_backend();

    // Delete library paths first (foreign key)
    let _ = state.db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM library_paths WHERE library_id = ?",
            vec![library_id.clone().into()],
        ))
        .await;

    // Delete the library
    match state.db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM libraries WHERE id = ?",
            vec![library_id.into()],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn add_virtual_folder_path(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LibraryPathQuery>,
) -> Response {
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
    Query(query): Query<LibraryPathQuery>,
) -> Response {
    match LibraryPaths::delete_many()
        .filter(library_paths::Column::LibraryId.eq(library_id_for_name(&query.name)))
        .filter(library_paths::Column::Path.eq(path_utils::normalize_path(&query.path)))
        .exec(&state.db)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
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
) -> Response {
    let name = query.get("name").map(String::as_str).unwrap_or_default().trim();
    let new_name = query.get("newName").map(String::as_str).unwrap_or_default().trim();

    if name.is_empty() || new_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "name and newName are required" })),
        )
            .into_response();
    }

    let library_id = library_id_for_name(name);
    let now = now_unix();
    let backend = state.db.get_database_backend();

    match state.db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE libraries SET name = ?, updated_at = ? WHERE id = ?",
            vec![new_name.into(), now.into(), library_id.into()],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

/// POST /Library/VirtualFolders/LibraryOptions — update library options
pub async fn update_library_options(
    State(_state): State<Arc<AppState>>,
    Json(_body): Json<Value>,
) -> Response {
    // Accept the options but most are not stored (we don't have a library_options table)
    // Just return success - the options are client-side preferences
    StatusCode::NO_CONTENT.into_response()
}

/// POST /Library/VirtualFolders/Paths/Update — update a library path
pub async fn update_virtual_folder_path(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query.get("name").map(String::as_str).unwrap_or_default().trim();
    let path = query.get("path").map(String::as_str).unwrap_or_default().trim();
    let new_path = query.get("newPath").map(String::as_str).unwrap_or_default().trim();

    if name.is_empty() || path.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let target_path = if !new_path.is_empty() { new_path } else { path };
    let library_id = library_id_for_name(name);
    let now = now_unix();
    let backend = state.db.get_database_backend();

    // Update the path in library_paths
    let path_id = crate::util::stable_text_id(&format!("library-path:{path}"));
    let _ = state.db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "UPDATE library_paths SET path = ?, library_id = ? WHERE id = ?",
            vec![target_path.into(), library_id.into(), path_id.into()],
        ))
        .await;

    StatusCode::NO_CONTENT.into_response()
}

/// POST /Library/SelectableMediaFolders — get selectable media folders
pub async fn selectable_media_folders(
    State(state): State<Arc<AppState>>,
) -> Response {
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
