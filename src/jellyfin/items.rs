mod discovery;
mod display_prefs;
mod item_detail;
mod item_operations;
mod lists;
mod metadata_mgmt;
mod recommendations;
mod remote_metadata;
mod tv_shows;

pub use crate::jellyfin::item_queries::{
    find_first_playable_child, find_media_item, find_media_item_for_admin,
};
pub use discovery::{search_hints, shows_missing, shows_next_up, similar_items};
pub use display_prefs::{get_display_preferences, update_display_preferences};
pub use item_detail::{enrich_episode_list, enrich_resume_items, item_by_id, item_by_id_public};
pub use item_operations::{
    add_item_tag, delete_info, delete_item_subtitle, delete_item_tag, delete_items,
    delete_single_item, make_item_private, make_item_public, update_item, update_item_content_type,
};
pub(crate) use item_operations::{normalize_item_update_body, update_item_inner};
pub(super) use lists::media_list_response;
pub use lists::{
    items, items_root, latest_items, latest_items_root, resume_items, trailers, user_items,
    user_items_root, views,
};
pub use metadata_mgmt::{
    active_encodings, alternate_sources, audiobooks_next_up, available_recording_options,
    delete_alternate_source, delete_lyrics, external_id_infos, item_counts, item_lyrics,
    item_subtitles, merge_versions, metadata_editor_info, metadata_reset,
    remote_lyrics_unavailable, scan_handler, stop_encodings, subtitle_provider_info, upload_lyrics,
    upload_subtitle,
};
pub use recommendations::{
    home_section_items, home_sections, movie_recommendations, user_suggestions,
};
pub use remote_metadata::{apply_remote_search, remote_search};
pub use tv_shows::{show_episodes, show_seasons};
