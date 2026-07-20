pub mod helpers;
mod migrate;
pub mod provider_ids;
pub mod row_ext;
mod schema;
mod seed;
pub mod settings;

use anyhow::Context;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use tokio::sync::OnceCell;

pub use migrate::{ensure_database_exists, migrate};
pub use seed::seed_default_data;

#[cfg(test)]
pub use migrate::test_db;

pub const DEFAULT_DATABASE_URL: &str = "postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs";

static BACKGROUND_DATABASE: OnceCell<DatabaseConnection> = OnceCell::const_new();

pub fn database_url_from_env() -> String {
    std::env::var("JELLYFIN_RS_DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_string())
}

pub async fn connect_foreground_database(database_url: &str) -> anyhow::Result<DatabaseConnection> {
    connect_database(database_url, foreground_max_connections(), "database").await
}

pub async fn background_connection(database_url: &str) -> anyhow::Result<DatabaseConnection> {
    let db = BACKGROUND_DATABASE
        .get_or_try_init(|| async move {
            connect_database(
                database_url,
                background_max_connections(),
                "background database",
            )
            .await
        })
        .await?;
    Ok(db.clone())
}

async fn connect_database(
    database_url: &str,
    max_connections: u32,
    label: &str,
) -> anyhow::Result<DatabaseConnection> {
    let mut opt = ConnectOptions::new(database_url.to_string());
    opt.max_connections(max_connections).sqlx_logging(false);
    tracing::info!("{label} pool max_connections={max_connections}");
    Database::connect(opt)
        .await
        .with_context(|| format!("failed to connect {label}: {database_url}"))
}

pub(crate) fn cpu_parallelism() -> usize {
    cpu::physical_core_count()
        .ok()
        .or_else(|| cpu::cpu_count().ok())
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|parallelism| parallelism.get())
        })
        .unwrap_or(1)
        .max(1)
}

pub(crate) fn online_cpu_count() -> usize {
    cpu::cpu_count()
        .ok()
        .or_else(|| {
            std::thread::available_parallelism()
                .ok()
                .map(|parallelism| parallelism.get())
        })
        .unwrap_or(1)
        .max(1)
}

fn foreground_max_connections() -> u32 {
    cpu_parallelism().saturating_mul(4).clamp(8, 32) as u32
}

fn background_max_connections() -> u32 {
    (cpu_parallelism() / 2).clamp(1, 4) as u32
}
