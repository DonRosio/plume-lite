//! Roles (manage roles). Every check is made inside the write transaction, against the roles
//! as they are at that moment: one only edits, deletes or hands out roles ranked below one's
//! own highest role, only to members ranked lower (or oneself), and only with permissions one
//! has, so that nobody can raise themselves or anyone else above what they already are.

use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{CreateRole, UpdateRole};
use plume_lite_protocol::model::Role;
use plume_lite_protocol::permissions::{self, Permissions, Standing};
use plume_lite_protocol::{Snowflake, validate};
use sqlx::SqliteConnection;

use super::{NOT_ALLOWED, OUTRANKED, UNKNOWN_GUILD, UNKNOWN_MEMBER};
use crate::auth::AuthUser;
use crate::db::{self, MAX_ROLES_PER_GUILD};
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

const UNKNOWN_ROLE: &str = "Rôle introuvable.";
const ROLE_ABOVE: &str = "Vous ne pouvez gérer que les rôles placés sous votre rôle le plus haut.";
const NOT_YOURS: &str = "Vous ne pouvez donner que des permissions que vous avez vous-même.";
const EVERYONE_FIXED: &str = "Le rôle @everyone ne se renomme pas, ne se colore pas et ne se déplace pas.";

/// `POST /guilds/{id}/roles`: a new role, ranked lowest.
pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<CreateRole>,
) -> Result<(StatusCode, Json<Role>)> {
    let name = validate::role_name(&body.name)?;
    let color = body.color.map(validate::role_color).transpose()?;
    if db::roles::count(&state.db, guild_id).await? > MAX_ROLES_PER_GUILD {
        return Err(AppError::BadRequest("Ce serveur a atteint la limite de 50 rôles.".into()));
    }
    let id = state.ids.next();
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let actor = manager(&mut tx, guild_id, caller.user.id).await?;
    if !actor.may_grant(Permissions::NONE, body.permissions) {
        return Err(AppError::Forbidden(NOT_YOURS));
    }
    db::roles::create(&mut tx, id, guild_id, &name, color, body.permissions).await?;
    let before = db::roles::list(&mut tx, guild_id).await?;
    let mut after = before.clone();
    permissions::rank_roles(&mut after, None);
    let changed = db::roles::save(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let role = after.into_iter().find(|r| r.id == id).ok_or_else(|| anyhow::anyhow!("role {id} vanished"))?;
    state.hub.dispatch_to_guild(guild_id, Event::GuildRoleCreate(role.clone()));
    dispatch_updates(&state, guild_id, changed, Some(id));
    Ok((StatusCode::CREATED, Json(role)))
}

/// `PATCH /guilds/{id}/roles/{role_id}`: name, colour, permissions, rank. `@everyone` only
/// takes permissions.
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, role_id)): Path<(Snowflake, Snowflake)>,
    Json(body): Json<UpdateRole>,
) -> Result<Json<Role>> {
    let name = body.name.as_deref().map(validate::role_name).transpose()?;
    let color = body.color.map(|c| c.map(validate::role_color).transpose()).transpose()?;

    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let actor = manager(&mut tx, guild_id, caller.user.id).await?;
    let before = db::roles::list(&mut tx, guild_id).await?;
    let mut after = before.clone();
    let role = after.iter_mut().find(|r| r.id == role_id).ok_or(AppError::NotFound(UNKNOWN_ROLE))?;
    if role.is_everyone() {
        if name.is_some() || color.is_some() || body.position.is_some() {
            return Err(AppError::BadRequest(EVERYONE_FIXED.into()));
        }
    } else if !actor.above_role(role.position) {
        return Err(AppError::Forbidden(ROLE_ABOVE));
    }
    if let Some(permissions) = body.permissions {
        if !actor.may_grant(role.permissions, permissions) {
            return Err(AppError::Forbidden(NOT_YOURS));
        }
        role.permissions = permissions;
    }
    if let Some(name) = name {
        role.name = name;
    }
    if let Some(color) = color {
        role.color = color;
    }
    if let Some(position) = body.position {
        // Not up to one's own rank or above.
        if !actor.above_role(i32::try_from(position).unwrap_or(i32::MAX)) {
            return Err(AppError::Forbidden(ROLE_ABOVE));
        }
        permissions::rank_roles(&mut after, Some((role_id, position)));
    }
    let changed = db::roles::save(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let role = after.into_iter().find(|r| r.id == role_id).ok_or(AppError::NotFound(UNKNOWN_ROLE))?;
    dispatch_updates(&state, guild_id, changed, None);
    if body.permissions.is_some() {
        // What members may see can change: everyone online gets the guild again.
        super::resend_guild(&state, guild_id, &state.hub.online_members(guild_id)).await;
    }
    Ok(Json(role))
}

/// `DELETE /guilds/{id}/roles/{role_id}`: its members lose it.
pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, role_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let actor = manager(&mut tx, guild_id, caller.user.id).await?;
    let before = db::roles::list(&mut tx, guild_id).await?;
    let role = before.iter().find(|r| r.id == role_id).ok_or(AppError::NotFound(UNKNOWN_ROLE))?;
    if role.is_everyone() {
        return Err(AppError::BadRequest("Le rôle @everyone ne peut pas être supprimé.".into()));
    }
    if !actor.above_role(role.position) {
        return Err(AppError::Forbidden(ROLE_ABOVE));
    }
    db::roles::delete(&mut tx, role_id).await?;
    let mut after: Vec<Role> = before.iter().filter(|r| r.id != role_id).cloned().collect();
    permissions::rank_roles(&mut after, None);
    let changed = db::roles::save(&mut tx, &before, &after).await?;
    tx.commit().await?;

    state.hub.dispatch_to_guild(guild_id, Event::GuildRoleDelete { guild_id, id: role_id });
    dispatch_updates(&state, guild_id, changed, None);
    super::resend_guild(&state, guild_id, &state.hub.online_members(guild_id)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /guilds/{id}/members/{user_id}/roles/{role_id}` (idempotent).
pub async fn give(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id, role_id)): Path<(Snowflake, Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    set(&state, &caller, guild_id, user_id, role_id, true).await
}

/// `DELETE /guilds/{id}/members/{user_id}/roles/{role_id}` (idempotent).
pub async fn take(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id, role_id)): Path<(Snowflake, Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    set(&state, &caller, guild_id, user_id, role_id, false).await
}

async fn set(
    state: &AppState,
    caller: &AuthUser,
    guild_id: Snowflake,
    user_id: Snowflake,
    role_id: Snowflake,
    on: bool,
) -> Result<StatusCode> {
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let actor = manager(&mut tx, guild_id, caller.user.id).await?;
    let roles = db::roles::list(&mut tx, guild_id).await?;
    let role = roles.iter().find(|r| r.id == role_id).ok_or(AppError::NotFound(UNKNOWN_ROLE))?;
    if role.is_everyone() {
        return Err(AppError::BadRequest("Tout le monde a le rôle @everyone.".into()));
    }
    if !actor.above_role(role.position) {
        return Err(AppError::Forbidden(ROLE_ABOVE));
    }
    // Handing out a role is handing out its permissions.
    if on && !actor.may_grant(Permissions::NONE, role.permissions) {
        return Err(AppError::Forbidden(NOT_YOURS));
    }
    let target = db::roles::standing(&mut tx, guild_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_MEMBER))?;
    if user_id != caller.user.id && !actor.outranks(&target) {
        return Err(AppError::Forbidden(OUTRANKED));
    }
    let changed = db::roles::set_member_role(&mut tx, guild_id, user_id, role_id, on).await?;
    let roles = db::roles::of_member(&mut tx, guild_id, user_id).await?;
    tx.commit().await?;

    if changed {
        state.hub.dispatch_to_guild(guild_id, Event::GuildMemberUpdate { guild_id, user_id, roles });
        // The channels they may see can change.
        super::resend_guild(state, guild_id, &[user_id]).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The caller, read in the transaction: a member (404 otherwise) who may manage roles (403).
async fn manager(conn: &mut SqliteConnection, guild_id: Snowflake, user_id: Snowflake) -> Result<Standing> {
    let standing = db::roles::standing(conn, guild_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    if !standing.can(Permissions::MANAGE_ROLES) {
        return Err(AppError::Forbidden(NOT_ALLOWED));
    }
    Ok(standing)
}

fn dispatch_updates(state: &AppState, guild_id: Snowflake, changed: Vec<Role>, created: Option<Snowflake>) {
    for role in changed.into_iter().filter(|r| Some(r.id) != created) {
        state.hub.dispatch_to_guild(guild_id, Event::GuildRoleUpdate(role));
    }
}
