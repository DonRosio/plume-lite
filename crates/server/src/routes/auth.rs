use axum::extract::State;
use axum::http::StatusCode;
use chrono::{TimeDelta, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::http::{AuthResponse, Credentials};
use plume_lite_protocol::model::User;
use plume_lite_protocol::validate;

use crate::auth::{self, AuthUser};
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{ClientIp, Json};
use crate::state::AppState;

const INVALID_CREDENTIALS: &str = "Nom d'utilisateur ou mot de passe incorrect.";

pub async fn register(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(body): Json<Credentials>,
) -> Result<(StatusCode, Json<AuthResponse>)> {
    state.limits.check(&state.limits.auth, ip).map_err(AppError::RateLimited)?;
    validate::username(&body.username)?;
    validate::password(&body.password)?;

    let password_hash = auth::hash_password(body.password).await?;
    let user = db::users::create(&state.db, state.ids.next(), &body.username, &password_hash).await?;
    let token = open_session(&state, user.id).await?;
    tracing::info!(user_id = %user.id, "user registered");
    Ok((StatusCode::CREATED, Json(AuthResponse { token, user })))
}

pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(body): Json<Credentials>,
) -> Result<Json<AuthResponse>> {
    state.limits.check(&state.limits.auth, ip).map_err(AppError::RateLimited)?;

    let Some(stored) = db::users::credentials(&state.db, &body.username).await? else {
        auth::verify_dummy(body.password).await?;
        return Err(AppError::Unauthorized(INVALID_CREDENTIALS));
    };
    if !auth::verify_password(body.password, stored.password_hash).await? {
        return Err(AppError::Unauthorized(INVALID_CREDENTIALS));
    }

    let token = open_session(&state, stored.id).await?;
    let user = User { id: stored.id, username: stored.username };
    Ok(Json(AuthResponse { token, user }))
}

pub async fn logout(State(state): State<AppState>, caller: AuthUser) -> Result<StatusCode> {
    db::users::delete_session(&state.db, &caller.token_hash).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn open_session(state: &AppState, user_id: Snowflake) -> Result<String> {
    let token = auth::generate_token()?;
    let ttl = TimeDelta::from_std(state.config.session_ttl).unwrap_or(TimeDelta::days(30));
    db::users::create_session(&state.db, &auth::hash_token(&token), user_id, Utc::now() + ttl).await?;
    Ok(token)
}
