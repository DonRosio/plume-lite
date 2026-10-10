use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{CreateChannel, CreateGuild, MEMBERS_PAGE_DEFAULT, MEMBERS_PAGE_MAX, MemberQuery};
use plume_lite_protocol::model::{Channel, ChannelKind, FullGuild, Member, Status};
use plume_lite_protocol::validate;

use super::{UNKNOWN_GUILD, require_member, require_owner};
use crate::auth::AuthUser;
use crate::db::guilds::MemberRow;
use crate::db::{self, MAX_CHANNELS_PER_GUILD, MAX_GUILDS_PER_USER};
use crate::error::{AppError, Result};
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Json(body): Json<CreateGuild>,
) -> Result<(StatusCode, Json<FullGuild>)> {
    let name = validate::guild_name(&body.name)?;
    if db::guilds::count_for_user(&state.db, caller.user.id).await? >= MAX_GUILDS_PER_USER {
        return Err(AppError::BadRequest("Vous avez atteint la limite de 100 serveurs.".into()));
    }
    let guild = db::guilds::create(&state.db, &state.ids, caller.user.id, &name).await?;

    state.hub.join_guild(caller.user.id, guild.guild.id);
    // Other sessions of the creator (other devices) learn about it too.
    state.hub.dispatch_to_user(caller.user.id, Event::GuildCreate(guild.clone()));
    Ok((StatusCode::CREATED, Json(guild)))
}

pub async fn get(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
) -> Result<Json<FullGuild>> {
    require_member(&state, guild_id, caller.user.id).await?;
    let mut guild = db::guilds::full(&state.db, guild_id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    state.voice.fill(std::slice::from_mut(&mut guild));
    Ok(Json(guild))
}

pub async fn delete(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
) -> Result<StatusCode> {
    require_owner(&state, guild_id, caller.user.id).await?;
    let voice_rooms: Vec<Snowflake> = db::channels::list(&state.db, guild_id)
        .await?
        .into_iter()
        .filter(|channel| channel.kind == ChannelKind::Voice)
        .map(|channel| channel.id)
        .collect();
    db::guilds::delete(&state.db, guild_id).await?;
    state.hub.dispatch_to_guild(guild_id, Event::GuildDelete { id: guild_id });
    state.hub.remove_guild(guild_id);
    state.voice.clear_guild(guild_id);
    for room in voice_rooms {
        super::voice::end_room(&state, room);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn channels(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
) -> Result<Json<Vec<Channel>>> {
    require_member(&state, guild_id, caller.user.id).await?;
    Ok(Json(db::channels::list(&state.db, guild_id).await?))
}

pub async fn create_channel(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<CreateChannel>,
) -> Result<(StatusCode, Json<Channel>)> {
    require_owner(&state, guild_id, caller.user.id).await?;
    let name = validate::channel_name(&body.name)?;
    if db::channels::count(&state.db, guild_id).await? >= MAX_CHANNELS_PER_GUILD {
        return Err(AppError::BadRequest("Ce serveur a atteint la limite de 200 salons.".into()));
    }
    if !matches!(body.kind, ChannelKind::Text | ChannelKind::Voice) {
        return Err(AppError::BadRequest("Type de salon inconnu.".into()));
    }
    let channel = db::channels::create(&state.db, state.ids.next(), guild_id, &name, body.kind).await?;
    state.hub.dispatch_to_guild(guild_id, Event::ChannelCreate(channel.clone()));
    Ok((StatusCode::CREATED, Json(channel)))
}

pub async fn members(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Query(query): Query<MemberQuery>,
) -> Result<Json<Vec<Member>>> {
    require_member(&state, guild_id, caller.user.id).await?;
    let limit = query.limit.unwrap_or(MEMBERS_PAGE_DEFAULT).clamp(1, MEMBERS_PAGE_MAX);
    let rows = db::guilds::members(&state.db, guild_id, query.after, limit).await?;
    Ok(Json(rows.into_iter().map(|row| to_member(&state, row)).collect()))
}

/// `DELETE /guilds/{id}/members/@me` leaves the guild; with another user id it kicks (owner only).
pub async fn remove_member(
    State(state): State<AppState>,
    caller: AuthUser,
    Path((guild_id, target)): Path<(Snowflake, String)>,
) -> Result<StatusCode> {
    let owner_id = require_member(&state, guild_id, caller.user.id).await?;
    let target_id = if target == "@me" {
        caller.user.id
    } else {
        target.parse::<Snowflake>().map_err(|_| AppError::BadRequest("Identifiant d'utilisateur invalide.".into()))?
    };

    if target_id == owner_id {
        return Err(if target_id == caller.user.id {
            AppError::BadRequest("Le propriétaire ne peut pas quitter son serveur : supprimez-le.".into())
        } else {
            AppError::Forbidden("Impossible d'exclure le propriétaire du serveur.")
        });
    }
    if target_id != caller.user.id && caller.user.id != owner_id {
        return Err(AppError::Forbidden("Seul le propriétaire peut exclure des membres."));
    }

    let member =
        db::guilds::member(&state.db, guild_id, target_id).await?.ok_or(AppError::NotFound("Membre introuvable."))?;
    db::guilds::remove_member(&state.db, guild_id, target_id).await?;

    state.hub.leave_guild(target_id, guild_id);
    super::voice::kick_from_guild(&state, guild_id, target_id);
    state.hub.dispatch_to_guild(guild_id, Event::GuildMemberRemove { guild_id, user: member.user });
    state.hub.dispatch_to_user(target_id, Event::GuildDelete { id: guild_id });
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn to_member(state: &AppState, row: MemberRow) -> Member {
    let status = if state.hub.is_online(row.user.id) { Status::Online } else { Status::Offline };
    Member { user: row.user, joined_at: row.joined_at, status }
}
