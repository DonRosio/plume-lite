//! Backups of the database: one file per day in `BACKUP_DIR`, the last `BACKUP_KEEP` kept.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use sqlx::SqlitePool;

use crate::db;

const PREFIX: &str = "plume-lite-";
const SUFFIX: &str = ".db";
/// A new daily backup is taken once the latest one is older than this.
const DAILY: Duration = Duration::from_secs(23 * 3600);

/// Writes a new backup now and returns its path.
pub async fn create(db: &SqlitePool, dir: &Path) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let mut path = dir.join(format!("{PREFIX}{}{SUFFIX}", Utc::now().format("%Y%m%d-%H%M%S")));
    // Two backups within the same second (a manual one right after the daily one).
    let mut n = 1;
    while tokio::fs::try_exists(&path).await? {
        n += 1;
        path = dir.join(format!("{PREFIX}{}-{n}{SUFFIX}", Utc::now().format("%Y%m%d-%H%M%S")));
    }
    db::backup(db, &path).await?;
    Ok(path)
}

/// The backups in `dir`, oldest first.
pub async fn list(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(err) => return Err(err.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(PREFIX) && name.ends_with(SUFFIX) {
            found.push(entry.path());
        }
    }
    // The names hold the date: sorting them sorts by age.
    found.sort();
    Ok(found)
}

/// Deletes the oldest backups beyond `keep`. Returns how many were deleted.
pub async fn prune(dir: &Path, keep: usize) -> anyhow::Result<usize> {
    let backups = list(dir).await?;
    let excess = backups.len().saturating_sub(keep);
    for path in &backups[..excess] {
        tokio::fs::remove_file(path).await?;
    }
    Ok(excess)
}

/// Takes the daily backup if it is due, then prunes. `None` when nothing was due.
pub async fn daily(db: &SqlitePool, dir: &Path, keep: usize) -> anyhow::Result<Option<PathBuf>> {
    if let Some(latest) = list(dir).await?.pop() {
        let modified = tokio::fs::metadata(&latest).await?.modified()?;
        if SystemTime::now().duration_since(modified).unwrap_or_default() < DAILY {
            return Ok(None);
        }
    }
    let path = create(db, dir).await?;
    prune(dir, keep).await?;
    Ok(Some(path))
}
