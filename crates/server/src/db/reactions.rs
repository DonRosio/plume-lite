use std::collections::HashMap;

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::Reaction;
use plume_lite_protocol::validate::REACTIONS_PER_MESSAGE_MAX;
use sqlx::SqlitePool;

use super::BEGIN_WRITE;
use crate::error::Result;

/// The reactions of these messages, each list in the order the emojis first appeared.
pub async fn of_messages(db: &SqlitePool, ids: &[Snowflake]) -> Result<HashMap<Snowflake, Vec<Reaction>>> {
    let mut found: HashMap<Snowflake, Vec<Reaction>> = HashMap::new();
    if ids.is_empty() {
        return Ok(found);
    }
    // SQLite has no array type: the ids travel as one JSON array, expanded by `json_each`.
    let ids: Vec<i64> = ids.iter().map(|id| id.0).collect();
    let ids = serde_json::to_string(&ids).map_err(anyhow::Error::from)?;
    let rows: Vec<(Snowflake, String, Snowflake)> = sqlx::query_as(
        "SELECT message_id, emoji, user_id FROM reactions
         WHERE message_id IN (SELECT value FROM json_each($1)) ORDER BY created_at, rowid",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    for (message_id, emoji, user_id) in rows {
        let reactions = found.entry(message_id).or_default();
        match reactions.iter_mut().find(|r| r.emoji == emoji) {
            Some(reaction) => reaction.users.push(user_id),
            None => reactions.push(Reaction { emoji, users: vec![user_id] }),
        }
    }
    Ok(found)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    Yes,
    /// The user had already chosen this emoji.
    Already,
    /// [`REACTIONS_PER_MESSAGE_MAX`] different emojis already.
    Full,
}

pub async fn add(db: &SqlitePool, message_id: Snowflake, user_id: Snowflake, emoji: &str) -> Result<Added> {
    let mut tx = db.begin_with(BEGIN_WRITE).await?;
    let (kinds, known): (i64, bool) = sqlx::query_as(
        "SELECT count(DISTINCT emoji), coalesce(max(emoji = $2), 0) FROM reactions WHERE message_id = $1",
    )
    .bind(message_id)
    .bind(emoji)
    .fetch_one(&mut *tx)
    .await?;
    if !known && kinds >= REACTIONS_PER_MESSAGE_MAX as i64 {
        return Ok(Added::Full);
    }
    let inserted =
        sqlx::query("INSERT INTO reactions (message_id, user_id, emoji) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(message_id)
            .bind(user_id)
            .bind(emoji)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    tx.commit().await?;
    Ok(if inserted == 1 { Added::Yes } else { Added::Already })
}

/// Returns `false` if the user had not chosen this emoji.
pub async fn remove(db: &SqlitePool, message_id: Snowflake, user_id: Snowflake, emoji: &str) -> Result<bool> {
    let removed = sqlx::query("DELETE FROM reactions WHERE message_id = $1 AND user_id = $2 AND emoji = $3")
        .bind(message_id)
        .bind(user_id)
        .bind(emoji)
        .execute(db)
        .await?
        .rows_affected();
    Ok(removed == 1)
}
