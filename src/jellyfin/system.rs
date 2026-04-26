use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use serde_json::{Value, json};
use sqlx::Row;

use crate::{
    app::state::{AppState, SERVER_NAME, VERSION},
    jellyfin::common::internal_error,
    util::now_unix,
};

pub async fn system_info() -> impl IntoResponse {
    Json(json!({
        "ServerName": SERVER_NAME,
        "Version": VERSION,
        "LocalAddress": "http://127.0.0.1:8096",
        "WanAddress": "http://127.0.0.1:8096",
        "OperatingSystem": std::env::consts::OS,
        "HasUpdateAvailable": false,
    }))
}

pub async fn public_system_info() -> impl IntoResponse {
    Json(json!({
        "ServerName": SERVER_NAME,
        "Version": VERSION,
        "Id": "jellyfin-rs",
    }))
}

pub async fn activity_log(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("Limit")
        .or_else(|| query.get("limit"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(15)
        .clamp(1, 100);
    let start_index = query
        .get("StartIndex")
        .or_else(|| query.get("startIndex"))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let has_user_id = query
        .get("hasUserId")
        .or_else(|| query.get("HasUserId"))
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));

    let sql = if has_user_id {
        "SELECT name, log_type, created_at, user_id FROM activity_log WHERE user_id IS NOT NULL ORDER BY created_at DESC LIMIT ? OFFSET ?"
            .to_string()
    } else {
        "SELECT name, log_type, created_at, user_id FROM activity_log ORDER BY created_at DESC LIMIT ? OFFSET ?"
            .to_string()
    };

    match sqlx::query(&sql)
        .bind(limit)
        .bind(start_index)
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let created_at: i64 = row.try_get("created_at").unwrap_or_default();
                    json!({
                        "Name": row.try_get::<String, _>("name").unwrap_or_default(),
                        "Type": row.try_get::<String, _>("log_type").unwrap_or_default(),
                        "Date": crate::util::unix_to_jellyfin_date(created_at),
                        "UserId": row.try_get::<Option<String>, _>("user_id").unwrap_or_default(),
                        "Severity": "Info",
                    })
                })
                .collect();
            Json(json!({ "Items": items, "TotalRecordCount": items.len() })).into_response()
        }
        Err(error) => internal_error(error.into()),
    }
}

type Response = axum::response::Response;

pub async fn scheduled_tasks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let scan_result = last_task_result(&state.db, "scan-library").await;
    let task = json!({
        "Name": "Scan media library",
        "State": "Idle",
        "Id": "scan-library",
        "Description": "Scans configured media library paths for new and updated media files.",
        "Category": "Library",
        "IsHidden": false,
        "LastExecutionResult": scan_result,
    });
    Json(vec![task])
}

async fn last_task_result(db: &sqlx::AnyPool, task_id: &str) -> Option<Value> {
    let row = sqlx::query(
        "SELECT status, start_time, end_time, message FROM task_results WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .ok()??;
    let status: String = row.try_get("status").ok()?;
    let start_time: Option<i64> = row.try_get("start_time").ok().flatten();
    let end_time: Option<i64> = row.try_get("end_time").ok().flatten();
    Some(json!({
        "Status": status,
        "StartTimeUtc": start_time.map(crate::util::unix_to_jellyfin_date),
        "EndTimeUtc": end_time.map(crate::util::unix_to_jellyfin_date),
        "Message": row.try_get::<Option<String>, _>("message").ok().flatten(),
    }))
}

pub async fn log_activity(
    db: &sqlx::AnyPool,
    name: &str,
    log_type: &str,
    user_id: Option<&str>,
    item_id: Option<&str>,
) {
    let now = now_unix();
    let id = crate::util::stable_text_id(&format!("activity:{now}:{name}:{log_type}"));
    let _ = sqlx::query(
        "INSERT INTO activity_log (id, name, log_type, user_id, item_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(log_type)
    .bind(user_id)
    .bind(item_id)
    .bind(now)
    .execute(db)
    .await;
}

pub async fn upsert_task_result(
    db: &sqlx::AnyPool,
    task_id: &str,
    status: &str,
    start_time: i64,
    end_time: i64,
    message: Option<&str>,
) {
    let _ = sqlx::query(
        "INSERT INTO task_results (task_id, status, start_time, end_time, message) VALUES (?, ?, ?, ?, ?) ON CONFLICT(task_id) DO UPDATE SET status = excluded.status, start_time = excluded.start_time, end_time = excluded.end_time, message = excluded.message",
    )
    .bind(task_id)
    .bind(status)
    .bind(start_time)
    .bind(end_time)
    .bind(message)
    .execute(db)
    .await;
}

pub async fn shutdown_handler() -> Response {
    tracing::info!("shutdown requested; exiting in 1 second");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        std::process::exit(0);
    });
    axum::http::StatusCode::NO_CONTENT.into_response()
}
