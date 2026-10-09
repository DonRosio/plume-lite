-- Plume Lite v0.3: going online.
--
-- Sessions get an id (to list and revoke them one by one), the device that opened them
-- and the time of their last use. The old sessions are dropped: everyone logs in again
-- once.
DROP TABLE sessions;
CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY,           -- Snowflake: the creation time is encoded in it
    token_hash   BLOB    NOT NULL UNIQUE,       -- SHA-256 of the bearer token
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device       TEXT    NOT NULL DEFAULT '' CHECK (length(device) <= 200),  -- User-Agent
    last_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
    expires_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- Set by `plume-lite-server disable <name>`: the account can no longer log in.
ALTER TABLE users ADD COLUMN disabled_at INTEGER;

-- Codes the administrator hands out to let someone create an account
-- (`plume-lite-server invite`). Registration is closed without one.
CREATE TABLE registration_codes (
    code       TEXT    PRIMARY KEY,
    uses       INTEGER NOT NULL DEFAULT 0,
    max_uses   INTEGER NOT NULL CHECK (max_uses > 0),
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
