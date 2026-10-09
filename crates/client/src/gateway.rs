//! Realtime connection, exposed to the UI as an iced `Subscription`.
//!
//! The subscription is keyed by `(url, token)`: logging out drops it (closing the
//! socket), logging in as someone else starts a new one. Connection losses are
//! retried forever with exponential backoff + jitter; every successful connection
//! starts with a fresh READY, which the UI treats as a full resync.

use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use iced::Subscription;
use iced::futures::channel::mpsc;
use plume_lite_protocol::gateway::{ClientFrame, Event, Ready, ServerFrame, close_code};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub enum GatewayEvent {
    Ready(Box<Ready>),
    Event(Box<Event>),
    Disconnected {
        reason: String,
        retry_in: Duration,
    },
    /// Token rejected: the user must log in again. The subscription stays idle.
    AuthFailed,
}

pub fn subscription(target: Target) -> Subscription<GatewayEvent> {
    Subscription::run_with(target, connect)
}

const MAX_BACKOFF: Duration = Duration::from_secs(30);

fn connect(target: &Target) -> impl Stream<Item = GatewayEvent> + use<> {
    let target = target.clone();
    iced::stream::channel(100, async move |mut output: mpsc::Sender<GatewayEvent>| {
        let mut attempt: u32 = 0;
        loop {
            let (reason, was_ready) = match run(&target, &mut output).await {
                End::AuthFailed => {
                    let _ = output.send(GatewayEvent::AuthFailed).await;
                    return std::future::pending().await;
                }
                End::Closed => return, // the UI dropped the subscription
                End::Lost { reason, was_ready } => (reason, was_ready),
            };
            if was_ready {
                attempt = 0;
            }
            let retry_in = backoff(attempt);
            attempt = attempt.saturating_add(1);
            if output.send(GatewayEvent::Disconnected { reason, retry_in }).await.is_err() {
                return;
            }
            tokio::time::sleep(retry_in).await;
        }
    })
}

/// 1 s, 2 s, 4 s … capped at 30 s, plus up to 1 s of jitter so that thousands of
/// clients do not reconnect in lockstep after a server restart.
fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(1u64 << attempt.min(5)).min(MAX_BACKOFF);
    let jitter = getrandom::u32().unwrap_or(0) % 1000;
    base + Duration::from_millis(u64::from(jitter))
}

enum End {
    AuthFailed,
    Closed,
    Lost { reason: String, was_ready: bool },
}

async fn run(target: &Target, output: &mut mpsc::Sender<GatewayEvent>) -> End {
    let lost = |reason: String, was_ready: bool| End::Lost { reason, was_ready };

    let socket = match connect_async(target.url.as_str()).await {
        Ok((socket, _response)) => socket,
        Err(err) => return lost(format!("connexion impossible ({err})"), false),
    };
    let (mut sink, mut stream) = socket.split();

    let interval = match stream.next().await {
        Some(Ok(WsMessage::Text(text))) => match serde_json::from_str(text.as_str()) {
            Ok(ServerFrame::Hello { heartbeat_interval_ms }) => Duration::from_millis(heartbeat_interval_ms),
            _ => return lost("réponse inattendue du serveur".into(), false),
        },
        _ => return lost("connexion fermée pendant la poignée de main".into(), false),
    };

    let identify = ClientFrame::Identify { token: target.token.clone() };
    if sink.send(frame(&identify)).await.is_err() {
        return lost("connexion fermée".into(), false);
    }

    let mut was_ready = false;
    let mut awaiting_ack = false;
    let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                // No ack since the previous beat: the connection is a zombie, drop it.
                if awaiting_ack {
                    return lost("le serveur ne répond plus".into(), was_ready);
                }
                awaiting_ack = true;
                if sink.send(frame(&ClientFrame::Heartbeat)).await.is_err() {
                    return lost("connexion perdue".into(), was_ready);
                }
            }
            incoming = stream.next() => {
                let event = match incoming {
                    Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<ServerFrame>(text.as_str()) {
                        Ok(ServerFrame::Ready(ready)) => {
                            was_ready = true;
                            GatewayEvent::Ready(ready)
                        }
                        Ok(ServerFrame::Dispatch(event)) => GatewayEvent::Event(event),
                        Ok(ServerFrame::HeartbeatAck) => {
                            awaiting_ack = false;
                            continue;
                        }
                        // Unknown frame or event type from a newer server: ignore it.
                        Ok(ServerFrame::Hello { .. }) | Err(_) => continue,
                    },
                    Some(Ok(WsMessage::Close(close))) => {
                        let code = close.as_ref().map(|c| u16::from(c.code));
                        if code == Some(close_code::AUTHENTICATION_FAILED) {
                            return End::AuthFailed;
                        }
                        let reason = close
                            .map(|c| c.reason.to_string())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| "connexion fermée par le serveur".into());
                        return lost(reason, was_ready);
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return lost(format!("connexion perdue ({err})"), was_ready),
                    None => return lost("connexion fermée".into(), was_ready),
                };
                if output.send(event).await.is_err() {
                    return End::Closed;
                }
            }
        }
    }
}

fn frame(frame: &ClientFrame) -> WsMessage {
    WsMessage::text(serde_json::to_string(frame).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert!(backoff(0) >= Duration::from_secs(1) && backoff(0) < Duration::from_secs(2));
        assert!(backoff(3) >= Duration::from_secs(8));
        assert!(backoff(50) <= MAX_BACKOFF + Duration::from_secs(1));
    }
}
