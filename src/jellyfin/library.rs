use std::sync::Arc;

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::state::AppState,
    db::row_ext::QueryResultExt,
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
    let backend = state.db.get_database_backend();
    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            "DELETE FROM library_paths WHERE library_id = ? AND path = ?",
            vec![
                library_id_for_name(&query.name).into(),
                path_utils::normalize_path(&query.path).into(),
            ],
        ))
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => internal_error(error.into()),
    }
}

pub async fn refresh_library(State(state): State<Arc<AppState>>) -> Response {
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
    match result {
        Ok(scanned) => Json(json!({ "Scanned": scanned })).into_response(),
        Err(error) => internal_error(error),
    }
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
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, collection_type = excluded.collection_type, updated_at = excluded.updated_at"#,
        vec![
            library_id.into(),
            name.into(),
            collection_type.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to create virtual folder")?;

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
    let backend = db.get_database_backend();

    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at"#,
        vec![
            library_id.clone().into(),
            name.into(),
            collection_type.into(),
            now.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to ensure library")?;

    db.execute(crate::db::helpers::portable_statement(
        backend,
        r#"INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET library_id = excluded.library_id"#,
        vec![
            stable_text_id(&format!("library-path:{path}")).into(),
            library_id.into(),
            path.into(),
            now.into(),
        ],
    ))
    .await
    .context("failed to upsert library path")?;

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
    let backend = state.db.get_database_backend();
    match state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT path FROM library_paths ORDER BY path ASC",
            vec![],
        ))
        .await
    {
        Ok(rows) => Json(
            rows.iter()
                .filter_map(|row| row.get_str("path").ok())
                .collect::<Vec<_>>(),
        )
        .into_response(),
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

struct VirtualFolder {
    id: String,
    name: String,
    collection_type: String,
    paths: Vec<String>,
}
