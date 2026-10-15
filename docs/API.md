# Plume Lite API v1

All the structures are defined in the [`plume-lite-protocol`](../crates/protocol/src) crate: it is
the source of truth, and this document is its summary.

Error messages and the texts of the application are in French (they are shown to its users); the
fields, codes and events below are the contract.

## Conventions

- **Base:** `https://<host>/api/v1` — JSON in UTF-8.
- **Authentication:** `Authorization: Bearer <token>` (obtained at sign-up or login).
- **Identifiers:** 64-bit Snowflakes **serialised as strings** (`"233558986324967424"`), sortable
  chronologically; the creation date is encoded in them.
- **Dates:** RFC 3339 in UTC.
- **Errors:** always the same envelope; clients rely on `code`, never on `message`.

```json
{ "code": "rate_limited", "message": "Trop de requêtes, réessayez dans un instant.", "retry_after_ms": 840 }
```

| HTTP | `code` | Meaning |
|---|---|---|
| 400 | `bad_request` | Validation (the `message` can be shown as is) |
| 401 | `unauthorized` | Token missing, invalid or expired; wrong credentials |
| 403 | `forbidden` | Action not allowed: missing permission, member or role ranked too high, reserved to the owner, missing or invalid invitation code, disabled account, wrong current password, banned from the server |
| 404 | `not_found` | Nonexistent **or** not visible (non-member) |
| 409 | `conflict` | Username already taken |
| 429 | `rate_limited` | Limit reached; `Retry-After` header + `retry_after_ms` |
| 500 | `internal` | Server error (details only in the logs) |
| 503 | `unavailable` | Feature not enabled on this server (voice without LiveKit) |

**Rate limits:** 100 requests / 5 s / IP; sign-up, login, password change and ownership transfer
10 / min / IP; messages 5 / 5 s / user; typing 5 / 10 s / user.

## Endpoints

