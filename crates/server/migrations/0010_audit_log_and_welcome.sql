-- Plume Lite v0.6.3: audit log, welcome messages.

-- Where newcomers are announced, and what is said to them.
ALTER TABLE guilds ADD COLUMN welcome_channel_id INTEGER REFERENCES channels (id) ON DELETE SET NULL;
ALTER TABLE guilds ADD COLUMN welcome_message TEXT CHECK (length(welcome_message) BETWEEN 1 AND 500);

-- 0: written by its author; 1: "joined the guild", posted by the server in their name.
ALTER TABLE messages ADD COLUMN kind INTEGER NOT NULL DEFAULT 0 CHECK (kind BETWEEN 0 AND 1);

-- What moderators did, kept 90 days. Names are copied at the time (what was deleted stays
-- readable); message contents never are.
CREATE TABLE audit_log (
    id             INTEGER PRIMARY KEY,          -- a Snowflake: its date is encoded in it
    guild_id       INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    actor_id       INTEGER REFERENCES users (id) ON DELETE SET NULL,
    action         TEXT    NOT NULL CHECK (length(action) BETWEEN 1 AND 40),
    target_user_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
    target_id      INTEGER,                      -- channel, category, role or message
    channel_id     INTEGER,                      -- the channel concerned, to hide private ones
    target_name    TEXT,
    reason         TEXT,
    changes        TEXT                          -- JSON array of {field, before, after}
) STRICT;
CREATE INDEX audit_log_guild_id_idx ON audit_log (guild_id, id DESC);
CREATE INDEX audit_log_actor_id_idx ON audit_log (actor_id);              -- ON DELETE SET NULL
CREATE INDEX audit_log_target_user_id_idx ON audit_log (target_user_id);  -- ON DELETE SET NULL
