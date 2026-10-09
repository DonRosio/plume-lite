use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Message, User};
use sqlx::SqlitePool;

use crate::error::Result;

#[derive(sqlx::FromRow)]
pub struct MessageRow {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    pub author_id: Snowflake,
    pub author_username: String,
    pub content: String,
    pub edited_at: Option<DateTime<Utc>>,
}

impl MessageRow {
    pub fn into_message(self, guild_id: Snowflake) -> Message {
        Message {
            id: self.id,
            channel_id: self.channel_id,
            guild_id,
            author: User { id: self.author_id, username: self.author_username },
            content: self.content,
            created_at: self.id.created_at(),
            edited_at: self.edited_at,
            nonce: None,
        }
    }
}

/// Shared projection, expanded with `concat!` so every query stays a `&'static str`.
macro_rules! select_messages {
    () => {
        "SELECT m.id, m.channel_id, m.author_id, u.username AS author_username, m.content, m.edited_at
         FROM messages m JOIN users u ON u.id = m.author_id "
    };
}

pub async fn insert(
    db: &SqlitePool,
    id: Snowflake,
    channel_id: Snowflake,
    author_id: Snowflake,
    content: &str,
) -> Result<()> {
    sqlx::query("INSERT INTO messages (id, channel_id, author_id, content) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(channel_id)
        .bind(author_id)
        .bind(content)
        .execute(db)
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

pub async fn update_content(db: &SqlitePool, message_id: Snowflake, content: &str) -> Result<Option<MessageRow>> {
    Ok(sqlx::query_as(
        "UPDATE messages SET content = $2, edited_at = unixepoch() WHERE id = $1
         RETURNING id, channel_id, author_id,
                   (SELECT username FROM users WHERE users.id = messages.author_id) AS author_username,
                   content, edited_at",
    )
    .bind(message_id)
    .bind(content)
    .fetch_optional(db)
    .await?)
}

pub async fn delete(db: &SqlitePool, message_id: Snowflake) -> Result<bool> {
    let deleted = sqlx::query("DELETE FROM messages WHERE id = $1").bind(message_id).execute(db).await?.rows_affected();
    Ok(deleted == 1)
}
