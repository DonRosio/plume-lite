use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{CreateMessage, EditMessage, MESSAGES_PAGE_DEFAULT, MESSAGES_PAGE_MAX, MessageQuery};
use plume_lite_protocol::model::Message;
use plume_lite_protocol::validate;

use super::require_channel;
use crate::auth::AuthUser;
use crate::db;
use crate::db::messages::Cursor;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

const UNKNOWN_MESSAGE: &str = "Message introuvable.";
const MAX_NONCE_LEN: usize = 64;

pub async fn list(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
    Query(query): Query<MessageQuery>,
) -> Result<Json<Vec<Message>>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    let cursor = match (query.before, query.after) {
        (Some(_), Some(_)) => {
            return Err(AppError::BadRequest("`before` et `after` sont mutuellement exclusifs.".into()));
        }
        (Some(before), None) => Cursor::Before(before),
        (None, Some(after)) => Cursor::After(after),
        (None, None) => Cursor::Latest,
    };
    let limit = query.limit.unwrap_or(MESSAGES_PAGE_DEFAULT).clamp(1, MESSAGES_PAGE_MAX);
    let rows = db::messages::page(&state.db, channel_id, cursor, limit).await?;
    let guild_id = access.channel.guild_id;
    Ok(Json(rows.into_iter().map(|row| row.into_message(guild_id)).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
    Json(body): Json<CreateMessage>,
) -> Result<(StatusCode, Json<Message>)> {
    state.limits.check(&state.limits.messages, caller.user.id).map_err(AppError::RateLimited)?;
    let content = validate::message_content(&body.content)?;
    if body.nonce.as_ref().is_some_and(|nonce| nonce.len() > MAX_NONCE_LEN) {
        return Err(AppError::BadRequest("Nonce trop long.".into()));
    }
    let access = require_channel(&state, channel_id, caller.user.id).await?;

    let id = state.ids.next();
    db::messages::insert(&state.db, id, channel_id, caller.user.id, &content).await?;
    let message = Message {
        id,
        channel_id,
        guild_id: access.channel.guild_id,
        author: caller.user,
        content,
        created_at: id.created_at(),
        edited_at: None,
        nonce: body.nonce,
    };
    state.hub.dispatch_to_guild(message.guild_id, Event::MessageCreate(message.clone()));
    Ok((StatusCode::CREATED, Json(message)))
}

pub async fn edit(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id)): Path<(Snowflake, Snowflake)>,
    Json(body): Json<EditMessage>,
) -> Result<Json<Message>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    let content = validate::message_content(&body.content)?;
    let existing =
        db::messages::get(&state.db, channel_id, message_id).await?.ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
    if existing.author_id != caller.user.id {
        return Err(AppError::Forbidden("Vous ne pouvez modifier que vos propres messages."));
    }

    let message = db::messages::update_content(&state.db, message_id, &content)
        .await?
        .ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?
        .into_message(access.channel.guild_id);
    state.hub.dispatch_to_guild(message.guild_id, Event::MessageUpdate(message.clone()));
    Ok(Json(message))
}

pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    let existing =
        db::messages::get(&state.db, channel_id, message_id).await?.ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
    // Authors delete their messages; the guild owner moderates everything.
    if existing.author_id != caller.user.id && access.owner_id != caller.user.id {
        return Err(AppError::Forbidden("Vous ne pouvez supprimer que vos propres messages."));
    }

    if db::messages::delete(&state.db, message_id).await? {
        let guild_id = access.channel.guild_id;
        state.hub.dispatch_to_guild(guild_id, Event::MessageDelete { id: message_id, channel_id, guild_id });
    }
    Ok(StatusCode::NO_CONTENT)
}
