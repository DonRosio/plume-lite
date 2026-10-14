use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Ban, User};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// Bans listed at most (the most recent first).
pub const LIST_MAX: i64 = 1000;

/// Bans a user (banning again only updates the reason), removes them from the guild and
/// deletes the invites they created there, so that none they handed out still works.
/// Returns whether they were a member.
pub async fn ban(
    conn: &mut SqliteConnection,
    guild_id: Snowflake,
    user_id: Snowflake,
    reason: Option<&str>,
    banned_by: Snowflake,
) -> Result<bool> {
    sqlx::query(
        "INSERT INTO bans (guild_id, user_id, reason, banned_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (guild_id, user_id) DO UPDATE SET reason = excluded.reason, banned_by = excluded.banned_by",
    )
    .bind(guild_id)
    .bind(user_id)
    .bind(reason)
    .bind(banned_by)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM invites WHERE guild_id = $1 AND inviter_id = $2")
        .bind(guild_id)
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    let removed = sqlx::query("DELETE FROM guild_members WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id)
        .bind(user_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(removed == 1)
}

pub async fn unban(db: &SqlitePool, guild_id: Snowflake, user_id: Snowflake) -> Result<bool> {
    let removed = sqlx::query("DELETE FROM bans WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id)
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(removed == 1)
}

pub async fn is_banned(conn: &mut SqliteConnection, guild_id: Snowflake, user_id: Snowflake) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM bans WHERE guild_id = $1 AND user_id = $2)")
        .bind(guild_id)
        .bind(user_id)
        .fetch_one(conn)
        .await?)
}

#[derive(sqlx::FromRow)]
struct BanRow {
    #[sqlx(flatten)]
    user: User,
    reason: Option<String>,
    created_at: DateTime<Utc>,
}

pub async fn list(db: &SqlitePool, guild_id: Snowflake) -> Result<Vec<Ban>> {
    let rows: Vec<BanRow> = sqlx::query_as(
        "SELECT u.id, u.username, b.reason, b.created_at
         FROM bans b JOIN users u ON u.id = b.user_id
         WHERE b.guild_id = $1
         ORDER BY b.created_at DESC, b.user_id
         LIMIT $2",
    )
    .bind(guild_id)
    .bind(LIST_MAX)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|row| Ban { user: row.user, reason: row.reason, created_at: row.created_at }).collect())
}
