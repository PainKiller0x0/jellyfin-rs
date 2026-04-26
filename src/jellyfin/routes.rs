use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    app::state::AppState,
    jellyfin::{auth, common, filters, images, items, library, playback, sessions, system},
    playback::streaming::{
        stream_audio, stream_audio_head, stream_subtitle, stream_subtitle_head, stream_video,
        stream_video_head,
    },
};

pub use common::{internal_error, not_found};
pub use items::find_media_item;
pub use playback::subtitle_stream_path;

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/System/Info", get(system::system_info))
        .route("/System/Info/Public", get(system::public_system_info))
        .route("/System/ActivityLog/Entries", get(system::activity_log))
        .route("/System/Shutdown", post(system::shutdown_handler))
        .route("/System/Restart", post(system::shutdown_handler))
        .route("/Library/MediaFolders", get(library::media_folders))
        .route("/Library/Refresh", post(library::refresh_library))
        .route(
            "/Library/VirtualFolders",
            get(library::virtual_folders).post(library::create_virtual_folder),
        )
        .route(
            "/Library/VirtualFolders/Paths",
            post(library::add_virtual_folder_path).delete(library::delete_virtual_folder_path),
        )
        .route(
            "/Users/authenticatebyname",
            post(auth::authenticate_by_name),
        )
        .route(
            "/Users/AuthenticateByName",
            post(auth::authenticate_by_name),
        )
        .route(
            "/users/authenticatebyname",
            post(auth::authenticate_by_name),
        )
        .route("/Users", get(auth::list_users))
        .route("/Users/Me", get(auth::current_user))
        .route("/Users/New", post(auth::create_user))
        .route(
            "/Users/{user_id}",
            get(auth::user_by_id).delete(auth::delete_user),
        )
        .route("/Users/{user_id}/Views", get(items::views))
        .route("/Users/{user_id}/Items", get(items::user_items))
        .route("/Users/{user_id}/Items/Latest", get(items::latest_items))
        .route("/Users/{user_id}/Items/Resume", get(items::resume_items))
        .route("/Users/{user_id}/Items/{item_id}", get(items::item_by_id))
        .route("/Users/{user_id}/Images/Primary", get(images::user_avatar))
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(playback::favorite_item),
        )
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}/Delete",
            post(playback::unfavorite_item),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(playback::mark_played),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}/Delete",
            post(playback::mark_unplayed),
        )
        .route(
            "/Users/{user_id}/Items/{item_id}/HideFromResume",
            post(playback::hide_from_resume),
        )
        .route(
            "/Users/{user_id}/Password",
            post(auth::update_user_password),
        )
        .route("/Items/{item_id}", post(items::update_item))
        .route("/Items/{item_id}/Images", get(images::item_images))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(images::get_item_image).post(images::upload_item_image),
        )
        .route(
            "/Items/{item_id}/Images/{first}/{second}",
            get(images::get_item_image_with_index).post(images::upload_item_image_with_index),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/Delete",
            post(images::delete_item_image),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{index}/Delete",
            post(images::delete_item_image_with_index),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            post(playback::playback_info),
        )
        .route("/Items/{item_id}/Refresh", post(items::scan_handler))
        .route("/Items/{item_id}/Similar", get(items::similar_items))
        .route(
            "/Items/{item_id}/ExternalIdInfos",
            get(items::external_id_infos),
        )
        .route("/Items/{item_id}/RemoteImages", get(images::remote_images))
        .route("/Items/{item_id}/Subtitles", get(items::item_subtitles))
        .route(
            "/Items/{item_id}/RemoteImages/Download",
            post(images::download_remote_image),
        )
        .route("/Items/{item_id}/DeleteInfo", get(items::delete_info))
        .route("/Items/Delete", post(items::delete_items))
        .route(
            "/Items/RemoteSearch/{item_type}",
            post(items::remote_search),
        )
        .route(
            "/Items/RemoteSearch/Apply/{item_id}",
            post(items::apply_remote_search),
        )
        .route("/items/metadata/reset", post(items::metadata_reset))
        .route("/Shows/{show_id}/Episodes", get(items::show_episodes))
        .route("/Shows/{show_id}/Seasons", get(items::show_seasons))
        .route("/Shows/NextUp", get(items::shows_next_up))
        .route("/Shows/Missing", get(items::shows_missing))
        .route("/Search/Hints", get(items::search_hints))
        .route("/Genres", get(filters::genres))
        .route("/Persons", get(filters::persons))
        .route("/Studios", get(filters::studios))
        .route("/Tags", get(filters::tags))
        .route("/Years", get(filters::years))
        .route("/OfficialRatings", get(filters::official_ratings))
        .route("/Containers", get(filters::containers))
        .route("/VideoCodecs", get(filters::video_codecs))
        .route("/ExtendedVideoTypes", get(filters::extended_video_types))
        .route("/LiveTv/Channels", get(common::empty_list))
        .route("/Videos/{item_id}/AdditionalParts", get(common::empty_list))
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(stream_video).head(stream_video_head),
        )
        .route(
            "/Videos/{item_id}/Subtitles/{index}/Stream.{format}",
            get(stream_subtitle).head(stream_subtitle_head),
        )
        .route(
            "/Audio/{item_id}/universal",
            get(stream_audio).head(stream_audio_head),
        )
        .route("/Sessions", get(sessions::sessions))
        .route("/Sessions/Capabilities", post(sessions::capabilities))
        .route(
            "/Sessions/Capabilities/Full",
            post(sessions::capabilities_full),
        )
        .route("/Sessions/Playing", post(playback::playback_start))
        .route(
            "/Sessions/Playing/Stopped",
            post(playback::playback_stopped),
        )
        .route(
            "/Sessions/Playing/Progress",
            post(playback::playback_progress),
        )
        .route("/ScheduledTasks", get(system::scheduled_tasks))
        .route(
            "/ScheduledTasks/Running/{task_id}",
            post(items::scan_handler),
        )
        .route(
            "/DisplayPreferences/{prefs_id}",
            get(items::get_display_preferences).post(items::update_display_preferences),
        )
        .route(
            "/Users/{user_id}/Items/{item_id}/Rating",
            post(playback::set_rating).delete(playback::delete_rating),
        )
        .route("/Items/Counts", get(items::item_counts))
}
