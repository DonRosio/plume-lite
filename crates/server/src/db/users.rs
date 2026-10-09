use chrono::{DateTime, Utc};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::User;
use sqlx::SqlitePool;

use crate::error::{AppError, Result};

pub async fn create(db: &SqlitePool, id: Snowflake, username: &str, password_hash: &str) -> Result<User> {
    sqlx::query_as::<_, User>(
        "INSERT INTO users (id, username, password_hash) VALUES ($1, $2, $3)
         RETURNING id, username",
    )
    .bind(id)
    .bind(username)
    .bind(password_hash)
    .fetch_one(db)
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
}

/// Case-insensitive: the `username` column is `COLLATE NOCASE`.
pub async fn credentials(db: &SqlitePool, username: &str) -> Result<Option<StoredCredentials>> {
    Ok(sqlx::query_as("SELECT id, username, password_hash FROM users WHERE username = $1")
        .bind(username)
        .fetch_optional(db)
        .await?)
}

pub async fn create_session(
    db: &SqlitePool,
    token_hash: &[u8],
    user_id: Snowflake,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(token_hash)
        .bind(user_id)
        .bind(expires_at.timestamp())
        .execute(db)
        .await?;
    Ok(())
}

/// Resolves a token hash to its user — one primary-key lookup per authenticated request.
pub async fn by_session(db: &SqlitePool, token_hash: &[u8]) -> Result<Option<User>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.username
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > unixepoch()",
    )
    .bind(token_hash)
    .fetch_optional(db)
    .await?)
}

pub async fn delete_session(db: &SqlitePool, token_hash: &[u8]) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(token_hash).execute(db).await?;
    Ok(())
}

pub async fn purge_expired_sessions(db: &SqlitePool) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM sessions WHERE expires_at <= unixepoch()").execute(db).await?.rows_affected())
}
