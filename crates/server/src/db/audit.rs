//! The audit log: what moderators did in a guild.

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{AUDIT_RETENTION_DAYS, AuditAction, AuditChange, AuditEntry, User};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::Result;

/// One thing a moderator did, about to be written.
#[derive(Debug, Clone)]
pub struct Record {
    pub guild_id: Snowflake,
    pub actor_id: Snowflake,
    pub action: AuditAction,
    pub target_user_id: Option<Snowflake>,
    pub target_id: Option<Snowflake>,
    /// The channel concerned: entries about a channel are shown only to those who see it.
    pub channel_id: Option<Snowflake>,
    pub target_name: Option<String>,
    pub reason: Option<String>,
    pub changes: Vec<AuditChange>,
}

impl Record {
    pub fn new(guild_id: Snowflake, actor_id: Snowflake, action: AuditAction) -> Self {
        Self {
            guild_id,
            actor_id,
            action,
            target_user_id: None,
            target_id: None,
            channel_id: None,
            target_name: None,
            reason: None,
            changes: Vec::new(),
        }
    }

    pub fn user(mut self, user_id: Snowflake) -> Self {
        self.target_user_id = Some(user_id);
        self
    }

    pub fn target(mut self, id: Snowflake, name: impl Into<String>) -> Self {
        self.target_id = Some(id);
        self.target_name = Some(name.into());
        self
    }

    pub fn channel(mut self, channel_id: Snowflake) -> Self {
        self.channel_id = Some(channel_id);
        self
    }

    pub fn reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }

    /// Adds a change when the value did change.
    pub fn change(mut self, field: &str, before: Option<String>, after: Option<String>) -> Self {
        if before != after {
            self.changes.push(AuditChange { field: field.to_owned(), before, after });
        }
        self
    }
}

fn action_name(action: AuditAction) -> String {
    serde_json::to_value(action).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

pub async fn record(conn: &mut SqliteConnection, id: Snowflake, record: &Record) -> Result<()> {
    let changes = (!record.changes.is_empty())
        .then(|| serde_json::to_string(&record.changes))
        .transpose()
        .map_err(anyhow::Error::from)?;
    sqlx::query(
        "INSERT INTO audit_log (id, guild_id, actor_id, action, target_user_id, target_id, channel_id, target_name,
                                reason, changes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(record.guild_id)
    .bind(record.actor_id)
    .bind(action_name(record.action))
    .bind(record.target_user_id)
    .bind(record.target_id)
    .bind(record.channel_id)
    .bind(&record.target_name)
    .bind(&record.reason)
    .bind(changes)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Snowflake,
    action: String,
    actor_id: Option<Snowflake>,
    actor_username: Option<String>,
    target_user_id: Option<Snowflake>,
    target_username: Option<String>,
    target_id: Option<Snowflake>,
    channel_id: Option<Snowflake>,
    target_name: Option<String>,
    reason: Option<String>,
    changes: Option<String>,
}

/// A page of a guild's log, newest first, with the channel each entry is about.
pub async fn page(
    db: &SqlitePool,
    guild_id: Snowflake,
    before: Option<Snowflake>,
    limit: u32,
) -> Result<Vec<(AuditEntry, Option<Snowflake>)>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.id, a.action, a.actor_id, au.username AS actor_username, a.target_user_id,
                tu.username AS target_username, a.target_id, a.channel_id, a.target_name, a.reason, a.changes
         FROM audit_log a
         LEFT JOIN users au ON au.id = a.actor_id
         LEFT JOIN users tu ON tu.id = a.target_user_id
         WHERE a.guild_id = $1 AND a.id < $2
         ORDER BY a.id DESC
         LIMIT $3",
    )
    .bind(guild_id)
    .bind(before.map_or(i64::MAX, |id| id.0))
    .bind(i64::from(limit))
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let entry = AuditEntry {
                id: row.id,
                action: serde_json::from_value(serde_json::Value::String(row.action)).unwrap_or(AuditAction::Unknown),
                actor: row.actor_id.zip(row.actor_username).map(|(id, username)| User { id, username }),
                target_user: row.target_user_id.zip(row.target_username).map(|(id, username)| User { id, username }),
                target_id: row.target_id,
                target_name: row.target_name,
                reason: row.reason,
                changes: row.changes.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default(),
            };
            (entry, row.channel_id)
        })
        .collect())
}

/// Forgets the entries older than [`AUDIT_RETENTION_DAYS`].
pub async fn purge(db: &SqlitePool) -> Result<u64> {
    let horizon = chrono::Utc::now().timestamp_millis() - AUDIT_RETENTION_DAYS * 24 * 3600 * 1000;
    let removed = sqlx::query("DELETE FROM audit_log WHERE id < $1")
        .bind(Snowflake::from_timestamp_ms(horizon))
        .execute(db)
        .await?
        .rows_affected();
    Ok(removed)
}
