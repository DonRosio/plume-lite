//! Voice channels. The audio itself flows through the LiveKit media server; this module
//! decides who may join which room (one LiveKit room per voice channel, named after its id),
//! keeps track of who is in which voice channel (LiveKit tells us through webhooks) and asks
//! LiveKit to mute or remove participants.
//!
//! Voice states live in memory: after a restart they are rebuilt from LiveKit
//! ([`Voice::resync`]). Server mutes are kept until the server restarts.

pub mod token;

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, bail};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::http::VoiceJoin;
use plume_lite_protocol::model::{FullGuild, User, VoiceState};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::LiveKitConfig;
use token::{Claims, VideoGrant};

/// Join tokens only need to be valid while connecting: LiveKit refreshes them afterwards.
const JOIN_TOKEN_TTL_SECS: i64 = 10 * 60;
const ADMIN_TOKEN_TTL_SECS: i64 = 60;

pub struct Voice {
    config: Option<LiveKitConfig>,
    http: reqwest::Client,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Who is in voice, by user (one voice channel at a time).
    states: HashMap<Snowflake, VoiceState>,
    /// Microphone and sound switches each user asked for, kept while they hop channels.
    flags: HashMap<Snowflake, (bool, bool)>,
    /// (guild, user) muted by a moderator.
    server_mutes: HashSet<(Snowflake, Snowflake)>,
}

