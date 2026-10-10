//! REST request/response bodies. Routes are documented in `docs/API.md`.

use serde::{Deserialize, Serialize};

use crate::Snowflake;
use crate::model::{ChannelKind, User};

pub const MESSAGES_PAGE_DEFAULT: u32 = 50;
pub const MESSAGES_PAGE_MAX: u32 = 100;
pub const MEMBERS_PAGE_DEFAULT: u32 = 500;
pub const MEMBERS_PAGE_MAX: u32 = 1000;

/// Body of `POST /auth/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Body of `POST /auth/register`. The invite code is required unless the instance has
/// open registration (see [`InstanceInfo::open_registration`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub username: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
}

/// Body of `PUT /users/@me/password`. Every other session of the account is revoked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

/// `GET /instance`: public facts about this server, readable before logging in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Version of the server (clients and server are released together).
    pub version: String,
    pub open_registration: bool,
    /// The desktop client this server distributes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRelease {
    pub version: String,
    /// Path on this server, e.g. `/download/plume-lite.exe`.
    pub download_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Opaque bearer token. Send it as `Authorization: Bearer <token>`.
    pub token: String,
    pub user: User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGuild {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChannel {
    pub name: String,
    #[serde(default)]
    pub kind: ChannelKind,
}

/// Answer of `POST /channels/{id}/voice`: where and how to join the voice channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceJoin {
    /// WebSocket URL of the media server (LiveKit), e.g. `wss://plume.donrgames.fr`.
    pub url: String,
    /// Short-lived access token for this channel only.
    pub token: String,
}

/// Body of `PATCH /users/@me/voice`: the caller's own microphone and sound.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UpdateVoiceState {
    pub self_mute: bool,
    pub self_deaf: bool,
}

/// Body of `PUT /guilds/{id}/voice/{user_id}/mute` (guild owner).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ServerMute {
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessage {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditMessage {
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateInvite {
    /// `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,
    /// `None` = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_secs: Option<i64>,
}

/// Query of `GET /channels/{id}/messages`. `before` and `after` are exclusive and
/// mutually exclusive; results are always returned oldest → newest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Snowflake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Snowflake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Query of `GET /guilds/{id}/members` (members sorted by user id).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Snowflake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Machine-readable error category. Clients branch on this, never on `message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    Internal,
    /// The feature is not available on this server (e.g. voice not configured).
    Unavailable,
    #[serde(other)]
    Unknown,
}

/// Error envelope returned with every non-2xx response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    /// Human readable, safe to show to end users.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}
