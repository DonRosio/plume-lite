use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{Ack, UpdateChannel};
use plume_lite_protocol::model::{Channel, ChannelKind};
use plume_lite_protocol::{layout, validate};

use super::{OWNER_ONLY, UNKNOWN_CATEGORY, UNKNOWN_CHANNEL, dispatch_layout, require_channel};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

/// `PATCH /channels/{id}` (guild owner): renames a channel, moves it to another category or
/// to another rank. Every channel whose rank changed is broadcast (`CHANNEL_UPDATE`).
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
    Json(body): Json<UpdateChannel>,
) -> Result<Json<Channel>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    if access.owner_id != caller.user.id {
        return Err(AppError::Forbidden(OWNER_ONLY));
    }
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
    dispatch_layout(&state, guild_id, changed, Vec::new(), None);
    Ok(Json(updated))
}

pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
) -> Result<StatusCode> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    if access.owner_id != caller.user.id {
        return Err(AppError::Forbidden(OWNER_ONLY));
    }
    db::channels::delete(&state.db, channel_id).await?;
    let guild_id = access.channel.guild_id;
    state.hub.dispatch_to_guild(guild_id, Event::ChannelDelete { id: channel_id, guild_id });
    if access.channel.kind == ChannelKind::Voice {
        for left in state.voice.clear_channel(channel_id) {
            state.hub.dispatch_to_guild(guild_id, Event::VoiceStateUpdate(left));
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
    let guild_id = access.channel.guild_id;
    state.hub.dispatch_to_guild(guild_id, Event::TypingStart { guild_id, channel_id, user_id: caller.user.id });
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
