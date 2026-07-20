// Entity definitions mirror the DB schema. Not all are used via SeaORM
// queries — complex joins and dynamic-table queries still use raw SQL.
#![allow(dead_code)]

pub mod access_tokens;
pub mod activity_log;
pub mod api_keys;
pub mod app_settings;
pub mod chapters;
pub mod display_preferences;
pub mod game_genres;
pub mod genres;
pub mod image_assets;
pub mod libraries;
pub mod library_paths;
pub mod linked_children;
pub mod media_game_genres;
pub mod media_genres;
pub mod media_items;
pub mod media_people;
pub mod media_streams;
pub mod media_studios;
pub mod media_tags;
pub mod people;
pub mod provider_ids;
pub mod studios;
pub mod tags;
pub mod task_results;
pub mod user_data;
pub mod users;
