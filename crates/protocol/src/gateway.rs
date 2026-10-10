//! Realtime gateway protocol: JSON text frames over a WebSocket.
//!
//! ```text
//! client                                   server
//!   | ---------- WebSocket upgrade ---------> |
//!   | <--------- hello {interval} ----------- |
//!   | ---------- identify {token} ----------> |   (within IDENTIFY_TIMEOUT_MS)
//!   | <--------- ready {user, guilds} ------- |
//!   | ---------- heartbeat -----------------> |   every `heartbeat_interval_ms`
//!   | <--------- heartbeat_ack -------------- |
//!   | <--------- dispatch {t, d} ------------ |   pushed as things happen
//! ```
//!
//! Clients must treat every event as an idempotent upsert: the same event can be
//! delivered twice around a (re)connection, and the REST response of a mutation
//! races with its own dispatch.

use serde::{Deserialize, Serialize};

use crate::Snowflake;
use crate::model::{Channel, FullGuild, Member, Message, Status, User, VoiceState};

/// Default interval at which clients must send [`ClientFrame::Heartbeat`].
pub const HEARTBEAT_INTERVAL_MS: u64 = 30_000;
/// Time allowed between the upgrade and [`ClientFrame::Identify`].
pub const IDENTIFY_TIMEOUT_MS: u64 = 10_000;

/// WebSocket close codes used by the gateway (4000-4999 are application-defined).
pub mod close_code {
    pub const UNKNOWN_ERROR: u16 = 4000;
    pub const DECODE_ERROR: u16 = 4002;
    pub const NOT_AUTHENTICATED: u16 = 4003;
    /// Invalid, expired or revoked token (logged out from another device, password changed,
    /// account disabled): do NOT reconnect, ask the user to log in again.
    pub const AUTHENTICATION_FAILED: u16 = 4004;
    pub const ALREADY_AUTHENTICATED: u16 = 4005;
    /// The client sent too many frames or could not keep up with the event stream.
    pub const RATE_LIMITED: u16 = 4008;
    pub const SESSION_TIMEOUT: u16 = 4009;
    /// Standard "service restart" code: reconnect (with backoff).
    pub const SERVICE_RESTART: u16 = 1012;
}

/// Frames sent by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ClientFrame {
    Identify { token: String },
    Heartbeat,
}

/// Frames sent by the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ServerFrame {
    Hello { heartbeat_interval_ms: u64 },
    Ready(Box<Ready>),
    HeartbeatAck,
    Dispatch(Box<Event>),
}

/// Initial state sent after a successful identify. A reconnect always starts
/// with a fresh `Ready`, which is the client's resynchronisation point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    pub user: User,
    pub guilds: Vec<FullGuild>,
}

/// Everything that can be pushed to a client after `Ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "d", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    MessageCreate(Message),
    MessageUpdate(Message),
    MessageDelete {
        id: Snowflake,
        channel_id: Snowflake,
        guild_id: Snowflake,
    },
    ChannelCreate(Channel),
    ChannelDelete {
        id: Snowflake,
        guild_id: Snowflake,
    },
    /// The current user joined (or created) a guild.
    GuildCreate(FullGuild),
    /// The guild was deleted, or the current user left / was removed from it.
    GuildDelete {
        id: Snowflake,
    },
    GuildMemberAdd {
        guild_id: Snowflake,
        member: Member,
    },
    GuildMemberRemove {
        guild_id: Snowflake,
        user: User,
    },
    PresenceUpdate {
        user_id: Snowflake,
        status: Status,
    },
    TypingStart {
        guild_id: Snowflake,
        channel_id: Snowflake,
        user_id: Snowflake,
    },
    /// Someone joined, left (`channel_id: None`) or changed state in a voice channel.
    VoiceStateUpdate(VoiceState),
    /// Sent only to the member the guild owner moves: their client, if it is in voice in that
    /// guild, switches to `channel_id` (the media server cannot move people between rooms).
    VoiceMove {
        guild_id: Snowflake,
        channel_id: Snowflake,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_a_stable_shape() {
        let hb = serde_json::to_string(&ClientFrame::Heartbeat).unwrap();
        assert_eq!(hb, r#"{"op":"heartbeat"}"#);

        let identify: ClientFrame = serde_json::from_str(r#"{"op":"identify","d":{"token":"abc"}}"#).unwrap();
        assert_eq!(identify, ClientFrame::Identify { token: "abc".into() });

        let frame = ServerFrame::Dispatch(Box::new(Event::GuildDelete { id: Snowflake(7) }));
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(json, r#"{"op":"dispatch","d":{"t":"GUILD_DELETE","d":{"id":"7"}}}"#);
        assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);

        let moved = Event::VoiceMove { guild_id: Snowflake(1), channel_id: Snowflake(2) };
        let json = serde_json::to_string(&moved).unwrap();
        assert_eq!(json, r#"{"t":"VOICE_MOVE","d":{"guild_id":"1","channel_id":"2"}}"#);
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), moved);
    }
}
