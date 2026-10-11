//! The desktop clients this server hands out: the files the release workflow puts in
//! `DOWNLOADS_DIR`, one per platform, each with its SHA-256 and, when the workflow signed it,
//! its minisign signature (`<file>.minisig`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use plume_lite_protocol::http::{ClientDownload, Platform};
use sha2::{Digest, Sha256};

/// File name of each build in `DOWNLOADS_DIR`, served under `/download/`.
pub const FILES: [(Platform, &str); 2] =
    [(Platform::Windows, "plume-lite.exe"), (Platform::Linux, "plume-lite-linux-x64.tar.gz")];

/// The public half of the key that signs the builds (the client checks updates with it).
pub const PUBLIC_KEY: &str = include_str!("../../../minisign.pub");

/// Digests already computed, by file: hashing 50 MB at every request would be wasteful,
/// and the files only change when the image does.
#[derive(Default)]
pub struct Downloads {
    digests: Mutex<HashMap<&'static str, (SystemTime, u64, String)>>,
}

impl Downloads {
    /// The builds present in `dir`.
    pub async fn list(&self, dir: &Path) -> Vec<ClientDownload> {
        let mut found = Vec::new();
        for (platform, name) in FILES {
            let path = dir.join(name);
            let Ok(metadata) = tokio::fs::metadata(&path).await else { continue };
            if !metadata.is_file() {
                continue;
            }
            let (modified, size) = (metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), metadata.len());
            let cached =
                self.lock().get(name).filter(|(m, s, _)| (*m, *s) == (modified, size)).map(|(_, _, d)| d.clone());
            let sha256 = match cached {
                Some(digest) => digest,
                None => {
                    let Ok(Ok(digest)) = tokio::task::spawn_blocking(move || sha256_of(&path)).await else { continue };
                    self.lock().insert(name, (modified, size, digest.clone()));
                    digest
                }
            };
            let signed = tokio::fs::metadata(dir.join(format!("{name}.minisig"))).await.is_ok_and(|m| m.is_file());
            found.push(ClientDownload {
                platform,
                path: format!("/download/{name}"),
                size,
                sha256,
                signature_path: signed.then(|| format!("/download/{name}.minisig")),
            });
        }
        found
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, (SystemTime, u64, String)>> {
        self.digests.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        match file.read(&mut buffer)? {
            0 => break,
            n => hasher.update(&buffer[..n]),
        }
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The second line of the public key file: what `minisign -P` takes.
pub fn public_key_base64() -> &'static str {
    PUBLIC_KEY
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment"))
        .unwrap_or("")
}
