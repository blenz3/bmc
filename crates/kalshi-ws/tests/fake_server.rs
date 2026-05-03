//! End-to-end smoke test against an in-process WebSocket server.
//!
//! Exercises:
//! - subscribe → server ack with assigned `sid`
//! - typed `Subscription<Ticker>` receives a server-pushed frame
//! - `Subscription::Drop` issues an `unsubscribe` command

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use kalshi_ws::{Client, Environment};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_receives_typed_frames_and_unsubscribes_on_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Server task: accept exactly one client, walk it through subscribe → ticker → unsubscribe.
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();

        // 1. Read subscribe command from the client.
        let frame = ws.next().await.expect("client message").unwrap();
        let text = frame.into_text().unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["cmd"], "subscribe");
        let req_id = v["id"].as_u64().unwrap();
        assert_eq!(v["params"]["channels"][0], "ticker");

        // 2. Send subscribed ack.
        let ack = json!({
            "id": req_id,
            "type": "subscribed",
            "msg": { "channel": "ticker", "sid": 99 }
        });
        ws.send(Message::Text(ack.to_string().into())).await.unwrap();

        // 3. Push one ticker frame.
        let tick = json!({
            "type": "ticker",
            "sid": 99,
            "msg": {
                "market_ticker": "FAKE-MKT",
                "price_dollars": 0.5,
                "yes_bid_dollars": 0.49,
                "yes_ask_dollars": 0.51,
                "volume_fp": 100,
                "open_interest_fp": 200,
                "dollar_volume": 50,
                "dollar_open_interest": 100,
                "yes_bid_size_fp": 10,
                "yes_ask_size_fp": 10,
                "last_trade_size_fp": 5,
                "ts_ms": 1
            }
        });
        ws.send(Message::Text(tick.to_string().into())).await.unwrap();

        // 4. Expect an unsubscribe (triggered by client-side Drop) within 3s.
        let unsub = tokio::time::timeout(Duration::from_secs(3), ws.next()).await;
        let unsub_frame = unsub
            .expect("unsubscribe arrived in time")
            .expect("not closed")
            .unwrap();
        let v: Value = serde_json::from_str(&unsub_frame.into_text().unwrap()).unwrap();
        assert_eq!(v["cmd"], "unsubscribe");
        assert_eq!(v["params"]["sids"][0], 99);

        // Politely close.
        let _ = ws.close(None).await;
    });

    let client = Client::builder()
        .environment(Environment::Custom {
            url: format!("ws://127.0.0.1:{port}/test"),
            path: "/test".into(),
        })
        .connect()
        .await
        .expect("client connects");

    {
        let mut sub = client
            .subscribe_ticker(vec!["FAKE-MKT".into()])
            .await
            .expect("subscribe");

        let frame = tokio::time::timeout(Duration::from_secs(3), sub.next())
            .await
            .expect("ticker arrives")
            .expect("not closed");
        assert_eq!(frame.market_ticker, "FAKE-MKT");
        assert!((frame.price_dollars - 0.5).abs() < 1e-9);
    }
    // Subscription dropped here → client should send unsubscribe.

    server.await.expect("server task completes");
    client.shutdown();
}
