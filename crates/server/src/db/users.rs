use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::User;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, Result};

pub async fn create(conn: &mut SqliteConnection, id: Snowflake, username: &str, password_hash: &str) -> Result<User> {
    sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, password_hash) VALUES ($1, $2, $3)
         RETURNING id, username",
    )
    .bind(id)
    .bind(username)
    .bind(password_hash)
    .fetch_one(conn)
    .await
    .map_err(|err| {
        if super::is_unique_violation(&err) {
            AppError::Conflict("Ce nom d'utilisateur est déjà pris.")
        } else {
            err.into()
        }
    })
}

#[derive(sqlx::FromRow)]
pub struct StoredCredentials {
    pub id: Snowflake,
    pub username: String,
    pub password_hash: String,
    pub disabled: bool,
}

/// Case-insensitive: the `username` column is `COLLATE NOCASE`.
pub async fn credentials(db: &SqlitePool, username: &str) -> Result<Option<StoredCredentials>> {
    Ok(sqlx::query_as(
        "SELECT id, username, password_hash, disabled_at IS NOT NULL AS disabled FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(db)
    .await?)
}

pub async fn get(db: &SqlitePool, user_id: Snowflake) -> Result<Option<User>> {
    Ok(sqlx::query_as("SELECT id, username FROM users WHERE id = $1").bind(user_id).fetch_optional(db).await?)
}

pub async fn by_username(db: &SqlitePool, username: &str) -> Result<Option<User>> {
    Ok(sqlx::query_as("SELECT id, username FROM users WHERE username = $1").bind(username).fetch_optional(db).await?)
}

pub async fn password_hash(db: &SqlitePool, user_id: Snowflake) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1").bind(user_id).fetch_optional(db).await?)
}

pub async fn set_password(db: &SqlitePool, user_id: Snowflake, password_hash: &str) -> Result<()> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind(password_hash)
        .execute(db)
        .await?;
    Ok(())
}

/// Disables or re-enables an account. Returns `false` if it was already in that state.
pub async fn set_disabled(db: &SqlitePool, user_id: Snowflake, disabled: bool) -> Result<bool> {
    let changed = sqlx::query(
        "UPDATE users SET disabled_at = CASE WHEN $2 THEN unixepoch() END
         WHERE id = $1 AND (disabled_at IS NOT NULL) <> $2",
    )
    .bind(user_id)
    .bind(disabled)
    .execute(db)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

/// One account, as listed by `plume-lite-server users`.
#[derive(sqlx::FromRow)]
pub struct AccountRow {
    pub username: String,
    pub created_at: DateTime<Utc>,
    pub disabled: bool,
    /// Last use of any of its sessions; `None` when logged out everywhere.
    pub last_seen_at: Option<DateTime<Utc>>,
    pub sessions: i64,
}

pub async fn accounts(db: &SqlitePool) -> Result<Vec<AccountRow>> {
    Ok(sqlx::query_as(
        "SELECT u.username, u.created_at, u.disabled_at IS NOT NULL AS disabled,
                max(s.last_seen_at) AS last_seen_at, count(s.id) AS sessions
         FROM users u LEFT JOIN sessions s ON s.user_id = u.id
         GROUP BY u.id
         ORDER BY u.username",
    )
    .fetch_all(db)
    .await?)
}

// --- Sessions -----------------------------------------------------------------------------------

pub async fn create_session(
    db: &SqlitePool,
    id: Snowflake,
    token_hash: &[u8],
    user_id: Snowflake,
    device: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("INSERT INTO sessions (id, token_hash, user_id, device, expires_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(token_hash)
        .bind(user_id)
        .bind(device)
        .bind(expires_at.timestamp())
        .execute(db)
        .await?;
    Ok(())
}

/// A valid session and its user.
#[derive(sqlx::FromRow)]
pub struct SessionUser {
    #[sqlx(flatten)]
    pub user: User,
    pub session_id: Snowflake,
    pub last_seen_at: DateTime<Utc>,
}

/// Resolves a token hash to its user — one unique-index lookup per authenticated request.
/// Expired sessions and disabled accounts resolve to nothing.
pub async fn by_session(db: &SqlitePool, token_hash: &[u8]) -> Result<Option<SessionUser>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username, s.id AS session_id, s.last_seen_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > unixepoch() AND u.disabled_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(db)
    .await?)
}

pub async fn touch_session(db: &SqlitePool, session_id: Snowflake) -> Result<()> {
    sqlx::query("UPDATE sessions SET last_seen_at = unixepoch() WHERE id = $1").bind(session_id).execute(db).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
pub struct SessionRow {
    pub id: Snowflake,
    pub device: String,
    pub last_seen_at: DateTime<Utc>,
}

/// The live sessions of a user, most recently used first.
pub async fn sessions(db: &SqlitePool, user_id: Snowflake) -> Result<Vec<SessionRow>> {
    Ok(sqlx::query_as(
        "SELECT id, device, last_seen_at FROM sessions
         WHERE user_id = $1 AND expires_at > unixepoch()
         ORDER BY last_seen_at DESC, id DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?)
}

pub async fn delete_session(db: &SqlitePool, token_hash: &[u8]) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(token_hash).execute(db).await?;
    Ok(())
}

/// Revokes one session of `user_id`. `false` if it does not exist or belongs to someone else.
pub async fn delete_session_of(db: &SqlitePool, user_id: Snowflake, session_id: Snowflake) -> Result<bool> {
    let deleted = sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2")
        .bind(session_id)
        .bind(user_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(deleted == 1)
}

/// Revokes every session of a user except `keep` (pass `None` to revoke them all).
pub async fn delete_sessions(db: &SqlitePool, user_id: Snowflake, keep: Option<Snowflake>) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id IS NOT $2")
        .bind(user_id)
        .bind(keep)
        .execute(db)
        .await?
        .rows_affected())
}

pub async fn purge_expired_sessions(db: &SqlitePool) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM sessions WHERE expires_at <= unixepoch()").execute(db).await?.rows_affected())
}
