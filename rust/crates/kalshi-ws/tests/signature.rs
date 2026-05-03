//! RSA-PSS signing test.
//!
//! PSS signatures are randomized — every call produces different bytes — so we
//! cannot compare a hard-coded signature value. Instead we generate a key,
//! sign with our `Credentials::sign`, and verify the signature with the
//! corresponding public key. A second pass also confirms two consecutive
//! signatures over the same input differ (proves PSS, not deterministic).

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rsa::pkcs8::EncodePrivateKey;
use rsa::pss::{Signature, VerifyingKey};
use rsa::signature::Verifier;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::Sha256;

use kalshi_ws::Credentials;

fn fresh_creds() -> (Credentials, RsaPublicKey) {
    let mut rng = rand::thread_rng();
    let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate key");
    let pub_key = RsaPublicKey::from(&priv_key);
    let pem = priv_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("to pem");
    let creds = Credentials::from_pem("TEST_KEY", pem.as_str()).expect("from pem");
    (creds, pub_key)
}

#[test]
fn signature_verifies() {
    let (creds, pub_key) = fresh_creds();
    let ts = 1_700_000_000_000i64;
    let method = "GET";
    let path = "/trade-api/ws/v2";
    let sig_b64 = creds.sign(ts, method, path);
    let sig_bytes = STANDARD.decode(&sig_b64).expect("base64 decode");
    let payload = format!("{ts}{method}{path}");

    let verifying_key: VerifyingKey<Sha256> = VerifyingKey::<Sha256>::new(pub_key);
    let signature = Signature::try_from(sig_bytes.as_slice()).expect("decode signature");
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .expect("PSS signature verifies");
}

#[test]
fn signatures_are_randomized() {
    let (creds, _) = fresh_creds();
    let a = creds.sign(1, "GET", "/x");
    let b = creds.sign(1, "GET", "/x");
    assert_ne!(a, b, "PSS signatures should differ each call");
}

#[test]
fn pkcs1_pem_loads() {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let mut rng = rand::thread_rng();
    let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate");
    let pem = priv_key
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("pkcs1 pem");
    Credentials::from_pem("KEY", pem.as_str()).expect("loads pkcs1");
}
