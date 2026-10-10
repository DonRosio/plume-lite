//! LiveKit access tokens and webhook signatures: JSON Web Tokens signed with HMAC-SHA256
//! (HS256) and the API secret shared with the LiveKit server.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// What a token allows in LiveKit (`video` claim). Field names are LiveKit's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoGrant {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub room: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub room_join: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub room_admin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub room_list: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_publish: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_subscribe: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_publish_data: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Claims {
    /// API key.
    pub iss: String,
    /// Participant identity (our user id).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub: String,
    /// Display name in the room.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub nbf: i64,
    pub exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoGrant>,
    /// Webhooks only: base64 SHA-256 of the request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Claims {
    /// Claims valid from now for `ttl_secs`.
    pub fn new(api_key: &str, ttl_secs: i64) -> Self {
        let now = Utc::now().timestamp();
        Self { iss: api_key.to_owned(), nbf: now, exp: now + ttl_secs, ..Self::default() }
    }
}

pub fn sign(claims: &Claims, secret: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    // Serialising plain structs of strings, numbers and booleans cannot fail.
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap_or_default());
    let signing_input = format!("{header}.{payload}");
    let signature = URL_SAFE_NO_PAD.encode(mac(secret, signing_input.as_bytes()).finalize().into_bytes());
    format!("{signing_input}.{signature}")
}

/// Checks the signature (constant time), the algorithm and the validity period.
pub fn verify(token: &str, secret: &str) -> Option<Claims> {
    let mut parts = token.split('.');
    let (header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header_len = header.len();
    let header: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    if header.get("alg")?.as_str()? != "HS256" {
        return None;
    }
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let signing_input = &token[..header_len + 1 + payload.len()];
    mac(secret, signing_input.as_bytes()).verify_slice(&signature).ok()?;
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    let now = Utc::now().timestamp();
    // A little tolerance for clocks that drift apart.
    (claims.nbf <= now + 60 && claims.exp > now - 60).then_some(claims)
}

/// The `sha256` claim LiveKit puts in webhook tokens.
pub fn body_hash(body: &[u8]) -> String {
    STANDARD.encode(Sha256::digest(body))
}

fn mac(secret: &str, data: &[u8]) -> HmacSha256 {
    // HMAC accepts keys of any length.
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC takes keys of any size");
    mac.update(data);
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_tokens_verify_and_tampering_is_caught() {
        let mut claims = Claims::new("key", 600);
        claims.sub = "42".into();
        claims.video = Some(VideoGrant { room: "7".into(), room_join: true, ..VideoGrant::default() });
        let token = sign(&claims, "secret");

        let decoded = verify(&token, "secret").unwrap();
        assert_eq!((decoded.iss.as_str(), decoded.sub.as_str()), ("key", "42"));
        assert_eq!(decoded.video.unwrap().room, "7");
        assert!(verify(&token, "other secret").is_none());

        let (head, rest) = token.split_once('.').unwrap();
        let (_, signature) = rest.split_once('.').unwrap();
        let forged = URL_SAFE_NO_PAD.encode(br#"{"iss":"key","sub":"1","nbf":0,"exp":9999999999}"#);
        assert!(verify(&format!("{head}.{forged}.{signature}"), "secret").is_none());
        let none = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        assert!(verify(&format!("{none}.{}", rest), "secret").is_none());
    }

    #[test]
    fn expired_tokens_are_refused() {
        let mut claims = Claims::new("key", 600);
        claims.exp = Utc::now().timestamp() - 3600;
        assert!(verify(&sign(&claims, "secret"), "secret").is_none());
    }

    #[test]
    fn grants_use_livekit_names() {
        let grant = VideoGrant {
            room: "7".into(),
            room_join: true,
            can_publish: Some(false),
            can_subscribe: Some(true),
            ..VideoGrant::default()
        };
        assert_eq!(
            serde_json::to_string(&grant).unwrap(),
            r#"{"room":"7","roomJoin":true,"canPublish":false,"canSubscribe":true}"#
        );
    }
}
