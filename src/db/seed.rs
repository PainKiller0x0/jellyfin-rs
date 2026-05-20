use anyhow::Context;
use sea_orm::ConnectionTrait;

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
    let backend = state.db.get_database_backend();

    state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO users (id, username, password_hash, display_name, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, 1, ?, ?) ON CONFLICT(id) DO NOTHING"#,
            vec![
                state.user_id.to_string().into(),
                username.clone().into(),
                password_hash.into(),
                username.into(),
                now.into(),
                now.into(),
            ],
        ))
        .await
        .context("failed to seed default user")?;

    state
        .db
        .execute(crate::db::helpers::portable_statement(
            backend,
            r#"INSERT INTO access_tokens (id, user_id, token_hash, name, created_at) VALUES (?, ?, ?, 'startup-token', ?) ON CONFLICT(token_hash) DO UPDATE SET user_id = excluded.user_id, name = excluded.name"#,
            vec![
                stable_text_id(&format!("token:{}", state.access_token)).into(),
                state.user_id.to_string().into(),
                stable_text_id(&state.access_token).into(),
                now.into(),
            ],
        ))
        .await
        .context("failed to seed startup access token")?;

    use sea_orm::EntityTrait;
    let existing = crate::entities::libraries::Entity::find()
        .all(&state.db)
        .await
        .context("failed to count libraries")?;
    if existing.is_empty() {
        for (id, name, collection_type) in [
            ("movies", "Movies", "movies"),
            ("tvshows", "TV Shows", "tvshows"),
            ("music", "Music", "music"),
        ] {
            state
                .db
                .execute(crate::db::helpers::portable_statement(
                    backend,
                    r#"INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO NOTHING"#,
                    vec![
                        id.into(),
                        name.into(),
                        collection_type.into(),
                        now.into(),
                        now.into(),
                    ],
                ))
                .await
                .with_context(|| format!("failed to seed library: {id}"))?;
        }
    }

    for path in &state.media_dirs {
        let path = path.to_string_lossy().to_string();
        let library_id = infer_library_id_from_path(&path);
        state
            .db
            .execute(crate::db::helpers::portable_statement(
                backend,
                r#"INSERT INTO library_paths (id, library_id, path, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET library_id = excluded.library_id"#,
                vec![
                    stable_text_id(&format!("library-path:{path}")).into(),
                    library_id.into(),
                    path.as_str().into(),
                    now.into(),
                ],
            ))
            .await
            .with_context(|| format!("failed to seed library path: {path}"))?;
    }

    Ok(())
}
