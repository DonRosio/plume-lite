-- Plume Lite v0.6.2: private channels and simplified per-channel rights.

-- "See channels" (1 << 10) and "send messages" (1 << 11) become permissions: every member
-- keeps them through @everyone.
UPDATE roles SET permissions = permissions | 3072 WHERE id = guild_id;

-- What a channel allows or denies to the members of a role (@everyone: the guild's id),
-- over their guild permissions. Both keys keep the channel and the role in the same guild.
CREATE UNIQUE INDEX channels_id_guild_id_idx ON channels (id, guild_id);
CREATE TABLE channel_overwrites (
    channel_id INTEGER NOT NULL,
    guild_id   INTEGER NOT NULL,
    role_id    INTEGER NOT NULL,
    allow      INTEGER NOT NULL DEFAULT 0,
    deny       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, role_id),
    FOREIGN KEY (channel_id, guild_id) REFERENCES channels (id, guild_id) ON DELETE CASCADE,
    FOREIGN KEY (role_id, guild_id) REFERENCES roles (id, guild_id) ON DELETE CASCADE,
    CHECK (allow & deny = 0)
) STRICT;
CREATE INDEX channel_overwrites_role_id_idx ON channel_overwrites (role_id, guild_id);  -- cascades from roles
