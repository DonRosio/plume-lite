use std::collections::HashMap;

use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Category, Channel, FullGuild, Guild, Role, User};
use sqlx::{SqliteConnection, SqlitePool};

use plume_lite_protocol::permissions::{Permissions, Standing, channel_permissions};

use crate::error::Result;
use crate::ids::IdGenerator;

pub const DEFAULT_CHANNEL: &str = "général";

/// Creates a guild with its owner as first member, its `@everyone` role and a default text
/// channel, atomically.
pub async fn create(db: &SqlitePool, ids: &IdGenerator, owner_id: Snowflake, name: &str) -> Result<FullGuild> {
    let mut tx = db.begin_with(super::BEGIN_WRITE).await?;
    let guild: Guild =
        sqlx::query_as("INSERT INTO guilds (id, name, owner_id) VALUES ($1, $2, $3) RETURNING id, name, owner_id, welcome_channel_id, welcome_message")
            .bind(ids.next())
            .bind(name)
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?;
    add_member(&mut tx, guild.id, owner_id).await?;
    let everyone = super::roles::create_everyone(&mut tx, guild.id).await?;
    let channel: Channel = sqlx::query_as(
        "INSERT INTO channels (id, guild_id, name, kind, position) VALUES ($1, $2, $3, 0, 0)
         RETURNING id, guild_id, name, kind, position, category_id",
    )
    .bind(ids.next())
    .bind(guild.id)
    .bind(DEFAULT_CHANNEL)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(FullGuild {
        guild,
        channels: vec![channel],
        categories: Vec::new(),
        roles: vec![everyone],
        my_roles: Vec::new(),
        member_count: 1,
        voice_states: Vec::new(),
    })
}

