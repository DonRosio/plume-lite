//! Typed REST client. One method per endpoint; all of them are cheap to call from
//! `Task::perform` because `Api` is a couple of `Arc`s.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use plume_lite_protocol::http::{
    ApiError as ErrorBody, AuthResponse, CreateChannel, CreateGuild, CreateInvite, CreateMessage, Credentials,
    EditMessage, ErrorCode,
};
use plume_lite_protocol::model::{Channel, FullGuild, Invite, Member, Message, User};
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
            .user_agent(concat!("plume-lite-desktop/", env!("CARGO_PKG_VERSION")))
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

    pub async fn register(&self, credentials: Credentials) -> Result<AuthResponse, ApiError> {
        self.call(Method::POST, "/auth/register", Some(&credentials)).await
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

    pub async fn create_channel(&self, guild_id: Snowflake, name: String) -> Result<Channel, ApiError> {
        self.call(Method::POST, &format!("/guilds/{guild_id}/channels"), Some(&CreateChannel { name })).await
    }

    pub async fn delete_channel(&self, channel_id: Snowflake) -> Result<(), ApiError> {
        self.call_empty(Method::DELETE, &format!("/channels/{channel_id}")).await
    }

    pub async fn messages(&self, channel_id: Snowflake, before: Option<Snowflake>) -> Result<Vec<Message>, ApiError> {
        let cursor = before.map(|id| format!("&before={id}")).unwrap_or_default();
        self.call(Method::GET, &format!("/channels/{channel_id}/messages?limit=50{cursor}"), None::<&()>).await
    }

    pub async fn send_message(
        &self,
        channel_id: Snowflake,
        content: String,
        nonce: String,
    ) -> Result<Message, ApiError> {
        let body = CreateMessage { content, nonce: Some(nonce) };
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
    fn invite_codes() {
        assert_eq!(parse_invite_code("AbCd1234").as_deref(), Some("AbCd1234"));
        assert_eq!(parse_invite_code(" https://plume.donrgames.fr/invite/AbCd1234/ ").as_deref(), Some("AbCd1234"));
        assert_eq!(parse_invite_code("   "), None);
        assert_eq!(parse_invite_code("bad code!"), None);
    }
}
