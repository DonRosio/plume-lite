-- Plume Lite v0.6.0: categories of channels, bans.

-- Headings that group a guild's channels. Not channels: nothing can be posted in them, and
-- the access checks of channels never see them.
CREATE TABLE categories (
    id       INTEGER PRIMARY KEY,
    guild_id INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    name     TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    position INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX categories_guild_id_idx ON categories (guild_id, position);

-- Deleting a category moves its channels out of it. The server checks that a channel's
-- category belongs to the same guild.
ALTER TABLE channels ADD COLUMN category_id INTEGER REFERENCES categories (id) ON DELETE SET NULL;
CREATE INDEX channels_category_id_idx ON channels (category_id);  -- ON DELETE SET NULL from categories

-- Banned users cannot join the guild again, whatever the invite.
CREATE TABLE bans (
    guild_id   INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    reason     TEXT    CHECK (length(reason) BETWEEN 1 AND 512),
    banned_by  INTEGER REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (guild_id, user_id)             -- also serves "the bans of a guild"
) STRICT;
CREATE INDEX bans_user_id_idx ON bans (user_id);      -- ON DELETE CASCADE from users
CREATE INDEX bans_banned_by_idx ON bans (banned_by);  -- ON DELETE SET NULL from users
