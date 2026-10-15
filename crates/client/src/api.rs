//! Typed REST client. One method per endpoint; all of them are cheap to call from
//! `Task::perform` because `Api` is a couple of `Arc`s.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use plume_lite_protocol::http::{
    Ack, ApiError as ErrorBody, AuditLog, AuthResponse, ChangePassword, CreateBan, CreateCategory, CreateChannel,
    CreateGuild, CreateInvite, CreateMessage, CreateRole, Credentials, EditMessage, ErrorCode, InstanceInfo, MoveVoice,
    Registration, SearchQuery, SearchResults, ServerMute, SetNotifications, TransferOwnership, UpdateCategory,
    UpdateChannel, UpdateGuild, UpdateRole, UpdateVoiceState, VoiceJoin,
};
use plume_lite_protocol::model::{
    Ban, Category, Channel, ChannelKind, FullGuild, Guild, Invite, Member, Message, NotificationLevel, Role,
    SessionInfo, User,
};
use plume_lite_protocol::{API_PREFIX, GATEWAY_PATH, Snowflake};
use reqwest::{Method, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: Option<u16>,
    pub code: Option<ErrorCode>,
    pub message: String,
}

impl ApiError {
    pub fn is_unauthorized(&self) -> bool {
        self.status == Some(401)
    }

    pub fn is_rate_limited(&self) -> bool {
        self.code == Some(ErrorCode::RateLimited)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        let message = if err.is_timeout() {
            "Le serveur ne répond pas (délai dépassé).".to_owned()
        } else if err.is_connect() {
            "Impossible de joindre le serveur.".to_owned()
        } else if err.is_decode() {
            "Réponse du serveur illisible (version incompatible ?).".to_owned()
        } else {
            format!("Erreur réseau : {err}")
        };
        Self { status: err.status().map(|s| s.as_u16()), code: None, message }
    }
}

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    origin: Arc<str>,
    token: Option<Arc<str>>,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api").field("origin", &self.origin).finish_non_exhaustive()
    }
}

