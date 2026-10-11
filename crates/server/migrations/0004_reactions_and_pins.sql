-- Plume Lite v0.5.1: reactions and pinned messages.

-- One row per (message, emoji, user): reacting twice with the same emoji changes nothing.
CREATE TABLE reactions (
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    emoji      TEXT    NOT NULL CHECK (length(CAST(emoji AS BLOB)) BETWEEN 1 AND 32),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (message_id, emoji, user_id)     -- also serves "the reactions of these messages"
) STRICT;
CREATE INDEX reactions_user_id_idx ON reactions (user_id);  -- ON DELETE CASCADE from users

-- Set by the guild owner. The partial index serves "the pinned messages of a channel".
ALTER TABLE messages ADD COLUMN pinned_at INTEGER;
CREATE INDEX messages_pinned_idx ON messages (channel_id, pinned_at) WHERE pinned_at IS NOT NULL;
