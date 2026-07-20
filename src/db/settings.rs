use anyhow::Context;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DeleteResult, EntityTrait, QueryFilter, QueryOrder, Set,
    sea_query::OnConflict,
};

use crate::{
    entities::app_settings::{self, Entity as AppSettings},
    util::now_unix,
};

pub async fn get(db: &DatabaseConnection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(AppSettings::find_by_id(key)
        .one(db)
        .await
        .with_context(|| format!("failed to read app setting {key}"))?
        .map(|model| model.value))
}

pub async fn get_non_empty_or_default(db: &DatabaseConnection, key: &str, default: &str) -> String {
    get(db, key)
        .await
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub async fn get_bool(db: &DatabaseConnection, key: &str, default: bool) -> bool {
    match get_non_empty_or_default(db, key, if default { "true" } else { "false" })
        .await
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" => true,
        "0" | "false" | "no" => false,
        _ => default,
    }
}

pub async fn is_true(db: &DatabaseConnection, key: &str) -> anyhow::Result<bool> {
    Ok(get(db, key)
        .await?
        .is_some_and(|value| value.eq_ignore_ascii_case("true")))
}

pub async fn find_by_prefix(
    db: &DatabaseConnection,
    prefix: &str,
) -> anyhow::Result<Vec<app_settings::Model>> {
    AppSettings::find()
        .filter(app_settings::Column::Key.like(format!("{prefix}%")))
        .order_by_asc(app_settings::Column::Key)
        .all(db)
        .await
        .with_context(|| format!("failed to list app settings with prefix {prefix}"))
}

pub async fn set(db: &DatabaseConnection, key: &str, value: &str) -> anyhow::Result<()> {
    set_with_updated_at(db, key, value, now_unix()).await
}

pub async fn set_with_updated_at(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
    updated_at: i64,
) -> anyhow::Result<()> {
    AppSettings::insert(app_settings::ActiveModel {
        key: Set(key.to_string()),
        value: Set(value.to_string()),
        updated_at: Set(updated_at),
    })
    .on_conflict(
        OnConflict::column(app_settings::Column::Key)
            .update_columns([app_settings::Column::Value, app_settings::Column::UpdatedAt])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to upsert app setting {key}"))?;
    Ok(())
}

pub async fn insert_if_missing(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
    updated_at: i64,
) -> anyhow::Result<()> {
    AppSettings::insert(app_settings::ActiveModel {
        key: Set(key.to_string()),
        value: Set(value.to_string()),
        updated_at: Set(updated_at),
    })
    .on_conflict_do_nothing()
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to insert app setting {key}"))?;
    Ok(())
}

pub async fn delete(db: &DatabaseConnection, key: &str) -> anyhow::Result<DeleteResult> {
    AppSettings::delete_by_id(key)
        .exec(db)
        .await
        .with_context(|| format!("failed to delete app setting {key}"))
}
