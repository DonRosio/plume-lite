use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::{
    CreateChannel, CreateGuild, MEMBERS_PAGE_DEFAULT, MEMBERS_PAGE_MAX, MemberQuery, TransferOwnership, UpdateGuild,
};
use plume_lite_protocol::model::{Channel, ChannelKind, FullGuild, Guild, Member, Status, User};
use plume_lite_protocol::permissions::{Overwrite, Permissions};
use plume_lite_protocol::{Snowflake, layout, validate};

use super::{
    UNKNOWN_CATEGORY, UNKNOWN_GUILD, UNKNOWN_MEMBER, dispatch_layout, require_member, require_outranked, require_owner,
    require_permission,
};
use crate::auth::{self, AuthUser};
use crate::db::guilds::MemberRow;
use crate::db::{self, MAX_CHANNELS_PER_GUILD, MAX_GUILDS_PER_USER};
use crate::error::{AppError, Result};
use crate::extract::{ClientIp, Json, Path, Query};
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
    let mut guild =
        db::guilds::full(&state.db, guild_id, caller.user.id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    state.voice.fill(std::slice::from_mut(&mut guild));
    Ok(Json(guild))
}

/// `PATCH /guilds/{id}` (manage the guild): renames the guild.
pub async fn update(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<UpdateGuild>,
) -> Result<Json<Guild>> {
    require_permission(&state, guild_id, caller.user.id, Permissions::MANAGE_GUILD).await?;
    let Some(name) = body.name else { return Err(AppError::BadRequest("Rien à modifier.".into())) };
    let name = validate::guild_name(&name)?;
    let guild = db::guilds::rename(&state.db, guild_id, &name).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    state.hub.dispatch_to_guild(guild_id, Event::GuildUpdate(guild.clone()));
    Ok(Json(guild))
}

/// `PUT /guilds/{id}/owner` (owner): hands the guild to another member. The password is asked
/// again (with the login rate limit): a stolen session must not be able to give a guild away.
pub async fn transfer(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<TransferOwnership>,
) -> Result<Json<Guild>> {
    state.limits.check(&state.limits.auth, ip).map_err(AppError::RateLimited)?;
    require_owner(&state, guild_id, caller.user.id).await?;
    if body.user_id == caller.user.id {
        return Err(AppError::BadRequest("Vous êtes déjà le propriétaire de ce serveur.".into()));
    }
    if db::guilds::member(&state.db, guild_id, body.user_id).await?.is_none() {
        return Err(AppError::NotFound(UNKNOWN_MEMBER));
    }
    let hash = db::users::password_hash(&state.db, caller.user.id).await?.ok_or_else(AppError::unauthenticated)?;
    if !auth::verify_password(body.password, hash).await? {
        return Err(AppError::Forbidden("Mot de passe incorrect."));
    }
    // One statement: still the owner, and the new owner still a member, at that very moment.
    let guild = db::guilds::transfer(&state.db, guild_id, caller.user.id, body.user_id)
        .await?
        .ok_or(AppError::NotFound(UNKNOWN_MEMBER))?;
    tracing::info!(%guild_id, from = %caller.user.id, to = %body.user_id, "guild ownership transferred");
    state.hub.dispatch_to_guild(guild_id, Event::GuildUpdate(guild.clone()));
    // The owner sees every channel: what both of them see changes.
    super::resend_guild(&state, guild_id, &[caller.user.id, body.user_id]).await;
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
    Ok(Json(super::visible_channels(&state, guild_id, caller.user.id).await?))
}

pub async fn create_channel(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<CreateChannel>,
) -> Result<(StatusCode, Json<Channel>)> {
    require_permission(&state, guild_id, caller.user.id, Permissions::MANAGE_CHANNELS).await?;
    let name = validate::channel_name(&body.name)?;
    if db::channels::count(&state.db, guild_id).await? >= MAX_CHANNELS_PER_GUILD {
        return Err(AppError::BadRequest("Ce serveur a atteint la limite de 200 salons.".into()));
    }
    if !matches!(body.kind, ChannelKind::Text | ChannelKind::Voice) {
        return Err(AppError::BadRequest("Type de salon inconnu.".into()));
    }
    let id = state.ids.next();
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    db::channels::create(&mut tx, id, guild_id, &name, body.kind, None).await?;
    if body.private {
        // Hidden from @everyone; shown to the creator's highest role (the owner sees all).
        let creator =
            db::roles::member(&mut tx, guild_id, caller.user.id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
        let mut overwrites =
            vec![Overwrite { role_id: guild_id, allow: Permissions::NONE, deny: Permissions::VIEW_CHANNEL }];
        if !creator.standing.owner {
            let roles = db::roles::list(&mut tx, guild_id).await?;
            let highest = roles
                .iter()
                .filter(|r| creator.roles.contains(&r.id))
                .max_by_key(|r| r.position)
                .ok_or_else(|| AppError::BadRequest("Il vous faut un rôle pour créer un salon privé.".into()))?;
            overwrites.push(Overwrite { role_id: highest.id, allow: Permissions::CHANNEL, deny: Permissions::NONE });
        }
        db::overwrites::replace(&mut tx, id, guild_id, &overwrites).await?;
    }
    let before = db::channels::layout(&mut tx, guild_id).await?;
    if body.category_id.is_some_and(|category| !before.categories.iter().any(|c| c.id == category)) {
        return Err(AppError::NotFound(UNKNOWN_CATEGORY)); // tx dropped → rolled back
    }
    // Last of its kind in its category.
    let mut after = before.clone();
    layout::move_channel(&mut after.categories, &mut after.channels, id, body.category_id, None);
    let (changed, _) = db::channels::save_layout(&mut tx, &before, &after).await?;
    tx.commit().await?;

    let channel =
        after.channels.into_iter().find(|c| c.id == id).ok_or_else(|| anyhow::anyhow!("channel {id} vanished"))?;
    super::dispatch_to_channel(&state, &channel, Event::ChannelCreate(channel.clone())).await;
    dispatch_layout(&state, guild_id, changed, Vec::new(), Some(id)).await;
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

/// `DELETE /guilds/{id}/members/@me` leaves the guild; with another user id it kicks (kick
/// members, and only someone ranked lower).
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
    if target_id != caller.user.id {
        let actor = require_permission(&state, guild_id, caller.user.id, Permissions::KICK_MEMBERS).await?;
        require_outranked(&state, &actor, guild_id, target_id).await?;
    }

    let member = db::guilds::member(&state.db, guild_id, target_id).await?.ok_or(AppError::NotFound(UNKNOWN_MEMBER))?;
    db::guilds::remove_member(&state.db, guild_id, target_id).await?;
    member_removed(&state, guild_id, member.user);
    Ok(StatusCode::NO_CONTENT)
}

/// A member that leaves the guild, whatever the reason: out of its live events and voice
/// channels; the others see them go, their devices lose the guild.
pub(super) fn member_removed(state: &AppState, guild_id: Snowflake, user: User) {
    let user_id = user.id;
    state.hub.leave_guild(user_id, guild_id);
    super::voice::kick_from_guild(state, guild_id, user_id);
    state.hub.dispatch_to_guild(guild_id, Event::GuildMemberRemove { guild_id, user });
    state.hub.dispatch_to_user(user_id, Event::GuildDelete { id: guild_id });
}

pub(super) fn to_member(state: &AppState, row: MemberRow) -> Member {
    let status = if state.hub.is_online(row.user.id) { Status::Online } else { Status::Offline };
    let roles = row.role_ids();
    Member { user: row.user, joined_at: row.joined_at, status, roles }
}
