use anyhow::Context;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set, sea_query::OnConflict,
};

use crate::entities::provider_ids::{self, Entity as ProviderIds};

pub async fn get(
    db: &DatabaseConnection,
    item_id: &str,
    provider: &str,
) -> anyhow::Result<Option<String>> {
    Ok(ProviderIds::find()
        .filter(provider_ids::Column::ItemId.eq(item_id))
        .filter(provider_ids::Column::Provider.eq(provider))
        .one(db)
        .await
        .with_context(|| format!("failed to read {provider} provider id for item {item_id}"))?
        .map(|model| model.provider_item_id))
}

pub async fn upsert(
    db: &DatabaseConnection,
    item_id: &str,
    provider: &str,
    provider_item_id: &str,
) -> anyhow::Result<()> {
    ProviderIds::insert(provider_ids::ActiveModel {
        item_id: Set(item_id.to_string()),
        provider: Set(provider.to_string()),
        provider_item_id: Set(provider_item_id.to_string()),
    })
    .on_conflict(
        OnConflict::columns([provider_ids::Column::ItemId, provider_ids::Column::Provider])
            .update_column(provider_ids::Column::ProviderItemId)
            .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .with_context(|| format!("failed to upsert {provider} provider id for item {item_id}"))?;
    Ok(())
}
