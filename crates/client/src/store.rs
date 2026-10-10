//! Normalised client-side cache. Pure data + reducers, no I/O: everything the
//! gateway or the REST API returns is folded in here, idempotently.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::{Event, Ready};
use plume_lite_protocol::model::{Channel, ChannelKind, FullGuild, Guild, Member, Message, VoiceState};

/// Size of a history page (must match what `Api::messages` requests).
pub const PAGE_SIZE: usize = 50;
pub const TYPING_TTL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct Store {
    guild_order: Vec<Snowflake>,
    guilds: HashMap<Snowflake, Guild>,
    member_counts: HashMap<Snowflake, i64>,
    channels: HashMap<Snowflake, Channel>,
    timelines: HashMap<Snowflake, Timeline>,
    members: HashMap<Snowflake, Vec<Member>>,
    members_requested: HashSet<Snowflake>,
    typing: HashMap<Snowflake, Vec<Typist>>,
    unread: HashSet<Snowflake>,
    /// Who is in a voice channel, by user (`channel_id` is always set here).
    voice: HashMap<Snowflake, VoiceState>,
}

/// Messages of one channel, sorted by id (= chronologically), plus our unconfirmed sends.
#[derive(Debug, Default)]
pub struct Timeline {
    pub messages: Vec<Message>,
    pub pending: Vec<Pending>,
    pub loaded: bool,
    pub loading: bool,
    pub reached_start: bool,
}

/// Optimistic copy of a message being sent, reconciled through its nonce.
#[derive(Debug, Clone)]
pub struct Pending {
    pub nonce: String,
    pub content: String,
    pub failed: bool,
}

#[derive(Debug, Clone)]
struct Typist {
    user_id: Snowflake,
    name: String,
    since: Instant,
}

impl Store {
    /// READY is the resync point: everything is rebuilt, only unsent drafts survive.
    pub fn load_ready(&mut self, ready: Ready) {
        let pending: Vec<(Snowflake, Vec<Pending>)> = self
            .timelines
            .drain()
            .filter(|(_, timeline)| !timeline.pending.is_empty())
            .map(|(channel_id, timeline)| (channel_id, timeline.pending))
            .collect();
        *self = Self::default();
        for guild in ready.guilds {
            self.insert_guild(guild);
        }
        for (channel_id, pending) in pending {
            if self.channels.contains_key(&channel_id) {
                self.timelines.entry(channel_id).or_default().pending = pending;
            }
        }
    }

    pub fn apply(&mut self, event: Event, me: Snowflake) {
        match event {
            Event::MessageCreate(message) => {
                self.stop_typing(message.channel_id, message.author.id);
                self.upsert_message(message);
            }
            Event::MessageUpdate(message) => {
                if let Some(timeline) = self.timelines.get_mut(&message.channel_id)
                    && let Ok(i) = timeline.messages.binary_search_by_key(&message.id, |m| m.id)
                {
                    timeline.messages[i] = message;
                }
            }
            Event::MessageDelete { id, channel_id, .. } => self.remove_message(channel_id, id),
            Event::ChannelCreate(channel) => {
                self.channels.insert(channel.id, channel);
            }
            Event::ChannelDelete { id, .. } => self.remove_channel(id),
            Event::GuildCreate(guild) => self.insert_guild(guild),
            Event::GuildDelete { id } => self.remove_guild(id),
            Event::GuildMemberAdd { guild_id, member } => match self.members.get_mut(&guild_id) {
                Some(list) if list.iter().any(|m| m.user.id == member.user.id) => {}
                Some(list) => {
                    list.push(member);
                    sort_members(list);
                    *self.member_counts.entry(guild_id).or_default() += 1;
                }
                None => *self.member_counts.entry(guild_id).or_default() += 1,
            },
            Event::GuildMemberRemove { guild_id, user } => {
                if let Some(list) = self.members.get_mut(&guild_id) {
                    list.retain(|m| m.user.id != user.id);
                }
                if let Some(count) = self.member_counts.get_mut(&guild_id) {
                    *count = (*count - 1).max(0);
                }
            }
            Event::PresenceUpdate { user_id, status } => {
                for member in self.members.values_mut().flatten().filter(|m| m.user.id == user_id) {
                    member.status = status;
                }
            }
            Event::TypingStart { guild_id, channel_id, user_id } => {
                if user_id != me {
                    let name = self.user_name(guild_id, user_id).unwrap_or_else(|| "Quelqu'un".into());
                    self.start_typing(channel_id, user_id, name, Instant::now());
                }
            }
            Event::VoiceStateUpdate(state) => self.set_voice_state(state),
        }
    }