/// Every guild of a user, fully loaded (READY payload). 6 queries whatever the number of guilds.
pub async fn full_for_user(db: &SqlitePool, user_id: Snowflake) -> Result<Vec<FullGuild>> {
    let guilds: Vec<Guild> = sqlx::query_as(
        "SELECT g.id, g.name, g.owner_id, g.welcome_channel_id, g.welcome_message
         FROM guild_members m JOIN guilds g ON g.id = m.guild_id
         WHERE m.user_id = $1
         ORDER BY m.joined_at, g.id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    hydrate(db, guilds, user_id).await
}

/// A guild as `user_id` sees it (with their roles).
pub async fn full(db: &SqlitePool, guild_id: Snowflake, user_id: Snowflake) -> Result<Option<FullGuild>> {
    let guild: Option<Guild> =
        sqlx::query_as("SELECT id, name, owner_id, welcome_channel_id, welcome_message FROM guilds WHERE id = $1")
            .bind(guild_id)
            .fetch_optional(db)
            .await?;
    Ok(hydrate(db, guild.into_iter().collect(), user_id).await?.pop())
}

async fn hydrate(db: &SqlitePool, guilds: Vec<Guild>, user_id: Snowflake) -> Result<Vec<FullGuild>> {
    if guilds.is_empty() {
        return Ok(Vec::new());
    }
    // SQLite has no array type: the ids travel as one JSON array, expanded by `json_each`.
    let ids: Vec<i64> = guilds.iter().map(|g| g.id.0).collect();
    let ids = serde_json::to_string(&ids).map_err(anyhow::Error::from)?;
    let mut channels: Vec<Channel> = sqlx::query_as(
        "SELECT id, guild_id, name, kind, position, category_id,
                (SELECT max(m.id) FROM messages m WHERE m.channel_id = channels.id) AS last_message_id
         FROM channels
         WHERE guild_id IN (SELECT value FROM json_each($1)) ORDER BY position, id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    super::overwrites::attach(&mut *db.acquire().await?, &mut channels).await?;
    let categories: Vec<Category> = sqlx::query_as(
        "SELECT id, guild_id, name, position FROM categories
         WHERE guild_id IN (SELECT value FROM json_each($1)) ORDER BY position, id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    let roles = super::roles::of_guilds(db, &ids).await?;
    let my_roles = super::roles::of_member_in(db, &ids, user_id).await?;
    let counts: Vec<(Snowflake, i64)> = sqlx::query_as(
        "SELECT guild_id, count(*) FROM guild_members
             WHERE guild_id IN (SELECT value FROM json_each($1)) GROUP BY guild_id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;

    let counts: HashMap<_, _> = counts.into_iter().collect();
    let mut by_guild: HashMap<Snowflake, Vec<Channel>> = HashMap::new();
    for channel in channels {
        by_guild.entry(channel.guild_id).or_default().push(channel);
    }
    let mut roles_by_guild: HashMap<Snowflake, Vec<Role>> = HashMap::new();
    for role in roles {
        roles_by_guild.entry(role.guild_id).or_default().push(role);
    }
    let mut my_roles_by_guild: HashMap<Snowflake, Vec<Snowflake>> = HashMap::new();
    for (guild_id, role_id) in my_roles {
        my_roles_by_guild.entry(guild_id).or_default().push(role_id);
    }
    let mut categories_by_guild: HashMap<Snowflake, Vec<Category>> = HashMap::new();
    for category in categories {
        categories_by_guild.entry(category.guild_id).or_default().push(category);
    }
    Ok(guilds
        .into_iter()
        .map(|guild| {
            let roles = roles_by_guild.remove(&guild.id).unwrap_or_default();
            let my_roles = my_roles_by_guild.remove(&guild.id).unwrap_or_default();
            // Only the channels this user may see.
            let standing = Standing::of(guild.id, guild.owner_id == user_id, &roles, &my_roles);
            let mut channels = by_guild.remove(&guild.id).unwrap_or_default();
            channels.retain(|c| {
                channel_permissions(guild.id, &standing, &my_roles, &c.overwrites).contains(Permissions::VIEW_CHANNEL)
            });
            (guild, channels, roles, my_roles)
        })
        .map(|(guild, channels, roles, my_roles)| FullGuild {
            channels,
            categories: categories_by_guild.remove(&guild.id).unwrap_or_default(),
            roles,
            my_roles,
            member_count: counts.get(&guild.id).copied().unwrap_or(0),
            // In memory, not in the database: filled in by `Voice::fill`.
            voice_states: Vec::new(),
            guild,
        })
        .collect())
}

/// `Some(owner_id)` when `user_id` is a member of `guild_id`.
pub async fn membership(db: &SqlitePool, guild_id: Snowflake, user_id: Snowflake) -> Result<Option<Snowflake>> {
    Ok(sqlx::query_scalar(
        "SELECT g.owner_id FROM guilds g
         JOIN guild_members m ON m.guild_id = g.id AND m.user_id = $2
         WHERE g.id = $1",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// The guild alone, read in a write transaction.
pub async fn get(conn: &mut SqliteConnection, guild_id: Snowflake) -> Result<Option<Guild>> {
    Ok(sqlx::query_as("SELECT id, name, owner_id, welcome_channel_id, welcome_message FROM guilds WHERE id = $1")
        .bind(guild_id)
        .fetch_optional(conn)
        .await?)
}

/// Stores its name and welcome settings.
pub async fn update(conn: &mut SqliteConnection, guild: &Guild) -> Result<Option<Guild>> {
    Ok(sqlx::query_as(
        "UPDATE guilds SET name = $2, welcome_channel_id = $3, welcome_message = $4 WHERE id = $1
         RETURNING id, name, owner_id, welcome_channel_id, welcome_message",
    )
    .bind(guild.id)
    .bind(&guild.name)
    .bind(guild.welcome_channel_id)
    .bind(&guild.welcome_message)
    .fetch_optional(conn)
    .await?)
}

/// Hands a guild from `owner_id` to `new_owner_id`, in one statement: only if `owner_id` still
/// owns it and `new_owner_id` is a member at that very moment.
pub async fn transfer(
    db: &SqlitePool,
    guild_id: Snowflake,
    owner_id: Snowflake,
    new_owner_id: Snowflake,
) -> Result<Option<Guild>> {
    Ok(sqlx::query_as(
        "UPDATE guilds SET owner_id = $3
         WHERE id = $1 AND owner_id = $2
           AND EXISTS (SELECT 1 FROM guild_members WHERE guild_id = $1 AND user_id = $3)
         RETURNING id, name, owner_id, welcome_channel_id, welcome_message",
    )
    .bind(guild_id)
    .bind(owner_id)
    .bind(new_owner_id)
    .fetch_optional(db)
    .await?)
}

pub async fn count_for_user(db: &SqlitePool, user_id: Snowflake) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM guild_members WHERE user_id = $1").bind(user_id).fetch_one(db).await?)
}

pub async fn delete(db: &SqlitePool, guild_id: Snowflake) -> Result<()> {
    // Channels, messages, members and invites go with it (ON DELETE CASCADE).
    sqlx::query("DELETE FROM guilds WHERE id = $1").bind(guild_id).execute(db).await?;
    Ok(())
}

/// Returns `false` if the user was already a member.
pub async fn add_member(conn: &mut SqliteConnection, guild_id: Snowflake, user_id: Snowflake) -> Result<bool> {
    let inserted = sqlx::query("INSERT INTO guild_members (guild_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(guild_id)
        .bind(user_id)
        .execute(conn)
        .await?
        .rows_affected();
    Ok(inserted == 1)
}

pub async fn remove_member(db: &SqlitePool, guild_id: Snowflake, user_id: Snowflake) -> Result<bool> {
    let removed = sqlx::query("DELETE FROM guild_members WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id)
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(removed == 1)
}

#[derive(sqlx::FromRow)]
pub struct MemberRow {
    #[sqlx(flatten)]
    pub user: User,
    pub joined_at: DateTime<Utc>,
    /// Their role ids, as a JSON array.
    pub roles: String,
}

impl MemberRow {
    pub fn role_ids(&self) -> Vec<Snowflake> {
        serde_json::from_str(&self.roles).unwrap_or_default()
    }
}

pub async fn member(db: &SqlitePool, guild_id: Snowflake, user_id: Snowflake) -> Result<Option<MemberRow>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username, m.joined_at,
                (SELECT json_group_array(r.role_id) FROM member_roles r
                 WHERE r.guild_id = m.guild_id AND r.user_id = m.user_id) AS roles
         FROM guild_members m JOIN users u ON u.id = m.user_id
         WHERE m.guild_id = $1 AND m.user_id = $2",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// Keyset pagination on the primary key (no OFFSET: constant cost per page).
pub async fn members(
    db: &SqlitePool,
    guild_id: Snowflake,
    after: Option<Snowflake>,
    limit: u32,
) -> Result<Vec<MemberRow>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username, m.joined_at,
                (SELECT json_group_array(r.role_id) FROM member_roles r
                 WHERE r.guild_id = m.guild_id AND r.user_id = m.user_id) AS roles
         FROM guild_members m JOIN users u ON u.id = m.user_id
         WHERE m.guild_id = $1 AND m.user_id > $2
         ORDER BY m.user_id
         LIMIT $3",
    )
    .bind(guild_id)
    .bind(after.map_or(-1, |id| id.0))
    .bind(i64::from(limit))
    .fetch_all(db)
    .await?)
}
