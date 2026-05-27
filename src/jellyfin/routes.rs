use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{
    app::state::AppState,
    jellyfin::{
        auth, collect, common, dlna, filters, images, items, library, persons, playback, sessions,
        system,
    },
    playback::streaming::{
        stream_audio, stream_audio_head, stream_subtitle, stream_subtitle_head,
        stream_subtitle_with_source, stream_subtitle_with_source_head,
        stream_video, stream_video_head,
    },
    ws,
};

pub use common::{internal_error, not_found};
pub use items::find_media_item;
pub use playback::subtitle_stream_path;

pub fn api_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/GetUtcTime", get(system::utc_time))
        .route("/System/Info", get(system::system_info))
        .route("/System/Info/Public", get(system::public_system_info))
        .route("/System/Endpoint", get(system::system_endpoint))
        .route(
            "/System/Ping",
            get(system::system_ping).post(system::system_ping),
        )
        .route("/System/Logs", get(system::system_logs))
        .route("/System/Logs/Log", get(system::system_log_file))
        .route("/System/Info/Storage", get(system::system_storage))
        .route(
            "/System/Configuration",
            get(system::server_configuration).post(system::update_server_configuration),
        )
        .route(
            "/System/Configuration/MetadataOptions/Default",
            get(system::default_metadata_options),
        )
        .route(
            "/System/Configuration/{key}",
            get(system::named_configuration).post(system::update_named_configuration),
        )
        .route("/web/ConfigurationPages", get(system::configuration_pages))
        .route(
            "/web/ConfigurationPage",
            get(system::dashboard_configuration_page),
        )
        .route(
            "/Startup/Configuration",
            get(system::startup_configuration).post(system::update_startup_configuration),
        )
        .route(
            "/Startup/User",
            get(system::startup_user).post(system::update_startup_user),
        )
        .route("/Startup/RemoteAccess", post(system::update_remote_access))
        .route("/Startup/Complete", post(system::complete_startup))
        .route("/Options", get(system::localization_options))
        .route("/Cultures", get(system::localization_cultures))
        .route("/Countries", get(system::localization_countries))
        .route("/ParentalRatings", get(system::parental_ratings))
        .route("/Localization/Options", get(system::localization_options))
        .route("/Localization/Cultures", get(system::localization_cultures))
        .route(
            "/Localization/Countries",
            get(system::localization_countries),
        )
        .route(
            "/Localization/ParentalRatings",
            get(system::parental_ratings),
        )
        .route("/System/ReleaseNotes", get(system::system_release_notes))
        .route("/System/ReleaseNotes/Versions", get(system::system_release_notes_versions))
        .route("/System/Logs/Query", get(system::system_logs_query))
        .route("/QuickConnect/Enabled", get(system::quick_connect_enabled))
        .route(
            "/QuickConnect/Authorize",
            post(system::quick_connect_result),
        )
        .route("/QuickConnect/Connect", get(system::quick_connect_result))
        .route("/QuickConnect/Initiate", post(system::quick_connect_result))
        .route(
            "/Branding/Configuration",
            get(system::branding_configuration),
        )
        .route(
            "/Branding/Splashscreen",
            get(common::image)
                .post(common::no_content)
                .delete(common::no_content),
        )
        .route("/Playback/BitrateTest", get(system::bitrate_test))
        .route("/System/Ext/ServerDomains", get(system::system_ext_server_domains))
        .route("/Dlna/ProfileInfos", get(dlna::profile_infos))
        .route("/Dlna/Profiles/Default", get(dlna::default_profile))
        .route("/Dlna/Profiles/{profile_id}", get(dlna::profile_by_id))
        .route(
            "/Dlna/{server_id}/description.xml",
            get(dlna::device_description),
        )
        .route("/description.xml", get(dlna::device_description))
        .route("/ClientLog/Document", post(common::no_content))
        .route("/Auth/Keys", get(auth::api_keys).post(auth::create_api_key))
        .route("/Auth/Keys/{key}", delete(auth::delete_api_key))
        .route("/Plugins", get(common::empty_array))
        .route("/Plugins/{plugin_id}", delete(common::no_content))
        .route(
            "/Plugins/{plugin_id}/Configuration",
            get(common::empty_object).post(common::no_content),
        )
        .route("/Plugins/{plugin_id}/Manifest", post(common::empty_object))
        .route("/Plugins/{plugin_id}/{version}", delete(common::no_content))
        .route(
            "/Plugins/{plugin_id}/{version}/Disable",
            post(common::no_content),
        )
        .route(
            "/Plugins/{plugin_id}/{version}/Enable",
            post(common::no_content),
        )
        .route("/Plugins/{plugin_id}/{version}/Image", get(common::image))
        .route("/Packages", get(common::empty_array))
        .route("/Packages/{name}", get(common::empty_object))
        .route("/Packages/Installed/{name}", post(common::no_content))
        .route(
            "/Packages/Installing/{package_id}",
            delete(common::no_content),
        )
        .route("/Repositories", get(common::empty_array))
        .route(
            "/Tmdb/ClientConfiguration",
            get(system::tmdb_client_configuration),
        )
        .route("/Devices", get(system::devices))
        .route("/Devices/Info", get(system::device_options))
        .route("/Devices/Options", get(system::device_options))
        .route("/Auth/Providers", get(common::empty_array))
        .route("/Auth/PasswordResetProviders", get(common::empty_array))
        .route("/Channels", get(common::empty_list))
        .route("/Channels/Features", get(common::empty_array))
        .route("/Channels/Items/Latest", get(common::empty_array))
        .route("/Channels/{channel_id}", get(common::empty_object))
        .route("/Channels/{channel_id}/Features", get(common::empty_array))
        .route("/Channels/{channel_id}/Items", get(common::empty_list))
        .route("/LiveTv/Info", get(common::empty_object))
        .route("/LiveTv/GuideInfo", get(common::empty_object))
        .route("/LiveTv/ChannelMappingOptions", get(common::empty_object))
        .route("/LiveTv/ChannelMappings", post(common::no_content))
        .route("/LiveTv/Channels", get(common::empty_list))
        .route("/LiveTv/Channels/{channel_id}", get(common::empty_object))
        .route(
            "/LiveTv/ListingProviders",
            get(common::empty_array).post(common::no_content),
        )
        .route(
            "/LiveTv/ListingProviders/Default",
            get(common::empty_object),
        )
        .route("/LiveTv/ListingProviders/Lineups", get(common::empty_array))
        .route(
            "/LiveTv/ListingProviders/SchedulesDirect/Countries",
            get(system::localization_countries),
        )
        .route(
            "/LiveTv/TunerHosts",
            get(common::empty_array).post(common::no_content),
        )
        .route("/LiveTv/TunerHosts/Types", get(common::empty_array))
        .route("/LiveTv/Tuners/Discover", get(common::empty_array))
        .route("/LiveTv/Tuners/Discvover", get(common::empty_array))
        .route("/LiveTv/Tuners/{tuner_id}/Reset", post(common::no_content))
        .route(
            "/LiveTv/Programs",
            get(common::empty_list).post(common::empty_list),
        )
        .route("/LiveTv/Programs/Recommended", get(common::empty_list))
        .route("/LiveTv/Programs/{program_id}", get(common::empty_object))
        .route("/LiveTv/Recordings", get(common::empty_list))
        .route("/LiveTv/Recordings/Series", get(common::empty_list))
        .route("/LiveTv/Recordings/Folders", get(common::empty_array))
        .route("/LiveTv/Recordings/Groups", get(common::empty_list))
        .route(
            "/LiveTv/Recordings/Groups/{group_id}",
            get(common::empty_object),
        )
        .route(
            "/LiveTv/Recordings/{recording_id}",
            get(common::empty_object).delete(common::no_content),
        )
        .route(
            "/LiveTv/SeriesTimers",
            get(common::empty_list).post(common::no_content),
        )
        .route(
            "/LiveTv/SeriesTimers/{timer_id}",
            get(common::empty_object)
                .post(common::no_content)
                .delete(common::no_content),
        )
        .route(
            "/LiveTv/Timers",
            get(common::empty_list).post(common::no_content),
        )
        .route("/LiveTv/Timers/Defaults", get(common::empty_object))
        .route(
            "/LiveTv/Timers/{timer_id}",
            get(common::empty_object)
                .post(common::no_content)
                .delete(common::no_content),
        )
        .route(
            "/Environment/DefaultDirectoryBrowser",
            get(system::default_directory_browser),
        )
        .route(
            "/Environment/DirectoryContents",
            get(system::directory_contents),
        )
        .route("/Environment/Drives", get(system::drives))
        .route("/Environment/ParentPath", get(system::parent_path))
        .route("/Environment/ValidatePath", post(system::validate_path))
        .route("/System/ActivityLog/Entries", get(system::activity_log))
        .route("/System/Shutdown", post(system::shutdown_handler))
        .route("/System/Restart", post(system::shutdown_handler))
        .route("/Library/MediaFolders", get(library::media_folders))
        .route("/Library/SelectableMediaFolders", get(library::selectable_media_folders))
        .route("/Libraries/AvailableOptions", get(common::empty_object))
        .route("/Library/PhysicalPaths", get(library::physical_paths))
        .route("/Library/Movies/Added", post(library::library_notify))
        .route("/Library/Series/Added", post(library::library_notify))
        .route("/Library/Media/Updated", post(library::library_notify))
        .route("/Library/Movies/Updated", post(library::library_notify))
        .route("/Library/Series/Updated", post(library::library_notify))
        .route("/Library/Refresh", post(library::refresh_library))
        .route(
            "/Library/VirtualFolders",
            get(library::virtual_folders)
                .post(library::create_virtual_folder)
                .delete(library::delete_virtual_folder),
        )
        .route(
            "/Library/VirtualFolders/Delete",
            post(library::delete_virtual_folder),
        )
        .route("/Library/VirtualFolders/Query", get(library::virtual_folders_query))
        .route("/Library/VirtualFolders/Name", post(library::rename_virtual_folder))
        .route(
            "/Library/VirtualFolders/LibraryOptions",
            post(library::update_library_options),
        )
        .route(
            "/Library/VirtualFolders/Paths",
            post(library::add_virtual_folder_path).delete(library::delete_virtual_folder_path),
        )
        .route(
            "/Library/VirtualFolders/Paths/Update",
            post(library::update_virtual_folder_path),
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
        .route("/Users/Prefixes", get(filters::users_prefixes))
        .route("/Users/Configuration", post(common::no_content))
        .route("/Users/Password", post(common::no_content))
        .route("/Users/ForgotPassword", post(auth::forgot_password))
        .route("/Users/ForgotPassword/Pin", post(auth::forgot_password))
        .route(
            "/Users/AuthenticateWithQuickConnect",
            post(auth::authenticate_by_name),
        )
        .route("/Users/Public", get(auth::public_users))
        .route("/users/public", get(auth::public_users))
        .route("/Users/Me", get(auth::current_user))
        .route("/Users/New", post(auth::create_user))
        .route(
            "/Users/{user_id}",
            get(auth::user_by_id)
                .post(auth::update_user)
                .delete(auth::delete_user),
        )
        .route("/Users/{user_id}/Views", get(items::views))
        .route("/Users/{user_id}/Items", get(items::user_items))
        .route("/Users/{user_id}/Items/Latest", get(items::latest_items))
        .route("/Users/{user_id}/Items/Resume", get(items::resume_items))
        .route("/Users/{user_id}/Suggestions", get(items::user_suggestions))
        .route("/Users/{user_id}/HomeSections", get(items::home_sections))
        .route(
            "/Users/{user_id}/Sections/{section_id}/Items",
            get(items::home_section_items),
        )
        .route("/Users/{user_id}/Items/{item_id}", get(items::item_by_id))
        .route(
            "/Users/{user_id}/Items/{item_id}/Intros",
            get(super::user_extras::user_item_intros),
        )
        .route(
            "/Users/{user_id}/Items/{item_id}/LocalTrailers",
            get(super::user_extras::user_item_local_trailers),
        )
        .route(
            "/Users/{user_id}/Images/Primary",
            get(images::user_avatar)
                .post(images::upload_user_avatar)
                .delete(images::delete_user_avatar),
        )
        .route(
            "/Users/{user_id}/Images/Primary/Delete",
            post(images::delete_user_avatar),
        )
        .route("/UserViews", get(items::views))
        .route("/UserViews/GroupingOptions", get(common::empty_array))
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(playback::favorite_item).delete(playback::unfavorite_item),
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
        .route(
            "/Users/{user_id}/Configuration",
            post(auth::update_user_configuration),
        )
        .route("/Users/{user_id}/Policy", post(auth::update_user_policy))
        .route("/Items", get(items::items_root))
        .route("/Items/Filters", get(super::user_extras::items_filters))
        .route("/Items/Filters2", get(super::user_extras::filters2))
        .route("/Items/Suggestions", get(super::user_extras::items_suggestions))
        .route(
            "/Items/{item_id}",
            get(items::item_by_id_public).post(items::update_item),
        )
        .route("/Items/{item_id}/Ancestors", get(super::user_extras::item_ancestors))
        .route("/Items/{item_id}/CriticReviews", get(super::user_extras::item_critic_reviews))
        .route("/Items/{item_id}/Download", get(super::user_extras::download_item))
        .route("/Items/{item_id}/File", get(super::user_extras::item_file_info))
        .route("/Items/{item_id}/ThemeMedia", get(super::user_extras::item_theme_media))
        .route("/Items/{item_id}/ThemeSongs", get(super::user_extras::item_theme_songs))
        .route("/Items/{item_id}/ThemeVideos", get(super::user_extras::item_theme_videos))
        .route("/Items/{item_id}/InstantMix", get(super::user_extras::item_instant_mix))
        .route("/Items/{item_id}/Intros", get(super::user_extras::item_intros))
        .route("/Items/{item_id}/LocalTrailers", get(super::user_extras::item_local_trailers))
        .route("/Items/{item_id}/SpecialFeatures", get(super::user_extras::item_special_features))
        .route("/Items/{item_id}/ThumbnailSet", get(super::user_extras::thumbnail_set))
        .route("/Items/{item_id}/Images", get(images::item_images))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(images::get_item_image)
                .head(super::user_extras::item_image_head)
                .post(images::upload_item_image),
        )
        .route(
            "/Items/{item_id}/Images/{first}/{second}",
            get(images::get_item_image_with_index)
                .head(super::user_extras::item_image_index_head)
                .post(images::upload_item_image_with_index),
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
        .route("/Albums/{item_id}/Similar", get(items::similar_items))
        .route("/Artists/{item_id}/Similar", get(items::similar_items))
        .route("/Artists", get(common::empty_list))
        .route("/Artists/AlbumArtists", get(common::empty_list))
        .route("/Artists/InstantMix", get(common::empty_list))
        .route("/Artists/Prefixes", get(filters::artists_prefixes))
        .route("/Artists/{name}", get(common::empty_object))
        .route("/Artists/{item_id}/InstantMix", get(common::empty_list))
        .route("/Artists/{name}/Images/{image_type}", get(super::user_extras::artist_image))
        .route("/Artists/{name}/Images/{image_type}/{index}", get(super::user_extras::artist_image))
        .route("/Movies/Recommendations", get(items::movie_recommendations))
        .route("/Movies/{item_id}/Similar", get(items::similar_items))
        .route("/Shows/{item_id}/Similar", get(items::similar_items))
        .route("/Trailers/{item_id}/Similar", get(items::similar_items))
        .route(
            "/Items/{item_id}/ExternalIdInfos",
            get(items::external_id_infos),
        )
        .route("/Items/{item_id}/RemoteImages", get(images::remote_images))
        .route(
            "/Items/{item_id}/RemoteImages/Providers",
            get(common::empty_array),
        )
        .route("/Items/{item_id}/Subtitles", get(items::item_subtitles))
        .route("/Items/{item_id}/Subtitles/{index}/Delete", post(items::delete_item_subtitle))
        .route(
            "/Items/{item_id}/RemoteSearch/Subtitles/{param}",
            get(super::user_extras::remote_subtitle_search)
                .post(super::user_extras::download_remote_subtitle),
        )
        .route(
            "/Items/{item_id}/RemoteImages/Download",
            post(images::download_remote_image),
        )
        .route("/Items/{item_id}/DeleteInfo", get(items::delete_info))
        .route("/Items/Delete", post(items::delete_items))
        .route("/Items/Prefixes", get(filters::items_prefixes))
        .route("/Items/{item_id}/Tags/Add", post(items::add_item_tag))
        .route("/Items/{item_id}/Tags/Delete", post(items::delete_item_tag))
        .route("/Items/{item_id}/MetadataEditor", post(super::system::metadata_editor))
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
        .route("/Shows/Upcoming", get(super::user_extras::shows_upcoming))
        .route("/Search/Hints", get(items::search_hints))
        .route("/Genres", get(filters::genres))
        .route("/Genres/{name}", get(super::user_extras::genre_by_name))
        .route("/Genres/{name}/Images/{image_type}", get(super::user_extras::genre_image))
        .route("/Genres/{name}/Images/{image_type}/{index}", get(super::user_extras::genre_image))
        .route("/GameGenres", get(filters::game_genres))
        .route("/GameGenres/{name}", get(super::user_extras::genre_by_name))
        .route("/GameGenres/{name}/Images/{image_type}", get(super::user_extras::genre_image))
        .route("/GameGenres/{name}/Images/{image_type}/{index}", get(super::user_extras::genre_image))
        .route("/MusicGenres", get(common::empty_list))
        .route("/MusicGenres/InstantMix", get(common::empty_list))
        .route("/MusicGenres/{genre_name}", get(common::empty_object))
        .route("/MusicGenres/{name}/InstantMix", get(common::empty_list))
        .route("/Persons", get(filters::persons))
        .route("/Persons/{name}", get(persons::person_by_name))
        .route("/Persons/{name}/Items", get(persons::person_items))
        .route("/Persons/{name}/Images/{image_type}", get(persons::person_image))
        .route("/Persons/{name}/Images/{first}/{second}", get(persons::person_image_with_index))
        .route("/Studios", get(filters::studios))
        .route("/Studios/{name}", get(super::user_extras::studio_by_name))
        .route("/Studios/{name}/Images/{image_type}", get(super::user_extras::studio_image))
        .route("/Studios/{name}/Images/{image_type}/{index}", get(super::user_extras::studio_image))
        .route("/Tags", get(filters::tags))
        .route("/Years", get(filters::years))
        .route("/OfficialRatings", get(filters::official_ratings))
        .route("/Containers", get(filters::containers))
        .route("/VideoCodecs", get(filters::video_codecs))
        .route("/ExtendedVideoTypes", get(filters::extended_video_types))
        .route("/AudioCodecs", get(super::user_extras::audio_codecs))
        .route("/AudioLayouts", get(super::user_extras::audio_layouts))
        .route("/SubtitleCodecs", get(super::user_extras::subtitle_codecs))
        .route("/StreamLanguages", get(super::user_extras::stream_languages))
        .route("/ItemTypes", get(super::user_extras::item_types))
        .route("/Videos/{item_id}/AdditionalParts", get(super::user_extras::video_additional_parts))
        .route("/Videos/MergeVersions", post(items::merge_versions))
        .route(
            "/Videos/{item_id}/Trickplay/{width}/tiles.m3u8",
            get(common::empty_object),
        )
        .route(
            "/Videos/{item_id}/Trickplay/{width}/{index}",
            get(common::image),
        )
        .route(
            "/Videos/{video_id}/{media_source_id}/Attachments/{index}",
            get(common::empty_object),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(stream_video).head(stream_video_head),
        )
        .route(
            "/Videos/{item_id}/Subtitles/{index}/Stream.{format}",
            get(stream_subtitle).head(stream_subtitle_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/Subtitles/{index}/Stream.{format}",
            get(stream_subtitle_with_source).head(stream_subtitle_with_source_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/Attachments/{index}/Stream",
            get(super::user_extras::attachment_stream),
        )
        .route(
            "/Audio/{item_id}/universal",
            get(stream_audio).head(stream_audio_head),
        )
        .route(
            "/Audio/{item_id}/Lyrics",
            get(common::empty_object)
                .post(common::no_content)
                .delete(common::no_content),
        )
        .route(
            "/Audio/{item_id}/RemoteSearch/Lyrics",
            get(common::empty_array),
        )
        .route(
            "/Audio/{item_id}/RemoteSearch/Lyrics/{lyric_id}",
            post(common::no_content),
        )
        .route("/Providers/Lyrics/{lyric_id}", get(common::empty_object))
        .route("/MediaSegments/{item_id}", get(super::user_extras::media_segments))
        .route("/Albums/{item_id}/InstantMix", get(common::empty_list))
        .route("/Playlists/{item_id}/InstantMix", get(common::empty_list))
        .route("/Songs/{item_id}/InstantMix", get(common::empty_list))
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
        .route("/Sessions/Logout", post(common::no_content))
        .route("/Sessions/Viewing", post(common::no_content))
        .route("/Sessions/Playing/Ping", post(common::no_content))
        .route("/Sessions/PlayQueue", get(super::user_extras::play_queue))
        .route("/Sessions/{session_id}/Viewing", post(common::no_content))
        .route("/Sessions/{session_id}/Command", post(common::no_content))
        .route(
            "/Sessions/{session_id}/Command/{command}",
            post(common::no_content),
        )
        .route("/Sessions/{session_id}/Message", post(common::no_content))
        .route(
            "/Sessions/{session_id}/Playing/{command}",
            post(common::no_content),
        )
        .route(
            "/Sessions/{session_id}/System/{command}",
            post(common::no_content),
        )
        .route(
            "/Sessions/{session_id}/User/{user_id}",
            post(common::no_content).delete(common::no_content),
        )
        .route("/ScheduledTasks", get(system::scheduled_tasks))
        .route("/ScheduledTasks/{task_id}", get(common::empty_object))
        .route(
            "/ScheduledTasks/{task_id}/Triggers",
            get(common::empty_array).post(common::no_content),
        )
        .route(
            "/ScheduledTasks/Running/{task_id}",
            post(items::scan_handler).delete(common::no_content),
        )
        .route("/UserItems/Resume", get(super::user_extras::user_items_resume))
        .route(
            "/UserItems/{item_id}/UserData",
            get(playback::get_user_item_data).post(playback::update_user_item_data),
        )
        .route(
            "/UserPlayedItems/{item_id}",
            post(common::no_content).delete(common::no_content),
        )
        .route(
            "/PlayingItems/{item_id}",
            post(playback::playing_item_start).delete(playback::playing_item_stop),
        )
        .route("/PlayingItems/{item_id}/Progress", post(playback::playing_item_progress))
        .route(
            "/UserSettings/{user_id}",
            get(super::user_extras::get_user_settings).post(super::user_extras::update_user_settings),
        )
        .route(
            "/Users/{user_id}/PlayingItems/{item_id}",
            post(playback::playing_item_start).delete(playback::playing_item_stop),
        )
        .route(
            "/Users/{user_id}/PlayingItems/{item_id}/Progress",
            post(playback::playing_item_progress),
        )
        .route(
            "/Users/{user_id}/TrackSelections/{track_type}",
            delete(super::user_extras::clear_track_selections),
        )
        .route(
            "/Users/{user_id}/TrackSelections/{track_type}/Delete",
            post(super::user_extras::clear_track_selections),
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
        .route("/Collections", post(collect::create_collection))
        .route(
            "/Collections/{collection_id}/Items",
            post(collect::add_to_collection).delete(collect::remove_from_collection),
        )
        .route("/Playlists", post(collect::create_playlist))
        .route(
            "/Playlists/{playlist_id}",
            get(collect::get_playlist).post(collect::update_playlist),
        )
        .route(
            "/Playlists/{playlist_id}/Items",
            get(collect::get_playlist_items)
                .post(collect::add_to_playlist)
                .delete(collect::remove_from_playlist),
        )
        .route(
            "/Playlists/{playlist_id}/AddToPlaylistInfo",
            get(super::user_extras::add_to_playlist_info),
        )
        .route(
            "/Playlists/{playlist_id}/Items/{item_id}/Move/{new_index}",
            post(super::user_extras::playlist_move_item),
        )
        .route("/websocket", get(ws::ws_handler))
        .route("/WebSocket", get(ws::ws_handler))
}
