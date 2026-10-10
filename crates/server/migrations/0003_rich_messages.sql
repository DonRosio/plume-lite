-- Plume Lite v0.5: richer messages.
--
-- Messages may now be 4000 characters long, answer another message and mention people.
-- SQLite cannot change a CHECK constraint, so the table is rebuilt (no table references it
-- yet, and the snowflake ids keep their order).
CREATE TABLE messages_v5 (
    id               INTEGER PRIMARY KEY,         -- creation time is encoded in the snowflake
    channel_id       INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    author_id        INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    content          TEXT    NOT NULL CHECK (length(content) BETWEEN 1 AND 4000),
    edited_at        INTEGER,
    -- The message this one answers. No foreign key on purpose: once that message is deleted,
    -- the reply still says it answered something ("original message deleted").
    reply_to_id      INTEGER,
    -- Written by the guild owner with @everyone: every member is notified.
    mention_everyone INTEGER NOT NULL DEFAULT 0 CHECK (mention_everyone IN (0, 1))
) STRICT;
INSERT INTO messages_v5 (id, channel_id, author_id, content, edited_at)
    SELECT id, channel_id, author_id, content, edited_at FROM messages;
DROP TABLE messages;
ALTER TABLE messages_v5 RENAME TO messages;
-- The hot path: "the N messages of a channel before/after X" is a single index range scan.
CREATE INDEX messages_channel_id_id_idx ON messages (channel_id, id DESC);
CREATE INDEX messages_author_id_idx ON messages (author_id);  -- ON DELETE CASCADE from users

-- Who each message mentions (members of its guild only), for unread mentions and
-- notifications. Rewritten when the message is edited.
CREATE TABLE message_mentions (
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, user_id)
) STRICT;
CREATE INDEX message_mentions_user_id_idx ON message_mentions (user_id, message_id);
