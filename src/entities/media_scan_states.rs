use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "media_scan_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub path: String,
    pub item_id: String,
    pub library_id: String,
    pub fingerprint: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
