//! HTTP surface: REST API under `/api/v1`, the gateway upgrade and health probes.

mod auth;
mod bans;
mod categories;
mod channels;
mod guilds;
pub mod instance;
mod invites;
mod messages;
mod roles;
mod users;
pub mod voice;

use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use plume_lite_protocol::gateway::Event;
use plume_lite_protocol::model::{Category, Channel};
use plume_lite_protocol::permissions::{Permissions, Standing};
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
        .route("/users/@me/notifications/{target_id}", put(users::set_notifications).delete(users::reset_notifications))
        .route("/users/@me/voice", patch(voice::update_self))
        .route("/channels/{channel_id}/voice", post(voice::join))
        .route("/guilds/{guild_id}/voice/{user_id}/mute", put(voice::server_mute))
        .route("/guilds/{guild_id}/voice/{user_id}/disconnect", post(voice::disconnect))
        .route("/guilds/{guild_id}/voice/{user_id}/move", post(voice::move_member))
        .route("/voice/webhook", post(voice::webhook))
        .route("/guilds", post(guilds::create))
        .route("/guilds/{guild_id}", get(guilds::get).patch(guilds::update).delete(guilds::delete))
        .route("/guilds/{guild_id}/owner", put(guilds::transfer))
        .route("/guilds/{guild_id}/channels", get(guilds::channels).post(guilds::create_channel))
        .route("/guilds/{guild_id}/categories", post(categories::create))
        .route("/categories/{category_id}", patch(categories::update).delete(categories::delete))
        .route("/guilds/{guild_id}/members", get(guilds::members))
        .route("/guilds/{guild_id}/members/{user}", delete(guilds::remove_member))
        .route("/guilds/{guild_id}/roles", post(roles::create))
        .route("/guilds/{guild_id}/roles/{role_id}", patch(roles::update).delete(roles::delete))
        .route("/guilds/{guild_id}/members/{user_id}/roles/{role_id}", put(roles::give).delete(roles::take))
        .route("/guilds/{guild_id}/bans", get(bans::list))
        .route("/guilds/{guild_id}/bans/{user_id}", put(bans::ban).delete(bans::unban))
        .route("/guilds/{guild_id}/invites", post(invites::create))
        .route("/invites/{code}", get(invites::get).post(invites::accept))
        .route("/channels/{channel_id}", patch(channels::update).delete(channels::delete))
        .route("/channels/{channel_id}/typing", post(channels::typing))
        .route("/channels/{channel_id}/ack", post(channels::ack))
        .route("/channels/{channel_id}/messages", get(messages::list).post(messages::create))
        .route("/channels/{channel_id}/messages/{message_id}", patch(messages::edit).delete(messages::delete))
        .route(
            "/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me",
            put(messages::react).delete(messages::unreact),
        )
        .route("/guilds/{guild_id}/messages/search", get(messages::search))
        .route("/channels/{channel_id}/pins", get(messages::pins))
        .route("/channels/{channel_id}/pins/{message_id}", put(messages::pin).delete(messages::unpin))
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
const UNKNOWN_CATEGORY: &str = "Catégorie introuvable.";
const UNKNOWN_MEMBER: &str = "Membre introuvable.";
const OWNER_ONLY: &str = "Action réservée au propriétaire du serveur.";
const NOT_ALLOWED: &str = "Vous n'avez pas la permission de faire cela.";
const OUTRANKED: &str = "Vous ne pouvez agir que sur les membres placés sous votre rôle le plus haut.";

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

/// Where a member stands in a guild (`None`: not a member).
async fn standing(state: &AppState, guild_id: Snowflake, user_id: Snowflake) -> Result<Option<Standing>> {
    let mut conn = state.db.acquire().await?;
    db::roles::standing(&mut conn, guild_id, user_id).await
}

/// A member who has `permission`: non-members get a 404, members without it a 403.
async fn require_permission(
    state: &AppState,
    guild_id: Snowflake,
    user_id: Snowflake,
    permission: Permissions,
) -> Result<Standing> {
    let standing = standing(state, guild_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_GUILD))?;
    if !standing.can(permission) {
        return Err(AppError::Forbidden(NOT_ALLOWED));
    }
    Ok(standing)
}

/// `actor` may act on the member `target` (someone else): a 404 if they are not a member, a
/// 403 if not ranked strictly below `actor`, or if they own the guild.
async fn require_outranked(state: &AppState, actor: &Standing, guild_id: Snowflake, target: Snowflake) -> Result<()> {
    let target = standing(state, guild_id, target).await?.ok_or(AppError::NotFound(UNKNOWN_MEMBER))?;
    if !actor.outranks(&target) {
        return Err(AppError::Forbidden(OUTRANKED));
    }
    Ok(())
}

async fn require_channel(state: &AppState, channel_id: Snowflake, user_id: Snowflake) -> Result<ChannelAccess> {
    db::channels::access(&state.db, channel_id, user_id).await?.ok_or(AppError::NotFound(UNKNOWN_CHANNEL))
}

/// Tells the guild about the channels and categories a change of layout touched (except
/// `created`, announced by its own `*_CREATE`).
fn dispatch_layout(
    state: &AppState,
    guild_id: Snowflake,
    channels: Vec<Channel>,
    categories: Vec<Category>,
    created: Option<Snowflake>,
) {
    for category in categories.into_iter().filter(|c| Some(c.id) != created) {
        state.hub.dispatch_to_guild(guild_id, Event::CategoryUpdate(category));
    }
    for channel in channels.into_iter().filter(|c| Some(c.id) != created) {
        state.hub.dispatch_to_guild(guild_id, Event::ChannelUpdate(channel));
    }
}