    // --- Guilds & channels -------------------------------------------------------------------

    fn insert_guild(&mut self, full: FullGuild) {
        let id = full.guild.id;
        if !self.guilds.contains_key(&id) {
            self.guild_order.push(id);
        }
        self.channels.retain(|_, c| c.guild_id != id);
        for channel in full.channels {
            self.channels.insert(channel.id, channel);
        }
        self.voice.retain(|_, state| state.guild_id != id);
        for state in full.voice_states {
            self.set_voice_state(state);
        }
        self.member_counts.insert(id, full.member_count);
        self.guilds.insert(id, full.guild);
    }

    fn remove_guild(&mut self, id: Snowflake) {
        self.guild_order.retain(|g| *g != id);
        self.guilds.remove(&id);
        self.member_counts.remove(&id);
        self.members.remove(&id);
        self.members_requested.remove(&id);
        self.voice.retain(|_, state| state.guild_id != id);
        let channels: Vec<Snowflake> = self.channels.values().filter(|c| c.guild_id == id).map(|c| c.id).collect();
        for channel_id in channels {
            self.remove_channel(channel_id);
        }
    }

    fn remove_channel(&mut self, id: Snowflake) {
        self.channels.remove(&id);
        self.timelines.remove(&id);
        self.typing.remove(&id);
        self.unread.remove(&id);
        self.voice.retain(|_, state| state.channel_id != Some(id));
    }

    pub fn guilds(&self) -> impl Iterator<Item = &Guild> {
        self.guild_order.iter().filter_map(|id| self.guilds.get(id))
    }

    pub fn guild(&self, id: Snowflake) -> Option<&Guild> {
        self.guilds.get(&id)
    }

    pub fn member_count(&self, guild_id: Snowflake) -> i64 {
        self.member_counts.get(&guild_id).copied().unwrap_or(0)
    }

    pub fn channel(&self, id: Snowflake) -> Option<&Channel> {
        self.channels.get(&id)
    }

    pub fn channels_of(&self, guild_id: Snowflake) -> Vec<&Channel> {
        let mut channels: Vec<&Channel> = self.channels.values().filter(|c| c.guild_id == guild_id).collect();
        channels.sort_by_key(|c| (c.position, c.id));
        channels
    }

    // --- Messages ----------------------------------------------------------------------------

    pub fn timeline(&self, channel_id: Snowflake) -> Option<&Timeline> {
        self.timelines.get(&channel_id)
    }

    /// True when the channel has never been loaded (or was reset by a READY).
    pub fn needs_history(&self, channel_id: Snowflake) -> bool {
        self.timelines.get(&channel_id).is_none_or(|t| !t.loaded && !t.loading)
    }

    pub fn can_load_older(&self, channel_id: Snowflake) -> Option<Snowflake> {
        let timeline = self.timelines.get(&channel_id)?;
        if !timeline.loaded || timeline.loading || timeline.reached_start {
            return None;
        }
        timeline.messages.first().map(|m| m.id)
    }

    pub fn set_loading(&mut self, channel_id: Snowflake, loading: bool) {
        self.timelines.entry(channel_id).or_default().loading = loading;
    }

    /// Folds a history page in. `older` pages are the ones fetched with `before`.
    pub fn add_history(&mut self, channel_id: Snowflake, page: Vec<Message>, older: bool) {
        if !self.channels.contains_key(&channel_id) {
            return;
        }
        let timeline = self.timelines.entry(channel_id).or_default();
        if !older || !timeline.loaded {
            timeline.reached_start = page.len() < PAGE_SIZE;
        } else if page.len() < PAGE_SIZE {
            timeline.reached_start = true;
        }
        timeline.loaded = true;
        timeline.loading = false;
        for message in page {
            insert_sorted(&mut timeline.messages, message);
        }
    }

    /// Inserts or replaces a message, and settles the matching optimistic copy.
    /// Messages of never-opened channels are not kept (the history fetch will bring them).
    pub fn upsert_message(&mut self, message: Message) {
        let Some(timeline) = self.timelines.get_mut(&message.channel_id) else { return };
        if let Some(nonce) = &message.nonce {
            timeline.pending.retain(|p| &p.nonce != nonce);
        }
        insert_sorted(&mut timeline.messages, message);
    }

    pub fn message(&self, channel_id: Snowflake, id: Snowflake) -> Option<&Message> {
        let timeline = self.timelines.get(&channel_id)?;
        let i = timeline.messages.binary_search_by_key(&id, |m| m.id).ok()?;
        timeline.messages.get(i)
    }

