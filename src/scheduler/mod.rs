pub mod tasks;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::app::state::AppState;
use crate::util::now_unix;

pub type TaskHandler = Arc<
    dyn Fn(Arc<AppState>) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync,
>;

pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub cron_expr: String,
    pub is_running: Arc<AtomicBool>,
    pub last_result: Arc<Mutex<Option<TaskResult>>>,
    pub handler: TaskHandler,
}

#[derive(Debug, Clone)]
pub struct TaskResult {
    pub status: String,
    pub start_time: i64,
    pub end_time: i64,
    pub message: Option<String>,
}

pub struct TaskScheduler {
    tasks: Vec<ScheduledTask>,
    cancel: CancellationToken,
}

impl TaskScheduler {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            cancel: CancellationToken::new(),
        }
    }

    pub fn register<F, Fut>(&mut self, id: &str, name: &str, category: &str, cron_expr: &str, handler: F)
    where
        F: Fn(Arc<AppState>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let handler: TaskHandler = Arc::new(move |state| Box::pin(handler(state)));
        self.tasks.push(ScheduledTask {
            id: id.to_string(),
            name: name.to_string(),
            description: name.to_string(),
            category: category.to_string(),
            cron_expr: cron_expr.to_string(),
            is_running: Arc::new(AtomicBool::new(false)),
            last_result: Arc::new(Mutex::new(None)),
            handler,
        });
    }

    pub fn start(self: Arc<Self>, state: Arc<AppState>) {
        for task in &self.tasks {
            let task_id = task.id.clone();
            let cron_expr = task.cron_expr.clone();
            let handler = task.handler.clone();
            let is_running = task.is_running.clone();
            let last_result = task.last_result.clone();
            let cancel = self.cancel.clone();
            let state = state.clone();

            tokio::spawn(async move {
                let schedule = match cron::Schedule::from_str(&cron_expr) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("invalid cron expression for task {task_id}: {e}");
                        return;
                    }
                };

                loop {
                    let next = schedule.upcoming(chrono::Utc).next();
                    let Some(next_time) = next else {
                        tracing::warn!("no upcoming time for task {task_id}");
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        continue;
                    };

                    let now = chrono::Utc::now();
                    let sleep_duration = (next_time - now)
                        .to_std()
                        .unwrap_or(std::time::Duration::from_secs(60));

                    tokio::select! {
                        _ = cancel.cancelled() => {
                            tracing::info!("task {task_id} cancelled");
                            break;
                        }
                        _ = tokio::time::sleep(sleep_duration) => {}
                    }

                    is_running.store(true, Ordering::SeqCst);
                    let start_time = now_unix();
                    let result = (handler)(state.clone()).await;
                    let end_time = now_unix();

                    let task_result = match result {
                        Ok(()) => TaskResult {
                            status: "Completed".to_string(),
                            start_time,
                            end_time,
                            message: None,
                        },
                        Err(e) => TaskResult {
                            status: "Failed".to_string(),
                            start_time,
                            end_time,
                            message: Some(e.to_string()),
                        },
                    };

                    *last_result.lock().await = Some(task_result.clone());
                    is_running.store(false, Ordering::SeqCst);

                    // Persist to task_results table
                    let _ = crate::jellyfin::system::upsert_task_result(
                        &state,
                        &task_id,
                        &task_result.status,
                        task_result.start_time,
                        task_result.end_time,
                        task_result.message.as_deref(),
                    )
                    .await;

                    // Broadcast task update
                    let _ = state.ws_event_tx.send(crate::ws::WsEvent::TaskUpdated);
                }
            });
        }
    }

    pub async fn run_now(&self, task_id: &str, state: Arc<AppState>) -> anyhow::Result<()> {
        let task = self
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| anyhow::anyhow!("task not found: {task_id}"))?;

        if task.is_running.load(Ordering::SeqCst) {
            anyhow::bail!("task {task_id} is already running");
        }

        task.is_running.store(true, Ordering::SeqCst);
        let start_time = now_unix();
        let result = (task.handler)(state).await;
        let end_time = now_unix();

        let task_result = match result {
            Ok(()) => TaskResult {
                status: "Completed".to_string(),
                start_time,
                end_time,
                message: None,
            },
            Err(e) => TaskResult {
                status: "Failed".to_string(),
                start_time,
                end_time,
                message: Some(e.to_string()),
            },
        };

        *task.last_result.lock().await = Some(task_result);
        task.is_running.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn list_tasks(&self) -> Vec<(&str, &str, &str, &str)> {
        self.tasks
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str(), t.description.as_str(), t.category.as_str()))
            .collect()
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new()
    }
}

use std::str::FromStr;
