use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use chrono::{TimeDelta, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::http::{AuthResponse, Credentials, Registration};
use plume_lite_protocol::model::User;
use plume_lite_protocol::validate;

use crate::auth::{self, AuthUser};
use crate::db;
use crate::error::{AppError, Result};
use crate::extract::{ClientIp, Json};
use crate::state::AppState;

const INVALID_CREDENTIALS: &str = "Nom d'utilisateur ou mot de passe incorrect.";
const INVALID_CODE: &str = "Code d'invitation invalide, expiré ou déjà utilisé.";
const DISABLED: &str = "Ce compte est désactivé. Contactez l'administrateur du serveur.";

pub async fn register(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Json(body): Json<Registration>,
) -> Result<(StatusCode, Json<AuthResponse>)> {
    state.limits.check(&state.limits.auth, ip).map_err(AppError::RateLimited)?;
    validate::username(&body.username)?;
    validate::password(&body.password)?;
    let code = body.invite_code.as_deref().map(str::trim).filter(|code| !code.is_empty());
    if !state.config.open_registration && code.is_none() {
        return Err(AppError::Forbidden("Un code d'invitation est nécessaire pour créer un compte."));
    }

    // Hash before taking the write lock: Argon2 takes tens of milliseconds.
    let password_hash = auth::hash_password(body.password).await?;
    let mut tx = state.db.begin_with(db::BEGIN_WRITE).await?;
    if !state.config.open_registration && !db::registration::redeem(&mut tx, code.unwrap_or_default()).await? {
        return Err(AppError::Forbidden(INVALID_CODE));
    }
    // A taken name rolls the transaction back: the code is not used up.
    let user = db::users::create(&mut tx, state.ids.next(), &body.username, &password_hash).await?;
    tx.commit().await?;

    let token = open_session(&state, user.id, &headers).await?;
    tracing::info!(user_id = %user.id, "user registered");
    Ok((StatusCode::CREATED, Json(AuthResponse { token, user })))
}

pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
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
    // Only told after the right password: strangers cannot probe which accounts are disabled.
    if stored.disabled {
        return Err(AppError::Forbidden(DISABLED));
    }

    let token = open_session(&state, stored.id, &headers).await?;
    let user = User { id: stored.id, username: stored.username };
    Ok(Json(AuthResponse { token, user }))
}

pub async fn logout(State(state): State<AppState>, caller: AuthUser) -> Result<StatusCode> {
    db::users::delete_session(&state.db, &caller.token_hash).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn open_session(state: &AppState, user_id: Snowflake, headers: &HeaderMap) -> Result<String> {
    let token = auth::generate_token()?;
    let ttl = TimeDelta::from_std(state.config.session_ttl).unwrap_or(TimeDelta::days(30));
    let device = device(headers);
    db::users::create_session(
        &state.db,
        state.ids.next(),
        &auth::hash_token(&token),
        user_id,
        &device,
        Utc::now() + ttl,
    )
    .await?;
    Ok(token)
}

/// What the user sees in their list of devices: the client's `User-Agent`, bounded and
/// without control characters.
fn device(headers: &HeaderMap) -> String {
    let agent = headers.get(header::USER_AGENT).and_then(|value| value.to_str().ok()).unwrap_or_default();
    agent.chars().filter(|c| !c.is_control()).take(200).collect::<String>().trim().to_owned()
}
