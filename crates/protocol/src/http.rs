//! REST request/response bodies. Routes are documented in `docs/API.md`.

use serde::{Deserialize, Serialize};

use crate::Snowflake;
use crate::model::User;

pub const MESSAGES_PAGE_DEFAULT: u32 = 50;
pub const MESSAGES_PAGE_MAX: u32 = 100;
pub const MEMBERS_PAGE_DEFAULT: u32 = 500;
pub const MEMBERS_PAGE_MAX: u32 = 1000;

/// Body of `POST /auth/register` and `POST /auth/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
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
