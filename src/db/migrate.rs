use anyhow::Context;
use sea_orm::sqlx::{Postgres, Sqlite, migrate::MigrateDatabase};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

use crate::db::schema::{migrations, optional_migrations};

pub async fn ensure_database_exists(database_url: &str) -> anyhow::Result<()> {
    if database_url.starts_with("sqlite:") {
        if !Sqlite::database_exists(database_url)
            .await
            .unwrap_or_default()
        {
            Sqlite::create_database(database_url)
                .await
                .with_context(|| format!("failed to create SQLite database: {database_url}"))?;
        }
    } else if database_url.starts_with("postgres:") || database_url.starts_with("postgresql:") {
        if !Postgres::database_exists(database_url)
            .await
            .unwrap_or_default()
        {
            Postgres::create_database(database_url)
                .await
                .with_context(|| format!("failed to create PostgreSQL database: {database_url}"))?;
        }
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
    db.execute(Statement::from_string(backend, migration_sql(backend, sql)))
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
        .execute(Statement::from_string(backend, migration_sql(backend, sql)))
        .await
    {
        let message = error.to_string().to_ascii_lowercase();
        if !message.contains("duplicate")
            && !message.contains("exists")
            && !message.contains("已经存在")
        {
            tracing::warn!("optional migration failed ({context}): {error}");
        }
    }
}

fn migration_sql(backend: DbBackend, sql: &str) -> String {
    if backend == DbBackend::Postgres {
        sql.replace(" BLOB ", " BYTEA ")
    } else {
        sql.to_string()
    }
}
