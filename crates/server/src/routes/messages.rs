use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{
    CreateMessage, EditMessage, MESSAGES_PAGE_DEFAULT, MESSAGES_PAGE_MAX, MessageQuery, SEARCH_PAGE, SearchQuery,
    SearchResults, search_terms,
};
use plume_lite_protocol::model::{AuditAction, Message, MessageKind, REPLY_EXCERPT_MAX, Replied, User};
use plume_lite_protocol::permissions::Permissions;
use plume_lite_protocol::{markup, validate};

use super::require_channel;
use crate::auth::AuthUser;
use crate::db;
use crate::db::messages::{Cursor, NewMessage, Pin, Search};
use crate::db::reactions::Added;
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
    Ok(Json(db::messages::with_reactions(&state.db, rows, access.channel.guild_id).await?))
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
    if !access.permissions.contains(Permissions::SEND_MESSAGES) {
        return Err(AppError::Forbidden(super::READ_ONLY));
    }
    let reply_to = match body.reply_to {
        Some(id) => {
            let answered = db::messages::get(&state.db, channel_id, id)
                .await?
                .ok_or_else(|| AppError::BadRequest("Le message auquel vous répondez n'existe plus.".into()))?;
            Some(Replied {
                id,
                author: Some(User { id: answered.author_id, username: answered.author_username }),
                excerpt: answered.content.chars().take(REPLY_EXCERPT_MAX).collect(),
            })
        }
        None => None,
    };
    let (mentions, everyone) = markup::mentions(&markup::parse(&content));
    let mention_everyone =
        everyone && may(&state, access.channel.guild_id, &caller, Permissions::MENTION_EVERYONE).await?;

    let id = state.ids.next();
    let guild_id = access.channel.guild_id;
    db::messages::insert(
        &state.db,
        &NewMessage {
            id,
            guild_id,
            channel_id,
            author_id: caller.user.id,
            content: &content,
            reply_to: body.reply_to,
            mention_everyone,
            mentions: &mentions,
        },
    )
    .await?;
    let message = Message {
        id,
        channel_id,
        guild_id,
        author: caller.user,
        content,
        kind: MessageKind::Default,
        created_at: id.created_at(),
        edited_at: None,
        reply_to,
        mention_everyone,
        reactions: Vec::new(),
        pinned: false,
        nonce: body.nonce,
    };
    super::dispatch_to_channel(&state, &access.channel, Event::MessageCreate(message.clone())).await;
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
    if existing.kind != 0 {
        return Err(AppError::Forbidden("Ce message ne se modifie pas."));
    }

    let (mentions, everyone) = markup::mentions(&markup::parse(&content));
    let mention_everyone =
        everyone && may(&state, access.channel.guild_id, &caller, Permissions::MENTION_EVERYONE).await?;
    let guild_id = access.channel.guild_id;
    if !db::messages::update_content(&state.db, guild_id, message_id, &content, mention_everyone, &mentions).await? {
        return Err(AppError::NotFound(UNKNOWN_MESSAGE));
    }
    let message = db::messages::full(&state.db, channel_id, message_id, guild_id)
        .await?
        .ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
    super::dispatch_to_channel(&state, &access.channel, Event::MessageUpdate(message.clone())).await;
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
    // Authors delete their messages; those who manage messages, everyone's.
    if existing.author_id != caller.user.id
        && !may(&state, access.channel.guild_id, &caller, Permissions::MANAGE_MESSAGES).await?
    {
        return Err(AppError::Forbidden("Vous ne pouvez supprimer que vos propres messages."));
    }

    if db::messages::delete(&state.db, message_id).await? {
        let guild_id = access.channel.guild_id;
        // A moderator removing someone else's message: who, and where (never what it said).
        if existing.author_id != caller.user.id {
            let record = db::audit::Record::new(guild_id, caller.user.id, AuditAction::MessageDelete)
                .user(existing.author_id)
                .target(access.channel.id, format!("#{}", access.channel.name))
                .channel(channel_id);
            super::audit(&state, record).await;
        }
        super::dispatch_to_channel(
            &state,
            &access.channel,
            Event::MessageDelete { id: message_id, channel_id, guild_id },
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /channels/{id}/messages/{id}/reactions/{emoji}/@me`: idempotent.
pub async fn react(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id, emoji)): Path<(Snowflake, Snowflake, String)>,
) -> Result<StatusCode> {
    state.limits.check(&state.limits.reactions, caller.user.id).map_err(AppError::RateLimited)?;
    validate::reaction(&emoji)?;
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    db::messages::get(&state.db, channel_id, message_id).await?.ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
    match db::reactions::add(&state.db, message_id, caller.user.id, &emoji).await? {
        Added::Yes => {
            let guild_id = access.channel.guild_id;
            let event = Event::ReactionAdd { guild_id, channel_id, message_id, user_id: caller.user.id, emoji };
            super::dispatch_to_channel(&state, &access.channel, event).await;
        }
        Added::Already => {}
        Added::Full => return Err(AppError::BadRequest("Ce message a déjà 20 réactions différentes.".into())),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /channels/{id}/messages/{id}/reactions/{emoji}/@me`: idempotent.
pub async fn unreact(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id, emoji)): Path<(Snowflake, Snowflake, String)>,
) -> Result<StatusCode> {
    state.limits.check(&state.limits.reactions, caller.user.id).map_err(AppError::RateLimited)?;
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    db::messages::get(&state.db, channel_id, message_id).await?.ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
    if db::reactions::remove(&state.db, message_id, caller.user.id, &emoji).await? {
        let guild_id = access.channel.guild_id;
        let event = Event::ReactionRemove { guild_id, channel_id, message_id, user_id: caller.user.id, emoji };
        super::dispatch_to_channel(&state, &access.channel, event).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /channels/{id}/pins`: the pinned messages, most recently pinned first.
pub async fn pins(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
) -> Result<Json<Vec<Message>>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    let rows = db::messages::pinned(&state.db, channel_id).await?;
    Ok(Json(db::messages::with_reactions(&state.db, rows, access.channel.guild_id).await?))
}

/// `PUT /channels/{id}/pins/{id}` (manage messages).
pub async fn pin(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    set_pinned(&state, &caller, channel_id, message_id, true).await
}

/// `DELETE /channels/{id}/pins/{id}` (manage messages).
pub async fn unpin(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((channel_id, message_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    set_pinned(&state, &caller, channel_id, message_id, false).await
}

/// Pinning shows as a `MESSAGE_UPDATE` (with `pinned`), which clients already apply.
async fn set_pinned(
    state: &AppState,
    caller: &AuthUser,
    channel_id: Snowflake,
    message_id: Snowflake,
    pinned: bool,
) -> Result<StatusCode> {
    let access = require_channel(state, channel_id, caller.user.id).await?;
    if !may(state, access.channel.guild_id, caller, Permissions::MANAGE_MESSAGES).await? {
        return Err(AppError::Forbidden("Vous n'avez pas la permission d'épingler des messages."));
    }
    let guild_id = access.channel.guild_id;
    match db::messages::set_pinned(&state.db, channel_id, message_id, pinned).await? {
        Pin::Changed => {
            let action = if pinned { AuditAction::MessagePin } else { AuditAction::MessageUnpin };
            let record = db::audit::Record::new(guild_id, caller.user.id, action)
                .target(access.channel.id, format!("#{}", access.channel.name))
                .channel(channel_id);
            super::audit(state, record).await;
            let message = db::messages::full(&state.db, channel_id, message_id, guild_id)
                .await?
                .ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
            super::dispatch_to_channel(state, &access.channel, Event::MessageUpdate(message)).await;
        }
        Pin::Unchanged => {
            db::messages::get(&state.db, channel_id, message_id).await?.ok_or(AppError::NotFound(UNKNOWN_MESSAGE))?;
        }
        Pin::Full => return Err(AppError::BadRequest("Ce salon a déjà 50 messages épinglés.".into())),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /guilds/{id}/messages/search?q=…`: the guild's messages containing every word (or a
/// longer word starting with it), newest first, one page at a time.
pub async fn search(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResults>> {
    state.limits.check(&state.limits.search, caller.user.id).map_err(AppError::RateLimited)?;
    let channels = super::visible_channels(&state, guild_id, caller.user.id).await?;
    if query.q.chars().count() > SEARCH_QUERY_MAX {
        return Err(AppError::BadRequest("La recherche est trop longue.".into()));
    }
    let terms = search_terms(&query.q);
    if terms.is_empty() {
        return Err(AppError::BadRequest("Indiquez au moins un mot à chercher.".into()));
    }
    let channels: Vec<Snowflake> = channels
        .into_iter()
        .filter(|c| c.kind == plume_lite_protocol::model::ChannelKind::Text)
        .map(|c| c.id)
        .collect();
    let search = Search {
        channels: &channels,
        terms: &terms,
        channel_id: query.channel_id,
        author_id: query.author_id,
        before: query.before,
        limit: SEARCH_PAGE,
    };
    let (total, rows) = db::messages::search(&state.db, &search).await?;
    let messages = db::messages::with_reactions(&state.db, rows, guild_id).await?;
    Ok(Json(SearchResults { total, messages }))
}

/// Characters of a search query.
const SEARCH_QUERY_MAX: usize = 200;

/// Whether the caller has a permission in the guild (they are a member: checked before).
async fn may(state: &AppState, guild_id: Snowflake, caller: &AuthUser, permission: Permissions) -> Result<bool> {
    Ok(super::standing(state, guild_id, caller.user.id).await?.is_some_and(|s| s.can(permission)))
}
