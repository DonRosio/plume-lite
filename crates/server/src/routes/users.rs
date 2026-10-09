use axum::extract::State;
use axum::http::StatusCode;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::http::ChangePassword;
use plume_lite_protocol::model::{FullGuild, SessionInfo, User};
use plume_lite_protocol::validate;

use crate::auth::{self, AuthUser};
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{ClientIp, Json, Path};
use crate::state::AppState;

pub async fn me(caller: AuthUser) -> Json<User> {
    Json(caller.user)
}

pub async fn guilds(State(state): State<AppState>, caller: AuthUser) -> Result<Json<Vec<FullGuild>>> {
    Ok(Json(db::guilds::full_for_user(&state.db, caller.user.id).await?))
}

/// Changes the password and logs out every other device (a stolen session dies with it).
pub async fn change_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    caller: AuthUser,
    Json(body): Json<ChangePassword>,
) -> Result<StatusCode> {
    // Same budget as logins: the current password must not be guessable from here either.
    state.limits.check(&state.limits.auth, ip).map_err(AppError::RateLimited)?;
    validate::password(&body.new_password)?;
    let hash = db::users::password_hash(&state.db, caller.user.id).await?.ok_or_else(AppError::unauthenticated)?;
    if !auth::verify_password(body.current_password, hash).await? {
        return Err(AppError::Forbidden("Le mot de passe actuel est incorrect."));
    }
    let new_hash = auth::hash_password(body.new_password).await?;
    db::users::set_password(&state.db, caller.user.id, &new_hash).await?;
    let revoked = db::users::delete_sessions(&state.db, caller.user.id, Some(caller.session_id)).await?;
    tracing::info!(user_id = %caller.user.id, revoked, "password changed");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn sessions(State(state): State<AppState>, caller: AuthUser) -> Result<Json<Vec<SessionInfo>>> {
    let sessions = db::users::sessions(&state.db, caller.user.id).await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(|row| SessionInfo {
                id: row.id,
                device: row.device,
                created_at: row.id.created_at(),
                last_seen_at: row.last_seen_at,
                current: row.id == caller.session_id,
            })
            .collect(),
    ))
}

/// Logs a device out. Its live gateway connection closes at its next heartbeat.
pub async fn revoke_session(
    State(state): State<AppState>,
    caller: AuthUser,
    Path(session_id): Path<Snowflake>,
) -> Result<StatusCode> {
    if !db::users::delete_session_of(&state.db, caller.user.id, session_id).await? {
        return Err(AppError::NotFound("Session introuvable."));
    }
    Ok(StatusCode::NO_CONTENT)
}
