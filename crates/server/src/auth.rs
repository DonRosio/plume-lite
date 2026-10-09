//! Password hashing, session tokens and the `AuthUser` extractor.

use std::fmt::Write as _;
use std::sync::LazyLock;

use anyhow::anyhow;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use chrono::Utc;
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::User;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

/// Argon2id (OWASP parameters: 19 MiB, 2 passes) costs tens of milliseconds and
/// ~19 MiB per hash: cap the concurrency so a login flood can neither exhaust the
/// memory nor the blocking thread pool.
static HASHING_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(std::thread::available_parallelism().map_or(4, |n| n.get())));

pub async fn hash_password(password: String) -> anyhow::Result<String> {
    run_hashing(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|e| anyhow!("password hashing failed: {e}"))
    })
    .await
}

pub async fn verify_password(password: String, hash: String) -> anyhow::Result<bool> {
    run_hashing(move || Ok(Argon2::default().verify_password(password.as_bytes(), hash.as_str()).is_ok())).await
}

/// Spends the same time as a real verification, so that "unknown user" and
/// "wrong password" cannot be told apart by measuring response times.
pub async fn verify_dummy(password: String) -> anyhow::Result<()> {
    static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
        Argon2::default().hash_password(b"plume-lite-timing-equaliser").map(|hash| hash.to_string()).unwrap_or_default()
    });
    run_hashing(move || {
        let _ = Argon2::default().verify_password(password.as_bytes(), DUMMY_HASH.as_str());
        Ok(())
    })
    .await
}

async fn run_hashing<T: Send + 'static>(job: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    let _permit = HASHING_SLOTS.acquire().await?;
    tokio::task::spawn_blocking(job).await?
}

/// 256-bit random bearer token, hex encoded.
pub fn generate_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("OS RNG failure: {e}"))?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Random alphanumeric code (invites). Rejection sampling keeps the distribution uniform.
pub fn random_code(len: usize) -> anyhow::Result<String> {
    random_string(len, b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789")
}

/// Random password handed out by `plume-lite-server reset-password`: no look-alike
/// characters (0/O, 1/l/I), so that it can be read out or copied by hand.
pub fn random_password() -> anyhow::Result<String> {
    random_string(16, b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789")
}

fn random_string(len: usize, alphabet: &[u8]) -> anyhow::Result<String> {
    // The largest multiple of the alphabet size that fits in a byte: values above would
    // bias the first characters.
    let limit = 256 - 256 % alphabet.len();
    let mut code = String::with_capacity(len);
    let mut buffer = [0u8; 32];
    while code.len() < len {
        getrandom::fill(&mut buffer).map_err(|e| anyhow!("OS RNG failure: {e}"))?;
        for &byte in &buffer {
            if usize::from(byte) < limit && code.len() < len {
                code.push(char::from(alphabet[usize::from(byte) % alphabet.len()]));
            }
        }
    }
    Ok(code)
}

/// The authenticated caller, resolved from `Authorization: Bearer <token>`.
pub struct AuthUser {
    pub user: User,
    pub token_hash: Vec<u8>,
    pub session_id: Snowflake,
}

/// `last_seen_at` of a session is refreshed at most this often (one write per active
/// device every few minutes, not one per request).
const LAST_SEEN_PRECISION_SECS: i64 = 300;

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(AppError::unauthenticated)?;
        let token_hash = hash_token(token);
        let session = db::users::by_session(&state.db, &token_hash).await?.ok_or_else(AppError::unauthenticated)?;
        if (Utc::now() - session.last_seen_at).num_seconds() >= LAST_SEEN_PRECISION_SECS {
            db::users::touch_session(&state.db, session.session_id).await?;
        }
        Ok(Self { user: session.user, token_hash, session_id: session.session_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn password_roundtrip() {
        let hash = hash_password("correct horse".into()).await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse".into(), hash.clone()).await.unwrap());
        assert!(!verify_password("wrong horse".into(), hash).await.unwrap());
        verify_dummy("whatever".into()).await.unwrap();
    }

    #[test]
    fn tokens_and_codes() {
        let token = generate_token().unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(token, generate_token().unwrap());
        assert_eq!(hash_token(&token).len(), 32);
        let code = random_code(8).unwrap();
        assert_eq!(code.len(), 8);
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
        let password = random_password().unwrap();
        assert_eq!(password.len(), 16);
        assert!(!password.contains(['0', 'O', '1', 'l', 'I']));
    }
}
