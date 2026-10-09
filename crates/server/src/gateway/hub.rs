//! In-process fan-out of gateway events and presence tracking.
//!
//! The hub only knows about *online* users: for each one, its live sessions and
//! the guilds it belongs to; for each guild, its online members. Publishing to a
//! guild therefore costs O(online members), never O(members).
//!
//! Events are serialised **once** and the same refcounted buffer is pushed to
//! every recipient's bounded queue. A recipient whose queue is full is
//! disconnected (it will reconnect and resync from READY) rather than letting
//! one slow client make the server buffer without limit.
//!
//! Scaling out: keep this type as the per-node delivery layer and put a broker
//! (Redis pub/sub or NATS, subject `guild.<id>` / `user.<id>`) in front of the
//! `dispatch_*` methods; presence then moves to Redis with TTL'd keys.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use axum::extract::ws::Utf8Bytes;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::{Event, ServerFrame};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_util::sync::CancellationToken;

pub type SessionId = u64;

/// Per-session outbound queue. Deep enough for bursts, small enough to bound memory.
pub const SESSION_QUEUE_CAPACITY: usize = 512;

struct Session {
    tx: mpsc::Sender<Utf8Bytes>,
    kill: CancellationToken,
}

#[derive(Default)]
struct OnlineUser {
    sessions: HashMap<SessionId, Session>,
    guilds: HashSet<Snowflake>,
}

#[derive(Default)]
struct Registry {
    users: HashMap<Snowflake, OnlineUser>,
    /// guild id → online member ids
    guilds: HashMap<Snowflake, HashSet<Snowflake>>,
}

#[derive(Default)]
pub struct Hub {
    registry: RwLock<Registry>,
    next_session: AtomicU64,
}

pub struct Registration {
    pub session_id: SessionId,
    pub events: mpsc::Receiver<Utf8Bytes>,
    /// `true` if this is the user's first live session (presence just went online).
    pub came_online: bool,
}

impl Hub {
    /// Registers a gateway session. Events published from now on are queued for it.
    pub fn connect(
        &self,
        user_id: Snowflake,
        guild_ids: impl IntoIterator<Item = Snowflake>,
        kill: CancellationToken,
    ) -> Registration {
        let (tx, events) = mpsc::channel(SESSION_QUEUE_CAPACITY);
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);

        let mut guard = self.write();
        let registry = &mut *guard;
        let came_online = !registry.users.contains_key(&user_id);
        let user = registry.users.entry(user_id).or_default();
        user.sessions.insert(session_id, Session { tx, kill });
        for guild_id in guild_ids {
            if user.guilds.insert(guild_id) {
                registry.guilds.entry(guild_id).or_default().insert(user_id);
            }
        }
        Registration { session_id, events, came_online }
    }

    /// Unregisters a session. Returns the user's guilds if it was its last session
    /// (the caller then broadcasts the offline presence).
    pub fn disconnect(&self, user_id: Snowflake, session_id: SessionId) -> Option<Vec<Snowflake>> {
        let mut guard = self.write();
        let registry = &mut *guard;
        let user = registry.users.get_mut(&user_id)?;
        user.sessions.remove(&session_id)?;
        if !user.sessions.is_empty() {
            return None;
        }
        let user = registry.users.remove(&user_id)?;
        for guild_id in &user.guilds {
            remove_from_guild(&mut registry.guilds, *guild_id, user_id);
        }
        Some(user.guilds.into_iter().collect())
    }

    pub fn is_online(&self, user_id: Snowflake) -> bool {
        self.read().users.contains_key(&user_id)
    }

    /// The user joined a guild: route that guild's events to its live sessions.
    pub fn join_guild(&self, user_id: Snowflake, guild_id: Snowflake) {
        let mut guard = self.write();
        let registry = &mut *guard;
        if let Some(user) = registry.users.get_mut(&user_id)
            && user.guilds.insert(guild_id)
        {
            registry.guilds.entry(guild_id).or_default().insert(user_id);
        }
    }

    pub fn leave_guild(&self, user_id: Snowflake, guild_id: Snowflake) {
        let mut guard = self.write();
        let registry = &mut *guard;
        if let Some(user) = registry.users.get_mut(&user_id) {
            user.guilds.remove(&guild_id);
        }
        remove_from_guild(&mut registry.guilds, guild_id, user_id);
    }

    pub fn remove_guild(&self, guild_id: Snowflake) {
        let mut guard = self.write();
        let registry = &mut *guard;
        for user_id in registry.guilds.remove(&guild_id).unwrap_or_default() {
            if let Some(user) = registry.users.get_mut(&user_id) {
                user.guilds.remove(&guild_id);
            }
        }
    }

    pub fn dispatch_to_guild(&self, guild_id: Snowflake, event: Event) {
        let Some(frame) = encode(event) else { return };
        let registry = self.read();
        for user_id in registry.guilds.get(&guild_id).into_iter().flatten() {
            deliver_to_user(&registry, *user_id, &frame);
        }
    }

    pub fn dispatch_to_user(&self, user_id: Snowflake, event: Event) {
        let Some(frame) = encode(event) else { return };
        deliver_to_user(&self.read(), user_id, &frame);
    }

    /// Delivers once per recipient, even to users sharing several of these guilds.
    pub fn dispatch_to_guilds(&self, guild_ids: &[Snowflake], event: Event) {
        let Some(frame) = encode(event) else { return };
        let registry = self.read();
        let recipients: HashSet<Snowflake> =
            guild_ids.iter().filter_map(|guild_id| registry.guilds.get(guild_id)).flatten().copied().collect();
        for user_id in recipients {
            deliver_to_user(&registry, user_id, &frame);
        }
    }

    #[cfg(test)]
    fn online_in(&self, guild_id: Snowflake) -> usize {
        self.read().guilds.get(&guild_id).map_or(0, HashSet::len)
    }

    // The registry is never left half-updated by a panic, so poisoning is safe to ignore.
    fn read(&self) -> RwLockReadGuard<'_, Registry> {
        self.registry.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, Registry> {
        self.registry.write().unwrap_or_else(|e| e.into_inner())
    }
}

