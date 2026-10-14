//! Normalised client-side cache. Pure data + reducers, no I/O: everything the
//! gateway or the REST API returns is folded in here, idempotently.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::gateway::{Event, Ready};
use plume_lite_protocol::layout::{self, Section};
use plume_lite_protocol::markup::{self, Block};
use plume_lite_protocol::model::{
    Category, Channel, ChannelKind, FullGuild, Guild, MENTIONS_COUNTED_MAX, Member, Message, NotificationLevel,
    REPLY_EXCERPT_MAX, Reaction, ReadState, Role, VoiceState,
};
use plume_lite_protocol::permissions::{Permissions, Standing, channel_permissions};

/// Size of a history page (must match what `Api::messages` requests).
pub const PAGE_SIZE: usize = 50;
pub const TYPING_TTL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct Store {
    guild_order: Vec<Snowflake>,
    guilds: HashMap<Snowflake, Guild>,
    member_counts: HashMap<Snowflake, i64>,
    channels: HashMap<Snowflake, Channel>,
    categories: HashMap<Snowflake, Category>,
    roles: HashMap<Snowflake, Role>,
    /// Our roles, by guild.
    my_roles: HashMap<Snowflake, Vec<Snowflake>>,
    timelines: HashMap<Snowflake, Timeline>,
    members: HashMap<Snowflake, Vec<Member>>,
    members_requested: HashSet<Snowflake>,
    typing: HashMap<Snowflake, Vec<Typist>>,
    /// How far we read each text channel (from the server: every device agrees).
    read: HashMap<Snowflake, ReadState>,
    /// Our notification settings, by guild or channel.
    notifications: HashMap<Snowflake, NotificationLevel>,
    /// Who is in a voice channel, by user (`channel_id` is always set here).
    voice: HashMap<Snowflake, VoiceState>,
}

/// Messages of one channel, sorted by id (= chronologically), plus our unconfirmed sends.
#[derive(Debug, Default)]
pub struct Timeline {
    pub messages: Vec<Message>,
    /// The markup of each message, parsed once rather than at every frame.
    parsed: HashMap<Snowflake, Vec<Block>>,
    pub pending: Vec<Pending>,
    pub loaded: bool,
    pub loading: bool,
    pub reached_start: bool,
}

impl Timeline {
    pub fn blocks(&self, message_id: Snowflake) -> &[Block] {
        self.parsed.get(&message_id).map_or(&[], Vec::as_slice)
    }

    /// Inserts or replaces a message.
    fn insert(&mut self, message: Message) {
        let unchanged = self.parsed.contains_key(&message.id)
            && self
                .messages
                .binary_search_by_key(&message.id, |m| m.id)
                .is_ok_and(|i| self.messages[i].content == message.content);
        if !unchanged {
            self.parsed.insert(message.id, markup::parse(&message.content));
        }
        match self.messages.binary_search_by_key(&message.id, |m| m.id) {
            Ok(i) => self.messages[i] = message,
            Err(i) => self.messages.insert(i, message),
        }
    }

    fn remove(&mut self, id: Snowflake) {
        if let Ok(i) = self.messages.binary_search_by_key(&id, |m| m.id) {
            self.messages.remove(i);
        }
        self.parsed.remove(&id);
        // Replies to it now say it was deleted.
        for reply in self.messages.iter_mut().filter_map(|m| m.reply_to.as_mut()).filter(|r| r.id == id) {
            reply.author = None;
            reply.excerpt.clear();
        }
    }

    /// Replies to an edited message show its new beginning.
    fn refresh_excerpts(&mut self, edited: &Message) {
        for reply in self.messages.iter_mut().filter_map(|m| m.reply_to.as_mut()).filter(|r| r.id == edited.id) {
            reply.excerpt = edited.content.chars().take(REPLY_EXCERPT_MAX).collect();
        }
    }
}

