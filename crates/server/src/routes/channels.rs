use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{Ack, UpdateChannel};
use plume_lite_protocol::model::{Channel, ChannelKind, Role};
use plume_lite_protocol::permissions::{Overwrite, Permissions};
use plume_lite_protocol::{layout, validate};

use super::{UNKNOWN_CATEGORY, UNKNOWN_CHANNEL, dispatch_layout, require_channel, require_permission};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

/// `PATCH /channels/{id}` (manage channels): renames a channel, moves it to another category or
/// to another rank, changes its rights. Every channel whose rank changed is broadcast
/// (`CHANNEL_UPDATE`) to those who see it; those who no longer do get a `CHANNEL_DELETE`.
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
    Json(body): Json<UpdateChannel>,
) -> Result<Json<Channel>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    require_permission(&state, access.channel.guild_id, caller.user.id, Permissions::MANAGE_CHANNELS).await?;
    let name = body.name.as_deref().map(validate::channel_name).transpose()?;
    let guild_id = access.channel.guild_id;

    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let before = db::channels::layout(&mut tx, guild_id).await?;
    let mut after = before.clone();
    let Some(channel) = after.channels.iter_mut().find(|c| c.id == channel_id) else {
        return Err(AppError::NotFound(UNKNOWN_CHANNEL)); // deleted meanwhile
    };
    if let Some(name) = name {
        channel.name = name;
    }
    if let Some(overwrites) = &body.overwrites {
        let actor =
            db::roles::member(&mut tx, guild_id, caller.user.id).await?.ok_or(AppError::NotFound(UNKNOWN_CHANNEL))?;
        let roles = db::roles::list(&mut tx, guild_id).await?;
        // Read in the transaction: as things are right now.
        let actor_in_channel = actor.in_channel(channel);
        channel.overwrites = checked_overwrites(&actor, actor_in_channel, &roles, &channel.overwrites, overwrites)?;
        db::overwrites::replace(&mut tx, channel_id, guild_id, &channel.overwrites).await?;
    }
    let current = channel.category_id;
    if body.category_id.is_some() || body.position.is_some() {
        let category = body.category_id.unwrap_or(current);
        if category.is_some_and(|id| !after.categories.iter().any(|c| c.id == id)) {
            return Err(AppError::NotFound(UNKNOWN_CATEGORY));
        }
        let index = body.position.map(|p| usize::try_from(p).unwrap_or(usize::MAX));
        layout::move_channel(&mut after.categories, &mut after.channels, channel_id, category, index);
    }
    let (changed, _) = db::channels::save_layout(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let updated = after.channels.into_iter().find(|c| c.id == channel_id).ok_or(AppError::NotFound(UNKNOWN_CHANNEL))?;
    if updated.overwrites != access.channel.overwrites {
        // Those who saw it and no longer do lose it (the others get the update below).
        let before = super::viewers(&state, &access.channel).await?;
        let after = super::viewers(&state, &updated).await?;
        let lost: Vec<Snowflake> = before.into_iter().filter(|user| !after.contains(user)).collect();
        state.hub.dispatch_to_users(&lost, Event::ChannelDelete { id: channel_id, guild_id });
    }
    dispatch_layout(&state, guild_id, changed, Vec::new(), None).await;
    if updated.kind == ChannelKind::Voice {
        super::voice::enforce(&state, guild_id).await;
    }
    Ok(Json(updated))
}

