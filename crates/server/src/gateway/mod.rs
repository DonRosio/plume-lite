//! WebSocket gateway: authentication, READY, heartbeats and event delivery.
//! The wire format is documented in `plume_lite_protocol::gateway`.

pub mod hub;

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::{
    ClientFrame, Event, HEARTBEAT_INTERVAL_MS, IDENTIFY_TIMEOUT_MS, Ready, ServerFrame, close_code,
};
use plume_lite_protocol::model::{Status, User};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_util::sync::CancellationToken;

use crate::state::AppState;
use crate::{auth, db};

/// Client frames are tiny (identify / heartbeat): refuse anything bigger early.
const MAX_CLIENT_FRAME_BYTES: usize = 4 * 1024;
/// A well-behaved client sends ~2 frames per minute.
const MAX_CLIENT_FRAMES_PER_MINUTE: u32 = 60;

pub async fn upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_CLIENT_FRAME_BYTES)
        .max_frame_size(MAX_CLIENT_FRAME_BYTES)
        .on_upgrade(move |socket| run_session(state, socket))
}

/// Why a session ended, and how to tell the client.
enum Exit {
    /// The peer closed or vanished: nothing left to send.
    Gone,
    Close(u16, &'static str),
}

async fn run_session(state: AppState, mut socket: WebSocket) {
    let hello = ServerFrame::Hello { heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS };
    if send(&mut socket, &hello).await.is_err() {
        return;
    }

    let (user, token_hash) =
        match timeout(Duration::from_millis(IDENTIFY_TIMEOUT_MS), identify(&state, &mut socket)).await {
            Ok(Ok(identified)) => identified,
            Ok(Err(exit)) => return close(socket, exit).await,
            Err(_) => return close(socket, Exit::Close(close_code::NOT_AUTHENTICATED, "identify timeout")).await,
        };

    let loaded = match db::guilds::full_for_user(&state.db, user.id).await {
        Ok(guilds) => match db::read_states::for_user(&state.db, user.id).await {
            Ok(read_states) => db::notifications::for_user(&state.db, user.id)
                .await
                .map(|notification_settings| (guilds, read_states, notification_settings)),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    };
    let (mut guilds, mut read_states, notification_settings) = match loaded {
        Ok(loaded) => loaded,
        Err(err) => {
            tracing::error!(error = %err, "failed to load READY state");
            return close(socket, Exit::Close(close_code::UNKNOWN_ERROR, "internal error")).await;
        }
    };
    state.voice.fill(&mut guilds);
    // Read states only for the channels this user may see.
    let visible: std::collections::HashSet<Snowflake> =
        guilds.iter().flat_map(|g| g.channels.iter().map(|c| c.id)).collect();
    read_states.retain(|read| visible.contains(&read.channel_id));
    let guild_ids: Vec<Snowflake> = guilds.iter().map(|g| g.guild.id).collect();

    // Register *before* sending READY: events published meanwhile are queued, not lost.
    // Clients apply events idempotently, so an event already reflected in READY is harmless.
    let kill = state.shutdown.child_token();
    let mut registration = state.hub.connect(user.id, guild_ids.iter().copied(), kill.clone());
    tracing::debug!(user_id = %user.id, session = registration.session_id, "gateway session ready");

    let ready = ServerFrame::Ready(Box::new(Ready { user: user.clone(), guilds, read_states, notification_settings }));
    let exit = if send(&mut socket, &ready).await.is_ok() {
        if registration.came_online {
            set_presence(&state, &user, &guild_ids, Status::Online);
        }
        pump(&state, &mut socket, &mut registration.events, &kill, &token_hash).await
    } else {
        Exit::Gone
    };

    if let Some(guilds) = state.hub.disconnect(user.id, registration.session_id) {
        set_presence(&state, &user, &guilds, Status::Offline);
    }
    let reason = match &exit {
        Exit::Gone => "peer gone",
        Exit::Close(_, reason) => reason,
    };
    tracing::debug!(user_id = %user.id, session = registration.session_id, reason, "gateway session closed");
    close(socket, exit).await;
}

/// Returns the user and the hash of its token, checked again at every heartbeat.
async fn identify(state: &AppState, socket: &mut WebSocket) -> Result<(User, Vec<u8>), Exit> {
    loop {
        let text = match socket.recv().await {
            Some(Ok(WsMessage::Text(text))) => text,
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Binary(_))) => {
                return Err(Exit::Close(close_code::DECODE_ERROR, "binary frames are not supported"));
            }
            Some(Ok(WsMessage::Close(_)) | Err(_)) | None => return Err(Exit::Gone),
        };
        return match serde_json::from_str::<ClientFrame>(&text) {
            Ok(ClientFrame::Identify { token }) => {
                let token_hash = auth::hash_token(&token);
                match db::users::by_session(&state.db, &token_hash).await {
                    Ok(Some(session)) => Ok((session.user, token_hash)),
                    Ok(None) => Err(Exit::Close(close_code::AUTHENTICATION_FAILED, "invalid token")),
                    Err(err) => {
                        tracing::error!(error = %err, "identify lookup failed");
                        Err(Exit::Close(close_code::UNKNOWN_ERROR, "internal error"))
                    }
                }
            }
            Ok(_) => Err(Exit::Close(close_code::NOT_AUTHENTICATED, "identify first")),
            Err(_) => Err(Exit::Close(close_code::DECODE_ERROR, "invalid frame")),
        };
    }
}

