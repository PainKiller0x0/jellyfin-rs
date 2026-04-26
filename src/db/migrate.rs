use anyhow::Context;
use sqlx::{AnyPool, Sqlite, migrate::MigrateDatabase};

use crate::db::schema::{migrations, optional_migrations};

pub async fn ensure_database_exists(database_url: &str) -> anyhow::Result<()> {
    if !database_url.starts_with("sqlite:") {
        return Ok(());
    }

    if !Sqlite::database_exists(database_url)
        .await
        .unwrap_or_default()
    {
        Sqlite::create_database(database_url)
            .await
            .with_context(|| format!("failed to create SQLite database: {database_url}"))?;
    }

    Ok(())
}

pub async fn migrate(db: &AnyPool) -> anyhow::Result<()> {
    for (sql, context) in migrations() {
        execute_migration(db, sql, context).await?;
    }

    for (sql, context) in optional_migrations() {
        execute_optional_migration(db, sql, context).await;
    }

    Ok(())
}

async fn execute_migration(db: &AnyPool, sql: &str, context: &'static str) -> anyhow::Result<()> {
    sqlx::query(sql).execute(db).await.context(context)?;
    Ok(())
}

async fn execute_optional_migration(db: &AnyPool, sql: &str, context: &'static str) {
    if let Err(error) = sqlx::query(sql).execute(db).await {
        let message = error.to_string().to_ascii_lowercase();
        if !message.contains("duplicate") && !message.contains("exists") {
            tracing::warn!("optional migration failed ({context}): {error}");
        }
    }
}
