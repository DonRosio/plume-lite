use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Message, MessageKind, Replied, User};
use plume_lite_protocol::validate::PINS_PER_CHANNEL_MAX;
use sqlx::{SqliteConnection, SqlitePool};

use super::{BEGIN_WRITE, reactions};
use crate::error::Result;

#[derive(sqlx::FromRow)]
pub struct MessageRow {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub author_id: Snowflake,
    pub author_username: String,
    pub content: String,
    pub kind: i64,
    pub edited_at: Option<DateTime<Utc>>,
    pub mention_everyone: bool,
    pub pinned: bool,
    pub reply_to_id: Option<Snowflake>,
    /// The answered message, `None` once deleted.
    pub reply_author_id: Option<Snowflake>,
    pub reply_author_username: Option<String>,
    pub reply_excerpt: Option<String>,
}

impl MessageRow {
    /// Without its reactions: see [`with_reactions`].
    pub fn into_message(self, guild_id: Snowflake) -> Message {
        let reply_author = self.reply_author_id.zip(self.reply_author_username);
        Message {
            id: self.id,
            channel_id: self.channel_id,
            guild_id,
            author: User { id: self.author_id, username: self.author_username },
            content: self.content,
            kind: MessageKind::from_i64(self.kind),
            created_at: self.id.created_at(),
            edited_at: self.edited_at,
            reply_to: self.reply_to_id.map(|id| Replied {
                id,
                author: reply_author.map(|(id, username)| User { id, username }),
                excerpt: self.reply_excerpt.unwrap_or_default(),
            }),
            mention_everyone: self.mention_everyone,
            reactions: Vec::new(),
            pinned: self.pinned,
            nonce: None,
        }
    }
}

/// Messages with their reactions (one more query for all of them).
pub async fn with_reactions(db: &SqlitePool, rows: Vec<MessageRow>, guild_id: Snowflake) -> Result<Vec<Message>> {
    let ids: Vec<Snowflake> = rows.iter().map(|row| row.id).collect();
    let mut reactions = reactions::of_messages(db, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let id = row.id;
            Message { reactions: reactions.remove(&id).unwrap_or_default(), ..row.into_message(guild_id) }
        })
        .collect())
}

/// One message, as clients see it.
pub async fn full(
    db: &SqlitePool,
    channel_id: Snowflake,
    message_id: Snowflake,
    guild_id: Snowflake,
) -> Result<Option<Message>> {
    let Some(row) = get(db, channel_id, message_id).await? else { return Ok(None) };
    Ok(with_reactions(db, vec![row], guild_id).await?.pop())
}

/// Shared projection, expanded with `concat!` so every query stays a `&'static str`. The
/// answered message comes with the reply (200 = `REPLY_EXCERPT_MAX` characters of it).
macro_rules! select_messages {
    () => {
        "SELECT m.id, m.channel_id, m.author_id, u.username AS author_username, m.content, m.kind, m.edited_at,
                m.mention_everyone, m.pinned_at IS NOT NULL AS pinned, m.reply_to_id, r.author_id AS reply_author_id,
                ru.username AS reply_author_username, substr(r.content, 1, 200) AS reply_excerpt
         FROM messages m
         JOIN users u ON u.id = m.author_id
         LEFT JOIN messages r ON r.id = m.reply_to_id
         LEFT JOIN users ru ON ru.id = r.author_id "
    };
}

/// `WHERE` of a search: $1 the FTS5 query, $2 the guild, $3 the channel, $4 the author and
/// $5 the newest id (each optional but the first two).
macro_rules! search_filter {
    () => {
        "WHERE m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH $1)
           AND m.channel_id IN (SELECT value FROM json_each($2))
           AND ($3 IS NULL OR m.channel_id = $3)
           AND ($4 IS NULL OR m.author_id = $4)
           AND ($5 IS NULL OR m.id < $5)"
    };
}

pub struct NewMessage<'a> {
    pub id: Snowflake,
    pub guild_id: Snowflake,
    pub channel_id: Snowflake,
    pub author_id: Snowflake,
    pub content: &'a str,
    pub reply_to: Option<Snowflake>,
    pub mention_everyone: bool,
    /// Users written as `<@id>`; only the guild's members are recorded.
    pub mentions: &'a [Snowflake],
}