impl Api {
    /// `server` may be `host:port`, `http://host:port` or `https://host`.
    pub fn new(server: &str) -> Self {
        let http = reqwest::Client::builder()
            // Shown in the user's list of devices: "Plume Lite 0.3.0 · Windows".
            .user_agent(format!("plume-lite-desktop/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { http, origin: normalize_origin(server).into(), token: None }
    }

    pub fn authenticated(&self, token: &str) -> Self {
        Self { token: Some(token.into()), ..self.clone() }
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// A file served outside the API (`/download/…`), with time to fetch a few dozen MB.
    pub async fn file(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        let response = self.http.get(format!("{}{path}", self.origin)).timeout(Duration::from_secs(600)).send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(ApiError {
                status: Some(status),
                code: None,
                message: format!("Téléchargement refusé ({status})."),
            });
        }
        Ok(response.bytes().await?.to_vec())
    }

    /// `ws(s)://…/gateway` for this server.
    pub fn gateway_url(&self) -> String {
        let origin = if let Some(rest) = self.origin.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.origin.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.origin.to_string()
        };
        format!("{origin}{GATEWAY_PATH}")
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let request = self.http.request(method, format!("{}{API_PREFIX}{path}", self.origin));
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T, ApiError> {
        let response = self.send(method, path, body).await?;
        Ok(response.json().await?)
    }

    async fn call_empty(&self, method: Method, path: &str) -> Result<(), ApiError> {
        self.send(method, path, None::<&()>).await.map(drop)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<reqwest::Response, ApiError> {
        let mut request = self.request(method, path);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        // Every API error uses the same envelope; anything else (proxy page…) gets a generic message.
        let body = response.json::<ErrorBody>().await.ok();
        Err(ApiError {
            status: Some(status.as_u16()),
            code: body.as_ref().map(|b| b.code),
            message: body.map_or_else(|| format!("Erreur HTTP {status}"), |b| b.message),
        })
    }

    pub async fn register(&self, registration: Registration) -> Result<AuthResponse, ApiError> {
        self.call(Method::POST, "/auth/register", Some(&registration)).await
    }

    pub async fn login(&self, credentials: Credentials) -> Result<AuthResponse, ApiError> {
        self.call(Method::POST, "/auth/login", Some(&credentials)).await
    }

    pub async fn logout(&self) -> Result<(), ApiError> {
        self.call_empty(Method::POST, "/auth/logout").await
    }

    pub async fn me(&self) -> Result<User, ApiError> {
        self.call(Method::GET, "/users/@me", None::<&()>).await
    }

    pub async fn instance(&self) -> Result<InstanceInfo, ApiError> {
        self.call(Method::GET, "/instance", None::<&()>).await
    }

    pub async fn change_password(&self, current_password: String, new_password: String) -> Result<(), ApiError> {
        let body = ChangePassword { current_password, new_password };
        self.send(Method::PUT, "/users/@me/password", Some(&body)).await.map(drop)
    }

    pub async fn sessions(&self) -> Result<Vec<SessionInfo>, ApiError> {
        self.call(Method::GET, "/users/@me/sessions", None::<&()>).await
    }

    pub async fn revoke_session(&self, session_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/users/@me/sessions/{session_id}")).await
    }

    pub async fn create_guild(&self, name: String) -> Result<FullGuild, ApiError> {
        self.call(Method::POST, "/guilds", Some(&CreateGuild { name })).await
    }

    pub async fn delete_guild(&self, guild_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/guilds/{guild_id}")).await
    }

    pub async fn leave_guild(&self, guild_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/guilds/{guild_id}/members/@me")).await
    }

    pub async fn members(&self, guild_id: Snowflake) -> Result<Vec<Member>, ApiError> {
        self.call(Method::GET, &format!("/guilds/{guild_id}/members?limit=1000"), None::<&()>).await
    }

    pub async fn update_guild(&self, guild_id: Snowflake, changes: UpdateGuild) -> Result<Guild, ApiError> {
        self.call(Method::PATCH, &format!("/guilds/{guild_id}"), Some(&changes)).await
    }

    pub async fn audit_log(&self, guild_id: Snowflake, before: Option<Snowflake>) -> Result<AuditLog, ApiError> {
        let cursor = before.map(|id| format!("?before={id}")).unwrap_or_default();
        self.call(Method::GET, &format!("/guilds/{guild_id}/audit-log{cursor}"), None::<&()>).await
    }

    pub async fn transfer_guild(
        &self,
        guild_id: Snowflake,
        user_id: Snowflake,
        password: String,
    ) -> Result<Guild, ApiError> {
        let body = TransferOwnership { user_id, password };
        self.call(Method::PUT, &format!("/guilds/{guild_id}/owner"), Some(&body)).await
    }

    pub async fn create_role(&self, guild_id: Snowflake, role: CreateRole) -> Result<Role, ApiError> {
        self.call(Method::POST, &format!("/guilds/{guild_id}/roles"), Some(&role)).await
    }

    pub async fn update_role(
        &self,
        guild_id: Snowflake,
        role_id: Snowflake,
        changes: UpdateRole,
    ) -> Result<Role, ApiError> {
        self.call(Method::PATCH, &format!("/guilds/{guild_id}/roles/{role_id}"), Some(&changes)).await
    }

    pub async fn delete_role(&self, guild_id: Snowflake, role_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/guilds/{guild_id}/roles/{role_id}")).await
    }

    /// Gives (`on`) or takes a role.
    pub async fn set_member_role(
        &self,
        guild_id: Snowflake,
        user_id: Snowflake,
        role_id: Snowflake,
        on: bool,
    ) -> Result<(), ApiError> {
        let path = format!("/guilds/{guild_id}/members/{user_id}/roles/{role_id}");
        self.call_empty(if on { Method::PUT } else { Method::DELETE }, &path).await
    }

    pub async fn kick(&self, guild_id: Snowflake, user_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/guilds/{guild_id}/members/{user_id}")).await
    }

    pub async fn bans(&self, guild_id: Snowflake) -> Result<Vec<Ban>, ApiError> {
        self.call(Method::GET, &format!("/guilds/{guild_id}/bans"), None::<&()>).await
    }

    pub async fn ban(&self, guild_id: Snowflake, user_id: Snowflake, reason: Option<String>) -> Result<(), ApiError> {
        let path = format!("/guilds/{guild_id}/bans/{user_id}");
        self.send(Method::PUT, &path, Some(&CreateBan { reason })).await.map(drop)
    }

    pub async fn unban(&self, guild_id: Snowflake, user_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/guilds/{guild_id}/bans/{user_id}")).await
    }

    pub async fn create_channel(
        &self,
        guild_id: Snowflake,
        name: String,
        kind: ChannelKind,
        category_id: Option<Snowflake>,
        private: bool,
    ) -> Result<Channel, ApiError> {
        let body = CreateChannel { name, kind, category_id, private };
        self.call(Method::POST, &format!("/guilds/{guild_id}/channels"), Some(&body)).await
    }

    pub async fn update_channel(&self, channel_id: Snowflake, changes: UpdateChannel) -> Result<Channel, ApiError> {
        self.call(Method::PATCH, &format!("/channels/{channel_id}"), Some(&changes)).await
    }

    pub async fn create_category(&self, guild_id: Snowflake, name: String) -> Result<Category, ApiError> {
        self.call(Method::POST, &format!("/guilds/{guild_id}/categories"), Some(&CreateCategory { name })).await
    }

    pub async fn update_category(&self, category_id: Snowflake, changes: UpdateCategory) -> Result<Category, ApiError> {
        self.call(Method::PATCH, &format!("/categories/{category_id}"), Some(&changes)).await
    }

    pub async fn delete_category(&self, category_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/categories/{category_id}")).await
    }

    /// A token to join a voice channel's room on the media server.
    pub async fn join_voice(&self, channel_id: Snowflake) -> Result<VoiceJoin, ApiError> {
        self.call(Method::POST, &format!("/channels/{channel_id}/voice"), None::<&()>).await
    }

    pub async fn update_voice_state(&self, self_mute: bool, self_deaf: bool) -> Result<(), ApiError> {
        self.send(Method::PATCH, "/users/@me/voice", Some(&UpdateVoiceState { self_mute, self_deaf })).await.map(drop)
    }

    pub async fn server_mute(&self, guild_id: Snowflake, user_id: Snowflake, muted: bool) -> Result<(), ApiError> {
        let path = format!("/guilds/{guild_id}/voice/{user_id}/mute");
        self.send(Method::PUT, &path, Some(&ServerMute { muted })).await.map(drop)
    }

    pub async fn disconnect_from_voice(&self, guild_id: Snowflake, user_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::POST, &format!("/guilds/{guild_id}/voice/{user_id}/disconnect")).await
    }

    pub async fn move_to_voice(
        &self,
        guild_id: Snowflake,
        user_id: Snowflake,
        channel_id: Snowflake,
    ) -> Result<(), ApiError> {
        let path = format!("/guilds/{guild_id}/voice/{user_id}/move");
        self.send(Method::POST, &path, Some(&MoveVoice { channel_id })).await.map(drop)
    }

    pub async fn delete_channel(&self, channel_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/channels/{channel_id}")).await
    }

    pub async fn search(&self, guild_id: Snowflake, query: &SearchQuery) -> Result<SearchResults, ApiError> {
        // Every byte of the words percent-encoded: they may hold `&`, `#` or anything.
        let encoded: String = query.q.bytes().map(|byte| format!("%{byte:02X}")).collect();
        let mut path = format!("/guilds/{guild_id}/messages/search?q={encoded}");
        for (name, value) in
            [("channel_id", query.channel_id), ("author_id", query.author_id), ("before", query.before)]
        {
            if let Some(value) = value {
                path.push_str(&format!("&{name}={value}"));
            }
        }
        self.call(Method::GET, &path, None::<&()>).await
    }

    pub async fn messages(&self, channel_id: Snowflake, before: Option<Snowflake>) -> Result<Vec<Message>, ApiError> {
        let cursor = before.map(|id| format!("&before={id}")).unwrap_or_default();
        self.call(Method::GET, &format!("/channels/{channel_id}/messages?limit=50{cursor}"), None::<&()>).await
    }

    pub async fn send_message(
        &self,
        channel_id: Snowflake,
        content: String,
        reply_to: Option<Snowflake>,
        nonce: String,
    ) -> Result<Message, ApiError> {
        let body = CreateMessage { content, reply_to, nonce: Some(nonce) };
        self.call(Method::POST, &format!("/channels/{channel_id}/messages"), Some(&body)).await
    }

    pub async fn edit_message(
        &self,
        channel_id: Snowflake,
        message_id: Snowflake,
        content: String,
    ) -> Result<Message, ApiError> {
        let path = format!("/channels/{channel_id}/messages/{message_id}");
        self.call(Method::PATCH, &path, Some(&EditMessage { content })).await
    }

    pub async fn delete_message(&self, channel_id: Snowflake, message_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/channels/{channel_id}/messages/{message_id}")).await
    }

    /// Adds (or removes) our reaction. Both are idempotent on the server.
    pub async fn react(
        &self,
        channel_id: Snowflake,
        message_id: Snowflake,
        emoji: &str,
        add: bool,
    ) -> Result<(), ApiError> {
        // Every byte percent-encoded: `#` (keycaps) or `*` must not be read as URL syntax.
        let emoji: String = emoji.bytes().map(|byte| format!("%{byte:02X}")).collect();
        let path = format!("/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me");
        self.call_empty(if add { Method::PUT } else { Method::DELETE }, &path).await
    }

    pub async fn pins(&self, channel_id: Snowflake) -> Result<Vec<Message>, ApiError> {
        self.call(Method::GET, &format!("/channels/{channel_id}/pins"), None::<&()>).await
    }

    pub async fn pin(&self, channel_id: Snowflake, message_id: Snowflake, pinned: bool) -> Result<(), ApiError> {
        let method = if pinned { Method::PUT } else { Method::DELETE };
        self.call_empty(method, &format!("/channels/{channel_id}/pins/{message_id}")).await
    }

    /// Which messages of a guild or channel notify us; `None` goes back to the default.
    pub async fn set_notifications(
        &self,
        target_id: Snowflake,
        level: Option<NotificationLevel>,
    ) -> Result<(), ApiError> {
        let path = format!("/users/@me/notifications/{target_id}");
        match level {
            Some(level) => self.send(Method::PUT, &path, Some(&SetNotifications { level })).await.map(drop),
            None => self.call_empty(Method::DELETE, &path).await,
        }
    }

    /// We read the channel up to this message.
    pub async fn ack(&self, channel_id: Snowflake, message_id: Snowflake) -> Result<(), ApiError> {
        self.send(Method::POST, &format!("/channels/{channel_id}/ack"), Some(&Ack { message_id })).await.map(drop)
    }

    pub async fn typing(&self, channel_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::POST, &format!("/channels/{channel_id}/typing")).await
    }

    pub async fn create_invite(&self, guild_id: Snowflake) -> Result<Invite, ApiError> {
        let body = CreateInvite { max_uses: None, max_age_secs: Some(7 * 24 * 3600) };
        self.call(Method::POST, &format!("/guilds/{guild_id}/invites"), Some(&body)).await
    }

    pub async fn accept_invite(&self, code: String) -> Result<FullGuild, ApiError> {
        self.call(Method::POST, &format!("/invites/{code}"), None::<&()>).await
    }
}

fn normalize_origin(server: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    if server.starts_with("http://") || server.starts_with("https://") {
        server.to_owned()
    } else {
        format!("http://{server}")
    }
}

/// `true` when `remote` ("0.3.1") is a newer version than `local` ("0.3.0").
pub fn is_newer(remote: &str, local: &str) -> bool {
    let parse = |version: &str| -> Vec<u64> { version.split('.').map(|part| part.parse().unwrap_or(0)).collect() };
    parse(remote) > parse(local)
}

/// Opens a link in the default browser.
pub fn open_url(url: &str) -> std::io::Result<()> {
    use std::process::Command;
    #[cfg(windows)]
    let mut command = Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    command.arg(url).spawn().map(drop)
}

/// Accepts a bare code or any link ending with it (`https://…/invite/AbCd1234`).
pub fn parse_invite_code(input: &str) -> Option<String> {
    let code = input.trim().trim_end_matches('/').rsplit('/').next()?.trim();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric())).then(|| code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_gateway_urls() {
        assert_eq!(Api::new("localhost:8080").origin(), "http://localhost:8080");
        assert_eq!(Api::new(" https://chat.example.com/ ").origin(), "https://chat.example.com");
        assert_eq!(Api::new("http://127.0.0.1:8080").gateway_url(), "ws://127.0.0.1:8080/gateway");
        assert_eq!(Api::new("https://chat.example.com").gateway_url(), "wss://chat.example.com/gateway");
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
    }

    #[test]
    fn invite_codes() {
        assert_eq!(parse_invite_code("AbCd1234").as_deref(), Some("AbCd1234"));
        assert_eq!(parse_invite_code(" https://plume.donrgames.fr/invite/AbCd1234/ ").as_deref(), Some("AbCd1234"));
        assert_eq!(parse_invite_code("   "), None);
        assert_eq!(parse_invite_code("bad code!"), None);
    }
}