/// Optimistic copy of a message being sent, reconciled through its nonce.
#[derive(Debug, Clone)]
pub struct Pending {
    pub nonce: String,
    pub content: String,
    pub reply_to: Option<Snowflake>,
    pub blocks: Vec<Block>,
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
        self.read = ready.read_states.into_iter().map(|read| (read.channel_id, read)).collect();
        self.notifications = ready.notification_settings.into_iter().map(|s| (s.target_id, s.level)).collect();
    }

    pub fn apply(&mut self, event: Event, me: Snowflake) {
        match event {
            Event::MessageCreate(message) => {
                self.stop_typing(message.channel_id, message.author.id);
                self.count_new_message(&message, me);
                self.upsert_message(message);
            }
            Event::MessageUpdate(message) => {
                if let Some(timeline) = self.timelines.get_mut(&message.channel_id) {
                    timeline.refresh_excerpts(&message);
                    if timeline.messages.binary_search_by_key(&message.id, |m| m.id).is_ok() {
                        timeline.insert(message);
                    }
                }
            }
            Event::MessageDelete { id, channel_id, .. } => self.remove_message(channel_id, id),
            Event::ChannelCreate(channel) | Event::ChannelUpdate(channel) => self.upsert_channel(channel),
            Event::ChannelDelete { id, .. } => self.remove_channel(id),
            Event::CategoryCreate(category) | Event::CategoryUpdate(category) => {
                if self.guilds.contains_key(&category.guild_id) {
                    self.categories.insert(category.id, category);
                }
            }
            Event::CategoryDelete { id, .. } => {
                self.categories.remove(&id);
                for channel in self.channels.values_mut().filter(|c| c.category_id == Some(id)) {
                    channel.category_id = None;
                }
            }
            Event::GuildRoleCreate(role) | Event::GuildRoleUpdate(role) => {
                if self.guilds.contains_key(&role.guild_id) {
                    self.roles.insert(role.id, role);
                }
            }
            Event::GuildRoleDelete { guild_id, id } => {
                self.roles.remove(&id);
                for roles in self
                    .my_roles
                    .get_mut(&guild_id)
                    .into_iter()
                    .chain(self.members.get_mut(&guild_id).into_iter().flatten().map(|member| &mut member.roles))
                {
                    roles.retain(|role| *role != id);
                }
            }
            Event::GuildMemberUpdate { guild_id, user_id, roles } => {
                if user_id == me && self.guilds.contains_key(&guild_id) {
                    self.my_roles.insert(guild_id, roles.clone());
                }
                if let Some(member) =
                    self.members.get_mut(&guild_id).and_then(|list| list.iter_mut().find(|m| m.user.id == user_id))
                {
                    member.roles = roles;
                }
            }
            Event::GuildCreate(guild) => self.insert_guild(guild),
            Event::GuildUpdate(guild) => {
                if let Some(known) = self.guilds.get_mut(&guild.id) {
                    *known = guild;
                }
            }
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
            Event::NotificationSettingUpdate { target_id, level } => match level {
                Some(level) => {
                    self.notifications.insert(target_id, level);
                }
                None => {
                    self.notifications.remove(&target_id);
                }
            },
            Event::ReadStateUpdate(read) => {
                // Never backwards: an older update arriving late changes nothing.
                if self.read.get(&read.channel_id).is_none_or(|current| read.last_read_id >= current.last_read_id) {
                    self.read.insert(read.channel_id, read);
                }
            }
            Event::ReactionAdd { channel_id, message_id, user_id, emoji, .. } => {
                self.set_reaction(channel_id, message_id, &emoji, user_id, true);
            }
            Event::ReactionRemove { channel_id, message_id, user_id, emoji, .. } => {
                self.set_reaction(channel_id, message_id, &emoji, user_id, false);
            }
            // For the call, not the data: handled by the app.
            Event::VoiceMove { .. } => {}
        }
    }

    // --- Guilds & channels -------------------------------------------------------------------

    fn insert_guild(&mut self, full: FullGuild) {
        let id = full.guild.id;
        if !self.guilds.contains_key(&id) {
            self.guild_order.push(id);
        }
        // Channels it no longer has (deleted, or no longer visible to us) go with their data.
        let gone: Vec<Snowflake> = self
            .channels
            .values()
            .filter(|c| c.guild_id == id && !full.channels.iter().any(|kept| kept.id == c.id))
            .map(|c| c.id)
            .collect();
        for channel_id in gone {
            self.remove_channel(channel_id);
        }
        for mut channel in full.channels {
            // The guild again (after a change of rights): keep what we know of its last message.
            if let Some(known) = self.channels.get(&channel.id) {
                channel.last_message_id = channel.last_message_id.max(known.last_message_id);
            }
            self.channels.insert(channel.id, channel);
        }
        self.categories.retain(|_, c| c.guild_id != id);
        for category in full.categories {
            self.categories.insert(category.id, category);
        }
        self.roles.retain(|_, r| r.guild_id != id);
        for role in full.roles {
            self.roles.insert(role.id, role);
        }
        self.my_roles.insert(id, full.my_roles);
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
        self.categories.retain(|_, c| c.guild_id != id);
        self.roles.retain(|_, r| r.guild_id != id);
        self.my_roles.remove(&id);
        let channels: Vec<Snowflake> = self.channels.values().filter(|c| c.guild_id == id).map(|c| c.id).collect();
        for channel_id in channels {
            self.remove_channel(channel_id);
        }
    }

    /// A created, renamed or moved channel. Updates do not carry the last message: the one
    /// already known stays (a message may also have arrived before the update).
    fn upsert_channel(&mut self, mut channel: Channel) {
        if !self.guilds.contains_key(&channel.guild_id) {
            return;
        }
        if let Some(known) = self.channels.get(&channel.id) {
            channel.last_message_id = channel.last_message_id.max(known.last_message_id);
        }
        self.channels.insert(channel.id, channel);
    }

    fn remove_channel(&mut self, id: Snowflake) {
        self.channels.remove(&id);
        self.timelines.remove(&id);
        self.typing.remove(&id);
        self.read.remove(&id);
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

    pub fn category(&self, id: Snowflake) -> Option<&Category> {
        self.categories.get(&id)
    }

    /// The guild's categories, in order.
    pub fn categories_of(&self, guild_id: Snowflake) -> Vec<&Category> {
        let mut categories: Vec<&Category> = self.categories.values().filter(|c| c.guild_id == guild_id).collect();
        categories.sort_by_key(|c| (c.position, c.id));
        categories
    }

    /// The guild's channels as listed: without a category first, then under each category.
    pub fn sections(&self, guild_id: Snowflake) -> Vec<Section<'_>> {
        layout::sections(
            self.categories.values().filter(|c| c.guild_id == guild_id),
            self.channels.values().filter(|c| c.guild_id == guild_id),
        )
    }

    /// The guild's roles, the strongest first (`@everyone` last).
    pub fn roles_of(&self, guild_id: Snowflake) -> Vec<&Role> {
        let mut roles: Vec<&Role> = self.roles.values().filter(|r| r.guild_id == guild_id).collect();
        roles.sort_by_key(|r| (std::cmp::Reverse(r.position), r.id));
        roles
    }

    pub fn role(&self, id: Snowflake) -> Option<&Role> {
        self.roles.get(&id)
    }

    /// A member's roles: ours always, the others' once the member list is loaded.
    pub fn member_roles(&self, guild_id: Snowflake, user_id: Snowflake, me: Snowflake) -> Option<&[Snowflake]> {
        if user_id == me {
            return self.my_roles.get(&guild_id).map(Vec::as_slice);
        }
        self.members.get(&guild_id)?.iter().find(|m| m.user.id == user_id).map(|m| m.roles.as_slice())
    }

    /// What a member may do, and how high they rank (as `@everyone` if unknown).
    pub fn standing(&self, guild_id: Snowflake, user_id: Snowflake, me: Snowflake) -> Standing {
        let owner = self.guilds.get(&guild_id).is_some_and(|g| g.owner_id == user_id);
        let member_roles = self.member_roles(guild_id, user_id, me).unwrap_or_default();
        Standing::of(guild_id, owner, self.roles.values().filter(|r| r.guild_id == guild_id), member_roles)
    }

    /// What we may do in a channel (nothing if we do not know it).
    pub fn channel_permissions(&self, channel_id: Snowflake, me: Snowflake) -> Permissions {
        let Some(channel) = self.channels.get(&channel_id) else { return Permissions::NONE };
        let standing = self.standing(channel.guild_id, me, me);
        let my_roles = self.member_roles(channel.guild_id, me, me).unwrap_or_default();
        channel_permissions(channel.guild_id, &standing, my_roles, &channel.overwrites)
    }

    /// The colour of a member's highest coloured role.
    pub fn role_color(&self, guild_id: Snowflake, user_id: Snowflake, me: Snowflake) -> Option<u32> {
        let member_roles = self.member_roles(guild_id, user_id, me)?;
        member_roles
            .iter()
            .filter_map(|id| self.roles.get(id))
            .filter_map(|role| role.color.map(|color| (role.position, color)))
            .max_by_key(|(position, _)| *position)
            .map(|(_, color)| color)
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
            timeline.insert(message);
        }
    }

    /// Inserts or replaces a message, and settles the matching optimistic copy.
    /// Messages of never-opened channels are not kept (the history fetch will bring them).
    pub fn upsert_message(&mut self, message: Message) {
        let Some(timeline) = self.timelines.get_mut(&message.channel_id) else { return };
        if let Some(nonce) = &message.nonce {
            timeline.pending.retain(|p| &p.nonce != nonce);
        }
        timeline.refresh_excerpts(&message);
        timeline.insert(message);
    }

    pub fn message(&self, channel_id: Snowflake, id: Snowflake) -> Option<&Message> {
        let timeline = self.timelines.get(&channel_id)?;
        let i = timeline.messages.binary_search_by_key(&id, |m| m.id).ok()?;
        timeline.messages.get(i)
    }

    pub fn remove_message(&mut self, channel_id: Snowflake, id: Snowflake) {
        if let Some(timeline) = self.timelines.get_mut(&channel_id) {
            timeline.remove(id);
        }
    }

    /// Idempotent: each reaction is a set of users, emptied ones disappear.
    pub fn set_reaction(
        &mut self,
        channel_id: Snowflake,
        message_id: Snowflake,
        emoji: &str,
        user: Snowflake,
        on: bool,
    ) {
        let Some(timeline) = self.timelines.get_mut(&channel_id) else { return };
        let Ok(i) = timeline.messages.binary_search_by_key(&message_id, |m| m.id) else { return };
        let reactions = &mut timeline.messages[i].reactions;
        match reactions.iter_mut().position(|r| r.emoji == emoji) {
            Some(r) if on => {
                if !reactions[r].users.contains(&user) {
                    reactions[r].users.push(user);
                }
            }
            Some(r) => {
                reactions[r].users.retain(|u| *u != user);
                if reactions[r].users.is_empty() {
                    reactions.remove(r);
                }
            }
            None if on => reactions.push(Reaction { emoji: emoji.to_owned(), users: vec![user] }),
            None => {}
        }
    }

    pub fn add_pending(&mut self, channel_id: Snowflake, nonce: String, content: String, reply_to: Option<Snowflake>) {
        let blocks = markup::parse(&content);
        let pending = Pending { nonce, content, reply_to, blocks, failed: false };
        self.timelines.entry(channel_id).or_default().pending.push(pending);
    }

    pub fn fail_pending(&mut self, channel_id: Snowflake, nonce: &str) {
        if let Some(p) = self.pending_mut(channel_id, nonce) {
            p.failed = true;
        }
    }

    /// Marks a failed message as in-flight again and returns its content and what it answers.
    pub fn retry_pending(&mut self, channel_id: Snowflake, nonce: &str) -> Option<(String, Option<Snowflake>)> {
        let pending = self.pending_mut(channel_id, nonce)?;
        pending.failed = false;
        Some((pending.content.clone(), pending.reply_to))
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
            for message in timeline.messages.drain(..excess) {
                timeline.parsed.remove(&message.id);
            }
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

    // --- Unread messages -----------------------------------------------------------------------

    /// A new message moves its channel forward. Ours mean we read it; one for us (our name,
    /// `@everyone` from someone allowed to, a reply to ours) counts as a mention, once: the same event
    /// twice is not newer than the channel's last message the second time.
    fn count_new_message(&mut self, message: &Message, me: Snowflake) {
        let Some(channel) = self.channels.get_mut(&message.channel_id) else { return };
        let is_new = channel.last_message_id.is_none_or(|last| message.id > last);
        if is_new {
            channel.last_message_id = Some(message.id);
        }
        let read = self.read.entry(message.channel_id).or_insert(ReadState {
            channel_id: message.channel_id,
            last_read_id: Snowflake(0),
            mentions: 0,
        });
        if message.author.id == me {
            if message.id > read.last_read_id {
                *read = ReadState { last_read_id: message.id, mentions: 0, ..*read };
            }
        } else if is_new && message.id > read.last_read_id && mentions(message, me) {
            read.mentions = (read.mentions + 1).min(MENTIONS_COUNTED_MAX);
        }
    }

    /// Marks a channel read up to its last message. Returns that message, to tell the server.
    pub fn mark_read(&mut self, channel_id: Snowflake) -> Option<Snowflake> {
        let last = self.channels.get(&channel_id)?.last_message_id?;
        let read =
            self.read.entry(channel_id).or_insert(ReadState { channel_id, last_read_id: Snowflake(0), mentions: 0 });
        if last <= read.last_read_id && read.mentions == 0 {
            return None;
        }
        *read = ReadState { last_read_id: last.max(read.last_read_id), mentions: 0, ..*read };
        Some(read.last_read_id)
    }

    pub fn is_unread(&self, channel_id: Snowflake) -> bool {
        let Some(channel) = self.channels.get(&channel_id).filter(|c| c.kind == ChannelKind::Text) else {
            return false;
        };
        let last_read = self.read.get(&channel_id).map_or(Snowflake(0), |read| read.last_read_id);
        channel.last_message_id.is_some_and(|last| last > last_read)
    }

    /// Our own setting for a guild or a channel, if any.
    pub fn notification_setting(&self, target_id: Snowflake) -> Option<NotificationLevel> {
        self.notifications.get(&target_id).copied()
    }

    /// What notifies us in a channel: its setting, else its guild's, else every message.
    pub fn notification_level(&self, channel_id: Snowflake) -> NotificationLevel {
        let guild_id = self.channels.get(&channel_id).map(|c| c.guild_id);
        self.notification_setting(channel_id)
            .or_else(|| guild_id.and_then(|g| self.notification_setting(g)))
            .unwrap_or_default()
    }

    /// Unread messages for us in a channel.
    pub fn mentions(&self, channel_id: Snowflake) -> u32 {
        self.read.get(&channel_id).map_or(0, |read| read.mentions)
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
        self.channels.values().any(|c| c.guild_id == guild_id && self.is_unread(c.id))
    }

    pub fn guild_mentions(&self, guild_id: Snowflake) -> u32 {
        let total = self.channels.values().filter(|c| c.guild_id == guild_id).map(|c| self.mentions(c.id)).sum::<u32>();
        total.min(MENTIONS_COUNTED_MAX)
    }

    /// For the window title: unread mentions everywhere, and whether anything is unread.
    pub fn unread_summary(&self) -> (u32, bool) {
        let mentions = self.channels.keys().map(|id| self.mentions(*id)).sum::<u32>().min(MENTIONS_COUNTED_MAX);
        (mentions, self.channels.keys().any(|id| self.is_unread(*id)))
    }
}

