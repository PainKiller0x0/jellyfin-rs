use anyhow::Context;
use sea_orm::ConnectionTrait;

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME},
    util::{hash_password, now_unix, stable_text_id},
};

pub async fn seed_default_data(state: &AppState) -> anyhow::Result<()> {
    let now = now_unix();
    let username =
        std::env::var("JELLYFIN_RS_USER").unwrap_or_else(|_| DEFAULT_USER_NAME.to_string());
    let password = std::env::var("JELLYFIN_RS_PASSWORD").unwrap_or_else(|_| username.clone());
    let password_hash = hash_password(&password)?;

    state
        .db
        .execute(crate::db::helpers::pg_statement(
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
        .execute(crate::db::helpers::pg_statement(
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

    Ok(())
}