    pub fn remove_message(&mut self, channel_id: Snowflake, id: Snowflake) {
        if let Some(timeline) = self.timelines.get_mut(&channel_id)
            && let Ok(i) = timeline.messages.binary_search_by_key(&id, |m| m.id)
        {
            timeline.messages.remove(i);
        }
    }

    pub fn add_pending(&mut self, channel_id: Snowflake, nonce: String, content: String) {
        self.timelines.entry(channel_id).or_default().pending.push(Pending { nonce, content, failed: false });
    }

    pub fn fail_pending(&mut self, channel_id: Snowflake, nonce: &str) {
        if let Some(p) = self.pending_mut(channel_id, nonce) {
            p.failed = true;
        }
    }

    /// Marks a failed message as in-flight again and returns its content.
    pub fn retry_pending(&mut self, channel_id: Snowflake, nonce: &str) -> Option<String> {
        let pending = self.pending_mut(channel_id, nonce)?;
        pending.failed = false;
        Some(pending.content.clone())
    }

    pub fn remove_pending(&mut self, channel_id: Snowflake, nonce: &str) {
        if let Some(timeline) = self.timelines.get_mut(&channel_id) {
            timeline.pending.retain(|p| p.nonce != nonce);
        }
    }

    fn pending_mut(&mut self, channel_id: Snowflake, nonce: &str) -> Option<&mut Pending> {
        self.timelines.get_mut(&channel_id)?.pending.iter_mut().find(|p| p.nonce == nonce)
    }

    /// Keeps only the `keep` most recent messages of a channel that is not displayed,
    /// so memory stays flat no matter how long the app runs.
    pub fn trim(&mut self, channel_id: Snowflake, keep: usize) {
        if let Some(timeline) = self.timelines.get_mut(&channel_id)
            && timeline.messages.len() > keep
        {
            let excess = timeline.messages.len() - keep;
            timeline.messages.drain(..excess);
            timeline.reached_start = false;
        }
    }

    // --- Members, presence, typing -----------------------------------------------------------

    /// Returns `true` the first time, i.e. when the caller should fetch the list.
    pub fn request_members(&mut self, guild_id: Snowflake) -> bool {
        !self.members.contains_key(&guild_id) && self.members_requested.insert(guild_id)
    }

    pub fn members_failed(&mut self, guild_id: Snowflake) {
        self.members_requested.remove(&guild_id);
    }

    pub fn set_members(&mut self, guild_id: Snowflake, mut members: Vec<Member>) {
        // The member count stays the server's (READY + events): the list may be a capped page.
        if self.guilds.contains_key(&guild_id) {
            sort_members(&mut members);
            self.members.insert(guild_id, members);
        }
    }

    pub fn members(&self, guild_id: Snowflake) -> Option<&[Member]> {
        self.members.get(&guild_id).map(Vec::as_slice)
    }

    /// A user's name, from the guild's member list or, failing that, from their messages.
    pub fn user_name(&self, guild_id: Snowflake, user_id: Snowflake) -> Option<String> {
        let from_members = self.members.get(&guild_id).and_then(|list| list.iter().find(|m| m.user.id == user_id));
        from_members.map(|m| m.user.username.clone()).or_else(|| {
            self.timelines
                .values()
                .flat_map(|t| t.messages.iter().rev())
                .find(|m| m.author.id == user_id)
                .map(|m| m.author.username.clone())
        })
    }

    fn start_typing(&mut self, channel_id: Snowflake, user_id: Snowflake, name: String, now: Instant) {
        let typists = self.typing.entry(channel_id).or_default();
        typists.retain(|t| t.user_id != user_id);
        typists.push(Typist { user_id, name, since: now });
    }

    fn stop_typing(&mut self, channel_id: Snowflake, user_id: Snowflake) {
        if let Some(typists) = self.typing.get_mut(&channel_id) {
            typists.retain(|t| t.user_id != user_id);
        }
    }

    pub fn typists(&self, channel_id: Snowflake) -> Vec<&str> {
        self.typing.get(&channel_id).into_iter().flatten().map(|t| t.name.as_str()).collect()
    }

    pub fn has_typists(&self) -> bool {
        self.typing.values().any(|t| !t.is_empty())
    }

    pub fn expire_typing(&mut self, now: Instant) {
        for typists in self.typing.values_mut() {
            typists.retain(|t| now.duration_since(t.since) < TYPING_TTL);
        }
        self.typing.retain(|_, t| !t.is_empty());
    }

    // --- Unread markers ----------------------------------------------------------------------

    pub fn mark_unread(&mut self, channel_id: Snowflake) {
        self.unread.insert(channel_id);
    }

