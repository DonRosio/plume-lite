use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Guild, Invite};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// Inserts an invite; `Ok(false)` on code collision (caller retries with a new code).
pub async fn create(
    db: &SqlitePool,
    code: &str,
    guild_id: Snowflake,
    inviter_id: Snowflake,
    max_uses: Option<i32>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO invites (code, guild_id, inviter_id, max_uses, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(code)
    .bind(guild_id)
    .bind(inviter_id)
    .bind(max_uses)
    .bind(expires_at.map(|at| at.timestamp()))
    .execute(db)
    .await;
    match result {
        Ok(_) => Ok(true),
        Err(err) if super::is_unique_violation(&err) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    code: String,
    uses: i32,
    max_uses: Option<i32>,
    expires_at: Option<DateTime<Utc>>,
    guild_id: Snowflake,
    guild_name: String,
    guild_owner_id: Snowflake,
    member_count: i64,
}

/// A *usable* invite (not expired, not exhausted).
pub async fn get(db: &SqlitePool, code: &str) -> Result<Option<Invite>> {
    let row: Option<InviteRow> = sqlx::query_as(
        "SELECT i.code, i.uses, i.max_uses, i.expires_at,
                g.id AS guild_id, g.name AS guild_name, g.owner_id AS guild_owner_id,
                (SELECT count(*) FROM guild_members gm WHERE gm.guild_id = g.id) AS member_count
         FROM invites i JOIN guilds g ON g.id = i.guild_id
         WHERE i.code = $1
           AND (i.expires_at IS NULL OR i.expires_at > unixepoch())
           AND (i.max_uses IS NULL OR i.uses < i.max_uses)",
    )
    .bind(code)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Invite {
        code: r.code,
        // The preview shows the guild, not its settings.
        guild: Guild {
            id: r.guild_id,
            name: r.guild_name,
            owner_id: r.guild_owner_id,
            welcome_channel_id: None,
            welcome_message: None,
        },
        member_count: r.member_count,
        uses: r.uses,
        max_uses: r.max_uses,
        expires_at: r.expires_at,
    }))
}

/// Atomically consumes one use. `None` if the invite became unusable in the meantime
/// (the conditional UPDATE is what makes `max_uses` race-free).
pub async fn redeem(conn: &mut SqliteConnection, code: &str) -> Result<Option<Snowflake>> {
    Ok(sqlx::query_scalar(
        "UPDATE invites SET uses = uses + 1
         WHERE code = $1
           AND (expires_at IS NULL OR expires_at > unixepoch())
           AND (max_uses IS NULL OR uses < max_uses)
         RETURNING guild_id",
    )
    .bind(code)
    .fetch_optional(conn)
    .await?)
}
