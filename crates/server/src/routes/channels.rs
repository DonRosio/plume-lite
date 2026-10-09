use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;

use super::{OWNER_ONLY, require_channel};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::Path;
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