    pub fn mark_read(&mut self, channel_id: Snowflake) {
        self.unread.remove(&channel_id);
    }

    pub fn is_unread(&self, channel_id: Snowflake) -> bool {
        self.unread.contains(&channel_id)
    }

    // --- Voice --------------------------------------------------------------------------------

    /// Idempotent: a user is in at most one voice channel; `channel_id: None` removes them,
    /// unless they are already in another guild's channel (a late "left").
    fn set_voice_state(&mut self, state: VoiceState) {
        if state.channel_id.is_some() {
            self.voice.insert(state.user_id, state);
        } else if self.voice.get(&state.user_id).is_some_and(|current| current.guild_id == state.guild_id) {
            self.voice.remove(&state.user_id);
        }
    }

    /// Who is in a voice channel, in a stable order.
    pub fn voice_members(&self, channel_id: Snowflake) -> Vec<&VoiceState> {
        let mut members: Vec<&VoiceState> =
            self.voice.values().filter(|state| state.channel_id == Some(channel_id)).collect();
        members.sort_by_key(|state| state.user_id);
        members
    }

    pub fn voice_state(&self, user_id: Snowflake) -> Option<&VoiceState> {
        self.voice.get(&user_id)
    }

    pub fn is_voice_channel(&self, channel_id: Snowflake) -> bool {
        self.channels.get(&channel_id).is_some_and(|c| c.kind == ChannelKind::Voice)
    }

    pub fn guild_has_unread(&self, guild_id: Snowflake) -> bool {
        self.unread.iter().any(|c| self.channels.get(c).is_some_and(|c| c.guild_id == guild_id))
    }
}

fn insert_sorted(messages: &mut Vec<Message>, message: Message) {
    match messages.binary_search_by_key(&message.id, |m| m.id) {
        Ok(i) => messages[i] = message,
        Err(i) => messages.insert(i, message),
    }
}

