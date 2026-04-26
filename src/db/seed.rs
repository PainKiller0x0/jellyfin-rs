use anyhow::Context;

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME},
    util::{hash_password, infer_library_id_from_path, now_unix, stable_text_id},
};

pub async fn seed_default_data(state: &AppState) -> anyhow::Result<()> {
    let now = now_unix();
    let username =
        std::env::var("JELLYFIN_RS_USER").unwrap_or_else(|_| DEFAULT_USER_NAME.to_string());
    let password = std::env::var("JELLYFIN_RS_PASSWORD").unwrap_or_else(|_| username.clone());
    let password_hash = hash_password(&password)?;

    sqlx::query(r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 1, ?, ?) ON CONFLICT(id) DO UPDATE SET username = excluded.username, password_hash = excluded.password_hash, display_name = excluded.display_name, is_admin = excluded.is_admin, updated_at = excluded.updated_at"#)
        .bind(state.user_id.to_string())
        .bind(&username)
        .bind(&password_hash)
        .bind(&username)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await
        .context("failed to seed default user")?;

    sqlx::query(r#"INSERT INTO access_tokens (id, user_id, token_hash, name, created_at) VALUES (?, ?, ?, 'startup-token', ?) ON CONFLICT(token_hash) DO UPDATE SET user_id = excluded.user_id, name = excluded.name"#)
        .bind(stable_text_id(&format!("token:{}", state.access_token)))
        .bind(state.user_id.to_string())
        .bind(stable_text_id(&state.access_token))
        .bind(now)
        .execute(&state.db)
        .await
        .context("failed to seed startup access token")?;

    for (id, name, collection_type) in [
        ("movies", "Movies", "movies"),
        ("tvshows", "TV Shows", "tvshows"),
        ("music", "Music", "music"),
    ] {
        sqlx::query(r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name, collection_type = excluded.collection_type, updated_at = excluded.updated_at"#)
            .bind(id)
            .bind(name)
            .bind(collection_type)
            .bind(now)
            .bind(now)
            .execute(&state.db)
            .await
            .with_context(|| format!("failed to seed library: {id}"))?;
    }

    for path in &state.media_dirs {
        let path = path.to_string_lossy().to_string();
        let library_id = infer_library_id_from_path(&path);
        sqlx::query(r#"INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET library_id = excluded.library_id"#)
            .bind(stable_text_id(&format!("library-path:{path}")))
            .bind(library_id)
            .bind(&path)
            .bind(now)
            .execute(&state.db)
            .await
            .with_context(|| format!("failed to seed library path: {path}"))?;
    }

    Ok(())
}
