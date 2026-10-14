use anyhow::anyhow;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{TimeDelta, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::http::CreateInvite;
use plume_lite_protocol::model::{FullGuild, Invite};
use plume_lite_protocol::validate::{INVITE_MAX_AGE_SECS, INVITE_MAX_USES};

use super::guilds::to_member;
use plume_lite_protocol::permissions::Permissions;

use super::{UNKNOWN_GUILD, require_permission};
use crate::auth::{self, AuthUser};
use crate::db::{self, MAX_GUILDS_PER_USER};
use crate::error::{AppError, Result};
use crate::extract::{Json, Path};
use crate::state::AppState;

const INVITE_CODE_LEN: usize = 8;
const INVALID_INVITE: &str = "Invitation invalide ou expirée.";

pub async fn create(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(guild_id): Path<Snowflake>,
    Json(body): Json<CreateInvite>,
) -> Result<(StatusCode, Json<Invite>)> {
    require_permission(&state, guild_id, caller.user.id, Permissions::CREATE_INVITE).await?;
    if body.max_uses.is_some_and(|n| !(1..=INVITE_MAX_USES).contains(&n)) {
        return Err(AppError::BadRequest(format!("max_uses doit être entre 1 et {INVITE_MAX_USES}.")));
    }
    if body.max_age_secs.is_some_and(|s| !(60..=INVITE_MAX_AGE_SECS).contains(&s)) {
        return Err(AppError::BadRequest("max_age_secs doit être entre 60 s et 30 jours.".into()));
    }
    let expires_at = body.max_age_secs.map(|secs| Utc::now() + TimeDelta::seconds(secs));

    // 62^8 ≈ 2·10^14 codes: collisions are rare, but handled.
    for _ in 0..5 {
        let code = auth::random_code(INVITE_CODE_LEN)?;
        if db::invites::create(&state.db, &code, guild_id, caller.user.id, body.max_uses, expires_at).await? {
            let invite = db::invites::get(&state.db, &code)
                .await?
                .ok_or_else(|| anyhow!("invite {code} vanished right after creation"))?;
            return Ok((StatusCode::CREATED, Json(invite)));
        }
    }
    Err(anyhow!("could not allocate a unique invite code").into())
}

/// Preview shown before joining ("You've been invited to …").
pub async fn get(State(state): State<AppState>, _caller: AuthUser, Path(code): Path<String>) -> Result<Json<Invite>> {
    db::invites::get(&state.db, &code).await?.map(Json).ok_or(AppError::NotFound(INVALID_INVITE))
}

/// Joins the invite's guild. Idempotent: an existing member just gets the guild back.
pub async fn accept(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(code): Path<String>,
) -> Result<Json<FullGuild>> {
    let user_id = caller.user.id;
    let invite = db::invites::get(&state.db, &code).await?.ok_or(AppError::NotFound(INVALID_INVITE))?;
    let guild_id = invite.guild.id;

    if db::guilds::membership(&state.db, guild_id, user_id).await?.is_none() {
        if db::guilds::count_for_user(&state.db, user_id).await? >= MAX_GUILDS_PER_USER {
            return Err(AppError::BadRequest("Vous avez atteint la limite de 100 serveurs.".into()));
        }
        let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
        // In the transaction: a ban cannot slip in between the check and the join.
        if db::bans::is_banned(&mut tx, guild_id, user_id).await? {
            return Err(AppError::Forbidden("Vous avez été banni de ce serveur."));
        }
        let joined = db::guilds::add_member(&mut tx, guild_id, user_id).await?;
        if joined {
            db::read_states::catch_up(&mut tx, guild_id, user_id).await?;
        }
        // Only consume a use when we actually joined (concurrent double-clicks are free).
        if joined && db::invites::redeem(&mut tx, &code).await?.is_none() {
            return Err(AppError::NotFound(INVALID_INVITE)); // tx dropped → rolled back
        }
        tx.commit().await?;

        if joined {
            state.hub.join_guild(user_id, guild_id);
            if let Some(row) = db::guilds::member(&state.db, guild_id, user_id).await? {
                let member = to_member(&state, row);
                state.hub.dispatch_to_guild(guild_id, Event::GuildMemberAdd { guild_id, member });
            }
        }
    }

    let mut guild = db::guilds::full(&state.db, guild_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    state.voice.fill(std::slice::from_mut(&mut guild));
    state.hub.dispatch_to_user(user_id, Event::GuildCreate(guild.clone()));
    Ok(Json(guild))
}