/// Main loop: forward queued events, answer heartbeats, enforce timeouts.
async fn pump(
    state: &AppState,
    socket: &mut WebSocket,
    events: &mut tokio::sync::mpsc::Receiver<axum::extract::ws::Utf8Bytes>,
    kill: &CancellationToken,
    token_hash: &[u8],
) -> Exit {
    let interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);
    let grace = interval + interval / 2;
    let mut deadline = Instant::now() + grace;
    let mut window_start = Instant::now();
    let mut frames_in_window = 0u32;

    loop {
        tokio::select! {
            () = kill.cancelled() => {
                return if state.shutdown.is_cancelled() {
                    Exit::Close(close_code::SERVICE_RESTART, "server restarting")
                } else {
                    Exit::Close(close_code::RATE_LIMITED, "event queue overflow")
                };
            }
            event = events.recv() => match event {
                Some(frame) => {
                    if socket.send(WsMessage::Text(frame)).await.is_err() {
                        return Exit::Gone;
                    }
                }
                None => return Exit::Close(close_code::UNKNOWN_ERROR, "session closed"),
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Text(text))) => {
                    if window_start.elapsed() >= Duration::from_secs(60) {
                        window_start = Instant::now();
                        frames_in_window = 0;
                    }
                    frames_in_window += 1;
                    if frames_in_window > MAX_CLIENT_FRAMES_PER_MINUTE {
                        return Exit::Close(close_code::RATE_LIMITED, "too many frames");
                    }
                    match serde_json::from_str::<ClientFrame>(&text) {
                        Ok(ClientFrame::Heartbeat) => {
                            // A revoked session (logged out from another device, password
                            // changed, account disabled) is already refused by the REST API;
                            // its live connection closes here, within one heartbeat.
                            match db::users::by_session(&state.db, token_hash).await {
                                Ok(Some(_)) => {}
                                Ok(None) => return Exit::Close(close_code::AUTHENTICATION_FAILED, "session revoked"),
                                Err(err) => tracing::warn!(error = %err, "session check failed"),
                            }
                            deadline = Instant::now() + grace;
                            if send(socket, &ServerFrame::HeartbeatAck).await.is_err() {
                                return Exit::Gone;
                            }
                        }
                        Ok(ClientFrame::Identify { .. }) => {
                            return Exit::Close(close_code::ALREADY_AUTHENTICATED, "already identified");
                        }
                        Err(_) => return Exit::Close(close_code::DECODE_ERROR, "invalid frame"),
                    }
                }
                Some(Ok(WsMessage::Binary(_))) => {
                    return Exit::Close(close_code::DECODE_ERROR, "binary frames are not supported");
                }
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                Some(Ok(WsMessage::Close(_)) | Err(_)) | None => return Exit::Gone,
            },
            () = sleep_until(deadline) => {
                return Exit::Close(close_code::SESSION_TIMEOUT, "heartbeat timeout");
            }
        }
    }
}

fn set_presence(state: &AppState, user: &User, guilds: &[Snowflake], status: Status) {
    state.hub.dispatch_to_guilds(guilds, Event::PresenceUpdate { user_id: user.id, status });
}

async fn send(socket: &mut WebSocket, frame: &ServerFrame) -> Result<(), ()> {
    let json = serde_json::to_string(frame).map_err(|_| ())?;
    socket.send(WsMessage::Text(json.into())).await.map_err(|_| ())
}

async fn close(mut socket: WebSocket, exit: Exit) {
    if let Exit::Close(code, reason) = exit {
        let frame = CloseFrame { code, reason: reason.into() };
        let _ = socket.send(WsMessage::Close(Some(frame))).await;
    }
}
