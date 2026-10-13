use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::Ack;
use plume_lite_protocol::model::ChannelKind;

use super::{OWNER_ONLY, require_channel};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

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
