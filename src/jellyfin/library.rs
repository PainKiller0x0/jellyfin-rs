use std::sync::Arc;

use anyhow::{Context, bail};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{AnyPool, Row};

use crate::{
    app::state::AppState,
    jellyfin::common::internal_error,
    jellyfin::system,
    library::scanner::scan_media_library,
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
    match upsert_library_path(&state.db, &query.name, &query.path).await {
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
    match sqlx::query("DELETE FROM library_paths WHERE library_id = ? AND path = ?")
        .bind(library_id_for_name(&query.name))
        .bind(query.path.trim())
        .execute(&state.db)
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
        &state.db,
        "scan-library",
        status,
        start,
        end,
        message.as_deref(),
    )
    .await;
    system::log_activity(&state.db, "Library scan", "LibraryScan", None, None).await;
    match result {
        Ok(scanned) => Json(json!({ "Scanned": scanned })).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn virtual_folders_inner(db: &AnyPool) -> anyhow::Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"SELECT libraries.id, libraries.name, libraries.collection_type, library_paths.path FROM libraries LEFT JOIN library_paths ON library_paths.library_id = libraries.id ORDER BY libraries.name ASC, library_paths.path ASC"#,
    )
    .fetch_all(db)
    .await
    .context("failed to list virtual folders")?;

    let mut folders = Vec::<VirtualFolder>::new();
    for row in rows {
        let id: String = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let collection_type: String = row.try_get("collection_type")?;
        let path: Option<String> = row.try_get("path")?;

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
    db: &AnyPool,
    query: VirtualFolderQuery,
) -> anyhow::Result<()> {
    let name = query.name.as_deref().unwrap_or_default().trim();
    if name.is_empty() {
        bail!("name is required");
    }

    let collection_type = query.collection_type.as_deref().unwrap_or("movies").trim();
    let library_id = library_id_for_name(name);
    let now = now_unix();
    sqlx::query(r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, collection_type = excluded.collection_type, updated_at = excluded.updated_at"#)
        .bind(&library_id)
        .bind(name)
        .bind(collection_type)
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .context("failed to create virtual folder")?;

    if let Some(paths) = query.paths {
        for path in paths.split('|').flat_map(|value| value.split(',')) {
            let path = path.trim();
            if !path.is_empty() {
                upsert_library_path(db, name, path).await?;
            }
        }
    }

    Ok(())
}

async fn upsert_library_path(db: &AnyPool, name: &str, path: &str) -> anyhow::Result<()> {
    let name = name.trim();
    let path = path.trim();
    if name.is_empty() {
        bail!("name is required");
    }
    if path.is_empty() {
        bail!("path is required");
    }

    let library_id = library_id_for_name(name);
    let now = now_unix();
    sqlx::query(r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at"#)
        .bind(&library_id)
        .bind(name)
        .bind("movies")
        .bind(now)
        .bind(now)
        .execute(db)
        .await
        .context("failed to ensure library")?;

    sqlx::query(r#"INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET library_id = excluded.library_id"#)
        .bind(stable_text_id(&format!("library-path:{path}")))
        .bind(library_id)
        .bind(path)
        .bind(now)
        .execute(db)
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

struct VirtualFolder {
    id: String,
    name: String,
    collection_type: String,
    paths: Vec<String>,
}
