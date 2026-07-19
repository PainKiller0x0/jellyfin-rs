use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::app::state::{AppState, PlaybackSession, session_timeout_seconds};
use crate::db::row_ext::QueryResultExt;
use crate::jellyfin::auth::request_token;
use crate::util::now_unix;

#[derive(Debug, Clone)]
pub enum WsEvent {
    SessionsChanged,
    ActivityCreated,
    TaskUpdated,
}

#[derive(Debug, Clone, Deserialize)]
struct IncomingMessage {
    #[serde(rename = "MessageType")]
    message_type: String,
    #[serde(rename = "Data")]
    data: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct OutgoingMessage {
    #[serde(rename = "MessageType")]
    message_type: String,
    #[serde(rename = "MessageId")]
    message_id: String,
    #[serde(rename = "Data", skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug)]
struct Subscription {
    interval_ms: u64,
    last_send: Instant,
}

struct ConnectionState {
    sender: Arc<RwLock<tokio::sync::mpsc::UnboundedSender<String>>>,
    subscriptions: HashMap<String, Subscription>,
}

const WS_LOST_TIMEOUT: u64 = 60;
const FORCE_KEEPALIVE_FACTOR: f32 = 0.75;

pub async fn ws_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = request_token(&headers, &query);
    ws.on_upgrade(move |socket| handle_socket(socket, state, headers, token))
}

async fn handle_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    _headers: HeaderMap,
    _token: Option<String>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sender = Arc::new(RwLock::new(tx));

    let connection_state = Arc::new(RwLock::new(ConnectionState {
        sender: sender.clone(),
        subscriptions: HashMap::new(),
    }));

    let mut event_rx = state.ws_event_tx.subscribe();
    let mut keepalive_timer = tokio::time::interval(tokio::time::Duration::from_secs(
        (WS_LOST_TIMEOUT as f32 * 0.2) as u64,
    ));
    let mut last_keepalive = Instant::now();

    let state_for_receiver = state.clone();
    let connection_for_receiver = connection_state.clone();
    let _sender_for_receiver = sender.clone();
    let mut event_rx_for_receiver = state.ws_event_tx.subscribe();

    tokio::spawn(async move {
        while let Ok(event) = event_rx_for_receiver.recv().await {
            tracing::debug!("WS event received: {event:?}");
            let conn = connection_for_receiver.read().await;
            match event {
                WsEvent::SessionsChanged => {
                    if conn.subscriptions.contains_key("Sessions") {
                        let data = build_sessions_data(&state_for_receiver).await;
                        send_message(&conn.sender, "Sessions", data).await;
                    }
                }
                WsEvent::ActivityCreated => {
                    if conn.subscriptions.contains_key("ActivityLogEntry") {
                        let data = build_activity_data(&state_for_receiver).await;
                        send_message(&conn.sender, "ActivityLogEntry", data).await;
                    }
                }
                WsEvent::TaskUpdated => {
                    if conn.subscriptions.contains_key("ScheduledTasksInfo") {
                        let data = build_tasks_data(&state_for_receiver).await;
                        send_message(&conn.sender, "ScheduledTasksInfo", data).await;
                    }
                }
            }
        }
    });

    let mut periodic_timer = tokio::time::interval(tokio::time::Duration::from_millis(1000));
    let state_for_periodic = state.clone();
    let connection_for_periodic = connection_state.clone();

    tokio::spawn(async move {
        loop {
            periodic_timer.tick().await;
            let now = Instant::now();
            let to_send: Vec<String> = {
                let conn = connection_for_periodic.read().await;
                conn.subscriptions
                    .iter()
                    .filter(|(_, sub)| {
                        now.duration_since(sub.last_send).as_millis() as u64 >= sub.interval_ms
                    })
                    .map(|(msg_type, _)| msg_type.clone())
                    .collect()
            };
            for msg_type in &to_send {
                let data = match msg_type.as_str() {
                    "Sessions" => build_sessions_data(&state_for_periodic).await,
                    "ActivityLogEntry" => build_activity_data(&state_for_periodic).await,
                    "ScheduledTasksInfo" => build_tasks_data(&state_for_periodic).await,
                    _ => continue,
                };
                {
                    let conn = connection_for_periodic.read().await;
                    send_message(&conn.sender, msg_type, data).await;
                }
                let mut conn = connection_for_periodic.write().await;
                if let Some(sub) = conn.subscriptions.get_mut(msg_type) {
                    sub.last_send = Instant::now();
                }
            }
        }
    });

    let (mut sender_for_ws, mut receiver) = socket.split();
    let sender_clone = sender.clone();

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sender_for_ws.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        last_keepalive = Instant::now();
                        if let Ok(incoming) = serde_json::from_str::<IncomingMessage>(&text) {
                            handle_client_message(
                                &state,
                                &connection_state,
                                &incoming,
                            )
                            .await;
                        }
                    }
                    Some(Ok(Message::Ping(_data))) => {
                        let _ = sender_clone
                            .write()
                            .await
                            .send(serde_json::to_string(&OutgoingMessage {
                                message_type: "KeepAlive".to_string(),
                                message_id: Uuid::new_v4().to_string(),
                                data: None,
                            })
                            .unwrap_or_default());
                        last_keepalive = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    _ => {}
                }
            }
            Ok(event) = event_rx.recv() => {
                // Already handled in the spawned task above
                let _ = event;
            }
            _ = keepalive_timer.tick() => {
                let elapsed = last_keepalive.elapsed().as_secs();
                if elapsed >= WS_LOST_TIMEOUT {
                    tracing::info!("WS connection lost (no keepalive for {elapsed}s)");
                    break;
                }
                if elapsed as f32 >= WS_LOST_TIMEOUT as f32 * FORCE_KEEPALIVE_FACTOR {
                    let msg = serde_json::to_string(&OutgoingMessage {
                        message_type: "ForceKeepAlive".to_string(),
                        message_id: Uuid::new_v4().to_string(),
                        data: Some(Value::Number((WS_LOST_TIMEOUT as i64).into())),
                    })
                    .unwrap_or_default();
                    let _ = sender_clone.write().await.send(msg);
                }
            }
        }
    }

    tracing::debug!("WebSocket connection closed");
}

