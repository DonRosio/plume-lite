//! Voice channels: joining one (a LiveKit token), one's own microphone and sound, the guild
//! owner's moderation, and the webhooks by which LiveKit says who actually is in which room.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{MoveVoice, ServerMute, UpdateVoiceState, VoiceJoin};
use plume_lite_protocol::model::{AuditAction, ChannelKind, VoiceState};

use plume_lite_protocol::permissions::Permissions;

use super::{UNKNOWN_CHANNEL, require_channel, require_outranked, require_permission};
use crate::auth::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;
use crate::voice::WebhookEvent;

const NOT_ENABLED: &str = "Le vocal n'est pas activé sur ce serveur.";
const NOT_VOICE: &str = "Ce salon n'est pas un salon vocal.";
const NOT_IN_VOICE: &str = "Ce membre n'est pas en vocal.";

/// `POST /channels/{id}/voice`: a token to join this voice channel.
pub async fn join(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(channel_id): Path<Snowflake>,
) -> Result<Json<VoiceJoin>> {
    let access = require_channel(&state, channel_id, caller.user.id).await?;
    if access.channel.kind != ChannelKind::Voice {
        return Err(AppError::BadRequest(NOT_VOICE.into()));
    }
    let can_publish = !state.voice.is_server_muted(access.channel.guild_id, caller.user.id);
    state.voice.join(&caller.user, channel_id, can_publish).map(Json).ok_or(AppError::Unavailable(NOT_ENABLED))
}

