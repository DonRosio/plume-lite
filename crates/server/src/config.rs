use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;

/// Runtime configuration, read from the environment (12-factor). See `.env.example`.
#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite database file, created on first start.
    pub database_path: PathBuf,
    pub bind_addr: SocketAddr,
    pub session_ttl: Duration,
    /// Trust `X-Forwarded-For` (only behind a reverse proxy you control).
    pub trust_proxy: bool,
    pub rate_limit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_path: PathBuf::from("data/plume-lite.db"),
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            session_ttl: Duration::from_secs(30 * 24 * 3600),
            trust_proxy: false,
            rate_limit: true,
        }
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let defaults = Self::default();
        Ok(Self {
            database_path: parse_or("DATABASE_PATH", defaults.database_path)?,
            bind_addr: parse_or("BIND_ADDR", defaults.bind_addr)?,
            session_ttl: Duration::from_secs(parse_or("SESSION_TTL_DAYS", 30u64)?.saturating_mul(24 * 3600)),
            trust_proxy: parse_or("TRUST_PROXY", defaults.trust_proxy)?,
            rate_limit: parse_or("RATE_LIMIT", defaults.rate_limit)?,
        })
    }
}

fn parse_or<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(raw) if !raw.trim().is_empty() => {
            raw.trim().parse().with_context(|| format!("invalid value for {key}: {raw:?}"))
        }
        _ => Ok(default),
    }
}
