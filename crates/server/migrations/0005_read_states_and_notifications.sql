-- Plume Lite v0.5.2: unread messages remembered by the server, notification settings.

-- Where each user stopped reading each text channel. Kept in sync across their devices.
CREATE TABLE read_states (
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    channel_id   INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    last_read_id INTEGER NOT NULL,             -- a message id: everything up to it is read
    PRIMARY KEY (user_id, channel_id)
) STRICT;
CREATE INDEX read_states_channel_id_idx ON read_states (channel_id);  -- ON DELETE CASCADE from channels

-- Until now unread markers only lived in the client: start with everything read, rather than
-- every message ever written showing up as unread.
INSERT INTO read_states (user_id, channel_id, last_read_id)
    SELECT gm.user_id, c.id, (SELECT max(m.id) FROM messages m WHERE m.channel_id = c.id)
    FROM guild_members gm
    JOIN channels c ON c.guild_id = gm.guild_id
    WHERE EXISTS (SELECT 1 FROM messages m WHERE m.channel_id = c.id);

-- Which messages notify a user, per guild or per channel (a channel overrides its guild).
-- No row: every message (the default).
CREATE TABLE notification_settings (
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    guild_id   INTEGER REFERENCES guilds (id) ON DELETE CASCADE,
    channel_id INTEGER REFERENCES channels (id) ON DELETE CASCADE,
    level      INTEGER NOT NULL CHECK (level BETWEEN 0 AND 2),  -- 0 all, 1 mentions only, 2 nothing
    CHECK ((guild_id IS NULL) <> (channel_id IS NULL))
) STRICT;
CREATE UNIQUE INDEX notification_settings_target_idx ON notification_settings (user_id, coalesce(guild_id, channel_id));
CREATE INDEX notification_settings_guild_id_idx ON notification_settings (guild_id);      -- cascades
CREATE INDEX notification_settings_channel_id_idx ON notification_settings (channel_id);
