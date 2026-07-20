use anyhow::Context;
use sea_orm::{EntityTrait, Set, sea_query::OnConflict};

use crate::{
    app::state::{AppState, DEFAULT_USER_NAME},
    entities::{
        access_tokens::{self, Entity as AccessTokens},
        users::{self, Entity as Users},
    },
    util::{hash_password, now_unix, stable_text_id},
};

pub async fn seed_default_data(state: &AppState) -> anyhow::Result<()> {
    let now = now_unix();
    let username =
        std::env::var("JELLYFIN_RS_USER").unwrap_or_else(|_| DEFAULT_USER_NAME.to_string());
    let password = std::env::var("JELLYFIN_RS_PASSWORD").unwrap_or_else(|_| username.clone());
    let password_hash = hash_password(&password)?;

    Users::insert(users::ActiveModel {
        id: Set(state.user_id.to_string()),
        username: Set(username.clone()),
        password_hash: Set(Some(password_hash)),
        display_name: Set(username),
        is_admin: Set(1),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .on_conflict_do_nothing()
    .exec_without_returning(&state.db)
    .await
    .context("failed to seed default user")?;

    AccessTokens::insert(access_tokens::ActiveModel {
        id: Set(stable_text_id(&format!("token:{}", state.access_token))),
        user_id: Set(state.user_id.to_string()),
        token_hash: Set(stable_text_id(&state.access_token)),
        name: Set(Some("startup-token".to_string())),
        created_at: Set(now),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(access_tokens::Column::TokenHash)
            .update_columns([access_tokens::Column::UserId, access_tokens::Column::Name])
            .to_owned(),
    )
    .exec_without_returning(&state.db)
    .await
    .context("failed to seed startup access token")?;

    crate::db::settings::insert_if_missing(&state.db, "StartupWizardCompleted", "true", now)
        .await
        .context("failed to seed startup wizard state")?;

    Ok(())
}