pub async fn insert(db: &SqlitePool, new: &NewMessage<'_>) -> Result<()> {
    let mut tx = db.begin_with(BEGIN_WRITE).await?;
    sqlx::query(
        "INSERT INTO messages (id, channel_id, author_id, content, reply_to_id, mention_everyone)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(new.id)
    .bind(new.channel_id)
    .bind(new.author_id)
    .bind(new.content)
    .bind(new.reply_to)
    .bind(new.mention_everyone)
    .execute(&mut *tx)
    .await?;
    set_mentions(&mut tx, new.guild_id, new.id, new.mentions).await?;
    // Writing in a channel means having read it.
    super::read_states::advance(&mut tx, new.author_id, new.channel_id, new.id).await?;
    tx.commit().await?;
    Ok(())
}

/// "X joined the guild", in its welcome channel, in the same transaction as the join. It
/// mentions nobody (nobody is notified more than by any message).
pub async fn insert_join(
    conn: &mut SqliteConnection,
    id: Snowflake,
    channel_id: Snowflake,
    author_id: Snowflake,
    content: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO messages (id, channel_id, author_id, content, kind) VALUES ($1, $2, $3, $4, 1)")
        .bind(id)
        .bind(channel_id)
        .bind(author_id)
        .bind(content)
        .execute(conn)
        .await?;
    Ok(())
}

/// Replaces who a message mentions with the members of `guild_id` among `users`.
async fn set_mentions(
    conn: &mut SqliteConnection,
    guild_id: Snowflake,
    message_id: Snowflake,
    users: &[Snowflake],
) -> Result<()> {
    sqlx::query("DELETE FROM message_mentions WHERE message_id = $1").bind(message_id).execute(&mut *conn).await?;
    if users.is_empty() {
        return Ok(());
    }
    // SQLite has no array type: the ids travel as one JSON array, expanded by `json_each`.
    let ids: Vec<i64> = users.iter().map(|id| id.0).collect();
    let ids = serde_json::to_string(&ids).map_err(anyhow::Error::from)?;
    sqlx::query(
        "INSERT INTO message_mentions (message_id, user_id)
         SELECT $1, user_id FROM guild_members
         WHERE guild_id = $2 AND user_id IN (SELECT value FROM json_each($3))",
    )
    .bind(message_id)
    .bind(guild_id)
    .bind(&ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum Cursor {
    Latest,
    Before(Snowflake),
    After(Snowflake),
}

/// A page of messages, always returned oldest → newest. Each variant is a single
/// range scan on `messages_channel_id_id_idx`.
pub async fn page(db: &SqlitePool, channel_id: Snowflake, cursor: Cursor, limit: u32) -> Result<Vec<MessageRow>> {
    let limit = i64::from(limit);
    let mut rows: Vec<MessageRow> = match cursor {
        Cursor::Latest | Cursor::Before(_) => {
            let before = match cursor {
                Cursor::Before(id) => id.0,
                _ => i64::MAX,
            };
            sqlx::query_as(concat!(
                select_messages!(),
                "WHERE m.channel_id = $1 AND m.id < $2 ORDER BY m.id DESC LIMIT $3"
            ))
            .bind(channel_id)
            .bind(before)
            .bind(limit)
            .fetch_all(db)
            .await?
        }
        Cursor::After(after) => {
            sqlx::query_as(concat!(
                select_messages!(),
                "WHERE m.channel_id = $1 AND m.id > $2 ORDER BY m.id ASC LIMIT $3"
            ))
            .bind(channel_id)
            .bind(after)
            .bind(limit)
            .fetch_all(db)
            .await?
        }
    };
    if !matches!(cursor, Cursor::After(_)) {
        rows.reverse();
    }
    Ok(rows)
}

pub async fn get(db: &SqlitePool, channel_id: Snowflake, message_id: Snowflake) -> Result<Option<MessageRow>> {
    Ok(sqlx::query_as(concat!(select_messages!(), "WHERE m.id = $1 AND m.channel_id = $2"))
        .bind(message_id)
        .bind(channel_id)
        .fetch_optional(db)
        .await?)
}

/// New content, and so new mentions. Returns `false` if the message is gone.
pub async fn update_content(
    db: &SqlitePool,
    guild_id: Snowflake,
    message_id: Snowflake,
    content: &str,
    mention_everyone: bool,
    mentions: &[Snowflake],
) -> Result<bool> {
    let mut tx = db.begin_with(BEGIN_WRITE).await?;
    let updated =
        sqlx::query("UPDATE messages SET content = $2, mention_everyone = $3, edited_at = unixepoch() WHERE id = $1")
            .bind(message_id)
            .bind(content)
            .bind(mention_everyone)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if updated == 0 {
        return Ok(false);
    }
    set_mentions(&mut tx, guild_id, message_id, mentions).await?;
    tx.commit().await?;
    Ok(true)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pin {
    Changed,
    /// Already pinned (or not), or not a message of this channel.
    Unchanged,
    /// [`PINS_PER_CHANNEL_MAX`] already.
    Full,
}

/// Pins or unpins a message of `channel_id`.
pub async fn set_pinned(db: &SqlitePool, channel_id: Snowflake, message_id: Snowflake, pinned: bool) -> Result<Pin> {
    let mut tx = db.begin_with(BEGIN_WRITE).await?;
    if pinned {
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM messages WHERE channel_id = $1 AND pinned_at IS NOT NULL")
                .bind(channel_id)
                .fetch_one(&mut *tx)
                .await?;
        if count >= PINS_PER_CHANNEL_MAX {
            return Ok(Pin::Full);
        }
    }
    let changed = sqlx::query(
        "UPDATE messages SET pinned_at = CASE WHEN $3 THEN unixepoch() END
         WHERE id = $1 AND channel_id = $2 AND (pinned_at IS NOT NULL) <> $3",
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(pinned)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(if changed == 1 { Pin::Changed } else { Pin::Unchanged })
}

/// The pinned messages of a channel, most recently pinned first.
pub async fn pinned(db: &SqlitePool, channel_id: Snowflake) -> Result<Vec<MessageRow>> {
    Ok(sqlx::query_as(concat!(
        select_messages!(),
        "WHERE m.channel_id = $1 AND m.pinned_at IS NOT NULL ORDER BY m.pinned_at DESC, m.id DESC LIMIT $2"
    ))
    .bind(channel_id)
    .bind(PINS_PER_CHANNEL_MAX)
    .fetch_all(db)
    .await?)
}

pub async fn delete(db: &SqlitePool, message_id: Snowflake) -> Result<bool> {
    let deleted = sqlx::query("DELETE FROM messages WHERE id = $1").bind(message_id).execute(db).await?.rows_affected();
    Ok(deleted == 1)
}

/// Filters of a search, within one guild's text channels.
pub struct Search<'a> {
    /// The text channels to search: those the user may see.
    pub channels: &'a [Snowflake],
    /// Words, each also matching longer words (prefix search).
    pub terms: &'a [String],
    pub channel_id: Option<Snowflake>,
    pub author_id: Option<Snowflake>,
    pub before: Option<Snowflake>,
    pub limit: u32,
}

/// One page of matches, newest first, and how many there are in all.
pub async fn search(db: &SqlitePool, search: &Search<'_>) -> Result<(u64, Vec<MessageRow>)> {
    // An FTS5 query of quoted words: `"ete"* "match"*`. The words only hold letters and
    // digits, so nothing in them can be read as query syntax.
    let query = search.terms.iter().map(|term| format!("\"{term}\"*")).collect::<Vec<_>>().join(" ");
    let channels = serde_json::to_string(search.channels).map_err(anyhow::Error::from)?;
    let (total,): (i64,) = sqlx::query_as(concat!("SELECT count(*) FROM messages m ", search_filter!()))
        .bind(&query)
        .bind(&channels)
        .bind(search.channel_id)
        .bind(search.author_id)
        .bind(None::<Snowflake>)
        .fetch_one(db)
        .await?;
    let rows = sqlx::query_as(concat!(select_messages!(), search_filter!(), " ORDER BY m.id DESC LIMIT $6"))
        .bind(&query)
        .bind(&channels)
        .bind(search.channel_id)
        .bind(search.author_id)
        .bind(search.before)
        .bind(i64::from(search.limit))
        .fetch_all(db)
        .await?;
    Ok((total.max(0) as u64, rows))
}
