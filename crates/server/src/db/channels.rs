use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Channel, ChannelKind};
use sqlx::SqlitePool;

use crate::error::Result;

pub async fn list(db: &SqlitePool, guild_id: Snowflake) -> Result<Vec<Channel>> {
    Ok(sqlx::query_as(
        "SELECT id, guild_id, name, kind, position FROM channels
         WHERE guild_id = $1 ORDER BY position, id",
    )
    .bind(guild_id)
    .fetch_all(db)
    .await?)
}

pub async fn count(db: &SqlitePool, guild_id: Snowflake) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM channels WHERE guild_id = $1").bind(guild_id).fetch_one(db).await?)
}

/// Appends a channel at the end of the guild's list.
pub async fn create(
    db: &SqlitePool,
    id: Snowflake,
    guild_id: Snowflake,
    name: &str,
    kind: ChannelKind,
) -> Result<Channel> {
    Ok(sqlx::query_as(
        "INSERT INTO channels (id, guild_id, name, kind, position)
         VALUES ($1, $2, $3, $4, (SELECT COALESCE(MAX(position) + 1, 0) FROM channels WHERE guild_id = $2))
         RETURNING id, guild_id, name, kind, position",
    )
    .bind(id)
    .bind(guild_id)
    .bind(name)
    .bind(kind)
    .fetch_one(db)
    .await?)
}

/// A channel seen by one of its guild's members.
#[derive(sqlx::FromRow)]
pub struct ChannelAccess {
    #[sqlx(flatten)]
    pub channel: Channel,
    pub owner_id: Snowflake,
}

/// Loads a channel *and* checks membership in one query. `None` for both
/// "unknown channel" and "not a member": we never reveal what exists elsewhere.
pub async fn access(db: &SqlitePool, channel_id: Snowflake, user_id: Snowflake) -> Result<Option<ChannelAccess>> {
    Ok(sqlx::query_as(
        "SELECT c.id, c.guild_id, c.name, c.kind, c.position, g.owner_id
         FROM channels c
         JOIN guilds g ON g.id = c.guild_id
         JOIN guild_members m ON m.guild_id = c.guild_id AND m.user_id = $2
         WHERE c.id = $1",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

pub async fn delete(db: &SqlitePool, channel_id: Snowflake) -> Result<()> {
    sqlx::query("DELETE FROM channels WHERE id = $1").bind(channel_id).execute(db).await?;
    Ok(())
}
