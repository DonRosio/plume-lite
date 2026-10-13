-- Plume Lite v0.5.3: search.
--
-- A full-text index of the messages (FTS5, "external content": the text itself stays in
-- `messages`, the index only holds what finds it). unicode61 with remove_diacritics 2 makes
-- searches insensitive to case and accents: "ete" finds "Été". A virtual table cannot be
-- STRICT.
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    content = 'messages',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO messages_fts (messages_fts) VALUES ('rebuild');

-- Kept in step with the messages, including the ones deleted along with their channel,
-- guild or author.
CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER messages_fts_update AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO messages_fts (rowid, content) VALUES (new.id, new.content);
END;
