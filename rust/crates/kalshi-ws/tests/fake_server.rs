//! End-to-end smoke test against an in-process WebSocket server.
//!
//! Exercises:
//! - subscribe → server ack with assigned `sid`
//! - typed `Subscription<Ticker>` receives a server-pushed frame
//! - `Subscription::Drop` issues an `unsubscribe` command
//! - Subscribed acks WITHOUT echoed `id` (Kalshi's real wire shape) are matched
//!   to the pending subscribe via FIFO.
//! - String-encoded price levels (`["0.0100","33413.00"]`) deserialize correctly.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use kalshi_ws::{Client, Environment, OrderbookEvent, ReconnectPolicy};

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

/// Regression: Kalshi production sends `subscribed` acks WITHOUT echoing `id`.
/// The client must still resolve the subscribe via the FIFO pending-subscribes
/// queue, and the orderbook snapshot must accept string-encoded price levels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribed_without_id_and_string_encoded_levels() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();

        // Read subscribe command (we don't care about the req_id since we
        // deliberately won't echo it).
        let frame = ws.next().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        assert_eq!(v["cmd"], "subscribe");
        assert_eq!(v["params"]["channels"][0], "orderbook_delta");

        // Send Subscribed ack with NO `id` field — matches Kalshi production wire.
        let ack = json!({
            "type": "subscribed",
            "msg": { "channel": "orderbook_delta", "sid": 7 }
        });
        ws.send(Message::Text(ack.to_string().into())).await.unwrap();

        // Push an orderbook snapshot with string-encoded fixed-point levels.
        let snap = json!({
            "type": "orderbook_snapshot",
            "sid": 7,
            "seq": 1,
            "msg": {
                "market_ticker": "KX-FAKE",
                "yes_dollars_fp": [["0.6500", "120.00"], ["0.6400", "180.00"]],
                "no_dollars_fp":  [["0.3400", "200.00"], ["0.3300", "150.00"]]
            }
        });
        ws.send(Message::Text(snap.to_string().into())).await.unwrap();

        // Wait briefly so the client can drain the snapshot before we close.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = ws.close(None).await;
    });

    let client = Client::builder()
        .environment(Environment::Custom {
            url: format!("ws://127.0.0.1:{port}/test"),
            path: "/test".into(),
        })
        .reconnect(kalshi_ws::ReconnectPolicy {
            // Disable reconnect so the test doesn't loop after the server closes.
            enabled: false,
            ..Default::default()
        })
        .connect()
        .await
        .expect("client connects");

    let mut sub = client
        .subscribe_orderbook(vec!["KX-FAKE".into()])
        .await
        .expect("subscribe completes despite missing id");

    let evt = tokio::time::timeout(Duration::from_secs(3), sub.next())
        .await
        .expect("snapshot arrives")
        .expect("stream not closed");

    match evt {
        OrderbookEvent::Snapshot { snapshot, .. } => {
            assert_eq!(snapshot.market_ticker, "KX-FAKE");
            // String-encoded levels parsed correctly.
            assert_eq!(snapshot.yes_dollars_fp.len(), 2);
            assert!((snapshot.yes_dollars_fp[0].0 - 0.65).abs() < 1e-9);
            assert_eq!(snapshot.yes_dollars_fp[0].1, 120);
            assert_eq!(snapshot.no_dollars_fp[0].1, 200);
        }
        other => panic!("expected Snapshot first, got {other:?}"),
    }

    server.await.expect("server task completes");
    client.shutdown();
}

