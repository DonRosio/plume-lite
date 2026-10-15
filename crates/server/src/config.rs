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
    /// Anyone can create an account. Otherwise a registration code is required
    /// (`plume-lite-server invite`). Meant for development only.
    pub open_registration: bool,
    /// Daily backups of the database go there; `None` disables them.
    pub backup_dir: Option<PathBuf>,
    /// Number of daily backups kept.
    pub backup_keep: usize,
    /// Files served under `/download/` (the desktop client); `None` serves nothing.
    pub downloads_dir: Option<PathBuf>,
    /// The LiveKit media server; `None` disables voice channels.
    pub livekit: Option<LiveKitConfig>,
}

/// Where the LiveKit media server is, and the key pair shared with it.
#[derive(Clone)]
pub struct LiveKitConfig {
    /// Given to clients, e.g. `wss://plume-lite.donr-corp.fr` (`LIVEKIT_URL`).
    pub url: String,
    /// Used by this server, e.g. `http://livekit:7880` (`LIVEKIT_API_URL`, defaults to `url`).
    pub api_url: String,
    pub api_key: String,
    pub api_secret: String,
}

impl std::fmt::Debug for LiveKitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret: configurations end up in logs.
        f.debug_struct("LiveKitConfig").field("url", &self.url).field("api_url", &self.api_url).finish_non_exhaustive()
    }
}

impl LiveKitConfig {
    fn from_env() -> anyhow::Result<Option<Self>> {
        let var = |key: &str| std::env::var(key).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let (Some(url), Some(api_key), Some(api_secret)) =
            (var("LIVEKIT_URL"), var("LIVEKIT_API_KEY"), var("LIVEKIT_API_SECRET"))
        else {
            return Ok(None);
        };
        if api_secret.len() < 32 {
            anyhow::bail!("LIVEKIT_API_SECRET must be at least 32 characters long");
        }
        let api_url = var("LIVEKIT_API_URL")
            .unwrap_or_else(|| url.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1));
        Ok(Some(Self { url, api_url: api_url.trim_end_matches('/').to_owned(), api_key, api_secret }))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_path: PathBuf::from("data/plume-lite.db"),
            bind_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            session_ttl: Duration::from_secs(30 * 24 * 3600),
            trust_proxy: false,
            rate_limit: true,
            open_registration: false,
            backup_dir: None,
            backup_keep: 7,
            downloads_dir: None,
            livekit: None,
        }
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let defaults = Self::default();
        let database_path: PathBuf = parse_or("DATABASE_PATH", defaults.database_path)?;
        // Backups sit next to the database unless told otherwise; BACKUP_DIR=off disables them.
        let backup_dir = match std::env::var("BACKUP_DIR") {
            Ok(dir) if dir.trim() == "off" => None,
            Ok(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir.trim())),
            _ => Some(database_path.parent().unwrap_or(std::path::Path::new(".")).join("backups")),
        };
        let downloads_dir = std::env::var("DOWNLOADS_DIR").ok().filter(|dir| !dir.trim().is_empty()).map(PathBuf::from);
        Ok(Self {
            database_path,
            bind_addr: parse_or("BIND_ADDR", defaults.bind_addr)?,
            session_ttl: Duration::from_secs(parse_or("SESSION_TTL_DAYS", 30u64)?.saturating_mul(24 * 3600)),
            trust_proxy: parse_or("TRUST_PROXY", defaults.trust_proxy)?,
            rate_limit: parse_or("RATE_LIMIT", defaults.rate_limit)?,
            open_registration: parse_or("OPEN_REGISTRATION", defaults.open_registration)?,
            backup_dir,
            backup_keep: parse_or("BACKUP_KEEP", defaults.backup_keep)?.max(1),
            downloads_dir,
            livekit: LiveKitConfig::from_env()?,
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
