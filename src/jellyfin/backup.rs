use std::{path::PathBuf, sync::Arc};

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sea_orm::ConnectionTrait;
use serde_json::json;

use crate::{app::state::AppState, jellyfin::common::internal_error};

fn backup_dir() -> PathBuf {
    PathBuf::from("data").join("backups")
}

fn database_url() -> String {
    std::env::var("JELLYFIN_RS_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://jellyfin-rs.db".to_string())
}

fn is_postgres() -> bool {
    let url = database_url();
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

fn sqlite_file_path() -> Option<PathBuf> {
    let url = database_url();
    let path = url.strip_prefix("sqlite://").unwrap_or(&url);
    let path = path.trim_start_matches("./");
    Some(PathBuf::from(path))
}

/// GET /BackupRestore/BackupInfo
pub async fn backup_info(State(_state): State<Arc<AppState>>) -> Response {
    let backup_dir = backup_dir();
    let backup_ext = if is_postgres() { "sql" } else { "db" };
    let backup_filename = format!("jellyfin-rs-backup.{backup_ext}");
    let backup_path = backup_dir.join(&backup_filename);

    let (db_size, db_location) = if is_postgres() {
        let location = database_url()
            .split('@')
            .last()
            .unwrap_or("PostgreSQL")
            .to_string();
        (0u64, location)
    } else {
        let path = sqlite_file_path().unwrap_or_else(|| PathBuf::from("jellyfin-rs.db"));
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        (size, path.to_string_lossy().to_string())
    };

    let (last_backup, backup_size) = if backup_path.exists() {
        let meta = std::fs::metadata(&backup_path).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| crate::util::unix_to_jellyfin_date(d.as_secs() as i64));
        (modified, size)
    } else {
        (None, 0)
    };

    Json(json!({
        "IsFileBackup": true,
        "DatabaseType": if is_postgres() { "PostgreSQL" } else { "SQLite" },
        "DatabaseLocation": db_location,
        "DatabaseSize": db_size,
        "BackupPath": backup_path.to_string_lossy(),
        "LastBackupDate": last_backup,
        "BackupSize": backup_size,
    }))
    .into_response()
}

/// POST /BackupRestore/Restore — create a backup
pub async fn create_backup(State(state): State<Arc<AppState>>) -> Response {
    let backup_dir = backup_dir();
    if let Err(error) = tokio::fs::create_dir_all(&backup_dir).await {
        return internal_error(error.into());
    }

    if is_postgres() {
        create_pg_backup(&state, &backup_dir).await
    } else {
        create_sqlite_backup(&state, &backup_dir).await
    }
}

async fn create_sqlite_backup(state: &AppState, backup_dir: &PathBuf) -> Response {
    let Some(db_path) = sqlite_file_path() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Cannot determine SQLite file path" })),
        )
            .into_response();
    };

    if !db_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "Database file not found" })),
        )
            .into_response();
    }

    let backup_path = backup_dir.join("jellyfin-rs-backup.db");

    let backend = state.db.get_database_backend();
    let vacuum_sql = format!(
        "VACUUM INTO '{}'",
        backup_path.to_string_lossy().replace('\'', "''")
    );

    match state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            &vacuum_sql,
            vec![],
        ))
        .await
    {
        Ok(_) => {
            let size = std::fs::metadata(&backup_path)
                .map(|m| m.len())
                .unwrap_or(0);
            Json(json!({
                "Success": true,
                "Method": "vacuum_into",
                "BackupPath": backup_path.to_string_lossy(),
                "BackupSize": size,
            }))
            .into_response()
        }
        Err(_) => match tokio::fs::copy(&db_path, &backup_path).await {
            Ok(size) => Json(json!({
                "Success": true,
                "Method": "file_copy",
                "BackupPath": backup_path.to_string_lossy(),
                "BackupSize": size,
            }))
            .into_response(),
            Err(error) => internal_error(error.into()),
        },
    }
}