/// The parts of a LiveKit webhook we use.
#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    pub event: String,
    #[serde(default)]
    pub room: Option<WebhookRoom>,
    #[serde(default)]
    pub participant: Option<WebhookParticipant>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookRoom {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct WebhookParticipant {
    pub identity: String,
}

impl Voice {
    pub fn new(config: Option<LiveKitConfig>) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_default();
        Self { config, http, inner: Mutex::default() }
    }

    pub fn enabled(&self) -> bool {
        self.config.is_some()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave the maps half-updated in a harmful way.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // --- Who is where ---------------------------------------------------------------------------

    pub fn of_guild(&self, guild_id: Snowflake) -> Vec<VoiceState> {
        let mut states: Vec<_> = self.lock().states.values().filter(|s| s.guild_id == guild_id).copied().collect();
        states.sort_by_key(|s| s.user_id);
        states
    }

    /// Adds the current voice states to guilds about to be sent to a client: only in the
    /// channels they carry (those the client may see).
    pub fn fill(&self, guilds: &mut [FullGuild]) {
        let inner = self.lock();
        for guild in guilds {
            let shown = |s: &&VoiceState| s.channel_id.is_some_and(|c| guild.channels.iter().any(|ch| ch.id == c));
            guild.voice_states = inner.states.values().filter(shown).copied().collect();
            guild.voice_states.sort_by_key(|s| s.user_id);
        }
    }

    pub fn of_user(&self, user_id: Snowflake) -> Option<VoiceState> {
        self.lock().states.get(&user_id).copied()
    }

    pub fn is_server_muted(&self, guild_id: Snowflake, user_id: Snowflake) -> bool {
        self.lock().server_mutes.contains(&(guild_id, user_id))
    }

    /// The user is now in `channel_id` (from LiveKit's `participant_joined`).
    pub fn joined(&self, guild_id: Snowflake, channel_id: Snowflake, user_id: Snowflake) -> VoiceState {
        let mut inner = self.lock();
        let (self_mute, self_deaf) = inner.flags.get(&user_id).copied().unwrap_or_default();
        let server_mute = inner.server_mutes.contains(&(guild_id, user_id));
        let state = VoiceState { guild_id, user_id, channel_id: Some(channel_id), self_mute, self_deaf, server_mute };
        inner.states.insert(user_id, state);
        state
    }

    /// The user left voice. With `Some(channel)`, only if they were still in that channel: a
    /// late "left the old channel" must not undo "joined the new one". Returns the update to
    /// broadcast (`channel_id: None`).
    pub fn left(&self, user_id: Snowflake, channel_id: Option<Snowflake>) -> Option<VoiceState> {
        let mut inner = self.lock();
        let current = *inner.states.get(&user_id)?;
        if channel_id.is_some_and(|channel| current.channel_id != Some(channel)) {
            return None;
        }
        inner.states.remove(&user_id);
        Some(VoiceState { channel_id: None, ..current })
    }

    /// Records the user's own microphone and sound switches; returns the new state if in voice.
    pub fn set_flags(&self, user_id: Snowflake, self_mute: bool, self_deaf: bool) -> Option<VoiceState> {
        let mut inner = self.lock();
        inner.flags.insert(user_id, (self_mute, self_deaf));
        let state = inner.states.get_mut(&user_id)?;
        state.self_mute = self_mute;
        state.self_deaf = self_deaf;
        Some(*state)
    }

    /// Returns the new state if the user is in voice in that guild.
    pub fn set_server_mute(&self, guild_id: Snowflake, user_id: Snowflake, muted: bool) -> Option<VoiceState> {
        let mut inner = self.lock();
        if muted {
            inner.server_mutes.insert((guild_id, user_id));
        } else {
            inner.server_mutes.remove(&(guild_id, user_id));
        }
        let state = inner.states.get_mut(&user_id).filter(|s| s.guild_id == guild_id)?;
        state.server_mute = muted;
        Some(*state)
    }

    /// Everyone leaves a deleted voice channel. Returns the updates to broadcast.
    pub fn clear_channel(&self, channel_id: Snowflake) -> Vec<VoiceState> {
        self.clear(|s| s.channel_id == Some(channel_id))
    }

    pub fn clear_guild(&self, guild_id: Snowflake) -> Vec<VoiceState> {
        let left = self.clear(|s| s.guild_id == guild_id);
        self.lock().server_mutes.retain(|(guild, _)| *guild != guild_id);
        left
    }

    fn clear(&self, matches: impl Fn(&VoiceState) -> bool) -> Vec<VoiceState> {
        let mut inner = self.lock();
        let users: Vec<Snowflake> = inner.states.values().filter(|s| matches(s)).map(|s| s.user_id).collect();
        users
            .into_iter()
            .filter_map(|user| inner.states.remove(&user))
            .map(|state| VoiceState { channel_id: None, ..state })
            .collect()
    }

    // --- LiveKit --------------------------------------------------------------------------------

    /// A token to join the room of `channel_id`, `None` when voice is not configured.
    pub fn join(&self, user: &User, channel_id: Snowflake, can_publish: bool) -> Option<VoiceJoin> {
        let config = self.config.as_ref()?;
        let mut claims = Claims::new(&config.api_key, JOIN_TOKEN_TTL_SECS);
        claims.sub = user.id.to_string();
        claims.name = user.username.clone();
        claims.video = Some(VideoGrant {
            room: channel_id.to_string(),
            room_join: true,
            can_publish: Some(can_publish),
            can_subscribe: Some(true),
            can_publish_data: Some(false),
            ..VideoGrant::default()
        });
        Some(VoiceJoin { url: config.url.clone(), token: token::sign(&claims, &config.api_secret) })
    }

    /// Checks a webhook from LiveKit: its `Authorization` header must be a token signed with
    /// our secret, whose `sha256` claim matches the body.
    pub fn verify_webhook(&self, authorization: &str, body: &[u8]) -> Option<WebhookEvent> {
        let config = self.config.as_ref()?;
        let token = authorization.strip_prefix("Bearer ").unwrap_or(authorization).trim();
        let claims = token::verify(token, &config.api_secret)?;
        if claims.iss != config.api_key || claims.sha256.as_deref() != Some(&token::body_hash(body)) {
            return None;
        }
        serde_json::from_slice(body).ok()
    }

    /// Removes someone from a room (they are disconnected from voice at once).
    pub async fn remove_participant(&self, room: Snowflake, user_id: Snowflake) -> anyhow::Result<()> {
        let body = json!({ "room": room.to_string(), "identity": user_id.to_string() });
        self.room_service("RemoveParticipant", &body, admin_of(room)).await.map(drop)
    }

    /// Lets someone speak again, or stops them (LiveKit unpublishes their microphone).
    pub async fn set_can_publish(&self, room: Snowflake, user_id: Snowflake, can_publish: bool) -> anyhow::Result<()> {
        let body = json!({
            "room": room.to_string(),
            "identity": user_id.to_string(),
            "permission": { "canSubscribe": true, "canPublish": can_publish, "canPublishData": false },
        });
        self.room_service("UpdateParticipant", &body, admin_of(room)).await.map(drop)
    }

    /// Ends a room: everyone in it is disconnected.
    pub async fn delete_room(&self, room: Snowflake) -> anyhow::Result<()> {
        self.room_service("DeleteRoom", &json!({ "room": room.to_string() }), list_grant()).await.map(drop)
    }

    /// Who is in which room, as LiveKit sees it: `(room, identities)`.
    pub async fn rooms(&self) -> anyhow::Result<Vec<(String, Vec<String>)>> {
        let rooms = self.room_service("ListRooms", &json!({}), list_grant()).await?;
        let mut found = Vec::new();
        for room in rooms["rooms"].as_array().into_iter().flatten() {
            let Some(name) = room["name"].as_str() else { continue };
            let Ok(id) = name.parse::<Snowflake>() else { continue };
            let participants = self.room_service("ListParticipants", &json!({ "room": name }), admin_of(id)).await?;
            let identities = participants["participants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["identity"].as_str().map(str::to_owned))
                .collect();
            found.push((name.to_owned(), identities));
        }
        Ok(found)
    }

    /// LiveKit's RoomService API (Twirp: JSON over HTTP POST), on the internal network.
    async fn room_service(&self, method: &str, body: &Value, grant: VideoGrant) -> anyhow::Result<Value> {
        let config = self.config.as_ref().context("voice is not configured")?;
        let mut claims = Claims::new(&config.api_key, ADMIN_TOKEN_TTL_SECS);
        claims.video = Some(grant);
        let url = format!("{}/twirp/livekit.RoomService/{method}", config.api_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(token::sign(&claims, &config.api_secret))
            .json(body)
            .send()
            .await
            .with_context(|| format!("LiveKit {method}"))?;
        let status = response.status();
        let answer: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(answer);
        }
        // Already gone: that is what we wanted.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(Value::Null);
        }
        bail!("LiveKit {method}: {status} {answer}")
    }
}

fn admin_of(room: Snowflake) -> VideoGrant {
    VideoGrant { room: room.to_string(), room_admin: true, ..VideoGrant::default() }
}

fn list_grant() -> VideoGrant {
    VideoGrant { room_list: true, room_admin: true, ..VideoGrant::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_follow_joins_leaves_and_switches() {
        let voice = Voice::new(None);
        let (guild, a, b, alice) = (Snowflake(1), Snowflake(10), Snowflake(11), Snowflake(100));

        voice.set_flags(alice, true, false);
        let state = voice.joined(guild, a, alice);
        assert_eq!((state.channel_id, state.self_mute), (Some(a), true), "switches carry over");

        // Moving to another channel: the late "left a" must not remove her from b.
        voice.joined(guild, b, alice);
        assert!(voice.left(alice, Some(a)).is_none());
        assert_eq!(voice.of_user(alice).unwrap().channel_id, Some(b));

        assert!(voice.set_server_mute(guild, alice, true).unwrap().server_mute);
        let left = voice.left(alice, Some(b)).unwrap();
        assert_eq!(left.channel_id, None);
        assert!(voice.of_guild(guild).is_empty());
        assert!(voice.joined(guild, a, alice).server_mute, "server mutes stick");

        assert_eq!(voice.clear_channel(a).len(), 1);
        assert!(voice.of_user(alice).is_none());
    }
}
