use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{
    app::state::AppState,
    jellyfin::{
        auth, backup, collect, common, dlna, filters, images, items, library, persons, playback,
        sessions, system,
    },
    playback::streaming::{
        stream_audio, stream_audio_container, stream_audio_container_head, stream_audio_head,
        stream_audio_simple, stream_audio_simple_head, stream_subtitle, stream_subtitle_head,
        stream_subtitle_with_source, stream_subtitle_with_source_head, stream_subtitle_with_ticks,
        stream_subtitle_with_ticks_head, stream_video, stream_video_head, stream_video_original,
        stream_video_original_container, stream_video_original_container_head,
        stream_video_original_head, stream_video_original_with_source,
        stream_video_original_with_source_container,
        stream_video_original_with_source_container_head, stream_video_original_with_source_head,
        stream_video_simple, stream_video_simple_head, stream_video_with_source,
        stream_video_with_source_head, stream_video_with_source_simple,
        stream_video_with_source_simple_head,
    },
    ws,
};

pub use common::{internal_error, not_found};
pub use items::{find_media_item, find_media_item_for_admin};
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
        .route("/Startup/FirstUser", get(system::startup_user))
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
        .route(
            "/System/ReleaseNotes/Versions",
            get(system::system_release_notes_versions),
        )
        .route("/System/Logs/Query", get(system::system_logs_query))
        .route(
            "/System/WakeOnLanInfo",
            get(system::system_wake_on_lan_info),
        )
        .route("/System/Logs/{name}", get(system::system_log_download))
        .route("/System/Logs/{name}/Lines", get(system::system_log_lines))
        .route(
            "/System/Configuration/Partial",
            post(system::update_server_configuration_partial),
        )
        .route("/QuickConnect/Enabled", get(system::quick_connect_enabled))
        .route(
            "/QuickConnect/Authorize",
            post(auth::quick_connect_authorize),
        )
        .route("/QuickConnect/Connect", get(auth::quick_connect_connect))
        .route(
            "/QuickConnect/Initiate",
            post(auth::quick_connect_initiate),
        )
        .route(
            "/Branding/Configuration",
            get(system::branding_configuration),
        )
        .route(
            "/System/Configuration/Branding",
            get(system::branding_configuration).post(system::update_branding_configuration),
        )
        .route(
            "/Branding/Splashscreen",
            get(system::branding_splashscreen)
                .post(system::upload_branding_splashscreen)
                .delete(system::delete_branding_splashscreen),
        )
        .route("/Playback/BitrateTest", get(system::bitrate_test))
        .route(
            "/Encoding/CodecConfiguration/Defaults",
            get(system::encoding_codec_configuration_defaults),
        )
        .route(
            "/Encoding/CodecInformation/Video",
            get(system::encoding_video_codec_information),
        )
        .route(
            "/System/Ext/ServerDomains",
            get(system::system_ext_server_domains),
        )
        .route("/Dlna/ProfileInfos", get(dlna::profile_infos))
        .route(
            "/Dlna/Profiles",
            get(dlna::profile_infos).post(dlna::create_profile),
        )
        .route("/Dlna/Profiles/Default", get(dlna::default_profile))
        .route(
            "/Dlna/Profiles/{profile_id}",
            get(dlna::profile_by_id)
                .post(dlna::update_profile)
                .delete(dlna::delete_profile),
        )
        .route("/description.xml", get(dlna::device_description))
        .route("/ClientLog/Document", post(system::client_log_document))
        .route("/Auth/Keys", get(auth::api_keys).post(auth::create_api_key))
        .route("/Auth/Keys/{key}", delete(auth::delete_api_key))
        .route("/Plugins", get(system::plugins))
        .route("/Plugins/{plugin_id}", delete(system::plugin_not_found))
        .route(
            "/Plugins/{plugin_id}/Configuration",
            get(system::plugin_not_found).post(system::plugin_not_found),
        )
        .route("/Plugins/{plugin_id}/Manifest", post(system::plugin_not_found))
        .route(
            "/Plugins/{plugin_id}/{version}",
            delete(system::plugin_not_found),
        )
        .route(
            "/Plugins/{plugin_id}/{version}/Disable",
            post(system::plugin_not_found),
        )
        .route(
            "/Plugins/{plugin_id}/{version}/Enable",
            post(system::plugin_not_found),
        )
        .route(
            "/Plugins/{plugin_id}/{version}/Image",
            get(system::plugin_not_found),
        )
        .route("/Plugins/{plugin_id}/Thumb", get(system::plugin_not_found))
        .route("/Packages", get(system::packages))
        .route("/Packages/{name}", get(system::package_by_name))
        .route(
            "/Packages/Installed/{name}",
            post(system::package_install_unavailable),
        )
        .route("/Packages/Updates", get(system::package_updates))
        .route(
            "/Packages/Installing/{package_id}",
            delete(system::package_install_unavailable),
        )
        .route(
            "/Repositories",
            get(system::repositories).post(system::update_repositories),
        )
        .route(
            "/Tmdb/ClientConfiguration",
            get(system::tmdb_client_configuration),
        )
        .route(
            "/System/Configuration/TmdbApiKey",
            post(system::update_tmdb_api_key),
        )
        .route("/Devices/Info", get(system::device_info))
        .route("/Auth/Providers", get(auth::auth_providers))
        .route(
            "/Auth/PasswordResetProviders",
            get(auth::password_reset_providers),
        )
        .route("/Channels", get(system::channels))
        .route("/Channels/Features", get(system::all_channel_features))
        .route("/Channels/Items/Latest", get(system::channel_items))
        .route("/Channels/{channel_id}", get(common::not_found))
        .route("/Channels/{channel_id}/Features", get(system::channel_features))
        .route("/Channels/{channel_id}/Items", get(system::channel_items))
        .route("/LiveTv/Info", get(system::live_tv_info))
        .route("/LiveTv/GuideInfo", get(system::live_tv_guide_info))
        .route(
            "/LiveTv/ChannelMappingOptions",
            get(system::live_tv_channel_mapping_options)
                .head(common::no_content)
                .options(common::no_content)
                .post(system::live_tv_unavailable)
                .put(system::live_tv_unavailable)
                .patch(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
        )
        .route("/LiveStreams/Open", post(system::live_stream_unavailable))
        .route("/LiveStreams/Close", post(system::live_stream_unavailable))
        .route(
            "/LiveStreams/MediaInfo",
            post(system::live_stream_media_info),
        )
        .route(
            "/LiveTv/ChannelMappings",
            get(common::empty_array)
                .head(common::no_content)
                .options(common::no_content)
                .post(system::live_tv_unavailable)
                .put(system::live_tv_unavailable)
                .patch(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
        )
        .route("/LiveTv/Channels", get(common::empty_list))
        .route("/LiveTv/Channels/{channel_id}", get(common::not_found))
        .route(
            "/LiveTv/ListingProviders",
            get(common::empty_array)
                .post(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/ListingProviders/Default",
            get(system::live_tv_default_listing_provider),
        )
        .route("/LiveTv/ListingProviders/Lineups", get(common::empty_array))
        .route(
            "/LiveTv/ListingProviders/Delete",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/ListingProviders/SchedulesDirect/Countries",
            get(system::localization_countries),
        )
        .route(
            "/LiveTv/TunerHosts",
            get(common::empty_array)
                .post(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
        )
        .route("/LiveTv/TunerHosts/Types", get(common::empty_array))
        .route(
            "/LiveTv/TunerHosts/Delete",
            post(system::live_tv_unavailable),
        )
        .route("/LiveTv/Tuners/Discover", get(common::empty_array))
        .route("/LiveTv/Tuners/Discvover", get(common::empty_array))
        .route(
            "/LiveTv/Tuners/{tuner_id}/Reset",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/Programs",
            get(common::empty_list).post(common::empty_list),
        )
        .route("/LiveTv/Programs/Recommended", get(common::empty_list))
        .route("/LiveTv/Programs/{program_id}", get(common::not_found))
        .route("/LiveTv/Recordings", get(common::empty_list))
        .route(
            "/LiveTv/AvailableRecordingOptions",
            get(items::available_recording_options),
        )
        .route("/LiveTv/Recordings/Series", get(common::empty_list))
        .route(
            "/LiveTv/Recordings/Folders",
            get(system::live_tv_recording_folders),
        )
        .route("/LiveTv/Recordings/Groups", get(common::empty_list))
        .route(
            "/LiveTv/Recordings/Groups/{group_id}",
            get(common::not_found),
        )
        .route(
            "/LiveTv/Recordings/{recording_id}",
            get(common::not_found).delete(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/SeriesTimers",
            get(common::empty_list).post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/SeriesTimers/{timer_id}",
            get(common::not_found)
                .post(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/Timers",
            get(common::empty_list).post(system::live_tv_unavailable),
        )
        .route("/LiveTv/Timers/Defaults", get(system::live_tv_timer_defaults))
        .route(
            "/LiveTv/Timers/{timer_id}",
            get(common::not_found)
                .post(system::live_tv_unavailable)
                .delete(system::live_tv_unavailable),
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
        .route(
            "/Library/SelectableMediaFolders",
            get(library::selectable_media_folders),
        )
        .route("/Libraries/AvailableOptions", get(library::available_options))
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
        .route(
            "/Library/VirtualFolders/Query",
            get(library::virtual_folders_query),
        )
        .route(
            "/Library/VirtualFolders/Name",
            post(library::rename_virtual_folder),
        )
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
            "/Users/AuthenticateByName",
            post(auth::authenticate_by_name),
        )
        .route("/Users", get(auth::list_users).post(auth::update_user_legacy))
        .route("/Users/Query", get(auth::users_query))
        .route("/Users/Prefixes", get(filters::users_prefixes))
        .route(
            "/Users/Configuration",
            post(auth::update_user_configuration_legacy),
        )
        .route("/Users/Password", post(auth::update_user_password_legacy))
        .route("/Users/ForgotPassword", post(auth::forgot_password))
        .route("/Users/ForgotPassword/Pin", post(auth::forgot_password))
        .route(
            "/Users/AuthenticateWithQuickConnect",
            post(auth::authenticate_with_quick_connect),
        )
        .route("/Users/Public", get(auth::public_users))
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
            "/Users/{user_id}/Items/{item_id}/SpecialFeatures",
            get(super::user_extras::user_item_special_features),
        )
        .route(
            "/Users/{user_id}/Images/Primary",
            get(images::user_avatar)
                .post(images::upload_user_avatar)
                .delete(images::delete_user_avatar),
        )
        .route(
            "/UserImage",
            get(images::current_user_avatar)
                .head(images::current_user_avatar_head)
                .post(images::upload_current_user_avatar)
                .delete(images::delete_current_user_avatar),
        )
        .route(
            "/Users/{user_id}/Images/Primary/Delete",
            post(images::delete_user_avatar),
        )
        .route("/UserViews", get(items::views))
        .route(
            "/UserViews/GroupingOptions",
            get(system::user_view_grouping_options),
        )
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
            post(playback::mark_played).delete(playback::mark_unplayed),
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
            "/Users/{user_id}/EasyPassword",
            post(auth::update_user_password),
        )
        .route(
            "/Users/{user_id}/Configuration",
            post(auth::update_user_configuration),
        )
        .route("/Users/{user_id}/Policy", post(auth::update_user_policy))
        .route("/Items", get(items::items).delete(items::delete_items))
        .route("/Items/File", get(super::user_extras::item_by_file))
        .route("/Items/Latest", get(items::latest_items_root))
        .route("/Items/Root", get(items::items_root))
        .route("/Items/Filters", get(super::user_extras::items_filters))
        .route("/Items/Filters2", get(super::user_extras::filters2))
        .route(
            "/Items/Suggestions",
            get(super::user_extras::items_suggestions),
        )
        .route(
            "/Items/{item_id}",
            get(items::item_by_id_public)
                .post(items::update_item)
                .delete(items::delete_single_item),
        )
        .route(
            "/Items/{item_id}/Ancestors",
            get(super::user_extras::item_ancestors),
        )
        .route(
            "/Items/{item_id}/CriticReviews",
            get(super::user_extras::item_critic_reviews),
        )
        .route("/Items/{item_id}/Collections", get(collect::item_collections))
        .route(
            "/Items/{item_id}/ContentType",
            post(items::update_item_content_type),
        )
        .route(
            "/Items/{item_id}/Download",
            get(super::user_extras::download_item).head(super::user_extras::download_item_head),
        )
        .route(
            "/Items/{item_id}/File",
            get(super::user_extras::item_file_info).head(super::user_extras::item_file_info_head),
        )
        .route(
            "/Items/{item_id}/ThemeMedia",
            get(super::user_extras::item_theme_media),
        )
        .route(
            "/Items/{item_id}/ThemeSongs",
            get(super::user_extras::item_theme_songs),
        )
        .route(
            "/Items/{item_id}/ThemeVideos",
            get(super::user_extras::item_theme_videos),
        )
        .route(
            "/Items/{item_id}/InstantMix",
            get(super::user_extras::item_instant_mix),
        )
        .route(
            "/Items/{item_id}/Intros",
            get(super::user_extras::item_intros),
        )
        .route(
            "/Items/{item_id}/LocalTrailers",
            get(super::user_extras::item_local_trailers),
        )
        .route(
            "/Items/{item_id}/SpecialFeatures",
            get(super::user_extras::item_special_features),
        )
        .route(
            "/Items/{item_id}/ThumbnailSet",
            get(super::user_extras::thumbnail_set),
        )
        .route("/Items/{item_id}/Images", get(images::item_images))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(images::get_item_image)
                .head(super::user_extras::item_image_head)
                .post(images::upload_item_image)
                .delete(images::delete_item_image),
        )
        .route(
            "/Items/{item_id}/Images/{first}/{second}",
            get(images::get_item_image_with_index)
                .head(super::user_extras::item_image_index_head)
                .post(images::upload_item_image_with_index)
                .delete(images::delete_item_image_with_index),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{image_index}/Index",
            get(images::get_item_image_with_index)
                .head(super::user_extras::item_image_index_head)
                .post(images::upload_item_image_with_index),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{image_index}/{tag}/{format}/{max_width}/{max_height}/{percent_played}/{unplayed_count}",
            get(images::get_item_image_legacy_path).head(images::get_item_image_legacy_path),
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
            get(playback::playback_info).post(playback::playback_info),
        )
        .route("/Items/{item_id}/Refresh", post(items::scan_handler))
        .route("/Items/{item_id}/Similar", get(items::similar_items))
        .route("/Albums/{item_id}/Similar", get(items::similar_items))
        .route("/Games/{item_id}/Similar", get(items::similar_items))
        .route("/Artists/{item_id}/Similar", get(items::similar_items))
        .route("/Artists", get(persons::artists))
        .route("/Artists/AlbumArtists", get(persons::album_artists))
        .route(
            "/Artists/InstantMix",
            get(super::user_extras::artist_instant_mix),
        )
        .route("/Artists/Prefixes", get(filters::artists_prefixes))
        .route("/Artists/{name}", get(persons::artist_by_name))
        .route(
            "/Artists/{item_id}/InstantMix",
            get(super::user_extras::artist_instant_mix_by_id),
        )
        .route(
            "/Artists/{name}/Images/{image_type}",
            get(persons::person_image).head(common::no_content),
        )
        .route(
            "/Artists/{name}/Images/{image_type}/{index}",
            get(persons::person_image_with_index).head(common::no_content),
        )
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
            get(images::remote_images_providers),
        )
        .route("/Items/{item_id}/Subtitles", get(items::item_subtitles))
        .route(
            "/Items/{item_id}/Subtitles/{index}/Delete",
            post(items::delete_item_subtitle),
        )
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
        .route(
            "/Items/{item_id}/MetadataEditor",
            get(items::metadata_editor_info).post(super::system::metadata_editor),
        )
        .route(
            "/Items/{item_id}/MakePrivate",
            post(items::make_item_private),
        )
        .route("/Items/{item_id}/MakePublic", post(items::make_item_public))
        .route(
            "/Items/RemoteSearch/{item_type}",
            post(items::remote_search),
        )
        .route("/Items/RemoteSearch/Book", post(items::remote_search))
        .route("/Items/RemoteSearch/BoxSet", post(items::remote_search))
        .route("/Items/RemoteSearch/Game", post(items::remote_search))
        .route(
            "/Items/RemoteSearch/Image",
            get(images::image_by_name_remote).post(items::remote_search),
        )
        .route("/Items/RemoteSearch/Movie", post(items::remote_search))
        .route("/Items/RemoteSearch/MusicAlbum", post(items::remote_search))
        .route("/Items/RemoteSearch/MusicArtist", post(items::remote_search))
        .route("/Items/RemoteSearch/MusicVideo", post(items::remote_search))
        .route("/Items/RemoteSearch/Person", post(items::remote_search))
        .route("/Items/RemoteSearch/Series", post(items::remote_search))
        .route("/Items/RemoteSearch/Trailer", post(items::remote_search))
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
        .route("/AudioBooks/NextUp", get(items::audiobooks_next_up))
        .route("/Search/Hints", get(items::search_hints))
        .route("/Genres", get(filters::genres))
        .route("/Genres/{name}", get(super::user_extras::genre_by_name))
        .route(
            "/Genres/{name}/Images/{image_type}",
            get(super::user_extras::genre_image).head(common::no_content),
        )
        .route(
            "/Genres/{name}/Images/{image_type}/{index}",
            get(super::user_extras::genre_image_with_index).head(common::no_content),
        )
        .route("/GameGenres", get(filters::game_genres))
        .route("/Games/SystemSummaries", get(system::game_system_summaries))
        .route(
            "/GameGenres/{name}",
            get(super::user_extras::game_genre_by_name),
        )
        .route(
            "/GameGenres/{name}/Images/{image_type}",
            get(super::user_extras::genre_image).head(common::no_content),
        )
        .route(
            "/GameGenres/{name}/Images/{image_type}/{index}",
            get(super::user_extras::genre_image_with_index).head(common::no_content),
        )
        .route(
            "/MusicGenres/InstantMix",
            get(super::user_extras::music_genre_instant_mix),
        )
        .route(
            "/MusicGenres/{name}/InstantMix",
            get(super::user_extras::music_genre_instant_mix_by_name),
        )
        .route("/Persons", get(filters::persons))
        .route("/Persons/{name}", get(persons::person_by_name))
        .route("/Persons/{name}/Items", get(persons::person_items))
        .route(
            "/Persons/{name}/Images/{image_type}",
            get(persons::person_image).head(common::no_content),
        )
        .route(
            "/Persons/{name}/Images/{first}/{second}",
            get(persons::person_image_with_index).head(common::no_content),
        )
        .route("/Studios", get(filters::studios))
        .route("/Studios/{name}", get(super::user_extras::studio_by_name))
        .route(
            "/Studios/{name}/Images/{image_type}",
            get(super::user_extras::studio_image).head(common::no_content),
        )
        .route(
            "/Studios/{name}/Images/{image_type}/{index}",
            get(super::user_extras::studio_image_with_index).head(common::no_content),
        )
        .route("/Tags", get(filters::tags))
        .route("/Years", get(filters::years))
        .route("/Years/{year}", get(filters::year_by_year))
        .route("/OfficialRatings", get(filters::official_ratings))
        .route("/Containers", get(filters::containers))
        .route("/VideoCodecs", get(filters::video_codecs))
        .route("/ExtendedVideoTypes", get(filters::extended_video_types))
        .route("/AudioCodecs", get(super::user_extras::audio_codecs))
        .route("/AudioLayouts", get(super::user_extras::audio_layouts))
        .route("/SubtitleCodecs", get(super::user_extras::subtitle_codecs))
        .route(
            "/StreamLanguages",
            get(super::user_extras::stream_languages),
        )
        .route("/ItemTypes", get(super::user_extras::item_types))
        .route(
            "/Videos/{item_id}/AdditionalParts",
            get(super::user_extras::video_additional_parts),
        )
        .route("/Videos/MergeVersions", post(items::merge_versions))
        .route(
            "/Videos/ActiveEncodings",
            get(items::active_encodings).delete(items::stop_encodings),
        )
        .route(
            "/Videos/{item_id}/AlternateSources",
            get(items::alternate_sources).delete(items::delete_alternate_source),
        )
        .route(
            "/Videos/{item_id}/Trickplay/{width}/tiles.m3u8",
            get(super::user_extras::trickplay_playlist),
        )
        .route(
            "/Videos/{item_id}/Trickplay/{width}/{index}",
            get(super::user_extras::trickplay_tile),
        )
        .route(
            "/Videos/{video_id}/{media_source_id}/Attachments/{index}",
            get(super::user_extras::attachment_file),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(stream_video).head(stream_video_head),
        )
        .route(
            "/Videos/{item_id}/stream",
            get(stream_video_simple).head(stream_video_simple_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream.{container}",
            get(stream_video_with_source).head(stream_video_with_source_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream",
            get(stream_video_with_source_simple).head(stream_video_with_source_simple_head),
        )
        .route(
            "/Videos/{item_id}/original.{container}",
            get(stream_video_original_container).head(stream_video_original_container_head),
        )
        .route(
            "/Videos/{item_id}/original",
            get(stream_video_original).head(stream_video_original_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/original.{container}",
            get(stream_video_original_with_source_container)
                .head(stream_video_original_with_source_container_head),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/original",
            get(stream_video_original_with_source).head(stream_video_original_with_source_head),
        )
        .route(
            "/Videos/{item_id}/subtitles.m3u8",
            get(common::not_found),
        )
        .route("/Videos/{item_id}/index.bif", get(common::not_found))
        .route("/Videos/{item_id}/live.m3u8", get(common::not_found))
        .route("/Videos/{item_id}/main.m3u8", get(common::not_found))
        .route(
            "/Videos/{item_id}/master.m3u8",
            get(common::not_found).head(common::not_found),
        )
        .route(
            "/Videos/{item_id}/hls/{playlist_id}/{segment_file}",
            get(common::not_found),
        )
        .route(
            "/Videos/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(common::not_found).head(common::not_found),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/Subtitles/{index}/subtitles.m3u8",
            get(common::not_found),
        )
        .route(
            "/Videos/{item_id}/Subtitles",
            post(items::upload_subtitle),
        )
        .route(
            "/Videos/{item_id}/Subtitles/{index}",
            delete(items::delete_item_subtitle),
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
            "/Videos/{item_id}/{media_source_id}/Subtitles/{index}/{start_ticks}/Stream.{format}",
            get(stream_subtitle_with_ticks).head(stream_subtitle_with_ticks_head),
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
            "/Audio/{item_id}/universal.{container}",
            get(stream_audio_container).head(stream_audio_container_head),
        )
        .route(
            "/Audio/{item_id}/stream",
            get(stream_audio_simple).head(stream_audio_simple_head),
        )
        .route(
            "/Audio/{item_id}/stream.{container}",
            get(stream_audio_container).head(stream_audio_container_head),
        )
        .route("/Audio/{item_id}/main.m3u8", get(common::not_found))
        .route(
            "/Audio/{item_id}/master.m3u8",
            get(common::not_found).head(common::not_found),
        )
        .route(
            "/Audio/{item_id}/hls1/{playlist_id}/{segment_file}",
            get(common::not_found).head(common::not_found),
        )
        .route(
            "/Audio/{item_id}/Lyrics",
            get(items::item_lyrics)
                .post(items::upload_lyrics)
                .delete(items::delete_lyrics),
        )
        .route(
            "/Audio/{item_id}/RemoteSearch/Lyrics",
            get(common::empty_array),
        )
        .route(
            "/Audio/{item_id}/RemoteSearch/Lyrics/{lyric_id}",
            post(items::remote_lyrics_unavailable),
        )
        .route(
            "/Providers/Lyrics/{lyric_id}",
            get(items::remote_lyrics_unavailable),
        )
        .route(
            "/Providers/Subtitles/Subtitles/{param}",
            get(items::subtitle_provider_info),
        )
        .route("/FallbackFont/Fonts", get(system::fallback_fonts))
        .route(
            "/FallbackFont/Fonts/{name}",
            get(system::fallback_font_file),
        )
        .route(
            "/MediaSegments/{item_id}",
            get(super::user_extras::media_segments),
        )
        .route(
            "/Albums/{item_id}/InstantMix",
            get(super::user_extras::item_instant_mix),
        )
        .route(
            "/Playlists/{item_id}/InstantMix",
            get(super::user_extras::item_instant_mix),
        )
        .route(
            "/Songs/{item_id}/InstantMix",
            get(super::user_extras::item_instant_mix),
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
        .route("/Sessions/Logout", post(sessions::logout))
        .route("/Sessions/Viewing", post(sessions::report_viewing))
        .route("/Sessions/Playing/Ping", post(sessions::playback_ping))
        .route("/Sessions/PlayQueue", get(super::user_extras::play_queue))
        .route(
            "/Sessions/{session_id}/Viewing",
            post(sessions::touch_session_by_id),
        )
        .route(
            "/Sessions/{session_id}/Command",
            post(sessions::touch_session_by_id),
        )
        .route(
            "/Sessions/{session_id}/Playing",
            post(sessions::touch_session_by_id),
        )
        .route(
            "/Sessions/{session_id}/Command/{command}",
            post(sessions::touch_session_command),
        )
        .route(
            "/Sessions/{session_id}/Message",
            post(sessions::touch_session_by_id),
        )
        .route(
            "/Sessions/{session_id}/Playing/{command}",
            post(sessions::playstate_command),
        )
        .route(
            "/Sessions/{session_id}/System/{command}",
            post(sessions::touch_session_command),
        )
        .route(
            "/Sessions/{session_id}/User/{user_id}",
            post(sessions::session_add_user).delete(sessions::session_remove_user),
        )
        .route(
            "/Sessions/{session_id}/Users/{user_id}",
            post(sessions::session_add_user).delete(sessions::session_remove_user),
        )
        .route("/ScheduledTasks", get(system::scheduled_tasks))
        .route("/ScheduledTasks/{task_id}", get(system::scheduled_task))
        .route(
            "/ScheduledTasks/{task_id}/Triggers",
            get(system::scheduled_task_triggers).post(system::update_scheduled_task_triggers),
        )
        .route(
            "/ScheduledTasks/Running/{task_id}",
            post(system::start_scheduled_task).delete(system::stop_scheduled_task),
        )
        .route(
            "/UserItems/Resume",
            get(super::user_extras::user_items_resume),
        )
        .route(
            "/UserItems/{item_id}/UserData",
            get(playback::get_user_item_data).post(playback::update_user_item_data),
        )
        .route(
            "/UserPlayedItems/{item_id}",
            post(playback::current_user_mark_played).delete(playback::current_user_mark_unplayed),
        )
        .route(
            "/UserFavoriteItems/{item_id}",
            post(playback::current_user_favorite_item)
                .delete(playback::current_user_unfavorite_item),
        )
        .route(
            "/UserItems/{item_id}/Rating",
            post(playback::current_user_set_rating).delete(playback::current_user_delete_rating),
        )
        .route(
            "/PlayingItems/{item_id}",
            post(playback::current_user_playing_item_start)
                .delete(playback::current_user_playing_item_stop),
        )
        .route(
            "/PlayingItems/{item_id}/Progress",
            post(playback::current_user_playing_item_progress),
        )
        .route(
            "/UserSettings/{user_id}",
            get(super::user_extras::get_user_settings)
                .post(super::user_extras::update_user_settings),
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
            post(collect::add_to_collection).delete(collect::remove_from_collection_delete),
        )
        .route(
            "/Collections/{collection_id}/Items/Delete",
            post(collect::remove_from_collection_batch),
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
        .route("/Playlists/{playlist_id}/Users", get(collect::get_playlist_users))
        .route(
            "/Playlists/{playlist_id}/Users/{user_id}",
            get(collect::get_playlist_user)
                .post(collect::update_playlist_user)
                .delete(collect::remove_playlist_user),
        )
        // ── Backup / Restore ──
        .route("/BackupRestore/BackupInfo", get(backup::backup_info))
        .route("/BackupRestore/Restore", post(backup::restore_backup))
        .route("/BackupRestore/RestoreData", post(backup::restore_backup))
        .route("/Backup", get(backup::list_backups))
        .route("/Backup/Create", post(backup::create_backup))
        .route("/Backup/Manifest", get(backup::backup_manifest))
        .route("/Backup/Restore", post(backup::restore_backup))
        // ── Branding CSS ──
        .route("/Branding/Css", get(system::branding_css))
        .route("/Branding/Css.css", get(system::branding_css))
        // ── Devices 补全 ──
        .route("/Devices", get(system::devices).delete(system::delete_devices))
        .route(
            "/Devices/CameraUploads",
            get(system::camera_uploads).post(system::upload_camera),
        )
        .route("/Devices/Delete", post(system::delete_devices))
        .route(
            "/Devices/Options",
            get(system::device_options).post(system::update_device_options),
        )
        // ── Environment 补全 ──
        .route("/Environment/NetworkDevices", get(common::empty_array))
        .route("/Environment/NetworkShares", get(common::empty_array))
        // ── Feature ──
        .route("/Features", get(system::features))
        // ── GenericUI ──
        .route("/UI/View", get(system::ui_view))
        .route("/UI/Command", post(system::ui_command))
        // ── User Images 补全 ──
        .route(
            "/Users/{user_id}/Images/{image_type}",
            get(images::user_avatar)
                .head(common::no_content)
                .post(images::upload_user_avatar)
                .delete(images::delete_user_avatar),
        )
        .route(
            "/Users/{user_id}/Images/{image_type}/{index}",
            get(images::user_avatar)
                .head(common::no_content)
                .post(images::upload_user_avatar)
                .delete(images::delete_user_avatar),
        )
        .route(
            "/Users/{user_id}/GroupingOptions",
            get(system::user_view_grouping_options),
        )
        // ── MusicGenres 补全 ──
        .route("/MusicGenres", get(filters::music_genres))
        .route(
            "/MusicGenres/{name}",
            get(super::user_extras::music_genre_by_name),
        )
        .route(
            "/MusicGenres/{name}/Images/{image_type}",
            get(super::user_extras::genre_image).head(common::no_content),
        )
        .route(
            "/MusicGenres/{name}/Images/{image_type}/{index}",
            get(super::user_extras::genre_image_with_index).head(common::no_content),
        )
        // ── Notifications ──
        .route("/Notifications/Types", get(system::notification_types))
        .route("/Notifications/Admin", post(system::send_admin_notification))
        .route("/Notifications/Services", get(system::notification_services))
        .route(
            "/Notifications/Services/Defaults",
            get(system::notification_services_defaults),
        )
        .route(
            "/Notifications/Services/Test",
            post(system::notification_services_test),
        )
        .route(
            "/Notifications/{user_id}",
            get(system::user_notifications),
        )
        .route(
            "/Notifications/{user_id}/Summary",
            get(system::user_notifications_summary),
        )
        .route(
            "/Notifications/{user_id}/Read",
            post(system::mark_notifications_read),
        )
        .route(
            "/Notifications/{user_id}/Unread",
            post(system::mark_notifications_unread),
        )
        .route(
            "/Notification/SMTP/Test/{user_id}",
            post(system::smtp_notification_test),
        )
        // Reports / News
        .route("/News/Product", get(system::news_product))
        .route("/Reports/Headers", get(system::reports_headers))
        .route("/Reports/Activities", get(system::reports_activities))
        .route("/Reports/Items", get(system::reports_items))
        .route("/Reports/Items/Download", get(system::reports_items_download))
        // Image-by-name compatibility
        .route("/Images/General", get(system::image_by_name_general))
        .route("/Images/General/{name}/{image_type}", get(common::image))
        .route("/Images/MediaInfo", get(system::image_by_name_media_info))
        .route("/Images/MediaInfo/{theme}/{name}", get(common::image))
        .route("/Images/Ratings", get(system::image_by_name_ratings))
        .route("/Images/Ratings/{theme}/{name}", get(common::image))
        .route("/Images/Remote", get(images::image_by_name_remote))
        // ── OpenApi / Swagger ──
        .route("/openapi", get(system::openapi_json))
        .route("/openapi.json", get(system::openapi_json))
        .route("/swagger", get(system::openapi_json))
        .route("/swagger.json", get(system::openapi_json))
        // ── Party / SyncPlay ──
        .route(
            "/Parties",
            get(common::empty_array).post(system::party_unavailable),
        )
        .route("/Parties/Info", get(system::party_unavailable))
        .route("/Parties/{id}/Join", post(system::party_unavailable))
        .route("/Parties/Leave", post(system::party_unavailable))
        // SyncPlay compatibility. Local sessions are tracked separately; group playback is a no-op.
        .route("/SyncPlay/List", get(common::empty_array))
        .route("/SyncPlay/{id}", get(common::not_found))
        .route("/SyncPlay/Buffering", post(common::no_content))
        .route("/SyncPlay/Join", post(system::sync_play_unavailable))
        .route("/SyncPlay/Leave", post(common::no_content))
        .route("/SyncPlay/MovePlaylistItem", post(common::no_content))
        .route("/SyncPlay/New", post(system::sync_play_unavailable))
        .route("/SyncPlay/NextItem", post(common::no_content))
        .route("/SyncPlay/Pause", post(common::no_content))
        .route("/SyncPlay/Ping", post(common::no_content))
        .route("/SyncPlay/PreviousItem", post(common::no_content))
        .route("/SyncPlay/Queue", post(common::no_content))
        .route("/SyncPlay/Ready", post(common::no_content))
        .route("/SyncPlay/RemoveFromPlaylist", post(common::no_content))
        .route("/SyncPlay/Seek", post(common::no_content))
        .route("/SyncPlay/SetIgnoreWait", post(common::no_content))
        .route("/SyncPlay/SetNewQueue", post(common::no_content))
        .route("/SyncPlay/SetPlaylistItem", post(common::no_content))
        .route("/SyncPlay/SetRepeatMode", post(common::no_content))
        .route("/SyncPlay/SetShuffleMode", post(common::no_content))
        .route("/SyncPlay/Stop", post(common::no_content))
        .route("/SyncPlay/Unpause", post(common::no_content))
        // ── Sync ──
        .route("/Sync/Items/Ready", get(common::empty_array))
        .route("/Sync/JobItems", get(system::sync_empty_query_result))
        .route(
            "/Sync/JobItems/{id}",
            get(common::not_found).delete(system::sync_unavailable),
        )
        .route("/Sync/JobItems/{id}/File", get(common::not_found))
        .route(
            "/Sync/JobItems/{id}/AdditionalFiles",
            get(common::empty_array),
        )
        .route(
            "/Sync/Jobs",
            get(system::sync_empty_query_result).post(system::sync_unavailable),
        )
        .route(
            "/Sync/Jobs/{id}",
            get(common::not_found)
                .post(system::sync_unavailable)
                .delete(system::sync_unavailable),
        )
        .route("/Sync/Jobs/{id}/Delete", post(system::sync_unavailable))
        .route("/Sync/Options", get(system::sync_options))
        .route("/Sync/Targets", get(system::sync_targets))
        .route("/Sync/Data", post(system::sync_data))
        .route("/Sync/Items/Cancel", post(system::sync_empty_response))
        .route("/Sync/OfflineActions", post(system::sync_empty_response))
        .route("/Sync/{item_id}/Status", post(system::sync_empty_response))
        .route("/Sync/{target_id}/Items", delete(system::sync_empty_response))
        .route(
            "/Sync/{target_id}/Items/Delete",
            post(system::sync_empty_response),
        )
        .route("/Sync/JobItems/{id}/Delete", post(system::sync_unavailable))
        .route("/Sync/JobItems/{id}/Enable", post(system::sync_unavailable))
        .route(
            "/Sync/JobItems/{id}/MarkForRemoval",
            post(system::sync_unavailable),
        )
        .route(
            "/Sync/JobItems/{id}/Transferred",
            post(system::sync_unavailable),
        )
        .route(
            "/Sync/JobItems/{id}/UnmarkForRemoval",
            post(system::sync_unavailable),
        )
        // ── User 补全 ──
        .route(
            "/Users/{user_id}/Configuration/Partial",
            post(auth::update_user_configuration),
        )
        .route("/Users/ItemAccess", get(system::users_item_access))
        .route(
            "/Users/{user_id}/TypedSettings/{key}",
            get(super::user_extras::get_typed_setting)
                .post(super::user_extras::update_typed_setting),
        )
        // ── UserLibrary 补全 ──
        .route("/Users/{user_id}/Items/Root", get(items::user_items_root))
        .route("/Items/Access", post(system::items_access))
        .route("/Items/Shared/Leave", post(system::items_shared_leave))
        // ── Trailers ──
        .route("/Trailers", get(items::trailers))
        // Legacy Emby user-usage-stats plugin endpoints.
        .route(
            "/user_usage_stats/user_list",
            get(system::user_usage_stats_user_list),
        )
        .route(
            "/user_usage_stats/type_filter_list",
            get(system::user_usage_stats_type_filter_list),
        )
        .route(
            "/user_usage_stats/session_list",
            get(system::user_usage_stats_session_list),
        )
        .route(
            "/user_usage_stats/process_list",
            get(system::user_usage_stats_process_list),
        )
        .route(
            "/user_usage_stats/user_activity",
            get(system::user_usage_stats_user_activity),
        )
        .route(
            "/user_usage_stats/PlayActivity",
            get(system::user_usage_stats_play_activity),
        )
        .route(
            "/user_usage_stats/MoviesReport",
            get(system::user_usage_stats_movies_report),
        )
        .route(
            "/user_usage_stats/TvShowsReport",
            get(system::user_usage_stats_tvshows_report),
        )
        .route(
            "/user_usage_stats/UserPlaylist",
            get(system::user_usage_stats_user_playlist),
        )
        .route(
            "/user_usage_stats/resource_usage",
            get(system::user_usage_stats_resource_usage),
        )
        .route(
            "/user_usage_stats/DurationHistogramReport",
            get(system::user_usage_stats_duration_histogram_report),
        )
        .route(
            "/user_usage_stats/HourlyReport",
            get(system::user_usage_stats_hourly_report),
        )
        .route(
            "/user_usage_stats/{breakdown_type}/BreakdownReport",
            get(system::user_usage_stats_breakdown_report),
        )
        .route(
            "/user_usage_stats/{user_id}/{date}/GetItems",
            get(system::user_usage_stats_user_date_items),
        )
        .route(
            "/user_usage_stats/import_backup",
            post(system::user_usage_stats_import_backup),
        )
        .route(
            "/user_usage_stats/load_backup",
            get(system::user_usage_stats_load_backup).post(system::user_usage_stats_load_backup),
        )
        .route(
            "/user_usage_stats/save_backup",
            get(system::user_usage_stats_save_backup).post(system::user_usage_stats_save_backup),
        )
        .route(
            "/user_usage_stats/submit_custom_query",
            post(system::user_usage_stats_submit_custom_query),
        )
        .route(
            "/user_usage_stats/user_manage/{action}/{id}",
            get(system::user_usage_stats_user_manage).post(system::user_usage_stats_user_manage),
        )
        // ── WebApp 补全 ──
        .route("/web/strings", get(system::web_strings))
        .route("/web/stringset", get(system::web_string_set))
        // ── LibraryStructure 补全 ──
        .route(
            "/Library/VirtualFolders/Paths/Delete",
            post(library::delete_virtual_folder_path),
        )
        // ── LiveTv 补全 ──
        .route("/LiveTv/ChannelTags", get(common::empty_array))
        .route("/LiveTv/ChannelTags/Prefixes", get(common::empty_array))
        .route("/LiveTv/EPG", get(system::live_tv_guide_info))
        .route("/LiveTv/Folder", get(common::not_found))
        .route(
            "/LiveTv/ListingProviders/Available",
            get(common::empty_array),
        )
        .route("/LiveTv/Manage/Channels", get(common::empty_list))
        .route(
            "/LiveTv/Manage/Channels/{id}/Disabled",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/Manage/Channels/{id}/SortIndex",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/Recordings/{id}/Delete",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/LiveRecordings/{id}/stream",
            get(common::not_found),
        )
        .route(
            "/LiveTv/LiveStreamFiles/{id}/stream.{container}",
            get(common::not_found),
        )
        .route(
            "/LiveTv/SeriesTimers/{id}/Delete",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/Timers/{id}/Delete",
            post(system::live_tv_unavailable),
        )
        .route(
            "/LiveTv/TunerHosts/Default/{type}",
            get(system::live_tv_default_tuner_host),
        )
        // ── DlnaServer ──
        .route("/Dlna/{id}/description", get(dlna::device_description))
        .route("/Dlna/{id}/description.xml", get(dlna::device_description))
        .route("/Dlna/{id}/icons/{filename}", get(common::image))
        .route("/Dlna/icons/{filename}", get(common::image))
        .route(
            "/Dlna/{id}/connectionmanager/connectionmanager",
            get(dlna::connection_manager_description),
        )
        .route(
            "/Dlna/{id}/connectionmanager/connectionmanager.xml",
            get(dlna::connection_manager_description),
        )
        .route(
            "/Dlna/{id}/contentdirectory/contentdirectory",
            get(dlna::content_directory_description),
        )
        .route(
            "/Dlna/{id}/contentdirectory/contentdirectory.xml",
            get(dlna::content_directory_description),
        )
        .route(
            "/Dlna/{id}/connectionmanager/control",
            post(dlna::connection_manager_control),
        )
        .route(
            "/Dlna/{id}/contentdirectory/control",
            post(dlna::content_directory_control),
        )
        .route("/websocket", get(ws::ws_handler))
        .route("/WebSocket", get(ws::ws_handler))
}

#[cfg(test)]
mod tests {
    #[test]
    fn trickplay_routes_include_openapi_tile_path() {
        let routes = include_str!("routes.rs");

        assert!(routes.contains("/Videos/{item_id}/Trickplay/{width}/tiles.m3u8"));
        assert!(routes.contains("/Videos/{item_id}/Trickplay/{width}/{index}"));
    }

    #[test]
    fn emby_video_stream_routes_include_media_source_variants() {
        let routes = include_str!("routes.rs");

        assert!(routes.contains("/Videos/{item_id}/{media_source_id}/stream.{container}"));
        assert!(routes.contains("/Videos/{item_id}/{media_source_id}/stream"));
        assert!(routes.contains("/Videos/{item_id}/original"));
        assert!(routes.contains("/Videos/{item_id}/{media_source_id}/original"));
    }

    #[test]
    fn api_routes_build_without_path_pattern_panics() {
        let _ = super::api_routes();
    }
}
