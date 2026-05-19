use anyhow::Context;
use sea_orm::sqlx::{Sqlite, migrate::MigrateDatabase};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

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

pub async fn migrate(db: &DatabaseConnection, url: &str) -> anyhow::Result<()> {
    let backend = if url.starts_with("sqlite:") {
        DbBackend::Sqlite
    } else {
        DbBackend::Postgres
    };

    for (sql, context) in migrations() {
        execute_migration(db, backend, sql, context).await?;
    }

    for (sql, context) in optional_migrations() {
        execute_optional_migration(db, backend, sql, context).await;
    }

    Ok(())
}

async fn execute_migration(
    db: &DatabaseConnection,
    backend: DbBackend,
    sql: &str,
    context: &'static str,
) -> anyhow::Result<()> {
    db.execute(Statement::from_string(backend, sql.to_string()))
        .await
        .context(context)?;
    Ok(())
}

async fn execute_optional_migration(
    db: &DatabaseConnection,
    backend: DbBackend,
    sql: &str,
    context: &'static str,
) {
    if let Err(error) = db
        .execute(Statement::from_string(backend, sql.to_string()))
        .await
    {
        let message = error.to_string().to_ascii_lowercase();
        if !message.contains("duplicate") && !message.contains("exists") {
            tracing::warn!("optional migration failed ({context}): {error}");
        }
    }
}
