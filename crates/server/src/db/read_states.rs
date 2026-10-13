use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{MENTIONS_COUNTED_MAX, ReadState};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// The unread messages of channel `c.id` after `<last read>` that mention user `$1`: by name,
/// `@everyone` from the owner, or a reply to one of their messages. Expanded with `concat!` so
/// every query stays a `&'static str`.
macro_rules! unread_mentions {
    ($last_read:literal) => {
        concat!(
            "min(",
            99, // MENTIONS_COUNTED_MAX
            ", (SELECT count(*) FROM messages m
                WHERE m.channel_id = c.id AND m.id > ",
            $last_read,
            " AND m.author_id <> $1
                  AND (m.mention_everyone
                       OR EXISTS (SELECT 1 FROM message_mentions mm WHERE mm.message_id = m.id AND mm.user_id = $1)
                       OR EXISTS (SELECT 1 FROM messages r WHERE r.id = m.reply_to_id AND r.author_id = $1))))"
        )
    };
}

/// The read states of the user's text channels: the ones they read, and the ones never read
/// that have messages (`last_read_id` 0). Unread mentions are counted in the same query.
pub async fn for_user(db: &SqlitePool, user_id: Snowflake) -> Result<Vec<ReadState>> {
    debug_assert_eq!(MENTIONS_COUNTED_MAX, 99);
    let rows: Vec<(Snowflake, i64, i64)> = sqlx::query_as(concat!(
        "SELECT c.id, coalesce(rs.last_read_id, 0), ",
        unread_mentions!("coalesce(rs.last_read_id, 0)"),
        " FROM guild_members gm
         JOIN channels c ON c.guild_id = gm.guild_id AND c.kind = 0
         LEFT JOIN read_states rs ON rs.user_id = $1 AND rs.channel_id = c.id
         WHERE gm.user_id = $1
           AND (rs.last_read_id IS NOT NULL OR EXISTS (SELECT 1 FROM messages m WHERE m.channel_id = c.id))"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(read_state).collect())
}

/// One channel's read state, `None` if never read.
pub async fn get(db: &SqlitePool, user_id: Snowflake, channel_id: Snowflake) -> Result<Option<ReadState>> {
    let row: Option<(Snowflake, i64, i64)> = sqlx::query_as(concat!(
        "SELECT c.id, rs.last_read_id, ",
        unread_mentions!("rs.last_read_id"),
        " FROM read_states rs JOIN channels c ON c.id = rs.channel_id
         WHERE rs.user_id = $1 AND rs.channel_id = $2"
    ))
    .bind(user_id)
    .bind(channel_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(read_state))
}

fn read_state((channel_id, last_read_id, mentions): (Snowflake, i64, i64)) -> ReadState {
    ReadState { channel_id, last_read_id: Snowflake(last_read_id), mentions: mentions.clamp(0, 99) as u32 }
}

/// The user read the channel up to `message_id`. Never goes backwards (an old device catching
/// up must not make read messages unread again). Returns `true` if it moved.
pub async fn advance(
    conn: &mut SqliteConnection,
    user_id: Snowflake,
    channel_id: Snowflake,
    message_id: Snowflake,
) -> Result<bool> {
    let changed = sqlx::query(
        "INSERT INTO read_states (user_id, channel_id, last_read_id) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, channel_id) DO UPDATE SET last_read_id = excluded.last_read_id
         WHERE excluded.last_read_id > read_states.last_read_id",
    )
    .bind(user_id)
    .bind(channel_id)
    .bind(message_id)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

/// A new member starts with the guild's history read.
pub async fn catch_up(conn: &mut SqliteConnection, guild_id: Snowflake, user_id: Snowflake) -> Result<()> {
    sqlx::query(
        "INSERT INTO read_states (user_id, channel_id, last_read_id)
         SELECT $2, c.id, (SELECT max(m.id) FROM messages m WHERE m.channel_id = c.id)
         FROM channels c
         WHERE c.guild_id = $1 AND EXISTS (SELECT 1 FROM messages m WHERE m.channel_id = c.id)
         ON CONFLICT (user_id, channel_id) DO NOTHING",
    )
    .bind(guild_id)
    .bind(user_id)
    .execute(conn)
    .await?;
    Ok(())
}
