pub mod helpers;
mod migrate;
pub mod provider_ids;
pub mod row_ext;
mod schema;
mod seed;
pub mod settings;

pub use migrate::{ensure_database_exists, migrate};
pub use seed::seed_default_data;

#[cfg(test)]
pub use migrate::test_db;

pub const DEFAULT_DATABASE_URL: &str = "postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs";
