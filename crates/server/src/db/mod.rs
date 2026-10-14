//! Data access. One module per aggregate, plain SQL (no ORM), no N+1 queries.
//!
//! The database is a single SQLite file next to the server: no database server to run,
//! back up or secure. Queries are checked at runtime and covered by the integration
//! tests (`tests/api.rs`).

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub mod bans;
pub mod channels;
pub mod guilds;
pub mod invites;
pub mod messages;
pub mod notifications;
pub mod overwrites;
pub mod reactions;
pub mod read_states;
pub mod registration;
pub mod roles;
pub mod users;

/// Upper bounds that keep a single tenant from degrading the service.
pub const MAX_GUILDS_PER_USER: i64 = 100;
pub const MAX_CHANNELS_PER_GUILD: i64 = 200;
pub const MAX_CATEGORIES_PER_GUILD: i64 = 50;
pub const MAX_ROLES_PER_GUILD: i64 = 50;

/// Write transactions start with `BEGIN IMMEDIATE`: they take the write lock up front
/// (waiting up to the busy timeout) instead of failing when a read turns into a write.
pub const BEGIN_WRITE: &str = "BEGIN IMMEDIATE";

/// Opens (and creates if needed) the database file.
///
/// WAL lets readers run while one connection writes; `synchronous = NORMAL` is durable
/// across application crashes and the usual setting with WAL.
pub async fn connect(path: &Path) -> anyhow::Result<SqlitePool> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    Ok(SqlitePoolOptions::new().max_connections(8).connect_with(options).await?)
}

/// Writes a consistent copy of the database to `path`, which must not exist yet, while the
/// server keeps running: `VACUUM INTO` copies one snapshot, compacted.
pub async fn backup(db: &SqlitePool, path: &Path) -> anyhow::Result<()> {
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("backup path is not valid UTF-8"))?;
    sqlx::query("VACUUM INTO $1").bind(path).execute(db).await?;
    Ok(())
}

pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}