/// Whether a message is for `me`: their name, an allowed `@everyone`, or a reply to them.
pub fn mentions(message: &Message, me: Snowflake) -> bool {
    is_for(message, &markup::parse(&message.content), me)
}

/// [`mentions`], with the message's markup already parsed.
pub fn is_for(message: &Message, blocks: &[Block], me: Snowflake) -> bool {
    if message.author.id == me {
        return false;
    }
    let (users, everyone) = markup::mentions(blocks);
    let answers_me = message.reply_to.as_ref().and_then(|r| r.author.as_ref()).is_some_and(|a| a.id == me);
    users.contains(&me) || (everyone && message.mention_everyone) || answers_me
}

fn sort_members(members: &mut [Member]) {
    members.sort_by_cached_key(|m| m.user.username.to_lowercase());
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use plume_lite_protocol::model::{ChannelKind, Replied, Status, User};

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
            reply_to: None,
            mention_everyone: false,
            reactions: vec![],
            pinned: false,
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
                    category_id: None,
                    overwrites: vec![],
                    last_message_id: None,
                }],
                categories: vec![],
                roles: vec![],
                my_roles: vec![],
                member_count: 2,
                voice_states: vec![],
            }],
            read_states: vec![],
            notification_settings: vec![],
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
        store.add_pending(CHANNEL, "n1".into(), "salut".into(), None);
        store.add_pending(CHANNEL, "n2".into(), "ça va ?".into(), Some(Snowflake(7)));
        store.fail_pending(CHANNEL, "n2");
        assert!(store.timeline(CHANNEL).unwrap().pending[1].failed);
        assert_eq!(store.retry_pending(CHANNEL, "n2"), Some(("ça va ?".into(), Some(Snowflake(7)))));

        store.apply(Event::MessageCreate(message(5, &user(1, "moi"), Some("n1"))), ME);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert_eq!(timeline.messages.len(), 1);
        assert_eq!(timeline.pending.len(), 1);
        assert_eq!(timeline.pending[0].nonce, "n2");
    }

    #[test]
    fn markup_is_parsed_once_and_replies_follow_what_they_answer() {
        let mut store = store();
        let alice = user(2, "alice");
        let original = Message { content: "**salut**".into(), ..message(1, &alice, None) };
        let replied = Replied { id: Snowflake(1), author: Some(alice.clone()), excerpt: "**salut**".into() };
        let reply = Message { reply_to: Some(replied), ..message(2, &user(1, "moi"), None) };
        store.add_history(CHANNEL, vec![original.clone(), reply], false);
        let bold = |store: &Store| match store.timeline(CHANNEL).unwrap().blocks(Snowflake(1)) {
            [Block::Paragraph(runs)] => runs[0].style.bold,
            other => panic!("{other:?}"),
        };
        assert!(bold(&store));

        store.apply(Event::MessageUpdate(Message { content: "re".into(), ..original }), ME);
        assert!(!bold(&store), "parsed again");
        assert_eq!(store.timeline(CHANNEL).unwrap().messages[1].reply_to.as_ref().unwrap().excerpt, "re");

        store.apply(Event::MessageDelete { id: Snowflake(1), channel_id: CHANNEL, guild_id: GUILD }, ME);
        let timeline = store.timeline(CHANNEL).unwrap();
        assert!(timeline.messages[0].reply_to.as_ref().unwrap().author.is_none(), "original deleted");
        assert!(timeline.blocks(Snowflake(1)).is_empty());
    }

    #[test]
    fn reactions_are_sets() {
        let mut store = store();
        let alice = user(2, "alice");
        store.add_history(CHANNEL, vec![message(1, &alice, None)], false);
        let event = |user: i64, add: bool| {
            let (guild_id, channel_id, message_id, user_id, emoji) =
                (GUILD, CHANNEL, Snowflake(1), Snowflake(user), "👍".to_owned());
            if add {
                Event::ReactionAdd { guild_id, channel_id, message_id, user_id, emoji }
            } else {
                Event::ReactionRemove { guild_id, channel_id, message_id, user_id, emoji }
            }
        };
        store.apply(event(2, true), ME);
        store.apply(event(2, true), ME);
        store.set_reaction(CHANNEL, Snowflake(1), "👍", ME, true);
        let users = |store: &Store| store.message(CHANNEL, Snowflake(1)).unwrap().reactions.clone();
        assert_eq!(users(&store), [Reaction { emoji: "👍".into(), users: vec![Snowflake(2), ME] }]);
        store.apply(event(2, false), ME);
        store.apply(event(2, false), ME);
        store.set_reaction(CHANNEL, Snowflake(1), "👍", ME, false);
        assert!(users(&store).is_empty(), "an emptied reaction disappears");
    }

    #[test]
    fn unread_messages_and_mentions() {
        let mut store = store();
        let alice = user(2, "alice");
        let for_me = |id: i64| Message { content: format!("<@{}> salut", ME.0), ..message(id, &alice, None) };
        assert!(!store.is_unread(CHANNEL));

        store.apply(Event::MessageCreate(message(10, &alice, None)), ME);
        store.apply(Event::MessageCreate(for_me(11)), ME);
        store.apply(Event::MessageCreate(for_me(11)), ME);
        assert!(store.is_unread(CHANNEL));
        assert_eq!(
            (store.mentions(CHANNEL), store.unread_summary()),
            (1, (1, true)),
            "the same event twice counts once"
        );

        // Read on another device, then here.
        let read = |id: i64, mentions: u32| ReadState { channel_id: CHANNEL, last_read_id: Snowflake(id), mentions };
        store.apply(Event::ReadStateUpdate(read(11, 0)), ME);
        assert!(!store.is_unread(CHANNEL));
        store.apply(Event::ReadStateUpdate(read(10, 1)), ME);
        assert_eq!(store.mentions(CHANNEL), 0, "a late, older state changes nothing");
        assert_eq!(store.mark_read(CHANNEL), None, "nothing new to tell the server");

        store.apply(Event::MessageCreate(message(12, &alice, None)), ME);
        assert_eq!(store.mark_read(CHANNEL), Some(Snowflake(12)));
        store.apply(Event::MessageCreate(message(13, &alice, None)), ME);
        store.apply(Event::MessageCreate(message(14, &user(1, "moi"), None)), ME);
        assert!(!store.is_unread(CHANNEL), "writing means having read");
    }

    #[test]
    fn a_channel_setting_overrides_its_guild() {
        let mut store = store();
        assert_eq!(store.notification_level(CHANNEL), NotificationLevel::All, "the default");
        let set = |target_id, level| Event::NotificationSettingUpdate { target_id, level };
        store.apply(set(GUILD, Some(NotificationLevel::Mentions)), ME);
        assert_eq!(store.notification_level(CHANNEL), NotificationLevel::Mentions);
        store.apply(set(CHANNEL, Some(NotificationLevel::Nothing)), ME);
        store.apply(set(CHANNEL, Some(NotificationLevel::Nothing)), ME);
        assert_eq!(store.notification_level(CHANNEL), NotificationLevel::Nothing);
        store.apply(set(CHANNEL, None), ME);
        assert_eq!(store.notification_level(CHANNEL), NotificationLevel::Mentions, "back to the guild's");
    }

    #[test]
    fn deleting_a_guild_forgets_everything_about_it() {
        let mut store = store();
        store.add_history(CHANNEL, vec![message(1, &user(2, "alice"), None)], false);
        store.apply(Event::MessageCreate(message(2, &user(2, "alice"), None)), ME);
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
        let member = |id, name, status| Member { user: user(id, name), joined_at: Utc::now(), status, roles: vec![] };
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
                category_id: None,
                overwrites: vec![],
                last_message_id: None,
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

    #[test]
    fn layout_events_are_idempotent() {
        let mut store = store();
        let games = Category { id: Snowflake(40), guild_id: GUILD, name: "Jeux".into(), position: 0 };
        store.apply(Event::CategoryCreate(games.clone()), ME);
        store.apply(Event::CategoryCreate(games.clone()), ME);
        assert_eq!(store.categories_of(GUILD).len(), 1);

        // A message arrives, then the channel moves into the category: still unread.
        store.apply(Event::MessageCreate(message(100, &user(2, "bob"), None)), ME);
        let moved = Channel {
            id: CHANNEL,
            guild_id: GUILD,
            name: "salon".into(),
            kind: ChannelKind::Text,
            position: 0,
            category_id: Some(games.id),
            overwrites: vec![],
            last_message_id: None,
        };
        store.apply(Event::ChannelUpdate(moved.clone()), ME);
        store.apply(Event::ChannelUpdate(moved), ME);
        assert_eq!(store.channel(CHANNEL).unwrap().name, "salon");
        assert!(store.is_unread(CHANNEL), "the update kept the last message");
        let sections = store.sections(GUILD);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].channels.is_empty());
        assert_eq!(sections[1].category.map(|c| c.id), Some(games.id));
        assert_eq!(sections[1].channels[0].id, CHANNEL);

        // Deleting the category lists the channel above every category again.
        store.apply(Event::CategoryDelete { id: games.id, guild_id: GUILD }, ME);
        store.apply(Event::CategoryDelete { id: games.id, guild_id: GUILD }, ME);
        assert_eq!(store.channel(CHANNEL).unwrap().category_id, None);
        assert_eq!(store.sections(GUILD).len(), 1);

        // Renamed, then handed to someone else.
        let guild = Guild { id: GUILD, name: "Rust France".into(), owner_id: Snowflake(2) };
        store.apply(Event::GuildUpdate(guild.clone()), ME);
        assert_eq!(store.guild(GUILD), Some(&guild));
        // Events about guilds we are not in are ignored.
        store.apply(Event::GuildUpdate(Guild { id: Snowflake(99), ..guild }), ME);
        store.apply(Event::CategoryCreate(Category { guild_id: Snowflake(99), ..games }), ME);
        assert!(store.guild(Snowflake(99)).is_none());
        assert!(store.category(Snowflake(40)).is_none());
    }

    #[test]
    fn roles_follow_their_events() {
        use plume_lite_protocol::permissions::Permissions;
        let mut store = store();
        let bob = Snowflake(2);
        let role = |id: i64, position, color, permissions| Role {
            id: Snowflake(id),
            guild_id: GUILD,
            name: format!("r{id}"),
            color,
            position,
            permissions,
        };
        store.apply(Event::GuildRoleCreate(role(10, 0, Some(0xAA), Permissions::NONE)), ME);
        store.apply(Event::GuildRoleCreate(role(50, 1, Some(0x11), Permissions::KICK_MEMBERS)), ME);
        store.apply(Event::GuildRoleUpdate(role(50, 2, Some(0x11), Permissions::KICK_MEMBERS)), ME);
        store.apply(Event::GuildRoleCreate(role(60, 1, None, Permissions::BAN_MEMBERS)), ME);
        assert_eq!(store.roles_of(GUILD).iter().map(|r| r.id.0).collect::<Vec<_>>(), [50, 60, 10]);

        // Our roles, from the event (twice is once).
        let update =
            Event::GuildMemberUpdate { guild_id: GUILD, user_id: bob, roles: vec![Snowflake(50), Snowflake(60)] };
        store.apply(update.clone(), bob);
        store.apply(update, bob);
        let standing = store.standing(GUILD, bob, bob);
        assert!(standing.can(Permissions::KICK_MEMBERS) && standing.can(Permissions::BAN_MEMBERS));
        assert_eq!(standing.rank, 2);
        assert_eq!(store.role_color(GUILD, bob, bob), Some(0x11), "highest coloured role");

        // A deleted role is taken from everyone.
        store.apply(Event::GuildRoleDelete { guild_id: GUILD, id: Snowflake(50) }, bob);
        assert_eq!(store.member_roles(GUILD, bob, bob), Some([Snowflake(60)].as_slice()));
        assert_eq!(store.role_color(GUILD, bob, bob), None);
        assert!(!store.standing(GUILD, bob, bob).can(Permissions::KICK_MEMBERS));
        // The owner may do anything; others are `@everyone` until known.
        assert!(store.standing(GUILD, ME, bob).can(Permissions::MANAGE_ROLES));
        assert_eq!(store.standing(GUILD, Snowflake(3), bob).rank, 0);
    }

    #[test]
    fn rights_follow_the_guild_sent_again() {
        use plume_lite_protocol::permissions::{Overwrite, Permissions};
        let mut store = store();
        let bob = Snowflake(2);
        let everyone = Role {
            id: GUILD,
            guild_id: GUILD,
            name: "@everyone".into(),
            color: None,
            position: 0,
            permissions: Permissions::DEFAULT_EVERYONE,
        };
        store.apply(Event::GuildRoleCreate(everyone.clone()), bob);
        store.apply(Event::MessageCreate(message(100, &user(3, "carol"), None)), bob);
        assert!(store.channel_permissions(CHANNEL, bob).contains(Permissions::SEND_MESSAGES));

        // Read only for @everyone.
        let mut channel = store.channel(CHANNEL).unwrap().clone();
        channel.overwrites =
            vec![Overwrite { role_id: GUILD, allow: Permissions::NONE, deny: Permissions::SEND_MESSAGES }];
        store.apply(Event::ChannelUpdate(channel), bob);
        let rights = store.channel_permissions(CHANNEL, bob);
        assert!(rights.contains(Permissions::VIEW_CHANNEL) && !rights.contains(Permissions::SEND_MESSAGES));

        // The guild again, without the channel (no longer visible): it goes with its data.
        store.add_history(CHANNEL, vec![message(100, &user(3, "carol"), None)], false);
        let guild = FullGuild {
            guild: store.guild(GUILD).unwrap().clone(),
            channels: vec![],
            categories: vec![],
            roles: vec![everyone],
            my_roles: vec![],
            member_count: 2,
            voice_states: vec![],
        };
        store.apply(Event::GuildCreate(guild), bob);
        assert!(store.channel(CHANNEL).is_none() && store.timeline(CHANNEL).is_none());
        assert!(!store.is_unread(CHANNEL));
        assert_eq!(store.channel_permissions(CHANNEL, bob), Permissions::NONE);
    }
}
