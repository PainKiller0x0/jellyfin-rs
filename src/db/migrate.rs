use anyhow::{Context, bail};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};

use crate::db::helpers::pg_statement;
use crate::db::schema::{migrations, optional_migrations};

pub async fn ensure_database_exists(database_url: &str) -> anyhow::Result<()> {
    if !is_postgres_url(database_url) {
        bail!("unsupported database URL; PostgreSQL is required");
    }

    if can_connect(database_url).await {
        return Ok(());
    }

    let database_name = postgres_database_name(database_url)
        .context("PostgreSQL database URL must include a database name")?;
    let maintenance_url = postgres_maintenance_url(database_url)
        .context("PostgreSQL database URL must include a database path")?;
    let mut opt = ConnectOptions::new(maintenance_url.clone());
    opt.max_connections(1).sqlx_logging(false);
    let db = Database::connect(opt).await.with_context(|| {
        format!("failed to connect PostgreSQL maintenance database: {maintenance_url}")
    })?;

    let exists = db
        .query_one_raw(pg_statement(
            "SELECT 1 FROM pg_database WHERE datname = ?",
            vec![database_name.clone().into()],
        ))
        .await
        .context("failed to check PostgreSQL database existence")?
        .is_some();
    if !exists {
        db.execute_raw(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE {}", quote_ident(&database_name)),
        ))
        .await
        .with_context(|| format!("failed to create PostgreSQL database: {database_url}"))?;
    }

    Ok(())
}

pub async fn migrate(db: &DatabaseConnection) -> anyhow::Result<()> {
    for (sql, context) in migrations() {
        execute_migration(db, sql, context).await?;
    }

    for (sql, context) in optional_migrations() {
        execute_optional_migration(db, sql, context).await;
    }

    Ok(())
}

pub(crate) fn is_postgres_url(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

async fn can_connect(database_url: &str) -> bool {
    let mut opt = ConnectOptions::new(database_url.to_string());
    opt.max_connections(1).sqlx_logging(false);
    Database::connect(opt).await.is_ok()
}

fn postgres_database_name(database_url: &str) -> Option<String> {
    let base = database_url.split('?').next().unwrap_or(database_url);
    base.rsplit_once('/')
        .map(|(_, name)| name)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

fn postgres_maintenance_url(database_url: &str) -> Option<String> {
    let (base, query) = database_url
        .split_once('?')
        .map(|(base, query)| (base, Some(query)))
        .unwrap_or((database_url, None));
    let (prefix, database_name) = base.rsplit_once('/')?;
    if database_name.is_empty() {
        return None;
    }
    Some(match query {
        Some(query) => format!("{prefix}/postgres?{query}"),
        None => format!("{prefix}/postgres"),
    })
}

async fn execute_migration(
    db: &DatabaseConnection,
    sql: &str,
    context: &'static str,
) -> anyhow::Result<()> {
    db.execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .context(context)?;
    Ok(())
}

async fn execute_optional_migration(db: &DatabaseConnection, sql: &str, context: &'static str) {
    if let Err(error) = db
        .execute_raw(Statement::from_string(DbBackend::Postgres, sql.to_string()))
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

fn quote_ident(value: &str) -> String {
    format!(r#""{}""#, value.replace('"', "\"\""))
}

#[cfg(test)]
pub async fn test_db() -> Option<DatabaseConnection> {
    let database_url = std::env::var("JELLYFIN_RS_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("JELLYFIN_RS_DATABASE_URL"))
        .ok()
        .filter(|url| is_postgres_url(url));

    let Some(database_url) = database_url else {
        eprintln!("skipping database test: JELLYFIN_RS_TEST_DATABASE_URL must be PostgreSQL");
        return None;
    };

    let mut opt = ConnectOptions::new(database_url);
    opt.max_connections(1).sqlx_logging(false);
    let db = match Database::connect(opt).await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("skipping database test: failed to connect PostgreSQL: {error}");
            return None;
        }
    };

    if let Err(error) = db
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            "SET search_path TO pg_temp".to_string(),
        ))
        .await
    {
        eprintln!("skipping database test: failed to initialize temporary schema: {error}");
        return None;
    }

    if let Err(error) = migrate(&db).await {
        eprintln!("skipping database test: failed to migrate schema: {error}");
        return None;
    }

    Some(db)
}