/// The new overwrites of a channel, if the caller may set them: roles of the guild, once
/// each, only channel permissions; and for every role whose overwrite changes, a role ranked
/// below the caller's (or `@everyone`), changing only permissions the caller has in the
/// channel. Empty overwrites are dropped.
fn checked_overwrites(
    actor: &db::overwrites::MemberAccess,
    actor_in_channel: Permissions,
    roles: &[Role],
    current: &[Overwrite],
    wanted: &[Overwrite],
) -> Result<Vec<Overwrite>> {
    let mut seen = Vec::new();
    let mut result = Vec::new();
    for overwrite in wanted {
        let role = roles.iter().find(|r| r.id == overwrite.role_id).ok_or(AppError::NotFound("Rôle introuvable."))?;
        if seen.contains(&role.id) {
            return Err(AppError::BadRequest("Un seul réglage par rôle.".into()));
        }
        seen.push(role.id);
        let both = Permissions(overwrite.allow.0 | overwrite.deny.0);
        if !Permissions::CHANNEL.contains(both) || overwrite.allow.0 & overwrite.deny.0 != 0 {
            return Err(AppError::BadRequest("Réglage de salon invalide.".into()));
        }
        if !overwrite.allow.is_empty() || !overwrite.deny.is_empty() {
            result.push(*overwrite);
        }
    }
    // What changes, role by role (set, changed or removed).
    let none = |role_id| Overwrite { role_id, allow: Permissions::NONE, deny: Permissions::NONE };
    let mut touched: Vec<Snowflake> = result.iter().chain(current).map(|o| o.role_id).collect();
    touched.sort_unstable();
    touched.dedup();
    for role_id in touched {
        let old = current.iter().find(|o| o.role_id == role_id).copied().unwrap_or(none(role_id));
        let new = result.iter().find(|o| o.role_id == role_id).copied().unwrap_or(none(role_id));
        if old == new {
            continue;
        }
        let changed = Permissions((old.allow.0 ^ new.allow.0) | (old.deny.0 ^ new.deny.0));
        let position = roles.iter().find(|r| r.id == role_id).map_or(0, |r| r.position);
        let everyone = role_id == roles.iter().find(|r| r.is_everyone()).map_or(Snowflake(0), |r| r.id);
        if !actor.standing.owner
            && (!(everyone || actor.standing.above_role(position)) || !actor_in_channel.contains(changed))
        {
            return Err(AppError::Forbidden(
                "Vous ne pouvez régler que les rôles placés sous le vôtre, et seulement les permissions que vous avez dans ce salon.",
            ));
        }
    }
    result.sort_by_key(|o| o.role_id);
    Ok(result)
}

pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
) -> Result<StatusCode> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    require_permission(&state, access.channel.guild_id, caller.user.id, Permissions::MANAGE_CHANNELS).await?;
    // Worked out while its rights still exist: only those who saw it hear about it.
    let viewers = super::viewers(&state, &access.channel).await?;
    db::channels::delete(&state.db, channel_id).await?;
    let guild_id = access.channel.guild_id;
    state.hub.dispatch_to_users(&viewers, Event::ChannelDelete { id: channel_id, guild_id });
    if access.channel.kind == ChannelKind::Voice {
        for left in state.voice.clear_channel(channel_id) {
            state.hub.dispatch_to_users(&viewers, Event::VoiceStateUpdate(left));
        }
        super::voice::end_room(&state, channel_id);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Ephemeral "X is typing…" signal. Nothing is stored; clients expire it after ~8 s.
pub async fn typing(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
) -> Result<StatusCode> {
    state.limits.check(&state.limits.typing, caller.user.id).map_err(AppError::RateLimited)?;
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    if !access.permissions.contains(Permissions::SEND_MESSAGES) {
        return Err(AppError::Forbidden(super::READ_ONLY));
    }
    let guild_id = access.channel.guild_id;
    let event = Event::TypingStart { guild_id, channel_id, user_id: caller.user.id };
    super::dispatch_to_channel(&state, &access.channel, event).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /channels/{id}/ack`: the caller read the channel up to a message. Never goes back;
/// their devices, this one included, get the new state (`READ_STATE_UPDATE`).
pub async fn ack(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
    Json(body): Json<Ack>,
) -> Result<StatusCode> {
    require_channel(&state, channel_id, caller.user.id).await?;
    db::messages::get(&state.db, channel_id, body.message_id)
        .await?
        .ok_or(AppError::NotFound("Message introuvable."))?;
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    let advanced = db::read_states::advance(&mut tx, caller.user.id, channel_id, body.message_id).await?;
    tx.commit().await?;
    if advanced && let Some(read) = db::read_states::get(&state.db, caller.user.id, channel_id).await? {
        state.hub.dispatch_to_user(caller.user.id, Event::ReadStateUpdate(read));
    }
    Ok(StatusCode::NO_CONTENT)
}
