//! HTTP surface: REST API under `/api/v1`, the gateway upgrade and health probes.

mod auth;
mod channels;
mod guilds;
pub mod instance;
mod invites;
mod messages;
mod users;

use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use plume_lite_protocol::{API_PREFIX, GATEWAY_PATH, Snowflake};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::ServeDir;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};

use crate::db;
use crate::db::channels::ChannelAccess;
use crate::error::{AppError, Result};
use crate::extract::client_ip;
use crate::gateway;
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/instance", get(instance::info))
        .route("/users/@me", get(users::me))
        .route("/users/@me/guilds", get(users::guilds))
        .route("/users/@me/password", put(users::change_password))
        .route("/users/@me/sessions", get(users::sessions))
        .route("/users/@me/sessions/{session_id}", delete(users::revoke_session))
        .route("/guilds", post(guilds::create))
        .route("/guilds/{guild_id}", get(guilds::get).delete(guilds::delete))
        .route("/guilds/{guild_id}/channels", get(guilds::channels).post(guilds::create_channel))
        .route("/guilds/{guild_id}/members", get(guilds::members))
        .route("/guilds/{guild_id}/members/{user}", delete(guilds::remove_member))
        .route("/guilds/{guild_id}/invites", post(invites::create))
        .route("/invites/{code}", get(invites::get).post(invites::accept))
        .route("/channels/{channel_id}", delete(channels::delete))
        .route("/channels/{channel_id}/typing", post(channels::typing))
        .route("/channels/{channel_id}/messages", get(messages::list).post(messages::create))
        .route("/channels/{channel_id}/messages/{message_id}", patch(messages::edit).delete(messages::delete))
        .fallback(|| async { AppError::NotFound("Route inconnue.") });

    let mut router = Router::new();
    if let Some(dir) = &state.config.downloads_dir {
        // The desktop client. ServeDir refuses paths that leave the folder.
        router = router.nest_service("/download", ServeDir::new(dir));
    }

    // Layers run outside-in from the bottom: request id → trace → … → handler.
    router
        .route("/", get(instance::home))
        .nest(API_PREFIX, api)
        .route(GATEWAY_PATH, get(gateway::upgrade))
        .route("/health", get(|| async { "ok" }))
        .route("/health/ready", get(readiness))
        .layer(middleware::from_fn_with_state(state.clone(), global_rate_limit))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(15)))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let request_id = req.headers().get("x-request-id").and_then(|v| v.to_str().ok()).unwrap_or("-");
                    tracing::info_span!("http", method = %req.method(), path = %req.uri().path(), request_id)
                })
                // One access-log line per request (status + latency). Silence with RUST_LOG=tower_http=warn.
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

/// Readiness probe for load balancers: fails while the database is unreachable.
async fn readiness(State(state): State<AppState>) -> Result<&'static str> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok("ready")
}

async fn global_rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.headers(), req.extensions(), state.config.trust_proxy);
    match state.limits.check(&state.limits.global, ip) {
        Ok(()) => next.run(req).await,
        Err(retry_after) => AppError::RateLimited(retry_after).into_response(),
    }
}

// --- Access control helpers shared by the handlers ---------------------------------------------

const UNKNOWN_GUILD: &str = "Serveur introuvable.";
const UNKNOWN_CHANNEL: &str = "Salon introuvable.";
const OWNER_ONLY: &str = "Action réservée au propriétaire du serveur.";

/// Membership check; returns the guild owner. Non-members get a 404, not a 403,
/// so that guild ids cannot be probed.
async fn require_member(state: &AppState, guild_id: Snowflake, user_id: Snowflake) -> Result<Snowflake> {
    db::guilds::membership(&state.db, guild_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))
}

async fn require_owner(state: &AppState, guild_id: Snowflake, user_id: Snowflake) -> Result<()> {
    if require_member(state, guild_id, user_id).await? != user_id {
        return Err(AppError::Forbidden(OWNER_ONLY));
    }
    Ok(())
}

async fn require_channel(state: &AppState, channel_id: Snowflake, user_id: Snowflake) -> Result<ChannelAccess> {
    db::channels::access(&state.db, channel_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_CHANNEL))
}
