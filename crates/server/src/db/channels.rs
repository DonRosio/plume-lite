use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{Category, Channel, ChannelKind};
use plume_lite_protocol::permissions::Permissions;

use super::overwrites::MemberAccess;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// Every channel of a guild, with its overwrites: the caller keeps those the user may see.
pub async fn list(db: &SqlitePool, guild_id: Snowflake) -> Result<Vec<Channel>> {
    let mut conn = db.acquire().await?;
    let mut channels: Vec<Channel> = sqlx::query_as(
        "SELECT id, guild_id, name, kind, position, category_id,
                (SELECT max(m.id) FROM messages m WHERE m.channel_id = channels.id) AS last_message_id
         FROM channels
         WHERE guild_id = $1 ORDER BY position, id",
    )
    .bind(guild_id)
    .fetch_all(&mut *conn)
    .await?;
    super::overwrites::attach(&mut conn, &mut channels).await?;
    Ok(channels)
}

pub async fn count(db: &SqlitePool, guild_id: Snowflake) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM channels WHERE guild_id = $1").bind(guild_id).fetch_one(db).await?)
}

/// Inserts a channel at the very end; the caller then puts it in its place with the layout
/// ([`layout`], [`save_layout`]) in the same transaction.
pub async fn create(
    conn: &mut SqliteConnection,
    id: Snowflake,
    guild_id: Snowflake,
    name: &str,
    kind: ChannelKind,
    category_id: Option<Snowflake>,
) -> Result<Channel> {
    Ok(sqlx::query_as(
        "INSERT INTO channels (id, guild_id, name, kind, position, category_id)
         VALUES ($1, $2, $3, $4, (SELECT COALESCE(MAX(position) + 1, 0) FROM channels WHERE guild_id = $2), $5)
         RETURNING id, guild_id, name, kind, position, category_id",
    )
    .bind(id)
    .bind(guild_id)
    .bind(name)
    .bind(kind)
    .bind(category_id)
    .fetch_one(conn)
    .await?)
}

/// A channel seen by one of its guild's members.
pub struct ChannelAccess {
    /// With its overwrites.
    pub channel: Channel,
    pub owner_id: Snowflake,
    pub member: MemberAccess,
    /// What they may do in it.
    pub permissions: Permissions,
}

/// Loads a channel and checks that the user may see it. `None` for "unknown channel",
/// "not a member" and "may not see it" alike: we never reveal what exists elsewhere.
pub async fn access(db: &SqlitePool, channel_id: Snowflake, user_id: Snowflake) -> Result<Option<ChannelAccess>> {
    let mut conn = db.acquire().await?;
    let found: Option<(Snowflake, Channel)> = sqlx::query_as::<_, ChannelRow>(
        "SELECT c.id, c.guild_id, c.name, c.kind, c.position, c.category_id, g.owner_id
         FROM channels c
         JOIN guilds g ON g.id = c.guild_id
         JOIN guild_members m ON m.guild_id = c.guild_id AND m.user_id = $2
         WHERE c.id = $1",
    )
    .bind(channel_id)
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?
    .map(|row| (row.owner_id, row.channel));
    let Some((owner_id, channel)) = found else { return Ok(None) };
    let mut channels = [channel];
    super::overwrites::attach(&mut conn, &mut channels).await?;
    let [channel] = channels;
    let Some(member) = super::roles::member(&mut conn, channel.guild_id, user_id).await? else { return Ok(None) };
    let permissions = member.in_channel(&channel);
    if !permissions.contains(Permissions::VIEW_CHANNEL) {
        return Ok(None);
    }
    Ok(Some(ChannelAccess { channel, owner_id, member, permissions }))
}

#[derive(sqlx::FromRow)]
struct ChannelRow {
    #[sqlx(flatten)]
    channel: Channel,
    owner_id: Snowflake,
}

