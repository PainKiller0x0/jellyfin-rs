use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "people")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub overview: Option<String>,
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
    pub home_page_url: Option<String>,
    pub premiere_date: Option<String>,
    pub end_date: Option<String>,
    pub production_locations: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
