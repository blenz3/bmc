//! API key signing for the Kalshi WebSocket upgrade handshake.
//!
//! Kalshi authenticates each request by signing a string built from the millisecond
//! timestamp, HTTP method, and path with RSA-PSS over SHA-256, then base64-encoding
//! the result. The signed string for a WebSocket connection is:
//!
//! ```text
//! <timestamp_ms> + "GET" + "/trade-api/ws/v2"
//! ```
//!
//! The signature, key id, and timestamp are sent as `KALSHI-ACCESS-SIGNATURE`,
//! `KALSHI-ACCESS-KEY`, and `KALSHI-ACCESS-TIMESTAMP` headers on the upgrade request.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rand::rngs::OsRng;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::SigningKey;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rsa::RsaPrivateKey;
use sha2::Sha256;

use crate::error::{KalshiError, Result};

/// Holds an API key id and its private key, ready to sign requests.
///
/// `Credentials` is `Clone` (the underlying RSA key is wrapped) so it can be passed
/// around freely. Cloning does not regenerate any keys.
#[derive(Clone)]
pub struct Credentials {
    pub key_id: String,
    private_key: RsaPrivateKey,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("key_id", &self.key_id)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl Credentials {
    /// Load credentials from a PEM-encoded private key string. Accepts both PKCS#8
    /// (`-----BEGIN PRIVATE KEY-----`) and PKCS#1 (`-----BEGIN RSA PRIVATE KEY-----`).
    pub fn from_pem(key_id: impl Into<String>, pem: &str) -> Result<Self> {
        let private_key = RsaPrivateKey::from_pkcs8_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(pem))
            .map_err(|e| KalshiError::Auth(format!("invalid PEM private key: {e}")))?;
        Ok(Self {
            key_id: key_id.into(),
            private_key,
        })
    }

    /// Load credentials from a private key file on disk.
    pub fn from_pem_file(key_id: impl Into<String>, path: impl AsRef<Path>) -> Result<Self> {
        let pem = std::fs::read_to_string(path.as_ref())?;
        Self::from_pem(key_id, &pem)
    }

    /// Sign `timestamp_ms || method || path` and return the base64-encoded signature
    /// (standard alphabet, with padding).
    pub fn sign(&self, timestamp_ms: i64, method: &str, path: &str) -> String {
        let signing_key: SigningKey<Sha256> = SigningKey::<Sha256>::new(self.private_key.clone());
        let payload = format!("{timestamp_ms}{method}{path}");
        let sig = signing_key.sign_with_rng(&mut OsRng, payload.as_bytes());
        STANDARD.encode(sig.to_bytes())
    }

    /// Build the three signed headers Kalshi expects on every authenticated request.
    /// Returns `[(name, value); 3]` for the WS upgrade or any REST call. Path is the
    /// request path (no query string), matching what gets signed.
    pub fn signed_headers(&self, method: &str, path: &str) -> [(&'static str, String); 3] {
        let ts = now_ms();
        let sig = self.sign(ts, method, path);
        [
            ("kalshi-access-key", self.key_id.clone()),
            ("kalshi-access-signature", sig),
            ("kalshi-access-timestamp", ts.to_string()),
        ]
    }
}

/// Current Unix time in milliseconds. Public so external callers (e.g., a REST
/// downloader in another crate) can build their own auth headers consistently.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
