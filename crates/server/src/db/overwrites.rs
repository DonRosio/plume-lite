//! Per-channel rights, and who may see a channel. Everything that shows a channel or its
//! content to someone goes through [`super::channels::access`] (one person) or [`viewers`]
//! (the people an event is for), both computed with `protocol::permissions`.

use std::collections::HashMap;

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::Channel;
use plume_lite_protocol::permissions::{Overwrite, Permissions, Standing, channel_permissions};
use sqlx::SqliteConnection;

use crate::error::Result;

type Row = (Snowflake, Snowflake, i64, i64);

fn overwrite((_, role_id, allow, deny): Row) -> Overwrite {
    Overwrite {
        role_id,
        allow: Permissions::known(allow.cast_unsigned()),
        deny: Permissions::known(deny.cast_unsigned()),
    }
}

/// Fills in the overwrites of these channels, in one query.
pub async fn attach(conn: &mut SqliteConnection, channels: &mut [Channel]) -> Result<()> {
    if channels.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = channels.iter().map(|c| c.id.0).collect();
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT channel_id, role_id, allow, deny FROM channel_overwrites
         WHERE channel_id IN (SELECT value FROM json_each($1)) ORDER BY channel_id, role_id",
    )
    .bind(serde_json::to_string(&ids).map_err(anyhow::Error::from)?)
    .fetch_all(conn)
    .await?;
    let mut by_channel: HashMap<Snowflake, Vec<Overwrite>> = HashMap::new();
    for row in rows {
        by_channel.entry(row.0).or_default().push(overwrite(row));
    }
    for channel in channels {
        channel.overwrites = by_channel.remove(&channel.id).unwrap_or_default();
    }
    Ok(())
}

/// Replaces all the overwrites of a channel.
pub async fn replace(
    conn: &mut SqliteConnection,
    channel_id: Snowflake,
    guild_id: Snowflake,
    overwrites: &[Overwrite],
) -> Result<()> {
    sqlx::query("DELETE FROM channel_overwrites WHERE channel_id = $1").bind(channel_id).execute(&mut *conn).await?;
    if overwrites.is_empty() {
        return Ok(());
    }
    let rows: Vec<_> = overwrites
        .iter()
        .map(|o| {
            serde_json::json!({
                "role": o.role_id.0, "allow": o.allow.0.cast_signed(), "deny": o.deny.0.cast_signed(),
            })
        })
        .collect();
    sqlx::query(
        "INSERT INTO channel_overwrites (channel_id, guild_id, role_id, allow, deny)
         SELECT $1, $2, r.value ->> 'role', r.value ->> 'allow', r.value ->> 'deny' FROM json_each($3) AS r",
    )
    .bind(channel_id)
    .bind(guild_id)
    .bind(serde_json::Value::from(rows).to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// A member: where they stand in the guild, and their roles.
#[derive(Debug, Clone)]
pub struct MemberAccess {
    pub standing: Standing,
    pub roles: Vec<Snowflake>,
}

impl MemberAccess {
    /// Their permissions in a channel of the guild.
    pub fn in_channel(&self, channel: &Channel) -> Permissions {
        channel_permissions(channel.guild_id, &self.standing, &self.roles, &channel.overwrites)
    }
}

/// Who, among `candidates` (members of the channel's guild), may see `channel` (its
/// overwrites attached). Three queries whatever the number of candidates.
pub async fn viewers(
    conn: &mut SqliteConnection,
    channel: &Channel,
    candidates: &[Snowflake],
) -> Result<Vec<Snowflake>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let guild_id = channel.guild_id;
    let owner_id: Option<Snowflake> = sqlx::query_scalar("SELECT owner_id FROM guilds WHERE id = $1")
        .bind(guild_id)
        .fetch_optional(&mut *conn)
        .await?;
    let roles = super::roles::list(&mut *conn, guild_id).await?;
    let ids: Vec<i64> = candidates.iter().map(|id| id.0).collect();
    let pairs: Vec<(Snowflake, Option<Snowflake>)> = sqlx::query_as(
        "SELECT m.user_id, r.role_id FROM guild_members m
         LEFT JOIN member_roles r ON r.guild_id = m.guild_id AND r.user_id = m.user_id
         WHERE m.guild_id = $1 AND m.user_id IN (SELECT value FROM json_each($2))",
    )
    .bind(guild_id)
    .bind(serde_json::to_string(&ids).map_err(anyhow::Error::from)?)
    .fetch_all(&mut *conn)
    .await?;
    let mut members: HashMap<Snowflake, Vec<Snowflake>> = HashMap::new();
    for (user_id, role_id) in pairs {
        members.entry(user_id).or_default().extend(role_id);
    }
    Ok(members
        .into_iter()
        .filter(|(user_id, member_roles)| {
            let standing = Standing::of(guild_id, owner_id == Some(*user_id), &roles, member_roles);
            channel_permissions(guild_id, &standing, member_roles, &channel.overwrites)
                .contains(Permissions::VIEW_CHANNEL)
        })
        .map(|(user_id, _)| user_id)
        .collect())
}