async fn handle_client_message(
    _state: &AppState,
    connection_state: &Arc<RwLock<ConnectionState>>,
    msg: &IncomingMessage,
) {
    match msg.message_type.as_str() {
        "KeepAlive" => {
            let conn = connection_state.write().await;
            let msg = serde_json::to_string(&OutgoingMessage {
                message_type: "KeepAlive".to_string(),
                message_id: Uuid::new_v4().to_string(),
                data: None,
            })
            .unwrap_or_default();
            let _ = conn.sender.write().await.send(msg);
        }
        "SessionsStart" => {
            let timing = parse_timing(msg.data.as_deref().unwrap_or("0,5000"));
            let mut conn = connection_state.write().await;
            conn.subscriptions.insert(
                "Sessions".to_string(),
                Subscription {
                    interval_ms: timing.1,
                    last_send: Instant::now() - tokio::time::Duration::from_millis(timing.0),
                },
            );
        }
        "SessionsStop" => {
            connection_state
                .write()
                .await
                .subscriptions
                .remove("Sessions");
        }
        "ActivityLogEntryStart" => {
            let timing = parse_timing(msg.data.as_deref().unwrap_or("0,5000"));
            let mut conn = connection_state.write().await;
            conn.subscriptions.insert(
                "ActivityLogEntry".to_string(),
                Subscription {
                    interval_ms: timing.1,
                    last_send: Instant::now() - tokio::time::Duration::from_millis(timing.0),
                },
            );
        }
        "ActivityLogEntryStop" => {
            connection_state
                .write()
                .await
                .subscriptions
                .remove("ActivityLogEntry");
        }
        "ScheduledTasksInfoStart" => {
            let timing = parse_timing(msg.data.as_deref().unwrap_or("0,5000"));
            let mut conn = connection_state.write().await;
            conn.subscriptions.insert(
                "ScheduledTasksInfo".to_string(),
                Subscription {
                    interval_ms: timing.1,
                    last_send: Instant::now() - tokio::time::Duration::from_millis(timing.0),
                },
            );
        }
        "ScheduledTasksInfoStop" => {
            connection_state
                .write()
                .await
                .subscriptions
                .remove("ScheduledTasksInfo");
        }
        "Sessions" | "ActivityLogEntry" | "ScheduledTasksInfo" => {}
        _ => {
            tracing::debug!("Unhandled WS message type: {}", msg.message_type);
        }
    }
}

fn parse_timing(data: &str) -> (u64, u64) {
    let parts: Vec<&str> = data.split(',').collect();
    let delay = parts
        .first()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let interval = parts
        .get(1)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5000)
        .max(500);
    (delay, interval)
}

async fn send_message(
    sender: &Arc<RwLock<tokio::sync::mpsc::UnboundedSender<String>>>,
    msg_type: &str,
    data: Value,
) {
    let msg = serde_json::to_string(&OutgoingMessage {
        message_type: msg_type.to_string(),
        message_id: Uuid::new_v4().to_string(),
        data: Some(data),
    })
    .unwrap_or_default();
    let _ = sender.read().await.send(msg);
}

async fn build_sessions_data(state: &AppState) -> Value {
    let now = now_unix();
    let timeout = session_timeout_seconds();
    let mut sessions_guard = state.playback_sessions.write().await;
    sessions_guard.retain(|_, session| now - session.last_activity_unix <= timeout);
    let sessions: Vec<&PlaybackSession> = sessions_guard.values().collect();
    Value::Array(
        sessions
            .into_iter()
            .map(|s| serde_json::to_value(s).unwrap_or(Value::Null))
            .collect(),
    )
}

async fn build_activity_data(state: &AppState) -> Value {
    let rows = state
        .db
        .query_all(crate::db::helpers::pg_statement(
            "SELECT name, log_type, created_at, user_id FROM activity_log ORDER BY created_at DESC LIMIT 15",
            vec![],
        ))
        .await;

    match rows {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .iter()
                .map(|row| {
                    let created_at: i64 = row.get_i64("created_at").unwrap_or_default();
                    serde_json::json!({
                        "Name": row.get_str("name").unwrap_or_default(),
                        "Type": row.get_str("log_type").unwrap_or_default(),
                        "Date": crate::util::unix_to_jellyfin_date(created_at),
                        "UserId": row.get_opt_str("user_id").ok().flatten(),
                        "Severity": "Info",
                    })
                })
                .collect();
            Value::Array(items)
        }
        Err(_) => Value::Array(vec![]),
    }
}

async fn build_tasks_data(state: &AppState) -> Value {
    Value::Array(vec![
        crate::jellyfin::system::scan_library_task(&state.db).await,
    ])
}
