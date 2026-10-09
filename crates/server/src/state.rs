use std::sync::Arc;

use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::gateway::hub::Hub;
use crate::ids::IdGenerator;
use crate::rate_limit::Limits;

/// Shared by every request handler and gateway session (cheap to clone).
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub ids: Arc<IdGenerator>,
    pub hub: Arc<Hub>,
    pub limits: Arc<Limits>,
    /// Cancelled on shutdown: gateway sessions then close with code 1012 so that
    /// clients reconnect once the server is back.
    pub shutdown: CancellationToken,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Config) -> Self {
        Self {
            db,
            ids: Arc::new(IdGenerator::new(0)),
            hub: Arc::new(Hub::default()),
            limits: Arc::new(Limits::new(config.rate_limit)),
            config: Arc::new(config),
            shutdown: CancellationToken::new(),
        }
    }
}