fn remove_from_guild(guilds: &mut HashMap<Snowflake, HashSet<Snowflake>>, guild_id: Snowflake, user_id: Snowflake) {
    if let Some(online) = guilds.get_mut(&guild_id) {
        online.remove(&user_id);
        if online.is_empty() {
            guilds.remove(&guild_id);
        }
    }
}

fn encode(event: Event) -> Option<Utf8Bytes> {
    match serde_json::to_string(&ServerFrame::Dispatch(Box::new(event))) {
        Ok(json) => Some(Utf8Bytes::from(json)),
        Err(err) => {
            tracing::error!(error = %err, "failed to serialise gateway event");
            None
        }
    }
}

fn deliver_to_user(registry: &Registry, user_id: Snowflake, frame: &Utf8Bytes) {
    let Some(user) = registry.users.get(&user_id) else { return };
    for (session_id, session) in &user.sessions {
        match session.tx.try_send(frame.clone()) {
            Ok(()) | Err(TrySendError::Closed(_)) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!(%user_id, session_id, "gateway session lagging behind, disconnecting");
                session.kill.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use plume_lite_protocol::model::Status;

    use super::*;

    fn presence(user: i64) -> Event {
        Event::PresenceUpdate { user_id: Snowflake(user), status: Status::Online }
    }

    #[tokio::test]
    async fn routes_events_to_online_guild_members_only() {
        let hub = Hub::default();
        let (g1, g2) = (Snowflake(100), Snowflake(200));
        let mut alice = hub.connect(Snowflake(1), [g1], CancellationToken::new());
        let mut bob = hub.connect(Snowflake(2), [g1, g2], CancellationToken::new());
        assert!(alice.came_online && bob.came_online);

        hub.dispatch_to_guild(g2, presence(9));
        assert!(bob.events.try_recv().is_ok());
        assert!(alice.events.try_recv().is_err(), "alice is not in g2");

        // Shared guilds do not cause duplicates.
        hub.dispatch_to_guilds(&[g1, g2], presence(9));
        assert!(bob.events.try_recv().is_ok());
        assert!(bob.events.try_recv().is_err());
        assert!(alice.events.try_recv().is_ok(), "alice shares g1");

        hub.join_guild(Snowflake(1), g2);
        hub.dispatch_to_guild(g2, presence(9));
        assert!(alice.events.try_recv().is_ok());

        hub.leave_guild(Snowflake(1), g2);
        hub.dispatch_to_guild(g2, presence(9));
        assert!(alice.events.try_recv().is_err());
    }

    #[test]
    fn presence_follows_the_last_session() {
        let hub = Hub::default();
        let g = Snowflake(100);
        let first = hub.connect(Snowflake(1), [g], CancellationToken::new());
        let second = hub.connect(Snowflake(1), [g], CancellationToken::new());
        assert!(first.came_online && !second.came_online);
        assert_eq!(hub.disconnect(Snowflake(1), first.session_id), None);
        assert!(hub.is_online(Snowflake(1)));
        assert_eq!(hub.disconnect(Snowflake(1), second.session_id), Some(vec![g]));
        assert!(!hub.is_online(Snowflake(1)));
        assert_eq!(hub.online_in(g), 0);
        assert_eq!(hub.disconnect(Snowflake(1), second.session_id), None, "idempotent");
    }

    #[test]
    fn slow_consumers_are_cut_off() {
        let hub = Hub::default();
        let kill = CancellationToken::new();
        let _registration = hub.connect(Snowflake(1), [Snowflake(100)], kill.clone());
        for _ in 0..=SESSION_QUEUE_CAPACITY {
            hub.dispatch_to_user(Snowflake(1), presence(9));
        }
        assert!(kill.is_cancelled());
    }

    #[test]
    fn removing_a_guild_unsubscribes_everyone() {
        let hub = Hub::default();
        let g = Snowflake(100);
        let _a = hub.connect(Snowflake(1), [g], CancellationToken::new());
        let _b = hub.connect(Snowflake(2), [g], CancellationToken::new());
        assert_eq!(hub.online_in(g), 2);
        hub.remove_guild(g);
        assert_eq!(hub.online_in(g), 0);
    }
}