/// A channel with its overwrites, whoever asks (for working out who may see it).
pub async fn get(db: &SqlitePool, channel_id: Snowflake) -> Result<Option<Channel>> {
    let mut conn = db.acquire().await?;
    let channel: Option<Channel> =
        sqlx::query_as("SELECT id, guild_id, name, kind, position, category_id FROM channels WHERE id = $1")
            .bind(channel_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(channel) = channel else { return Ok(None) };
    let mut channels = [channel];
    super::overwrites::attach(&mut conn, &mut channels).await?;
    let [channel] = channels;
    Ok(Some(channel))
}

pub async fn delete(db: &SqlitePool, channel_id: Snowflake) -> Result<()> {
    sqlx::query("DELETE FROM channels WHERE id = $1").bind(channel_id).execute(db).await?;
    Ok(())
}

// --- Layout: categories and the order of channels -----------------------------------------------

/// Everything that decides how a guild's channels are listed (without `last_message_id`).
#[derive(Debug, Clone)]
pub struct Layout {
    pub categories: Vec<Category>,
    pub channels: Vec<Channel>,
}

/// Read inside the write transaction that changes it: nobody else can move things meanwhile.
/// Overwrites included, so that the `CHANNEL_UPDATE`s it leads to carry them.
pub async fn layout(conn: &mut SqliteConnection, guild_id: Snowflake) -> Result<Layout> {
    let categories = sqlx::query_as("SELECT id, guild_id, name, position FROM categories WHERE guild_id = $1")
        .bind(guild_id)
        .fetch_all(&mut *conn)
        .await?;
    let channels =
        sqlx::query_as("SELECT id, guild_id, name, kind, position, category_id FROM channels WHERE guild_id = $1")
            .bind(guild_id)
            .fetch_all(&mut *conn)
            .await?;
    let mut channels: Vec<Channel> = channels;
    super::overwrites::attach(&mut *conn, &mut channels).await?;
    Ok(Layout { categories, channels })
}

/// Stores what differs between `before` and `after` (names, ranks, categories of channels):
/// one statement per table whatever the number of rows. Returns what changed, to broadcast.
pub async fn save_layout(
    conn: &mut SqliteConnection,
    before: &Layout,
    after: &Layout,
) -> Result<(Vec<Channel>, Vec<Category>)> {
    let channels: Vec<Channel> =
        after.channels.iter().filter(|channel| !before.channels.contains(channel)).cloned().collect();
    let categories: Vec<Category> =
        after.categories.iter().filter(|category| !before.categories.contains(category)).cloned().collect();
    if !channels.is_empty() {
        let rows: Vec<_> = channels
            .iter()
            .map(|c| serde_json::json!({ "id": c.id.0, "name": c.name, "position": c.position, "category_id": c.category_id.map(|id| id.0) }))
            .collect();
        sqlx::query(
            "UPDATE channels
             SET name = r.value ->> 'name', position = r.value ->> 'position', category_id = r.value ->> 'category_id'
             FROM json_each($1) AS r
             WHERE channels.id = r.value ->> 'id'",
        )
        .bind(serde_json::Value::from(rows).to_string())
        .execute(&mut *conn)
        .await?;
    }
    if !categories.is_empty() {
        let rows: Vec<_> = categories
            .iter()
            .map(|c| serde_json::json!({ "id": c.id.0, "name": c.name, "position": c.position }))
            .collect();
        sqlx::query(
            "UPDATE categories
             SET name = r.value ->> 'name', position = r.value ->> 'position'
             FROM json_each($1) AS r
             WHERE categories.id = r.value ->> 'id'",
        )
        .bind(serde_json::Value::from(rows).to_string())
        .execute(&mut *conn)
        .await?;
    }
    Ok((channels, categories))
}

pub async fn count_categories(db: &SqlitePool, guild_id: Snowflake) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM categories WHERE guild_id = $1").bind(guild_id).fetch_one(db).await?)
}

/// Inserts a category at the very end (the caller renumbers in the same transaction).
pub async fn create_category(
    conn: &mut SqliteConnection,
    id: Snowflake,
    guild_id: Snowflake,
    name: &str,
) -> Result<Category> {
    Ok(sqlx::query_as(
        "INSERT INTO categories (id, guild_id, name, position)
         VALUES ($1, $2, $3, (SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE guild_id = $2))
         RETURNING id, guild_id, name, position",
    )
    .bind(id)
    .bind(guild_id)
    .bind(name)
    .fetch_one(conn)
    .await?)
}

/// A category seen by one of its guild's members (`None` otherwise, like [`access`]).
#[derive(sqlx::FromRow)]
pub struct CategoryAccess {
    #[sqlx(flatten)]
    pub category: Category,
    pub owner_id: Snowflake,
}

pub async fn category_access(
    db: &SqlitePool,
    category_id: Snowflake,
    user_id: Snowflake,
) -> Result<Option<CategoryAccess>> {
    Ok(sqlx::query_as(
        "SELECT k.id, k.guild_id, k.name, k.position, g.owner_id
         FROM categories k
         JOIN guilds g ON g.id = k.guild_id
         JOIN guild_members m ON m.guild_id = k.guild_id AND m.user_id = $2
         WHERE k.id = $1",
    )
    .bind(category_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

/// Its channels lose their category (`ON DELETE SET NULL`).
pub async fn delete_category(conn: &mut SqliteConnection, category_id: Snowflake) -> Result<()> {
    sqlx::query("DELETE FROM categories WHERE id = $1").bind(category_id).execute(conn).await?;
    Ok(())
}
