//! Resources exposed by the API and pushed by the gateway.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Snowflake;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct User {
    pub id: Snowflake,
    pub username: String,
}

/// A community ("server" in the UI, "guild" in the API to avoid the ambiguity with the backend).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Guild {
    pub id: Snowflake,
    pub name: String,
    pub owner_id: Snowflake,
}

/// A guild with everything a client needs to render it (sent in `READY` and `GUILD_CREATE`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullGuild {
    #[serde(flatten)]
    pub guild: Guild,
    pub channels: Vec<Channel>,
    pub member_count: i64,
    /// Who is in which voice channel right now.
    #[serde(default)]
    pub voice_states: Vec<VoiceState>,
}

/// A member's presence in a voice channel. `channel_id: None` means they left voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceState {
    pub guild_id: Snowflake,
    pub user_id: Snowflake,
    pub channel_id: Option<Snowflake>,
    /// Muted their own microphone.
    #[serde(default)]
    pub self_mute: bool,
    /// Turned their own sound off (implies not hearing anyone).
    #[serde(default)]
    pub self_deaf: bool,
    /// Muted by the guild owner: cannot speak until unmuted.
    #[serde(default)]
    pub server_mute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[repr(i16)]
pub enum ChannelKind {
    #[default]
    Text = 0,
    /// Voice channel: members join it to talk (through the LiveKit media server).
    Voice = 1,
    /// A kind introduced by a newer server: old clients keep working and simply ignore it.
    #[serde(other)]
    Unknown = -1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Channel {
    pub id: Snowflake,
    pub guild_id: Snowflake,
    pub name: String,
    pub kind: ChannelKind,
    pub position: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Online,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub user: User,
    pub joined_at: DateTime<Utc>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub guild_id: Snowflake,
    pub author: User,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// Client-chosen token echoed back on creation so the sender can reconcile
    /// its optimistic copy. Never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub code: String,
    pub guild: Guild,
    pub member_count: i64,
    pub uses: i32,
    pub max_uses: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// One logged-in device of the current user (`GET /users/@me/sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Snowflake,
    /// The `User-Agent` of the client that opened the session, e.g.
    /// `plume-lite-desktop/0.3.0 (windows)`.
    pub device: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// The session making this request.
    pub current: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_kind_is_tolerated() {
        let kind: ChannelKind = serde_json::from_str("\"stage\"").unwrap();
        assert_eq!(kind, ChannelKind::Unknown);
        assert_eq!(serde_json::from_str::<ChannelKind>("\"voice\"").unwrap(), ChannelKind::Voice);
    }

    #[test]
    fn full_guild_is_flattened() {
        let g = FullGuild {
            guild: Guild { id: Snowflake(1), name: "Rust FR".into(), owner_id: Snowflake(2) },
            channels: vec![],
            member_count: 1,
            voice_states: vec![],
        };
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["name"], "Rust FR");
        assert_eq!(json["owner_id"], "2");
        assert_eq!(serde_json::from_value::<FullGuild>(json).unwrap(), g);
    }
}