fn sort_members(members: &mut [Member]) {
    members.sort_by_cached_key(|m| m.user.username.to_lowercase());
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use plume_lite_protocol::model::{ChannelKind, Status, User};

    use super::*;

    const ME: Snowflake = Snowflake(1);
    const GUILD: Snowflake = Snowflake(10);
    const CHANNEL: Snowflake = Snowflake(20);

    fn user(id: i64, name: &str) -> User {
        User { id: Snowflake(id), username: name.into() }
    }

    fn message(id: i64, author: &User, nonce: Option<&str>) -> Message {
        Message {
            id: Snowflake(id),
            channel_id: CHANNEL,
            guild_id: GUILD,
            author: author.clone(),
            content: format!("message {id}"),
            created_at: Utc::now(),
            edited_at: None,
            nonce: nonce.map(Into::into),
        }
    }

    fn store() -> Store {
        let mut store = Store::default();
        store.load_ready(Ready {
            user: user(1, "moi"),
            guilds: vec![FullGuild {
                guild: Guild { id: GUILD, name: "Rust FR".into(), owner_id: ME },
                channels: vec![Channel {
                    id: CHANNEL,
                    guild_id: GUILD,
                    name: "général".into(),
                    kind: ChannelKind::Text,
                    position: 0,
                }],
                member_count: 2,
                voice_states: vec![],
            }],
        });
        store
    }

    #[test]
    fn history_pages_merge_without_duplicates() {
        let mut store = store();
        let alice = user(2, "alice");
        store.add_history(CHANNEL, (100..150).map(|i| message(i, &alice, None)).collect(), false);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert!(timeline.loaded && !timeline.reached_start);
        assert_eq!(store.can_load_older(CHANNEL), Some(Snowflake(100)));

        store.add_history(CHANNEL, (90..101).map(|i| message(i, &alice, None)).collect(), true);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert_eq!(timeline.messages.len(), 60);
        assert!(timeline.messages.windows(2).all(|w| w[0].id < w[1].id));
        assert!(timeline.reached_start, "short page means start of history");

        // The same event twice is harmless.
        store.apply(Event::MessageCreate(message(150, &alice, None)), ME);
        store.apply(Event::MessageCreate(message(150, &alice, None)), ME);
        assert_eq!(store.timeline(CHANNEL).unwrap().messages.len(), 61);
    }

    #[test]
    fn optimistic_messages_are_reconciled_by_nonce() {
        let mut store = store();
        store.add_history(CHANNEL, vec![], false);
        store.add_pending(CHANNEL, "n1".into(), "salut".into());
        store.add_pending(CHANNEL, "n2".into(), "ça va ?".into());
        store.fail_pending(CHANNEL, "n2");
        assert!(store.timeline(CHANNEL).unwrap().pending[1].failed);
        assert_eq!(store.retry_pending(CHANNEL, "n2").as_deref(), Some("ça va ?"));

        store.apply(Event::MessageCreate(message(5, &user(1, "moi"), Some("n1"))), ME);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert_eq!(timeline.messages.len(), 1);
        assert_eq!(timeline.pending.len(), 1);
        assert_eq!(timeline.pending[0].nonce, "n2");
    }

    #[test]
    fn deleting_a_guild_forgets_everything_about_it() {
        let mut store = store();
        store.add_history(CHANNEL, vec![message(1, &user(2, "alice"), None)], false);
        store.mark_unread(CHANNEL);
        assert!(store.guild_has_unread(GUILD));
        store.apply(Event::GuildDelete { id: GUILD }, ME);
        assert_eq!(store.guilds().count(), 0);
        assert!(store.channel(CHANNEL).is_none());
        assert!(store.timeline(CHANNEL).is_none());
        assert!(!store.is_unread(CHANNEL));
    }

    #[test]
    fn members_presence_and_typing() {
        let mut store = store();
        assert!(store.request_members(GUILD));
        assert!(!store.request_members(GUILD), "only one request in flight");
        let member = |id, name, status| Member { user: user(id, name), joined_at: Utc::now(), status };
        store.set_members(GUILD, vec![member(3, "zoé", Status::Offline), member(2, "Alice", Status::Offline)]);
        assert_eq!(store.members(GUILD).unwrap()[0].user.username, "Alice");

        store.apply(Event::PresenceUpdate { user_id: Snowflake(2), status: Status::Online }, ME);
        assert_eq!(store.members(GUILD).unwrap()[0].status, Status::Online);

        store.apply(Event::GuildMemberAdd { guild_id: GUILD, member: member(4, "bob", Status::Online) }, ME);
        store.apply(Event::GuildMemberAdd { guild_id: GUILD, member: member(4, "bob", Status::Online) }, ME);
        assert_eq!(store.member_count(GUILD), 3);

        store.apply(Event::TypingStart { guild_id: GUILD, channel_id: CHANNEL, user_id: Snowflake(2) }, ME);
        store.apply(Event::TypingStart { guild_id: GUILD, channel_id: CHANNEL, user_id: ME }, ME);
        assert_eq!(store.typists(CHANNEL), ["Alice"]);
        store.expire_typing(Instant::now() + TYPING_TTL);
        assert!(!store.has_typists());
    }

    #[test]
    fn trimming_hidden_channels_bounds_memory() {
        let mut store = store();
        let alice = user(2, "alice");
        store.add_history(CHANNEL, (0..50).map(|i| message(i, &alice, None)).collect(), false);
        store.add_history(CHANNEL, (50..300).map(|i| message(i, &alice, None)).collect(), true);
        store.trim(CHANNEL, 100);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert_eq!(timeline.messages.len(), 100);
        assert_eq!(timeline.messages[0].id, Snowflake(200));
        assert!(!timeline.reached_start);
    }

    #[test]
    fn voice_states_are_idempotent_and_follow_channels() {
        let mut store = store();
        let voice = Snowflake(30);
        store.apply(
            Event::ChannelCreate(Channel {
                id: voice,
                guild_id: GUILD,
                name: "vocal".into(),
                kind: ChannelKind::Voice,
                position: 1,
            }),
            ME,
        );
        assert!(store.is_voice_channel(voice));
        let bob = Snowflake(2);
        let state = |channel_id, self_mute| VoiceState {
            guild_id: GUILD,
            user_id: bob,
            channel_id,
            self_mute,
            self_deaf: false,
            server_mute: false,
        };

        store.apply(Event::VoiceStateUpdate(state(Some(voice), false)), ME);
        store.apply(Event::VoiceStateUpdate(state(Some(voice), true)), ME);
        assert_eq!(store.voice_members(voice).len(), 1, "an update twice is one member");
        assert!(store.voice_state(bob).unwrap().self_mute);

        // A "left" for another guild does not remove him from this one.
        store.apply(Event::VoiceStateUpdate(VoiceState { guild_id: Snowflake(99), ..state(None, false) }), ME);
        assert_eq!(store.voice_members(voice).len(), 1);
        store.apply(Event::VoiceStateUpdate(state(None, false)), ME);
        assert!(store.voice_members(voice).is_empty());

        // Deleting the channel empties it.
        store.apply(Event::VoiceStateUpdate(state(Some(voice), false)), ME);
        store.apply(Event::ChannelDelete { id: voice, guild_id: GUILD }, ME);
        assert!(store.voice_state(bob).is_none());
    }
}
