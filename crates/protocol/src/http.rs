//! REST request/response bodies. Routes are documented in `docs/API.md`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    /// Path on this server of the Windows build (`/download/plume-lite.exe`), the only one
    /// clients before 0.5.1 know about.
    pub download_path: String,
    /// Every build, one per platform.
    #[serde(default)]
    pub downloads: Vec<ClientDownload>,
}

impl ClientRelease {
    /// The build for the platform this code runs on.
    pub fn for_this_platform(&self) -> Option<&ClientDownload> {
        self.downloads.iter().find(|download| download.platform == Platform::current())
    }
}

/// One build of the desktop client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientDownload {
    pub platform: Platform,
    /// Path on this server, e.g. `/download/plume-lite-linux-x64.tar.gz`.
    pub path: String,
    pub size: u64,
    /// SHA-256 of the file, in lowercase hexadecimal.
    pub sha256: String,
    /// Its minisign signature, made by the release workflow with the key whose public half
    /// is built into the client (`minisign.pub`). Absent from builds made by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Windows, x86-64.
    Windows,
    /// Linux, x86-64.
    Linux,
    /// A platform introduced by a newer server.
    #[serde(other)]
    Unknown,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unknown
        }
    }
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

/// Body of `PATCH /guilds/{id}` (manage the guild). Absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateGuild {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A text channel of the guild; `Some(None)` (`null`): newcomers are not announced.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub welcome_channel_id: Option<Option<Snowflake>>,
    /// `Some(None)` (`null`): back to [`crate::model::JOIN_MESSAGE`].
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub welcome_message: Option<Option<String>>,
}

/// Entries per page of `GET /guilds/{id}/audit-log`.
pub const AUDIT_PAGE: u32 = 50;

/// Query of `GET /guilds/{id}/audit-log`: newest first, older than `before`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Snowflake>,
}

/// Answer of `GET /guilds/{id}/audit-log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub entries: Vec<crate::model::AuditEntry>,
    /// Where the next page starts (`before`), if there may be more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Snowflake>,
}

/// Body of `PUT /guilds/{id}/owner` (guild owner): hands the guild to another member. The
/// owner's password is asked again, so that a stolen session cannot give the guild away.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferOwnership {
    pub user_id: Snowflake,
    pub password: String,
}

/// Body of `POST /guilds/{id}/roles` (manage roles).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRole {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(default)]
    pub permissions: crate::permissions::Permissions,
}

/// Body of `PATCH /guilds/{id}/roles/{role_id}` (manage roles). Absent fields are left
/// unchanged; `@everyone` only takes `permissions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRole {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `Some(None)` (`null`): no colour.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub color: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<crate::permissions::Permissions>,
    /// Rank among the roles, from 1 (just above `@everyone`); past the top: highest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// Body of `PUT /guilds/{id}/bans/{user_id}` (ban members).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateBan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChannel {
    pub name: String,
    #[serde(default)]
    pub kind: ChannelKind,
    /// Listed under this category of the same guild, after its other channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Snowflake>,
    /// Hidden from `@everyone`: only the owner and the creator's highest role see it at
    /// first (more roles are added with its overwrites).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

/// Body of `PATCH /channels/{id}` (manage channels). Absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateChannel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `Some(None)` (`null`): out of its category.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub category_id: Option<Option<Snowflake>>,
    /// Rank among the channels of the same kind in its (new) category, from 0; past the
    /// end: last. Without it, a channel changing category goes last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    /// Replaces all its overwrites (one per role at most).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrites: Option<Vec<crate::permissions::Overwrite>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCategory {
    pub name: String,
}

/// Body of `PATCH /categories/{id}` (manage channels). Absent fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCategory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Rank among the guild's categories, from 0; past the end: last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// `Option<Option<T>>` in a PATCH body: absent = unchanged, `null` = cleared. Use with
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`.
mod double_option {
    use super::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(value: &Option<Option<T>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

/// Answer of `POST /channels/{id}/voice`: where and how to join the voice channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceJoin {
    /// WebSocket URL of the media server (LiveKit), e.g. `wss://plume-lite.donr-corp.fr`.
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

/// Body of `PUT /users/@me/notifications/{guild or channel id}`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SetNotifications {
    pub level: crate::model::NotificationLevel,
}

/// Body of `POST /channels/{id}/ack`: the user read the channel up to this message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Ack {
    pub message_id: Snowflake,
}

/// Body of `PUT /guilds/{id}/voice/{user_id}/mute` (mute members).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ServerMute {
    pub muted: bool,
}

/// Body of `POST /guilds/{id}/voice/{user_id}/move` (move members): the voice channel of the
/// same guild to send the member to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MoveVoice {
    pub channel_id: Snowflake,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessage {
    pub content: String,
    /// Answers this message of the same channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Snowflake>,
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

/// Results per page of `GET /guilds/{id}/messages/search`.
pub const SEARCH_PAGE: u32 = 25;
/// Words of a search that count (the others are ignored).
pub const SEARCH_TERMS_MAX: usize = 10;

/// Query of `GET /guilds/{id}/messages/search`: messages containing every word of `q` (or a
/// word starting with it), insensitive to case and accents, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<Snowflake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_id: Option<Snowflake>,
    /// The next page: messages older than this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Snowflake>,
}

/// Answer of `GET /guilds/{id}/messages/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    /// Every match, not only this page.
    pub total: u64,
    pub messages: Vec<crate::model::Message>,
}

/// The words of a search, as the index stores them: letters and digits only. Empty when
/// there is nothing to look for.
pub fn search_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(SEARCH_TERMS_MAX)
        .map(str::to_lowercase)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_bodies_tell_absent_from_null() {
        let absent: UpdateChannel = serde_json::from_str(r#"{"name":"a"}"#).unwrap();
        assert_eq!(absent.category_id, None);
        let cleared: UpdateChannel = serde_json::from_str(r#"{"category_id":null}"#).unwrap();
        assert_eq!(cleared.category_id, Some(None));
        let set: UpdateChannel = serde_json::from_str(r#"{"category_id":"7","position":2}"#).unwrap();
        assert_eq!((set.category_id, set.position), (Some(Some(Snowflake(7))), Some(2)));

        let json = serde_json::to_string(&UpdateChannel { category_id: Some(None), ..UpdateChannel::default() });
        assert_eq!(json.unwrap(), r#"{"category_id":null}"#);
        assert_eq!(serde_json::to_string(&UpdateChannel::default()).unwrap(), "{}");
    }

    #[test]
    fn search_terms_are_words() {
        assert_eq!(search_terms("  Été, l'équipe!! <@12> "), ["été", "l", "équipe", "12"]);
        assert!(search_terms(" \"*:() ").is_empty(), "nothing to look for");
        assert_eq!(search_terms(&"mot ".repeat(50)).len(), SEARCH_TERMS_MAX);
    }
}
