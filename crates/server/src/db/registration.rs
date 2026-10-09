//! Registration codes: created by the administrator, required to open an account.

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// Inserts a code; `Ok(false)` on collision (the caller retries with a new code).
pub async fn create(db: &SqlitePool, code: &str, max_uses: i64, expires_at: DateTime<Utc>) -> Result<bool> {
    let result = sqlx::query("INSERT INTO registration_codes (code, max_uses, expires_at) VALUES ($1, $2, $3)")
        .bind(code)
        .bind(max_uses)
        .bind(expires_at.timestamp())
        .execute(db)
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(err) if super::is_unique_violation(&err) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[derive(sqlx::FromRow)]
pub struct CodeRow {
    pub code: String,
    pub uses: i64,
    pub max_uses: i64,
    pub expires_at: DateTime<Utc>,
}

/// Codes that can still be used, soonest to expire first.
pub async fn usable(db: &SqlitePool) -> Result<Vec<CodeRow>> {
    Ok(sqlx::query_as(
        "SELECT code, uses, max_uses, expires_at FROM registration_codes
         WHERE expires_at > unixepoch() AND uses < max_uses
         ORDER BY expires_at",
    )
    .fetch_all(db)
    .await?)
}

pub async fn delete(db: &SqlitePool, code: &str) -> Result<bool> {
    Ok(sqlx::query("DELETE FROM registration_codes WHERE code = $1").bind(code).execute(db).await?.rows_affected() == 1)
}

/// Atomically consumes one use. `false` if the code is unknown, expired or used up.
pub async fn redeem(conn: &mut SqliteConnection, code: &str) -> Result<bool> {
    let redeemed = sqlx::query(
        "UPDATE registration_codes SET uses = uses + 1
         WHERE code = $1 AND expires_at > unixepoch() AND uses < max_uses",
    )
    .bind(code)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(redeemed == 1)
}

/// Removes codes that expired more than a week ago or are used up (housekeeping).
pub async fn purge(db: &SqlitePool) -> Result<u64> {
    Ok(sqlx::query("DELETE FROM registration_codes WHERE expires_at <= unixepoch() - 7 * 86400 OR uses >= max_uses")
        .execute(db)
        .await?
        .rows_affected())
}