async fn create_pg_backup(state: &AppState, backup_dir: &PathBuf) -> Response {
    let backup_path = backup_dir.join("jellyfin-rs-backup.sql");

    // Get all user tables from information_schema
    let backend = state.db.get_database_backend();
    let tables = match state
        .db
        .query_all(crate::db::helpers::portable_statement(
            backend,
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE' ORDER BY table_name",
            vec![],
        ))
        .await
    {
        Ok(rows) => rows,
        Err(error) => return internal_error(error.into()),
    };

    let mut sql_output = String::new();
    sql_output.push_str("-- jellyfin-rs database backup\n");
    sql_output.push_str("-- Generated by jellyfin-rs backup API\n\n");
    sql_output.push_str("BEGIN;\n\n");

    for table_row in &tables {
        let table_name: String = match crate::db::row_ext::QueryResultExt::get_str(table_row, "table_name") {
            Ok(name) => name,
            Err(_) => continue,
        };

        // Get column names
        let columns = match state
            .db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                "SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
                vec![table_name.clone().into()],
            ))
            .await
        {
            Ok(cols) => cols,
            Err(_) => continue,
        };

        let col_names: Vec<String> = columns
            .iter()
            .filter_map(|c| crate::db::row_ext::QueryResultExt::get_str(c, "column_name").ok())
            .collect();

        if col_names.is_empty() {
            continue;
        }

        // Get row count
        let count_sql = format!("SELECT COUNT(*) as cnt FROM \"{}\"", table_name.replace('"', "\"\""));
        let count = match state
            .db
            .query_one(crate::db::helpers::portable_statement(
                backend,
                &count_sql,
                vec![],
            ))
            .await
        {
            Ok(Some(row)) => crate::db::row_ext::QueryResultExt::get_i64(&row, "cnt").unwrap_or(0),
            _ => 0,
        };

        if count == 0 {
            sql_output.push_str(&format!("-- Table: {} (empty)\n\n", table_name));
            continue;
        }

        sql_output.push_str(&format!("-- Table: {} ({} rows)\n", table_name, count));

        // Export data using COPY TO STDOUT equivalent - fetch all rows
        let select_sql = format!(
            "SELECT {} FROM \"{}\"",
            col_names
                .iter()
                .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(", "),
            table_name.replace('"', "\"\"")
        );

        let rows = match state
            .db
            .query_all(crate::db::helpers::portable_statement(
                backend,
                &select_sql,
                vec![],
            ))
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };

        let cols_str = col_names
            .iter()
            .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(", ");

        for row in &rows {
            let mut values = Vec::new();
            for col in &col_names {
                // Try different types using row_ext helpers
                let value_str = if let Ok(v) = crate::db::row_ext::QueryResultExt::get_opt_str(row, col) {
                    match v {
                        Some(s) => format!("'{}'", s.replace('\'', "''")),
                        None => "NULL".to_string(),
                    }
                } else if let Ok(v) = crate::db::row_ext::QueryResultExt::get_opt_i64(row, col) {
                    match v {
                        Some(n) => n.to_string(),
                        None => "NULL".to_string(),
                    }
                } else {
                    "NULL".to_string()
                };
                values.push(value_str);
            }
            sql_output.push_str(&format!(
                "INSERT INTO \"{}\" ({}) VALUES ({});\n",
                table_name.replace('"', "\"\""),
                cols_str,
                values.join(", ")
            ));
        }
        sql_output.push('\n');
    }

    sql_output.push_str("COMMIT;\n");

    match tokio::fs::write(&backup_path, &sql_output).await {
        Ok(()) => {
            let size = std::fs::metadata(&backup_path)
                .map(|m| m.len())
                .unwrap_or(0);
            Json(json!({
                "Success": true,
                "Method": "sql_export",
                "BackupPath": backup_path.to_string_lossy(),
                "BackupSize": size,
            }))
            .into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

/// POST /BackupRestore/RestoreData — restore from backup
pub async fn restore_backup(State(_state): State<Arc<AppState>>) -> Response {
    let backup_dir = backup_dir();

    if is_postgres() {
        restore_pg_backup(&backup_dir).await
    } else {
        restore_sqlite_backup(&backup_dir).await
    }
}

async fn restore_sqlite_backup(backup_dir: &PathBuf) -> Response {
    let Some(db_path) = sqlite_file_path() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "Error": "Cannot determine SQLite file path" })),
        )
            .into_response();
    };

    let backup_path = backup_dir.join("jellyfin-rs-backup.db");
    if !backup_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "No backup file found" })),
        )
            .into_response();
    }

    match tokio::fs::copy(&backup_path, &db_path).await {
        Ok(_) => {
            tracing::info!("SQLite database restored from backup; server restart required");
            Json(json!({
                "Success": true,
                "Message": "Database restored. Server restart required to apply changes.",
                "RequiresRestart": true,
            }))
            .into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

async fn restore_pg_backup(backup_dir: &PathBuf) -> Response {
    let backup_path = backup_dir.join("jellyfin-rs-backup.sql");
    if !backup_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "Error": "No backup file found" })),
        )
            .into_response();
    }

    // Note: SQL restore requires manual execution via psql or similar tool
    // We return the path and instructions
    Json(json!({
        "Success": true,
        "Method": "sql_file",
        "BackupPath": backup_path.to_string_lossy(),
        "Message": "SQL backup file ready. To restore, execute the SQL file against your PostgreSQL database using psql or a database client.",
        "RestoreCommand": format!("psql -h <host> -U <user> -d <database> -f {}", backup_path.to_string_lossy()),
    }))
    .into_response()
}
