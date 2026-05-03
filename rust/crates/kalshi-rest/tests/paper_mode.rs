//! Paper mode must hard-refuse destructive calls without ever touching the network.

use std::time::Duration;

use kalshi_rest::{Client, Credentials, Environment, Mode, OrderRequest, RestError};
use rsa::pkcs8::EncodePrivateKey;
use rsa::RsaPrivateKey;

fn fake_creds() -> Credentials {
    // 1024-bit key — these tests never actually sign over the wire (paper mode
    // short-circuits before HTTP), so a small key keeps test runtime down.
    let mut rng = rand::thread_rng();
    let priv_key = RsaPrivateKey::new(&mut rng, 1024).expect("generate test key");
    let pem = priv_key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("encode pem");
    Credentials::from_pem("TEST_KEY", pem.as_str()).expect("parse pem")
}

fn paper_client() -> Client {
    Client::builder()
        .environment(Environment::Custom("http://127.0.0.1:1".into()))
        .credentials(fake_creds())
        .mode(Mode::Paper)
        .request_timeout(Duration::from_millis(50))
        .build()
        .unwrap()
}

#[tokio::test]
async fn paper_refuses_place_order() {
    let c = paper_client();
    let err = c
        .place_order(OrderRequest::buy_yes_limit("KX-X", 50, 1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, RestError::PaperRefused { action: "place_order" }),
        "expected PaperRefused, got {err:?}"
    );
}

#[tokio::test]
async fn paper_refuses_cancel_order() {
    let c = paper_client();
    let err = c.cancel_order("ord_1").await.unwrap_err();
    assert!(
        matches!(err, RestError::PaperRefused { action: "cancel_order" }),
        "expected PaperRefused, got {err:?}"
    );
}

#[tokio::test]
async fn paper_refuses_decrease() {
    use kalshi_rest::DecreaseAmount;
    let c = paper_client();
    let err = c
        .decrease_order("ord_1", DecreaseAmount::ReduceBy(1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, RestError::PaperRefused { action: "decrease_order" }),
        "expected PaperRefused, got {err:?}"
    );
}
