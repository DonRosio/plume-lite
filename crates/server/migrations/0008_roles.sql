-- Plume Lite v0.6.1: roles and permissions.

-- A role is a set of permissions (bits of `protocol::permissions::Permissions`) given to
-- the members who have it. `@everyone`, whose id is the guild's, is every member's.
CREATE TABLE roles (
    id          INTEGER PRIMARY KEY,
    guild_id    INTEGER NOT NULL REFERENCES guilds (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    color       INTEGER CHECK (color BETWEEN 0 AND 16777215),    -- 0xRRGGBB, NULL = none
    position    INTEGER NOT NULL DEFAULT 0,                       -- @everyone 0, higher = stronger
    permissions INTEGER NOT NULL DEFAULT 0,
    UNIQUE (id, guild_id)                                         -- target of member_roles' key
) STRICT;
CREATE INDEX roles_guild_id_idx ON roles (guild_id, position);

-- Existing guilds: @everyone may create invites, as every member could until now.
INSERT INTO roles (id, guild_id, name, position, permissions) SELECT id, id, '@everyone', 0, 1 FROM guilds;

-- Who has which role. Both keys keep it consistent: leaving the guild drops one's roles, and a
-- role can only be given in its own guild.
CREATE TABLE member_roles (
    guild_id INTEGER NOT NULL,
    user_id  INTEGER NOT NULL,
    role_id  INTEGER NOT NULL,
    PRIMARY KEY (guild_id, user_id, role_id),
    FOREIGN KEY (guild_id, user_id) REFERENCES guild_members (guild_id, user_id) ON DELETE CASCADE,
    FOREIGN KEY (role_id, guild_id) REFERENCES roles (id, guild_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX member_roles_role_id_idx ON member_roles (role_id, guild_id);  -- cascades from roles
