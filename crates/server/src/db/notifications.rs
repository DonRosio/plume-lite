use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{NotificationLevel, NotificationSetting};
use sqlx::SqlitePool;

use super::BEGIN_WRITE;
use crate::error::Result;

/// What a setting applies to.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Guild(Snowflake),
    Channel(Snowflake),
}

pub async fn for_user(db: &SqlitePool, user_id: Snowflake) -> Result<Vec<NotificationSetting>> {
    let rows: Vec<(Snowflake, i64)> =
        sqlx::query_as("SELECT coalesce(guild_id, channel_id), level FROM notification_settings WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(target_id, level)| NotificationSetting { target_id, level: NotificationLevel::from_i64(level) })
        .collect())
}

/// Replaces the setting of `target`; `None` goes back to the default (every message).
pub async fn set(db: &SqlitePool, user_id: Snowflake, target: Target, level: Option<NotificationLevel>) -> Result<()> {
    let (guild_id, channel_id) = match target {
        Target::Guild(id) => (Some(id), None),
        Target::Channel(id) => (None, Some(id)),
    };
    let mut tx = db.begin_with(BEGIN_WRITE).await?;
    sqlx::query("DELETE FROM notification_settings WHERE user_id = $1 AND coalesce(guild_id, channel_id) = $2")
        .bind(user_id)
        .bind(guild_id.or(channel_id))
        .execute(&mut *tx)
        .await?;
    if let Some(level) = level {
        sqlx::query("INSERT INTO notification_settings (user_id, guild_id, channel_id, level) VALUES ($1, $2, $3, $4)")
            .bind(user_id)
            .bind(guild_id)
            .bind(channel_id)
            .bind(level as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