| Method | Path | Access | Body → Response |
|---|---|---|---|
| `GET` | `/instance` | public | → `{version, open_registration, client?: {version, download_path, downloads: [{platform: "windows" \| "linux", path, size, sha256, signature_path?}]}}`; `download_path` = the Windows version (clients ≤ 0.5.0); `signature_path` = the file's minisign signature |
| `POST` | `/auth/register` | public | `{username, password, invite_code?}` → **201** `{token, user}`; code required unless registration is open (**403** otherwise) |
| `POST` | `/auth/login` | public | `{username, password}` → `{token, user}`; **403** if the account is disabled |
| `POST` | `/auth/logout` | logged in | → **204** (revokes this token) |
| `GET` | `/users/@me` | logged in | → `User` |
| `GET` | `/users/@me/guilds` | logged in | → `[FullGuild]` |
| `PUT` | `/users/@me/password` | logged in | `{current_password, new_password}` → **204**; logs out the other devices |
| `GET` | `/users/@me/sessions` | logged in | → `[{id, device, created_at, last_seen_at, current}]`, the most recently used first |
| `DELETE` | `/users/@me/sessions/{id}` | logged in | → **204** (logs this device out); **404** if it is not one of their sessions |
| `POST` | `/guilds` | logged in | `{name}` → **201** `FullGuild` (with the "général" channel) |
| `GET` | `/guilds/{guild_id}` | member | → `FullGuild` |
| `PATCH` | `/guilds/{guild_id}` | manage the server | `{name?, welcome_channel_id?, welcome_message?}` → `Guild`; `welcome_channel_id`: a text channel the caller sees (`null`: no announcement); `welcome_message`: 500 characters at most (`null` or empty: "a rejoint le serveur."); broadcasts `GUILD_UPDATE` |
| `GET` | `/guilds/{guild_id}/audit-log?before=` | view the audit log | → `{entries: [AuditEntry], next?}`: what moderators did, newest first, 50 per page (`next` = the `before` of the next page); without the entries about a channel the reader cannot see; kept 90 days |
| `PUT` | `/guilds/{guild_id}/owner` | owner | `{user_id, password}` → `Guild`: the server goes to that member; the owner's password is asked again (**403** if wrong, same limit as login); **404** if not a member; broadcasts `GUILD_UPDATE` |
| `DELETE` | `/guilds/{guild_id}` | owner | → **204** (channels, messages, invitations deleted) |
| `GET` | `/guilds/{guild_id}/channels` | member | → `[Channel]` sorted by position |
| `POST` | `/guilds/{guild_id}/channels` | manage channels | `{name, kind?: "text" \| "voice", category_id?, private?}` → **201** `Channel` (normalised name), last of its kind in its category; **404** for a category of another server; `private`: hidden from `@everyone`, shown to the creator's highest role (**400** if they have none, except the owner) |
| `PATCH` | `/channels/{channel_id}` | manage channels | `{name?, category_id?, position?, overwrites?}` → `Channel`: rename; change category (`null`: none); `position` = rank among the channels of the same kind in its category (from 0; beyond: last); `overwrites` replaces the channel's rights (see below; only for `@everyone` and roles ranked below one's own, and only the permissions one has in that channel). Broadcasts `CHANNEL_UPDATE` to those who see the channel; those who no longer do receive `CHANNEL_DELETE` |
| `POST` | `/guilds/{guild_id}/categories` | manage channels | `{name}` → **201** `Category` (1-100 characters, kept as typed), after the others; 50 at most |
| `PATCH` | `/categories/{category_id}` | manage channels | `{name?, position?}` → `Category`; `position` = rank among the categories; its channels follow (`CHANNEL_UPDATE`) |
| `DELETE` | `/categories/{category_id}` | manage channels | → **204**; its channels stay, above the categories |
| `GET` | `/guilds/{guild_id}/members?after=&limit=` | member | → `[Member]` sorted by id (limit ≤ 1000) |
| `DELETE` | `/guilds/{guild_id}/members/@me` | member | → **204** leave (forbidden to the owner) |
| `DELETE` | `/guilds/{guild_id}/members/{user_id}` | kick ¹ | → **204** kick |
| `POST` | `/guilds/{guild_id}/roles` | manage roles | `{name, color?, permissions?}` → **201** `Role`, placed at the very bottom (position 1); only permissions one has; 50 roles at most |
| `PATCH` | `/guilds/{guild_id}/roles/{role_id}` | manage roles ² | `{name?, color?, permissions?, position?}` → `Role`; `color: null` = none; `position` = rank from 1 (beyond: at the very top), never at one's own rank or above; one only changes permissions one has. `@everyone` (id = the server's): only `permissions`. Broadcasts `GUILD_ROLE_UPDATE` for each role moved |
| `DELETE` | `/guilds/{guild_id}/roles/{role_id}` | manage roles ² | → **204**; its members lose it; **400** for `@everyone` |
| `PUT` / `DELETE` | `/guilds/{guild_id}/members/{user_id}/roles/{role_id}` | manage roles ¹ ² | → **204** (idempotent): give or take a role (oneself too); giving it requires having all its permissions; broadcasts `GUILD_MEMBER_UPDATE` |
| `GET` | `/guilds/{guild_id}/bans` | ban | → `[{user, reason?, created_at}]`, the most recent first (1,000 at most) |
| `PUT` | `/guilds/{guild_id}/bans/{user_id}` | ban ¹ | `{reason?}` (512 characters at most) → **204**: removed from the server if they were a member (even if they left before), their invitations to this server deleted; they can no longer join (**403** on the invitation). Banning again only changes the reason; **404** for an unknown user |
| `DELETE` | `/guilds/{guild_id}/bans/{user_id}` | ban | → **204**: they can come back with an invitation; **404** if they were not banned |
| `POST` | `/guilds/{guild_id}/invites` | create invitations | `{max_uses?, max_age_secs?}` → **201** `Invite` |
| `GET` | `/invites/{code}` | logged in | → `Invite` (preview: server + number of members) |
| `POST` | `/invites/{code}` | logged in | → `FullGuild` (join; idempotent); **403** for a banned user |
| `DELETE` | `/channels/{channel_id}` | manage channels | → **204** |
| `POST` | `/channels/{channel_id}/typing` | member | → **204** (broadcasts `TYPING_START`) |
| `PUT` | `/users/@me/notifications/{guild_id or channel_id}` | member | `{level: "all" \| "mentions" \| "nothing"}` → **204**; sends `NOTIFICATION_SETTING_UPDATE` to all the user's devices; **404** for an inaccessible server or channel |
| `DELETE` | `/users/@me/notifications/{guild_id or channel_id}` | member | → **204**: back to the default (all messages; for a channel, its server's setting) |
| `POST` | `/channels/{channel_id}/ack` | member | `{message_id}` → **204**: read up to that message (never backwards); sends `READ_STATE_UPDATE` to all the user's devices; **404** if the message is not of this channel. Writing a message counts as reading |
| `POST` | `/channels/{channel_id}/voice` | member | → `{url, token}`: LiveKit token (10 min) for this voice channel only; **400** for a text channel, **503** if voice is not enabled |
| `PATCH` | `/users/@me/voice` | logged in | `{self_mute, self_deaf}` → **204**; broadcasts `VOICE_STATE_UPDATE` if in voice |
| `PUT` | `/guilds/{guild_id}/voice/{user_id}/mute` | mute ¹ | `{muted}` → **204**; the member can no longer speak (LiveKit removes their right to publish) |
| `POST` | `/guilds/{guild_id}/voice/{user_id}/disconnect` | move ¹ | → **204**; **404** if not in voice |
| `POST` | `/guilds/{guild_id}/voice/{user_id}/move` | move ¹ | `{channel_id}` → **204**; sends `VOICE_MOVE` to the moved member only; **400** for a text channel, **404** if not in voice or if the channel is not of this server |
| `POST` | `/voice/webhook` | LiveKit | Joins and leaves; signed with the shared secret (`Authorization`: JWT whose `sha256` = the body's hash) |
| `GET` | `/channels/{channel_id}/messages?before=&after=&limit=` | member | → `[Message]` from oldest to newest (limit ≤ 100, default 50) |
| `POST` | `/channels/{channel_id}/messages` | member | `{content, reply_to?, nonce?}` → **201** `Message`; `reply_to`: a message of the same channel (**400** otherwise) |
| `PATCH` | `/channels/{channel_id}/messages/{message_id}` | author | `{content}` → `Message` |
| `DELETE` | `/channels/{channel_id}/messages/{message_id}` | author or manage messages | → **204** |
| `PUT` | `/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me` | member | → **204** (idempotent); broadcasts `REACTION_ADD`; **400** if it is not an emoji or if the message already has 20 different emojis; 10 reactions/5 s per user |
| `DELETE` | `/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me` | member | → **204** (idempotent); broadcasts `REACTION_REMOVE` |
| `GET` | `/guilds/{guild_id}/messages/search?q=&channel_id=&author_id=&before=` | member | → `{total, messages: [Message]}`: the messages of the text channels that contain every word of `q` (or a word that starts with it), insensitive to case and accents, from newest to oldest, 25 per page (`before` = next page); **400** without a word to search or beyond 200 characters; 10 searches/10 s per user |
| `GET` | `/channels/{channel_id}/pins` | member | → pinned `[Message]`, from the most recently pinned to the oldest (50 at most) |
| `PUT` / `DELETE` | `/channels/{channel_id}/pins/{message_id}` | manage messages | → **204** (idempotent); broadcasts `MESSAGE_UPDATE` (`pinned`); **400** beyond 50 pinned |

¹ On another member: only if they rank strictly lower (their highest role below ours), never on the
owner. ² Only a role ranked below our highest role. The owner has all the permissions; deleting or
transferring the server stays reserved to them.

Outside the API: `GET /health` (liveness), `GET /health/ready` (database reachable), `GET /` (home
page: how to install the client) and `GET /download/plume-lite.exe` (the Windows client, when the
server distributes one).

The client identifies itself by its `User-Agent` (`plume-lite-desktop/0.3.0 (windows)`), shown in
the list of devices. Registration codes cannot be obtained through the API: the administrator
creates them on the server (`plume-lite-server invite`, see `deploy/README.md`).

Validation rules (shared by client and server, `protocol::validate`): username 2-32 characters
`[A-Za-z0-9_.]`; password 8-128; server name 2-100; normalised channel name (`"Mon Salon"` →
`mon-salon`), ≤ 100; category name 1-100; ban reason ≤ 512; message 1-4000 characters after
`trim`, without control characters other than `\n` and `\t`.

**Roles and permissions** (`protocol::permissions`, the same computation for the server and the
client): `FullGuild` carries `roles` (`[{id, guild_id, name, color?, position, permissions}]`,
`@everyone` included, whose id is the server's) and `my_roles` (the user's roles); each `Member`
carries `roles`. A member's permissions are those of `@everyone` plus those of their roles;
`position` ranks the roles (`@everyone` 0, higher = stronger). `permissions` is an integer as a
decimal string (Discord's bits): create invitations 1, kick 2, ban 4, manage channels 16, manage
the server 32, view the audit log 128, manage messages 8192, mention @everyone 131072, mute
4194304, move 16777216, manage roles 268435456, see channels 1024, send messages 2048; unknown bits
are ignored. A new server, and the ones created before, give `@everyone` "create invitations", "see
channels" and "send messages".

**Per-channel rights and private channels**: each `Channel` carries `overwrites`
(`[{role_id, allow, deny}]`, at most one per role, `@everyone` included) that allow or deny "see
channels" and "send messages" over the server's permissions: first `@everyone`, then the member's
roles (a role that allows beats a role that denies); the owner sees everything. A channel one
cannot see does not exist for oneself: absent from `ready`, from `GUILD_CREATE` and from
`GET /guilds/{id}/channels`, **404** on all its routes (messages, pins, reactions, typing, reading,
voice, notification settings), absent from the search and from the unread markers, none of its
events (including `VOICE_STATE_UPDATE`). Without "send messages", **403** to write or to signal
typing. When roles or a channel's rights change what a member sees, they receive the server again
(`GUILD_CREATE`) or `CHANNEL_DELETE`, and leave the voice of a channel they no longer see.

**Audit log**: each `AuditEntry` carries `id` (its date), `action` (`guild_update`,
`ownership_transfer`, `channel_create` / `_update` / `_delete`, `category_…`, `role_…`,
`member_role_add` / `_remove`, `member_kick`, `member_ban`, `member_unban`, `message_delete`,
`message_pin` / `_unpin`, `voice_mute` / `_unmute` / `_disconnect` / `_move`, `invite_create`;
unknown: `unknown`), `actor?`, `target_user?`, `target_id?`, `target_name?` (the name at the time
of the action), `reason?` and `changes` (`[{field, before?, after?}]`). Never the content of a
deleted message nor an invitation code.

**Welcome**: `Guild` carries `welcome_channel_id?` and `welcome_message?`. When someone joins the
server, a `kind: "member_join"` message is posted in that channel in their name, with that text (or
"a rejoint le serveur."); it cannot be edited. Other messages have no `kind` (`default`).

**Channel order** (`protocol::layout`, the same for the server and the client): first the channels
without a category, then each category in order; within each, text channels before voice channels.
After every change, the server renumbers the channels' `position` in that order (0, 1, …) and the
categories' among themselves: sorting by `position` gives the displayed order, even for a client
that ignores categories. `FullGuild` carries `categories` (`[{id, guild_id, name, position}]`) and
each `Channel` its `category_id` (absent: none).

**Messages.** A `Message` carries `reply_to` when it answers another: `{id, author?, excerpt}`,
where `excerpt` is the beginning of the targeted message (200 characters); `author` is absent once
that message is deleted. `mention_everyone: true` when the author, who has the "mention @everyone"
permission, wrote `@everyone` (without it, it is only text). `reactions`: `[{emoji, users}]`, in
the order the emojis first appeared (a Unicode emoji, encoded in the URL); `pinned: true` for a
pinned message. The content follows a subset of Discord's Markdown, read the same way by the server
and the client (`protocol::markup`):

| Written | Rendered |
|---|---|
| `**bold**`, `*italic*` or `_italic_`, `__underlined__`, `~~strikethrough~~` | formatting (combinable) |
| `` `code` ``, ```` ```rust … ``` ```` | inline code, code block |
| `> quote`, `>>> ` (all the rest of the message) | quote |
| `# Title`, `## …`, `### …`, `- item` | headings, list |
| `\|\|spoiler\|\|` | hidden until clicked |
| `https://…` | clickable link |
| `<@id>`, `@everyone` | mention (the client shows `@name`) |
| `\*` | literal character |

### Example

```bash
TOKEN=$(curl -s localhost:8080/api/v1/auth/register -H 'content-type: application/json' \
  -d '{"username":"vincent","password":"a-long-password"}' | jq -r .token)
GUILD=$(curl -s localhost:8080/api/v1/guilds -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name":"Rust France"}')
CHANNEL=$(echo "$GUILD" | jq -r '.channels[0].id')
curl -s localhost:8080/api/v1/channels/$CHANNEL/messages -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"content":"Hello!"}'
```

## Real-time gateway

`GET /gateway` → WebSocket, JSON text frames of the form `{"op": "...", "d": ...}`.

| Direction | `op` | `d` |
|---|---|---|
| server → client | `hello` | `{heartbeat_interval_ms}` — sent as soon as the connection opens |
| client → server | `identify` | `{token}` — within 10 s |
| server → client | `ready` | `{user, guilds: [FullGuild]}` — complete state, resynchronisation point |
| client → server | `heartbeat` | — every `heartbeat_interval_ms` |
| server → client | `heartbeat_ack` | — |
| server → client | `dispatch` | `{t: "<EVENT>", d: …}` |

Events (`t`): `MESSAGE_CREATE`, `MESSAGE_UPDATE` (`Message`), `MESSAGE_DELETE`
(`{id, channel_id, guild_id}`), `CHANNEL_CREATE` (`Channel`), `CHANNEL_DELETE`
(`{id, guild_id}`), `GUILD_CREATE` (`FullGuild`: you joined or created a server), `GUILD_DELETE`
(`{id}`: deleted, left or kicked), `GUILD_MEMBER_ADD` (`{guild_id, member}`),
`GUILD_MEMBER_REMOVE` (`{guild_id, user}`: left, kicked or banned), `GUILD_UPDATE` (`Guild`:
renamed or transferred), `GUILD_MEMBER_UPDATE` (`{guild_id, user_id, roles}`: the complete list of
their roles), `GUILD_ROLE_CREATE` / `GUILD_ROLE_UPDATE` (`Role`), `GUILD_ROLE_DELETE`
(`{guild_id, id}`: its members lose it), `CHANNEL_UPDATE` (`Channel` without `last_message_id`:
keep the known one), `CATEGORY_CREATE` / `CATEGORY_UPDATE` (`Category`), `CATEGORY_DELETE`
(`{id, guild_id}`: its channels no longer have a category), `PRESENCE_UPDATE`
(`{user_id, status: "online" | "offline"}`), `TYPING_START` (`{guild_id, channel_id, user_id}`),
`VOICE_STATE_UPDATE` (`{guild_id, user_id, channel_id, self_mute, self_deaf, server_mute}`;
`channel_id: null` = left voice), `VOICE_MOVE` (`{guild_id, channel_id}`, sent to the moved member
only: if they are in voice in this server, their client joins `channel_id`), `REACTION_ADD` /
`REACTION_REMOVE` (`{guild_id, channel_id, message_id, user_id, emoji}`: to be applied as adding
or removing an element of a set), `READ_STATE_UPDATE` (`{channel_id, last_read_id, mentions}`,
sent to the user only, to be ignored if older than the known state),
`NOTIFICATION_SETTING_UPDATE` (`{target_id, level}`, `level: null` = back to the default; sent to
the user only). `ready` also carries `notification_settings` (`[{target_id, level}]`) and
`read_states`: for each text channel read, or never read but not empty (`last_read_id` 0), where
the user stopped and how many unread messages mention them (99 at most); each channel carries
`last_message_id`. In `ready` and `GUILD_CREATE`, each server also carries `voice_states`: who is
in which voice channel.

The sound does not go through this server: the client joins the channel's LiveKit room (named
after its id) with the token from `POST /channels/{id}/voice`, its identity being its user id.
LiveKit tells the server about joins and leaves (`/voice/webhook`), which broadcasts them.

Rules for clients:

1. Apply each event **idempotently** (it can arrive twice, and the REST response of a mutation
   races with its own event).
2. Ignore unknown frames or events (compatibility with newer servers).
3. Without a `heartbeat_ack` before the next beat: close and reconnect.
4. Reconnect with exponential backoff + jitter; each `ready` replaces the whole local state.

Close codes: `4002` invalid frame · `4003` identify missing or late ·
**`4004` token refused or revoked, checked at every beat (do not reconnect: back to the login
screen)** · `4005` duplicate identify · `4008` too many frames / client too slow · `4009` missed
heartbeat · `1012` server restart (reconnect).
