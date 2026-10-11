//! Plume Lite server: REST API + realtime gateway in one process, over one SQLite file.
//!
//! The binary (`main.rs`) only wires configuration, the database pool and the
//! listener; everything else lives here so integration tests can boot the exact
//! same application on a random port.

pub mod admin;
pub mod auth;
pub mod backup;
pub mod config;
pub mod db;
pub mod downloads;
pub mod error;
pub mod extract;
pub mod gateway;
pub mod ids;
pub mod rate_limit;
pub mod routes;
pub mod state;
pub mod voice;

use std::time::Duration;

pub use config::Config;
pub use routes::router;
pub use state::AppState;

/// Embedded SQL migrations, applied at startup.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Housekeeping loops. They stop when `state.shutdown` is cancelled.
pub fn spawn_background_tasks(state: &AppState) {
    if state.voice.enabled() {
        // Who is in voice survives a restart of this server, not of its memory: ask LiveKit,
        // which may still be starting (both start together).
        let s = state.clone();
        tokio::spawn(async move {
            for attempt in 1..=10u64 {
                match routes::voice::resync(&s).await {
                    Ok(found) => {
                        tracing::info!(found, "voice states resynchronised from LiveKit");
                        return;
                    }
                    Err(err) => tracing::warn!(error = %err, attempt, "LiveKit not reachable yet"),
                }
                tokio::select! {
                    () = s.shutdown.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(3 * attempt)) => {}
                }
            }
        });
    }

    let s = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                () = s.shutdown.cancelled() => break,
                _ = tick.tick() => s.limits.prune(),
            }
        }
    });

    let s = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                () = s.shutdown.cancelled() => break,
                _ = tick.tick() => {
                    match db::users::purge_expired_sessions(&s.db).await {
                        Ok(0) => {}
                        Ok(purged) => tracing::info!(purged, "expired sessions purged"),
                        Err(err) => tracing::warn!(error = %err, "session purge failed"),
                    }
                    if let Err(err) = db::registration::purge(&s.db).await {
                        tracing::warn!(error = %err, "registration code purge failed");
                    }
                    if let Some(dir) = &s.config.backup_dir {
                        match backup::daily(&s.db, dir, s.config.backup_keep).await {
                            Ok(None) => {}
                            Ok(Some(path)) => tracing::info!(path = %path.display(), "database backed up"),
                            Err(err) => tracing::error!(error = %err, "database backup failed"),
                        }
                    }
                }
            }
        }
    });
}
