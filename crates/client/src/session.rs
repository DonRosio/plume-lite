//! Persisted login (`~/.config/plume-lite/session.json`, mode 0600 on Unix) and
//! preferences (`preferences.json`, shared by every profile).
//!
//! `PLUME_LITE_PROFILE=<name>` selects another file, which lets you run several
//! accounts side by side on one machine (handy for demos and manual testing).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedSession {
    pub server_url: String,
    /// `None` after a logout: we still remember the server.
    pub token: Option<String>,
}

fn path() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("app", "Plume Lite", "plume-lite")?;
    let file = match std::env::var("PLUME_LITE_PROFILE") {
        Ok(profile) if !profile.trim().is_empty() => {
            let safe: String =
                profile.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
            format!("session-{safe}.json")
        }
        _ => "session.json".to_owned(),
    };
    Some(dirs.config_dir().join(file))
}

pub fn load() -> Option<SavedSession> {
    let raw = fs::read_to_string(path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(session: &SavedSession) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::other("no config directory"))?;
    write_private(&path, session)
}

/// Settings shared by every profile on this machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub dark: bool,
}

fn preferences_path() -> Option<PathBuf> {
    Some(ProjectDirs::from("app", "Plume Lite", "plume-lite")?.config_dir().join("preferences.json"))
}

pub fn load_preferences() -> Preferences {
    preferences_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_preferences(preferences: &Preferences) -> io::Result<()> {
    let path = preferences_path().ok_or_else(|| io::Error::other("no config directory"))?;
    write_private(&path, preferences)
}

fn write_private(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600); // the session file holds a credential
    }
    options.open(path)?.write_all(&json)
}

pub fn forget_token() {
    if let Some(mut session) = load() {
        session.token = None;
        let _ = save(&session);
    }
}
