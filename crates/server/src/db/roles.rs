use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::Role;
use plume_lite_protocol::permissions::{Permissions, Standing};
use sqlx::{SqliteConnection, SqlitePool};

use super::overwrites::MemberAccess;
use crate::error::Result;

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: Snowflake,
    guild_id: Snowflake,
    name: String,
    color: Option<i64>,
    position: i32,
    permissions: i64,
}

impl From<RoleRow> for Role {
    fn from(row: RoleRow) -> Self {
        Self {
            id: row.id,
            guild_id: row.guild_id,
            name: row.name,
            color: row.color.and_then(|c| u32::try_from(c).ok()),
            position: row.position,
            permissions: Permissions::known(row.permissions.cast_unsigned()),
        }
    }
}

fn bits(permissions: Permissions) -> i64 {
    permissions.0.cast_signed()
}

/// The `@everyone` role of a new guild (its id is the guild's).
pub async fn create_everyone(conn: &mut SqliteConnection, guild_id: Snowflake) -> Result<Role> {
    let row: RoleRow = sqlx::query_as(
        "INSERT INTO roles (id, guild_id, name, position, permissions) VALUES ($1, $1, '@everyone', 0, $2)
         RETURNING id, guild_id, name, color, position, permissions",
    )
    .bind(guild_id)
    .bind(bits(Permissions::DEFAULT_EVERYONE))
    .fetch_one(conn)
    .await?;
    Ok(row.into())
}

/// Inserts a role at the bottom (position 0, beside `@everyone`): the caller ranks it in the
/// same transaction ([`list`], [`save`]).
pub async fn create(
    conn: &mut SqliteConnection,
    id: Snowflake,
    guild_id: Snowflake,
    name: &str,
    color: Option<u32>,
    permissions: Permissions,
) -> Result<()> {
    sqlx::query("INSERT INTO roles (id, guild_id, name, color, position, permissions) VALUES ($1, $2, $3, $4, 0, $5)")
        .bind(id)
        .bind(guild_id)
        .bind(name)
        .bind(color)
        .bind(bits(permissions))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn list(conn: &mut SqliteConnection, guild_id: Snowflake) -> Result<Vec<Role>> {
    let rows: Vec<RoleRow> = sqlx::query_as(
        "SELECT id, guild_id, name, color, position, permissions FROM roles WHERE guild_id = $1 ORDER BY position, id",
    )
    .bind(guild_id)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(Role::from).collect())
}

pub async fn count(db: &SqlitePool, guild_id: Snowflake) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM roles WHERE guild_id = $1").bind(guild_id).fetch_one(db).await?)
}

/// The roles of several guilds (`guild_ids`: a JSON array), for READY.
pub async fn of_guilds(db: &SqlitePool, guild_ids: &str) -> Result<Vec<Role>> {
    let rows: Vec<RoleRow> = sqlx::query_as(
        "SELECT id, guild_id, name, color, position, permissions FROM roles
         WHERE guild_id IN (SELECT value FROM json_each($1)) ORDER BY position, id",
    )
    .bind(guild_ids)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Role::from).collect())
}

/// `(guild, role)` for each role of `user_id` in those guilds (`guild_ids`: a JSON array).
pub async fn of_member_in(db: &SqlitePool, guild_ids: &str, user_id: Snowflake) -> Result<Vec<(Snowflake, Snowflake)>> {
    Ok(sqlx::query_as(
        "SELECT guild_id, role_id FROM member_roles
         WHERE user_id = $2 AND guild_id IN (SELECT value FROM json_each($1))",
    )
    .bind(guild_ids)
    .bind(user_id)
    .fetch_all(db)
    .await?)
}

pub async fn of_member(conn: &mut SqliteConnection, guild_id: Snowflake, user_id: Snowflake) -> Result<Vec<Snowflake>> {
    Ok(sqlx::query_scalar("SELECT role_id FROM member_roles WHERE guild_id = $1 AND user_id = $2 ORDER BY role_id")
        .bind(guild_id)
        .bind(user_id)
        .fetch_all(conn)
        .await?)
}

/// Where a member stands (`None`: not a member), in one query: the guild's owner and the
/// permissions and positions of `@everyone` and of the member's roles.
pub async fn member(
    conn: &mut SqliteConnection,
    guild_id: Snowflake,
    user_id: Snowflake,
) -> Result<Option<MemberAccess>> {
    let rows: Vec<(Snowflake, Snowflake, i32, i64)> = sqlx::query_as(
        "SELECT g.owner_id, r.id, r.position, r.permissions
         FROM guild_members m
         JOIN guilds g ON g.id = m.guild_id
         JOIN roles r ON r.guild_id = m.guild_id
         WHERE m.guild_id = $1 AND m.user_id = $2
           AND (r.id = $1 OR r.id IN (SELECT role_id FROM member_roles WHERE guild_id = $1 AND user_id = $2))",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    let Some(owner_id) = rows.first().map(|row| row.0) else { return Ok(None) };
    let roles: Vec<Role> = rows
        .iter()
        .map(|(_, id, position, permissions)| Role {
            id: *id,
            guild_id,
            name: String::new(),
            color: None,
            position: *position,
            permissions: Permissions::known(permissions.cast_unsigned()),
        })
        .collect();
    let ids: Vec<Snowflake> = roles.iter().map(|r| r.id).filter(|id| *id != guild_id).collect();
    let standing = Standing::of(guild_id, owner_id == user_id, &roles, &ids);
    Ok(Some(MemberAccess { standing, roles: ids }))
}

/// [`member`], without the roles.
pub async fn standing(
    conn: &mut SqliteConnection,
    guild_id: Snowflake,
    user_id: Snowflake,
) -> Result<Option<Standing>> {
    Ok(member(conn, guild_id, user_id).await?.map(|m| m.standing))
}

/// Stores what differs between `before` and `after` (one statement); returns what changed.
pub async fn save(conn: &mut SqliteConnection, before: &[Role], after: &[Role]) -> Result<Vec<Role>> {
    let changed: Vec<Role> = after.iter().filter(|role| !before.contains(role)).cloned().collect();
    if !changed.is_empty() {
        let rows: Vec<_> = changed
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.0, "name": r.name, "color": r.color, "position": r.position, "permissions": bits(r.permissions),
                })
            })
            .collect();
        sqlx::query(
            "UPDATE roles
             SET name = r.value ->> 'name', color = r.value ->> 'color', position = r.value ->> 'position',
                 permissions = r.value ->> 'permissions'
             FROM json_each($1) AS r
             WHERE roles.id = r.value ->> 'id'",
        )
        .bind(serde_json::Value::from(rows).to_string())
        .execute(&mut *conn)
        .await?;
    }
    Ok(changed)
}

/// Its members lose it (`ON DELETE CASCADE`).
pub async fn delete(conn: &mut SqliteConnection, role_id: Snowflake) -> Result<()> {
    sqlx::query("DELETE FROM roles WHERE id = $1").bind(role_id).execute(conn).await?;
    Ok(())
}

/// Gives or takes a role; `false` when nothing changed.
pub async fn set_member_role(
    conn: &mut SqliteConnection,
    guild_id: Snowflake,
    user_id: Snowflake,
    role_id: Snowflake,
    on: bool,
) -> Result<bool> {
    let query = if on {
        "INSERT INTO member_roles (guild_id, user_id, role_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM member_roles WHERE guild_id = $1 AND user_id = $2 AND role_id = $3"
    };
    let changed = sqlx::query(query).bind(guild_id).bind(user_id).bind(role_id).execute(conn).await?.rows_affected();
    Ok(changed == 1)
}
