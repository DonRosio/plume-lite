//! Resources exposed by the API and pushed by the gateway.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Snowflake;
use crate::permissions::Permissions;

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
    /// Where newcomers are announced (`MemberJoin` messages); `None`: nowhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_channel_id: Option<Snowflake>,
    /// Shown in those announcements, under "X a rejoint le serveur".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_message: Option<String>,
}

/// A guild with everything a client needs to render it (sent in `READY` and `GUILD_CREATE`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullGuild {
    #[serde(flatten)]
    pub guild: Guild,
    pub channels: Vec<Channel>,
    /// The headings channels are grouped under (see [`crate::layout`]).
    #[serde(default)]
    pub categories: Vec<Category>,
    /// Every role of the guild, `@everyone` (whose id is the guild's) included.
    #[serde(default)]
    pub roles: Vec<Role>,
    /// The roles of the user this guild is sent to (`@everyone` aside).
    #[serde(default)]
    pub my_roles: Vec<Snowflake>,
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
    /// Muted by a moderator: cannot speak until unmuted.
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
    /// Rank in the guild's display order (see [`crate::layout`]).
    pub position: i32,
    /// The category it is listed under; `None`: above every category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Snowflake>,
    /// What it allows or denies per role, over the guild permissions (see
    /// [`crate::permissions::channel_permissions`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "sqlx", sqlx(skip))]
    pub overwrites: Vec<crate::permissions::Overwrite>,
    /// The newest message, to tell unread channels apart (`READY` and `GUILD_CREATE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "sqlx", sqlx(default))]
    pub last_message_id: Option<Snowflake>,
}

/// A heading that groups channels of a guild. Not a channel: nothing can be posted in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Category {
    pub id: Snowflake,
    pub guild_id: Snowflake,
    pub name: String,
    pub position: i32,
}

/// A set of permissions given to the members who have it (see [`crate::permissions`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub id: Snowflake,
    pub guild_id: Snowflake,
    pub name: String,
    /// `0xRRGGBB`; members show in the colour of their highest coloured role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Rank: `@everyone` is 0, the higher the stronger.
    pub position: i32,
    pub permissions: Permissions,
}

impl Role {
    /// The role every member has, whose id is the guild's.
    pub fn is_everyone(&self) -> bool {
        self.id == self.guild_id
    }
}

/// Someone banned from a guild: removed from the guild and unable to come back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ban {
    pub user: User,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// How far the current user has read a text channel. Missing: never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    pub channel_id: Snowflake,
    pub last_read_id: Snowflake,
    /// Unread messages that mention the user (by name, `@everyone` from someone allowed to, or a reply
    /// to one of theirs), up to [`MENTIONS_COUNTED_MAX`].
    #[serde(default)]
    pub mentions: u32,
}

/// Which messages notify the user. Set per guild or per channel (a channel overrides its
/// guild); without a setting, every message does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    #[default]
    All = 0,
    /// Only messages that mention the user (see [`ReadState::mentions`]).
    Mentions = 1,
    Nothing = 2,
}

impl NotificationLevel {
    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::Mentions,
            2 => Self::Nothing,
            _ => Self::All,
        }
    }
}

/// The user's setting for a guild or a channel (`target_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSetting {
    pub target_id: Snowflake,
    pub level: NotificationLevel,
}

/// Unread mentions are counted up to this ("99+").
pub const MENTIONS_COUNTED_MAX: u32 = 99;

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
    /// Their roles (`@everyone` aside).
    #[serde(default)]
    pub roles: Vec<Snowflake>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub guild_id: Snowflake,
    pub author: User,
    pub content: String,
    /// A message written by someone, or one the server posts in their name.
    #[serde(default, skip_serializing_if = "MessageKind::is_default")]
    pub kind: MessageKind,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
    /// The message this one answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Replied>,
    /// The author, allowed to mention everyone, wrote `@everyone`: every member is notified.
    /// From anyone else, `@everyone` is just text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mention_everyone: bool,
    /// Reactions, in the order they first appeared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    /// Pinned to its channel (manage messages).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Client-chosen token echoed back on creation so the sender can reconcile
    /// its optimistic copy. Never persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Default,
    /// Its author joined the guild; the content is the guild's welcome message (or
    /// [`JOIN_MESSAGE`]). Nobody can edit it.
    MemberJoin,
    /// A kind introduced by a newer server.
    #[serde(other)]
    Unknown,
}

impl MessageKind {
    pub fn is_default(&self) -> bool {
        *self == Self::Default
    }

    pub fn from_i64(value: i64) -> Self {
        match value {
            0 => Self::Default,
            1 => Self::MemberJoin,
            _ => Self::Unknown,
        }
    }
}

/// Content of a [`MessageKind::MemberJoin`] message when the guild has no welcome message.
pub const JOIN_MESSAGE: &str = "a rejoint le serveur.";

/// What a moderator did (an entry of the audit log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    GuildUpdate,
    OwnershipTransfer,
    ChannelCreate,
    ChannelUpdate,
    ChannelDelete,
    CategoryCreate,
    CategoryUpdate,
    CategoryDelete,
    RoleCreate,
    RoleUpdate,
    RoleDelete,
    MemberRoleAdd,
    MemberRoleRemove,
    MemberKick,
    MemberBan,
    MemberUnban,
    MessageDelete,
    MessagePin,
    MessageUnpin,
    VoiceMute,
    VoiceUnmute,
    VoiceDisconnect,
    VoiceMove,
    InviteCreate,
    /// An action introduced by a newer server.
    #[serde(other)]
    Unknown,
}

/// One entry of a guild's audit log. Its date is its id's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Snowflake,
    pub action: AuditAction,
    /// Who did it (`None` once their account is gone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<User>,
    /// The member it was done to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_user: Option<User>,
    /// The channel, category, role or message it was done to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<Snowflake>,
    /// Its name at the time (a channel, category or role, possibly deleted since).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<AuditChange>,
}

/// What changed: a field, its value before and after (`None`: unset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditChange {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Audit log entries are kept this long.
pub const AUDIT_RETENTION_DAYS: i64 = 90;

/// One emoji under a message, and who chose it (a set: adding twice changes nothing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<Snowflake>,
}

/// What a reply shows of the message it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replied {
    pub id: Snowflake,
    /// `None` once that message has been deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<User>,
    /// Its first [`REPLY_EXCERPT_MAX`] characters (empty once deleted).
    #[serde(default)]
    pub excerpt: String,
}

/// Length of [`Replied::excerpt`], in characters.
pub const REPLY_EXCERPT_MAX: usize = 200;

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
            guild: Guild {
                id: Snowflake(1),
                name: "Rust FR".into(),
                owner_id: Snowflake(2),
                welcome_channel_id: None,
                welcome_message: None,
            },
            channels: vec![],
            categories: vec![],
            roles: vec![],
            my_roles: vec![],
            member_count: 1,
            voice_states: vec![],
        };
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["name"], "Rust FR");
        assert_eq!(json["owner_id"], "2");
        assert_eq!(serde_json::from_value::<FullGuild>(json).unwrap(), g);
    }
}
