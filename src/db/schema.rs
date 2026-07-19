pub type Migration = (&'static str, &'static str);

pub fn migrations() -> Vec<Migration> {
    vec![
        (
            r#"CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT, display_name TEXT NOT NULL, is_admin BIGINT NOT NULL DEFAULT 0, is_disabled BIGINT NOT NULL DEFAULT 0, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL, last_login_at BIGINT)"#,
            "failed to migrate users",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_users_username ON users(username)",
            "failed to create users username index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS access_tokens (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE, name TEXT, device_id TEXT, created_at BIGINT NOT NULL, last_used_at BIGINT, expires_at BIGINT, revoked_at BIGINT, FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE)"#,
            "failed to migrate access_tokens",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS api_keys (id TEXT PRIMARY KEY, access_token TEXT NOT NULL UNIQUE, name TEXT NOT NULL, user_id TEXT NOT NULL, created_at BIGINT NOT NULL, last_used_at BIGINT, FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE)"#,
            "failed to migrate api_keys",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys(user_id)",
            "failed to create api key user index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_access_tokens_user_id ON access_tokens(user_id)",
            "failed to create access token user index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS libraries (id TEXT PRIMARY KEY, name TEXT NOT NULL, collection_type TEXT NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL)"#,
            "failed to migrate libraries",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_libraries_collection_type ON libraries(collection_type)",
            "failed to create libraries collection type index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS library_paths (id TEXT PRIMARY KEY, library_id TEXT NOT NULL, path TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL, FOREIGN KEY(library_id) REFERENCES libraries(id) ON DELETE CASCADE)"#,
            "failed to migrate library_paths",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_library_paths_library_id ON library_paths(library_id)",
            "failed to create library paths library index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_items (id TEXT PRIMARY KEY, title TEXT NOT NULL, path TEXT NOT NULL UNIQUE, library_id TEXT NOT NULL, parent_id TEXT NOT NULL, item_type TEXT NOT NULL, is_folder BIGINT NOT NULL DEFAULT 0, is_public BIGINT NOT NULL DEFAULT 1, container TEXT, overview TEXT, official_rating TEXT, extended_video_type TEXT, production_year BIGINT, premiere_date TEXT, runtime_ticks BIGINT, size_bytes BIGINT, season_number BIGINT, episode_number BIGINT, modified_at BIGINT NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL DEFAULT 0)"#,
            "failed to migrate media_items",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_parent_id ON media_items(parent_id)",
            "failed to create media parent index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_item_type ON media_items(item_type)",
            "failed to create media item type index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_streams (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, stream_index BIGINT NOT NULL, stream_type TEXT NOT NULL, codec TEXT, profile TEXT, codec_tag TEXT, language TEXT, title TEXT, comment TEXT, bit_rate BIGINT, width BIGINT, height BIGINT, aspect_ratio TEXT, average_frame_rate DOUBLE PRECISION, real_frame_rate DOUBLE PRECISION, reference_frame_rate DOUBLE PRECISION, channels BIGINT, channel_layout TEXT, sample_rate BIGINT, bit_depth BIGINT, ref_frames BIGINT, is_interlaced BIGINT NOT NULL DEFAULT 0, is_avc BIGINT, is_anamorphic BIGINT, pixel_format TEXT, level BIGINT, color_range TEXT, color_space TEXT, color_transfer TEXT, color_primaries TEXT, time_base TEXT, codec_time_base TEXT, nal_length_size TEXT, rotation BIGINT, video_range TEXT, video_range_type TEXT, hdr10_plus_present_flag BIGINT, is_default BIGINT NOT NULL DEFAULT 0, is_forced BIGINT NOT NULL DEFAULT 0, is_hearing_impaired BIGINT NOT NULL DEFAULT 0, is_original BIGINT, path TEXT, is_external BIGINT NOT NULL DEFAULT 0, created_at BIGINT NOT NULL, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, UNIQUE(item_id, stream_index))"#,
            "failed to migrate media_streams",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_streams_item_id ON media_streams(item_id)",
            "failed to create media stream item index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS user_data (user_id TEXT NOT NULL, item_id TEXT NOT NULL, is_favorite BIGINT NOT NULL DEFAULT 0, played BIGINT NOT NULL DEFAULT 0, playback_position_ticks BIGINT NOT NULL DEFAULT 0, played_percentage DOUBLE PRECISION, play_count BIGINT NOT NULL DEFAULT 0, last_played_at BIGINT, updated_at BIGINT NOT NULL, PRIMARY KEY(user_id, item_id), FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE)"#,
            "failed to migrate user_data",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_user_data_item_id ON user_data(item_id)",
            "failed to create user data item index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS people (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "failed to migrate people",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_people (item_id TEXT NOT NULL, person_id TEXT NOT NULL, role TEXT, person_type TEXT, sort_order BIGINT NOT NULL DEFAULT 0, PRIMARY KEY(item_id, person_id, person_type), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(person_id) REFERENCES people(id) ON DELETE CASCADE)"#,
            "failed to migrate media_people",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS genres (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "failed to migrate genres",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_genres (item_id TEXT NOT NULL, genre_id TEXT NOT NULL, PRIMARY KEY(item_id, genre_id), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(genre_id) REFERENCES genres(id) ON DELETE CASCADE)"#,
            "failed to migrate media_genres",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS tags (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "failed to migrate tags",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_tags (item_id TEXT NOT NULL, tag_id TEXT NOT NULL, PRIMARY KEY(item_id, tag_id), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE)"#,
            "failed to migrate media_tags",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS studios (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "failed to migrate studios",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_studios (item_id TEXT NOT NULL, studio_id TEXT NOT NULL, PRIMARY KEY(item_id, studio_id), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(studio_id) REFERENCES studios(id) ON DELETE CASCADE)"#,
            "failed to migrate media_studios",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS provider_ids (item_id TEXT NOT NULL, provider TEXT NOT NULL, provider_item_id TEXT NOT NULL, PRIMARY KEY(item_id, provider), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE)"#,
            "failed to migrate provider_ids",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS image_assets (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, image_type TEXT NOT NULL, image_index BIGINT NOT NULL DEFAULT 0, path TEXT, etag TEXT, width BIGINT, height BIGINT, size_bytes BIGINT, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, UNIQUE(item_id, image_type, image_index))"#,
            "failed to migrate image_assets",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS activity_log (id TEXT PRIMARY KEY, name TEXT NOT NULL, log_type TEXT NOT NULL, user_id TEXT, item_id TEXT, severity TEXT NOT NULL DEFAULT 'Info', created_at BIGINT NOT NULL)"#,
            "failed to migrate activity_log",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_activity_log_created_at ON activity_log(created_at DESC)",
            "failed to create activity log index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS task_results (task_id TEXT PRIMARY KEY, status TEXT NOT NULL, start_time BIGINT, end_time BIGINT, message TEXT)"#,
            "failed to migrate task_results",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at BIGINT NOT NULL)"#,
            "failed to migrate app_settings",
        ),
    ]
}

pub fn optional_migrations() -> Vec<Migration> {
    vec![
        (
            "ALTER TABLE media_items ADD COLUMN library_id TEXT NOT NULL DEFAULT ''",
            "add media_items.library_id",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN is_folder BIGINT NOT NULL DEFAULT 0",
            "add media_items.is_folder",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN is_public BIGINT NOT NULL DEFAULT 1",
            "add media_items.is_public",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN overview TEXT",
            "add media_items.overview",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN production_year BIGINT",
            "add media_items.production_year",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN premiere_date TEXT",
            "add media_items.premiere_date",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN official_rating TEXT",
            "add media_items.official_rating",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN extended_video_type TEXT",
            "add media_items.extended_video_type",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN runtime_ticks BIGINT",
            "add media_items.runtime_ticks",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN bit_rate BIGINT",
            "add media_streams.bit_rate",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN width BIGINT",
            "add media_streams.width",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN height BIGINT",
            "add media_streams.height",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN channels BIGINT",
            "add media_streams.channels",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN sample_rate BIGINT",
            "add media_streams.sample_rate",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN profile TEXT",
            "add media_streams.profile",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN codec_tag TEXT",
            "add media_streams.codec_tag",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN comment TEXT",
            "add media_streams.comment",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN aspect_ratio TEXT",
            "add media_streams.aspect_ratio",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN average_frame_rate DOUBLE PRECISION",
            "add media_streams.average_frame_rate",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN real_frame_rate DOUBLE PRECISION",
            "add media_streams.real_frame_rate",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN reference_frame_rate DOUBLE PRECISION",
            "add media_streams.reference_frame_rate",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN channel_layout TEXT",
            "add media_streams.channel_layout",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN bit_depth BIGINT",
            "add media_streams.bit_depth",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN ref_frames BIGINT",
            "add media_streams.ref_frames",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_interlaced BIGINT NOT NULL DEFAULT 0",
            "add media_streams.is_interlaced",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_avc BIGINT",
            "add media_streams.is_avc",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_anamorphic BIGINT",
            "add media_streams.is_anamorphic",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN pixel_format TEXT",
            "add media_streams.pixel_format",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN level BIGINT",
            "add media_streams.level",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN color_range TEXT",
            "add media_streams.color_range",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN color_space TEXT",
            "add media_streams.color_space",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN color_transfer TEXT",
            "add media_streams.color_transfer",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN color_primaries TEXT",
            "add media_streams.color_primaries",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN time_base TEXT",
            "add media_streams.time_base",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN codec_time_base TEXT",
            "add media_streams.codec_time_base",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN nal_length_size TEXT",
            "add media_streams.nal_length_size",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN rotation BIGINT",
            "add media_streams.rotation",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN video_range TEXT",
            "add media_streams.video_range",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN video_range_type TEXT",
            "add media_streams.video_range_type",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN hdr10_plus_present_flag BIGINT",
            "add media_streams.hdr10_plus_present_flag",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_default BIGINT NOT NULL DEFAULT 0",
            "add media_streams.is_default",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_forced BIGINT NOT NULL DEFAULT 0",
            "add media_streams.is_forced",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_hearing_impaired BIGINT NOT NULL DEFAULT 0",
            "add media_streams.is_hearing_impaired",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_original BIGINT",
            "add media_streams.is_original",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN updated_at BIGINT NOT NULL DEFAULT 0",
            "add media_items.updated_at",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN path TEXT",
            "add media_streams.path",
        ),
        (
            "ALTER TABLE media_streams ADD COLUMN is_external BIGINT NOT NULL DEFAULT 0",
            "add media_streams.is_external",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_library_id ON media_items(library_id)",
            "create media library index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS studios (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "create studios table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_studios (item_id TEXT NOT NULL, studio_id TEXT NOT NULL, PRIMARY KEY(item_id, studio_id), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(studio_id) REFERENCES studios(id) ON DELETE CASCADE)"#,
            "create media_studios table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS display_preferences (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, preferences_json TEXT NOT NULL, created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL)"#,
            "create display_preferences table",
        ),
        (
            "ALTER TABLE user_data ADD COLUMN rating DOUBLE PRECISION",
            "add user_data.rating",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS linked_children (parent_id TEXT NOT NULL, item_id TEXT NOT NULL, sort_order BIGINT NOT NULL DEFAULT 0, PRIMARY KEY(parent_id, item_id), FOREIGN KEY(parent_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE)"#,
            "create linked_children table",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_linked_children_parent ON linked_children(parent_id, sort_order)",
            "create linked_children index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at BIGINT NOT NULL)"#,
            "create app_settings table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS api_keys (id TEXT PRIMARY KEY, access_token TEXT NOT NULL UNIQUE, name TEXT NOT NULL, user_id TEXT NOT NULL, created_at BIGINT NOT NULL, last_used_at BIGINT, FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE)"#,
            "create api_keys table",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys(user_id)",
            "create api key user index",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN season_number BIGINT",
            "add media_items.season_number",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN episode_number BIGINT",
            "add media_items.episode_number",
        ),
        (
            // Drop FK so image_assets can reference people IDs too
            "ALTER TABLE image_assets DROP CONSTRAINT IF EXISTS image_assets_item_id_fkey",
            "drop image_assets FK to allow person images",
        ),
        (
            // Drop FK on user_data.item_id to allow favoriting people (not just media_items)
            "ALTER TABLE user_data DROP CONSTRAINT IF EXISTS user_data_item_id_fkey",
            "drop user_data FK to allow person favorites",
        ),
        (
            "ALTER TABLE people ADD COLUMN overview TEXT",
            "add people.overview (biography)",
        ),
        (
            "ALTER TABLE people ADD COLUMN tmdb_id TEXT",
            "add people.tmdb_id",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN community_rating DOUBLE PRECISION",
            "add media_items.community_rating",
        ),
        (
            "ALTER TABLE media_items ADD COLUMN critic_rating DOUBLE PRECISION",
            "add media_items.critic_rating",
        ),
        // Performance indexes
        (
            "CREATE INDEX IF NOT EXISTS idx_media_people_person_id ON media_people(person_id)",
            "add media_people.person_id index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_user_data_user_favorite ON user_data(user_id, is_favorite)",
            "add user_data.user_id+is_favorite index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_folder_type ON media_items(is_folder, item_type)",
            "add media_items.is_folder+item_type index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_genres_genre_id ON media_genres(genre_id)",
            "add media_genres.genre_id index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_streams_type_codec ON media_streams(stream_type, codec)",
            "add media_streams.stream_type+codec index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_library_type ON media_items(library_id, item_type, is_folder)",
            "add media_items.library_id+item_type+is_folder index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_media_items_parent_type ON media_items(parent_id, item_type)",
            "add media_items.parent_id+item_type index",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_user_data_user_id ON user_data(user_id)",
            "add user_data.user_id index",
        ),
        // --- StrmAssistant integration tables ---
        (
            r#"CREATE TABLE IF NOT EXISTS chapters (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, start_position_ticks BIGINT NOT NULL, name TEXT NOT NULL DEFAULT '', marker_type TEXT, source TEXT NOT NULL DEFAULT 'manual', created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE)"#,
            "create chapters table",
        ),
        (
            "CREATE INDEX IF NOT EXISTS idx_chapters_item_id ON chapters(item_id)",
            "create chapters.item_id index",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS audio_fingerprints (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, fingerprint BYTEA NOT NULL, duration_seconds DOUBLE PRECISION, created_at BIGINT NOT NULL, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, UNIQUE(item_id))"#,
            "create audio_fingerprints table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS trickplay_images (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, width BIGINT NOT NULL, tile_count BIGINT NOT NULL, interval_ticks BIGINT NOT NULL, path TEXT NOT NULL, created_at BIGINT NOT NULL, FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, UNIQUE(item_id, width))"#,
            "create trickplay_images table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS merge_groups (id TEXT PRIMARY KEY, representative_id TEXT NOT NULL, member_id TEXT NOT NULL, provider TEXT NOT NULL, provider_item_id TEXT NOT NULL, created_at BIGINT NOT NULL, FOREIGN KEY(representative_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(member_id) REFERENCES media_items(id) ON DELETE CASCADE, UNIQUE(member_id))"#,
            "create merge_groups table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS game_genres (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at BIGINT NOT NULL)"#,
            "create game_genres table",
        ),
        (
            r#"CREATE TABLE IF NOT EXISTS media_game_genres (item_id TEXT NOT NULL, game_genre_id TEXT NOT NULL, PRIMARY KEY(item_id, game_genre_id), FOREIGN KEY(item_id) REFERENCES media_items(id) ON DELETE CASCADE, FOREIGN KEY(game_genre_id) REFERENCES game_genres(id) ON DELETE CASCADE)"#,
            "create media_game_genres table",
        ),
    ]
}