/// Regression: after the supervisor reconnects, replay must successfully
/// re-subscribe and the new sid must be wired up so frames on the second
/// connection still propagate. Earlier the supervisor `await`ed
/// `replay_subscriptions` *before* `run_session` started, so the replay's
/// subscribe-acks would time out (no one was reading the new ws), the
/// pending_subscribes queue would be cleaned up, and the eventually-arriving
/// Subscribed acks (production omits the echoed `id`) would land with nothing
/// to match — leaving the new sids unregistered and silently dropping every
/// data frame after the first reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_replays_subscription_and_resumes_orderbook_flow() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Server task: accept twice. First connection: ack the subscribe, push
    // one snapshot, then close. Second connection: ack the *replayed*
    // subscribe, push a different snapshot. We assert that both arrive on
    // the same Subscription handle.
    let server = tokio::spawn(async move {
        // ---- Connection #1 ----
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        let frame = ws.next().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        assert_eq!(v["cmd"], "subscribe");
        assert_eq!(v["params"]["channels"][0], "orderbook_delta");
        // Production-shape ack: no echoed `id`.
        let ack = json!({
            "type": "subscribed",
            "msg": { "channel": "orderbook_delta", "sid": 1 }
        });
        ws.send(Message::Text(ack.to_string().into())).await.unwrap();
        let snap = json!({
            "type": "orderbook_snapshot", "sid": 1, "seq": 1,
            "msg": {
                "market_ticker": "KX-RECONNECT",
                "yes_dollars_fp": [["0.6500", "100.00"]],
                "no_dollars_fp":  [["0.3400", "200.00"]]
            }
        });
        ws.send(Message::Text(snap.to_string().into())).await.unwrap();
        // Close the first connection — supervisor should reconnect.
        let _ = ws.close(None).await;
        drop(ws);

        // ---- Connection #2 ----
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        let frame = ws.next().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(&frame.into_text().unwrap()).unwrap();
        // The replay must have re-sent the subscribe with the same params.
        assert_eq!(v["cmd"], "subscribe");
        assert_eq!(v["params"]["channels"][0], "orderbook_delta");
        let ack = json!({
            "type": "subscribed",
            "msg": { "channel": "orderbook_delta", "sid": 2 }
        });
        ws.send(Message::Text(ack.to_string().into())).await.unwrap();
        // Push a second snapshot on the new sid. The fix is to forward this
        // through the same Subscription. Pre-fix, this frame was silently
        // dropped because the new sid had no dispatcher.
        let snap = json!({
            "type": "orderbook_snapshot", "sid": 2, "seq": 1,
            "msg": {
                "market_ticker": "KX-RECONNECT",
                "yes_dollars_fp": [["0.7000", "50.00"]],
                "no_dollars_fp":  [["0.2900", "75.00"]]
            }
        });
        ws.send(Message::Text(snap.to_string().into())).await.unwrap();
        // Hold the connection open briefly so the client can drain.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = ws.close(None).await;
    });

    let client = Client::builder()
        .environment(Environment::Custom {
            url: format!("ws://127.0.0.1:{port}/test"),
            path: "/test".into(),
        })
        .reconnect(ReconnectPolicy {
            enabled: true,
            max_attempts: Some(3),
            base_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(200),
            jitter: 0.0,
        })
        // Tighten the request timeout so a regression here would surface as a
        // test timeout in seconds, not the default 15.
        .request_timeout(Duration::from_secs(2))
        .connect()
        .await
        .expect("client connects");

    let mut sub = client
        .subscribe_orderbook(vec!["KX-RECONNECT".into()])
        .await
        .expect("first subscribe");

    // Frame from connection #1.
    let evt = tokio::time::timeout(Duration::from_secs(3), sub.next())
        .await
        .expect("snapshot #1 arrives")
        .expect("stream not closed");
    match evt {
        OrderbookEvent::Snapshot { snapshot, .. } => {
            assert_eq!(snapshot.yes_dollars_fp[0].1, 100);
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // Frame from connection #2 (must arrive after replay re-subscribed).
    let evt = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("snapshot #2 arrives after reconnect+replay")
        .expect("stream not closed");
    match evt {
        OrderbookEvent::Snapshot { snapshot, .. } => {
            assert_eq!(snapshot.yes_dollars_fp[0].1, 50);
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    server.await.expect("server task completes");
    client.shutdown();
}
