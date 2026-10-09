use std::net::SocketAddr;

use anyhow::Context;
use plume_lite_server::{AppState, Config, MIGRATOR, db, router, spawn_background_tasks};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("plume_lite_server=info,tower_http=info")),
        )
        .init();

    let config = Config::from_env()?;
    let db = db::connect(&config.database_path)
        .await
        .with_context(|| format!("cannot open the database {}", config.database_path.display()))?;
    MIGRATOR.run(&db).await.context("database migration failed")?;

    let bind_addr = config.bind_addr;
    let state = AppState::new(db, config);
    spawn_background_tasks(&state);

    let listener =
        tokio::net::TcpListener::bind(bind_addr).await.with_context(|| format!("cannot listen on {bind_addr}"))?;
    tracing::info!(%bind_addr, "plume lite server listening");

    axum::serve(listener, router(state.clone()).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal(state.shutdown.clone()))
        .await?;
    // Folds the write-ahead log back into the database file.
    state.db.close().await;
    tracing::info!("bye");
    Ok(())
}

/// Ctrl+C / SIGTERM: stop accepting connections, close gateway sessions with 1012
/// (clients reconnect with backoff) and let in-flight requests finish.
async fn shutdown_signal(shutdown: CancellationToken) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown requested");
    shutdown.cancel();
}
