//! Bans (ban members): a banned user is removed from the guild and cannot join it again.

use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::http::CreateBan;
use plume_lite_protocol::model::Ban;
use plume_lite_protocol::permissions::Permissions;
use plume_lite_protocol::{Snowflake, validate};

use super::guilds::member_removed;
use super::{NOT_ALLOWED, OUTRANKED, require_permission};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

/// `GET /guilds/{id}/bans`: the most recent first.
pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
) -> Result<Json<Vec<Ban>>> {
    require_permission(&state, guild_id, caller.user.id, Permissions::BAN_MEMBERS).await?;
    Ok(Json(db::bans::list(&state.db, guild_id).await?))
}

/// `PUT /guilds/{id}/bans/{user_id}`: also works on someone who already left (they could
/// otherwise dodge a ban by leaving first). A member must be ranked lower than the caller.
/// Banning again only changes the reason.
pub async fn ban(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id)): Path<(Snowflake, Snowflake)>,
    Json(body): Json<CreateBan>,
) -> Result<StatusCode> {
    require_permission(&state, guild_id, caller.user.id, Permissions::BAN_MEMBERS).await?;
    let reason = validate::ban_reason(body.reason.as_deref().unwrap_or_default())?;
    if user_id == caller.user.id {
        return Err(AppError::BadRequest("Vous ne pouvez pas vous bannir vous-même.".into()));
    }
    let user = db::users::get(&state.db, user_id).await?.ok_or(AppError::NotFound("Utilisateur introuvable."))?;

    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    // Checked inside the transaction: roles may have changed since, and nothing can change now.
    let actor = db::roles::standing(&mut tx, guild_id, caller.user.id).await?;
    if !actor.is_some_and(|actor| actor.can(Permissions::BAN_MEMBERS)) {
        return Err(AppError::Forbidden(NOT_ALLOWED));
    }
    if let (Some(actor), Some(target)) = (actor, db::roles::standing(&mut tx, guild_id, user_id).await?)
        && !actor.outranks(&target)
    {
        return Err(AppError::Forbidden(OUTRANKED));
    }
    let was_member = db::bans::ban(&mut tx, guild_id, user_id, reason.as_deref(), caller.user.id).await?;
    tx.commit().await?;
    tracing::info!(%guild_id, %user_id, by = %caller.user.id, "user banned");

    if was_member {
        member_removed(&state, guild_id, user);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /guilds/{id}/bans/{user_id}`: they may join again, with a new invite.
pub async fn unban(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    require_permission(&state, guild_id, caller.user.id, Permissions::BAN_MEMBERS).await?;
    if !db::bans::unban(&state.db, guild_id, user_id).await? {
        return Err(AppError::NotFound("Ce membre n'est pas banni."));
    }
    Ok(StatusCode::NO_CONTENT)
}