/// `PATCH /users/@me/voice`: the caller muted or unmuted their microphone or sound.
pub async fn update_self(
    State(state): State<AppState>,
    caller: AuthUser,
    Json(body): Json<UpdateVoiceState>,
) -> Result<StatusCode> {
    if let Some(updated) = state.voice.set_flags(caller.user.id, body.self_mute, body.self_deaf) {
        broadcast(&state, updated, updated.channel_id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /guilds/{id}/voice/{user_id}/mute` (mute members, on someone ranked lower): stops or
/// lets a member speak.
pub async fn server_mute(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id)): Path<(Snowflake, Snowflake)>,
    Json(body): Json<ServerMute>,
) -> Result<StatusCode> {
    let actor = require_permission(&state, guild_id, caller.user.id, Permissions::MUTE_MEMBERS).await?;
    if user_id != caller.user.id {
        require_outranked(&state, &actor, guild_id, user_id).await?;
    }
    let action = if body.muted { AuditAction::VoiceMute } else { AuditAction::VoiceUnmute };
    super::audit(&state, db::audit::Record::new(guild_id, caller.user.id, action).user(user_id)).await;
    if let Some(updated) = state.voice.set_server_mute(guild_id, user_id, body.muted) {
        if let Some(room) = updated.channel_id {
            state.voice.set_can_publish(room, user_id, !body.muted).await?;
        }
        broadcast(&state, updated, updated.channel_id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /guilds/{id}/voice/{user_id}/disconnect` (move members, on someone ranked lower):
/// takes a member out of voice.
pub async fn disconnect(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id)): Path<(Snowflake, Snowflake)>,
) -> Result<StatusCode> {
    let actor = require_permission(&state, guild_id, caller.user.id, Permissions::MOVE_MEMBERS).await?;
    if user_id != caller.user.id {
        require_outranked(&state, &actor, guild_id, user_id).await?;
    }
    let current =
        state.voice.of_user(user_id).filter(|s| s.guild_id == guild_id).ok_or(AppError::NotFound(NOT_IN_VOICE))?;
    if let Some(room) = current.channel_id {
        state.voice.remove_participant(room, user_id).await?;
    }
    super::audit(&state, db::audit::Record::new(guild_id, caller.user.id, AuditAction::VoiceDisconnect).user(user_id))
        .await;
    if let Some(left) = state.voice.left(user_id, current.channel_id) {
        broadcast(&state, left, current.channel_id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /guilds/{id}/voice/{user_id}/move` (move members, on someone ranked lower): sends a member to another voice channel
/// of the guild. A self-hosted LiveKit cannot move someone between rooms, so their client is
/// asked to switch; LiveKit's webhooks then report the leave and the join as usual.
pub async fn move_member(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, user_id)): Path<(Snowflake, Snowflake)>,
    Json(body): Json<MoveVoice>,
) -> Result<StatusCode> {
    let actor = require_permission(&state, guild_id, caller.user.id, Permissions::MOVE_MEMBERS).await?;
    if user_id != caller.user.id {
        require_outranked(&state, &actor, guild_id, user_id).await?;
    }
    let destination = require_channel(&state, body.channel_id, caller.user.id).await?.channel;
    if destination.guild_id != guild_id {
        return Err(AppError::NotFound(UNKNOWN_CHANNEL));
    }
    if destination.kind != ChannelKind::Voice {
        return Err(AppError::BadRequest(NOT_VOICE.into()));
    }
    let current =
        state.voice.of_user(user_id).filter(|s| s.guild_id == guild_id).ok_or(AppError::NotFound(NOT_IN_VOICE))?;
    if db::channels::access(&state.db, destination.id, user_id).await?.is_none() {
        return Err(AppError::Forbidden("Ce membre ne peut pas voir ce salon."));
    }
    if current.channel_id != Some(destination.id) {
        let record = db::audit::Record::new(guild_id, caller.user.id, AuditAction::VoiceMove)
            .user(user_id)
            .target(destination.id, destination.name.clone())
            .channel(destination.id);
        super::audit(&state, record).await;
        state.hub.dispatch_to_user(user_id, Event::VoiceMove { guild_id, channel_id: destination.id });
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /voice/webhook`: LiveKit reports joins and leaves. Signed with the shared secret.
pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Result<StatusCode> {
    let authorization = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let event = state
        .voice
        .verify_webhook(authorization, &body)
        .ok_or(AppError::Unauthorized("Signature de webhook invalide."))?;
    handle(&state, event).await?;
    Ok(StatusCode::OK)
}

async fn handle(state: &AppState, event: WebhookEvent) -> Result<()> {
    let room = event.room.and_then(|room| room.name.parse::<Snowflake>().ok());
    let user = event.participant.and_then(|p| p.identity.parse::<Snowflake>().ok());
    match (event.event.as_str(), room, user) {
        ("participant_joined", Some(channel_id), Some(user_id)) => admit(state, channel_id, user_id).await?,
        ("participant_left", Some(channel_id), Some(user_id)) => {
            if let Some(left) = state.voice.left(user_id, Some(channel_id)) {
                broadcast(state, left, Some(channel_id)).await;
            }
        }
        ("room_finished", Some(channel_id), _) => {
            for left in state.voice.clear_channel(channel_id) {
                broadcast(state, left, Some(channel_id)).await;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Someone is in a room: record it, if they still belong there.
async fn admit(state: &AppState, channel_id: Snowflake, user_id: Snowflake) -> Result<()> {
    match db::channels::access(&state.db, channel_id, user_id).await? {
        Some(access) if access.channel.kind == ChannelKind::Voice => {
            let previous = state.voice.of_user(user_id);
            let joined = state.voice.joined(access.channel.guild_id, channel_id, user_id);
            // Hopping to another guild's channel: the old guild sees them leave.
            if let Some(previous) = previous.filter(|p| p.guild_id != joined.guild_id) {
                broadcast(state, VoiceState { channel_id: None, ..previous }, previous.channel_id).await;
            }
            broadcast(state, joined, Some(channel_id)).await;
        }
        // Left the guild, lost sight of the channel or the channel was deleted since the token
        // was issued.
        _ => {
            let voice = state.voice.clone();
            tokio::spawn(async move {
                if let Err(err) = voice.remove_participant(channel_id, user_id).await {
                    tracing::warn!(error = %err, "cannot remove an intruder from voice");
                }
            });
        }
    }
    Ok(())
}

/// Tells those who may see `room` (the channel joined, or left) about someone's voice state.
async fn broadcast(state: &AppState, voice_state: VoiceState, room: Option<Snowflake>) {
    let event = Event::VoiceStateUpdate(voice_state);
    let channel = match room {
        Some(room) => db::channels::get(&state.db, room).await,
        None => Ok(None),
    };
    match channel {
        Ok(Some(channel)) => super::dispatch_to_channel(state, &channel, event).await,
        // A deleted channel: its viewers already lost it, the others never knew.
        Ok(None) => {}
        Err(err) => tracing::error!(error = %err, "cannot load a voice channel"),
    }
}

/// Takes out of voice those of a guild who may no longer see their voice channel (their roles
/// or its rights changed).
pub(super) async fn enforce(state: &AppState, guild_id: Snowflake) {
    for current in state.voice.of_guild(guild_id) {
        let Some(room) = current.channel_id else { continue };
        match db::channels::access(&state.db, room, current.user_id).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                let left = state.voice.left(current.user_id, Some(room));
                if let Some(left) = left {
                    broadcast(state, left, Some(room)).await;
                }
                let voice = state.voice.clone();
                tokio::spawn(async move {
                    if let Err(err) = voice.remove_participant(room, current.user_id).await {
                        tracing::warn!(error = %err, "cannot remove someone from voice");
                    }
                });
            }
            Err(err) => tracing::error!(error = %err, "cannot check voice access"),
        }
    }
}

/// A voice channel (or its guild) was deleted: end its LiveKit room, in the background.
pub(super) fn end_room(state: &AppState, room: Snowflake) {
    let voice = state.voice.clone();
    if !voice.enabled() {
        return;
    }
    tokio::spawn(async move {
        if let Err(err) = voice.delete_room(room).await {
            tracing::warn!(error = %err, %room, "cannot end a voice room");
        }
    });
}

/// A member left or was removed from a guild: out of its voice channels too.
pub(super) fn kick_from_guild(state: &AppState, guild_id: Snowflake, user_id: Snowflake) {
    let Some(current) = state.voice.of_user(user_id).filter(|s| s.guild_id == guild_id) else { return };
    if let Some(left) = state.voice.left(user_id, current.channel_id) {
        let state = state.clone();
        tokio::spawn(async move { broadcast(&state, left, current.channel_id).await });
    }
    if let Some(room) = current.channel_id {
        let voice = state.voice.clone();
        tokio::spawn(async move {
            if let Err(err) = voice.remove_participant(room, user_id).await {
                tracing::warn!(error = %err, "cannot remove a former member from voice");
            }
        });
    }
}

/// After a restart: rebuilds who is in which voice channel from LiveKit's rooms.
pub async fn resync(state: &AppState) -> anyhow::Result<usize> {
    let mut found = 0;
    for (room, identities) in state.voice.rooms().await? {
        let Ok(channel_id) = room.parse::<Snowflake>() else { continue };
        for identity in identities {
            let Ok(user_id) = identity.parse::<Snowflake>() else { continue };
            admit(state, channel_id, user_id).await?;
            found += 1;
        }
    }
    Ok(found)
}
