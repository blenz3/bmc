//! Verifies that every `ServerMessage` variant deserializes from a representative
//! JSON sample. Catches `#[serde(tag)]`/`rename_all` mistakes and field typos
//! before they hit the wire.

use kalshi_ws::ServerMessage;
use serde_json::json;

fn assert_parses(value: serde_json::Value) {
    let s = value.to_string();
    let parsed: ServerMessage =
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("parse failed for {s}: {e}"));
    let _reser = serde_json::to_value(&parsed).expect("reserialize");
}

#[test]
fn parses_subscribed_ack() {
    assert_parses(json!({
        "id": 1,
        "type": "subscribed",
        "msg": { "channel": "ticker", "sid": 42 }
    }));
}

#[test]
fn parses_ok_with_optional_fields() {
    assert_parses(json!({
        "id": 2, "sid": 7, "seq": 1, "type": "ok", "msg": {}
    }));
    assert_parses(json!({
        "id": 2, "sid": 7, "seq": 1, "type": "ok",
        "msg": { "market_tickers": ["FOO", "BAR"] }
    }));
}

#[test]
fn parses_unsubscribed() {
    assert_parses(json!({
        "id": 9, "sid": 7, "seq": 12, "type": "unsubscribed"
    }));
}

#[test]
fn parses_error_with_and_without_id() {
    assert_parses(json!({
        "id": 123, "type": "error",
        "msg": { "code": 2, "msg": "Params required" }
    }));
    assert_parses(json!({
        "type": "error",
        "msg": { "code": 17, "msg": "Internal error" }
    }));
}

#[test]
fn parses_ticker() {
    assert_parses(json!({
        "type": "ticker",
        "sid": 1,
        "msg": {
            "market_ticker": "KX-DJT",
            "market_id": "abc",
            "price_dollars": 0.55,
            "yes_bid_dollars": 0.54,
            "yes_ask_dollars": 0.56,
            "volume_fp": 12345,
            "open_interest_fp": 6789,
            "dollar_volume": 1000,
            "dollar_open_interest": 2000,
            "yes_bid_size_fp": 100,
            "yes_ask_size_fp": 200,
            "last_trade_size_fp": 50,
            "ts_ms": 1714512345678i64
        }
    }));
}

#[test]
fn parses_trade() {
    assert_parses(json!({
        "type": "trade", "sid": 1,
        "msg": {
            "trade_id": "t-1",
            "market_ticker": "KX-DJT",
            "yes_price_dollars": 0.55,
            "no_price_dollars": 0.45,
            "count_fp": 100,
            "taker_side": "yes",
            "ts_ms": 1
        }
    }));
}

#[test]
fn parses_orderbook_snapshot_and_delta() {
    assert_parses(json!({
        "type": "orderbook_snapshot", "sid": 1, "seq": 1,
        "msg": {
            "market_ticker": "KX-DJT",
            "yes_dollars_fp": [[0.55, 100], [0.56, 200]],
            "no_dollars_fp": [[0.44, 50]]
        }
    }));
    assert_parses(json!({
        "type": "orderbook_delta", "sid": 1, "seq": 2,
        "msg": {
            "market_ticker": "KX-DJT",
            "price_dollars": 0.55,
            "delta_fp": -50,
            "side": "yes",
            "ts_ms": 1
        }
    }));
}

#[test]
fn parses_fill() {
    assert_parses(json!({
        "type": "fill", "sid": 1,
        "msg": {
            "trade_id": "t1",
            "order_id": "o1",
            "market_ticker": "M",
            "is_taker": true,
            "side": "yes",
            "yes_price_dollars": 0.5,
            "count_fp": 10,
            "fee_cost": 0.01,
            "action": "buy",
            "ts_ms": 1,
            "post_position_fp": 10,
            "purchased_side": "yes"
        }
    }));
}

#[test]
fn parses_user_order() {
    assert_parses(json!({
        "type": "user_order", "sid": 1,
        "msg": {
            "order_id": "o1",
            "user_id": "u1",
            "ticker": "M",
            "status": "resting",
            "side": "yes",
            "is_yes": true,
            "yes_price_dollars": 0.5,
            "fill_count_fp": 0,
            "remaining_count_fp": 100,
            "initial_count_fp": 100,
            "taker_fill_cost_dollars": 0.0,
            "maker_fill_cost_dollars": 0.0,
            "taker_fees_dollars": 0.0,
            "maker_fees_dollars": 0.0,
            "created_ts_ms": 1
        }
    }));
}

#[test]
fn parses_market_position() {
    assert_parses(json!({
        "type": "market_position", "sid": 1,
        "msg": {
            "user_id": "u",
            "market_ticker": "M",
            "position_fp": 10,
            "position_cost_dollars": 5.0,
            "realized_pnl_dollars": 0.0,
            "fees_paid_dollars": 0.01,
            "position_fee_cost_dollars": 0.01,
            "volume_fp": 10
        }
    }));
}

#[test]
fn parses_market_lifecycle_v2() {
    assert_parses(json!({
        "type": "market_lifecycle_v2", "sid": 1,
        "msg": {
            "event_type": "settled",
            "market_ticker": "M",
            "result": "yes",
            "settlement_value": 1.0
        }
    }));
}

#[test]
fn parses_event_lifecycle() {
    assert_parses(json!({
        "type": "event_lifecycle", "sid": 1,
        "msg": {
            "event_ticker": "E",
            "title": "T",
            "subtitle": "S",
            "collateral_return_type": "binary",
            "series_ticker": "SR"
        }
    }));
}

#[test]
fn parses_multivariate_lookup() {
    assert_parses(json!({
        "type": "multivariate_lookup", "sid": 1,
        "msg": {
            "collection_ticker": "C",
            "event_ticker": "E",
            "market_ticker": "M",
            "selected_markets": [
                {"event_ticker": "E1", "market_ticker": "M1", "side": "yes"}
            ]
        }
    }));
}

#[test]
fn parses_order_group_updates() {
    assert_parses(json!({
        "type": "order_group_updates", "sid": 1, "seq": 1,
        "msg": {
            "event_type": "created",
            "order_group_id": "og1",
            "contracts_limit_fp": 100
        }
    }));
}

#[test]
fn parses_communication_events() {
    assert_parses(json!({
        "type": "rfq_created", "sid": 1,
        "msg": {
            "id": "r1",
            "creator_id": "u1",
            "market_ticker": "M",
            "created_ts": 1
        }
    }));
    assert_parses(json!({
        "type": "rfq_deleted", "sid": 1,
        "msg": {
            "id": "r1",
            "creator_id": "u1",
            "market_ticker": "M",
            "created_ts": 1,
            "deleted_ts": 2
        }
    }));
    assert_parses(json!({
        "type": "quote_created", "sid": 1,
        "msg": {
            "quote_id": "q1",
            "rfq_id": "r1",
            "quote_creator_id": "u2",
            "market_ticker": "M",
            "yes_bid_dollars": 0.5,
            "no_bid_dollars": 0.5,
            "created_ts": 1
        }
    }));
    assert_parses(json!({
        "type": "quote_accepted", "sid": 1,
        "msg": {
            "quote_id": "q1",
            "rfq_id": "r1",
            "quote_creator_id": "u2",
            "market_ticker": "M",
            "yes_bid_dollars": 0.5,
            "no_bid_dollars": 0.5,
            "created_ts": 1,
            "accepted_side": "yes"
        }
    }));
    assert_parses(json!({
        "type": "quote_executed", "sid": 1,
        "msg": {
            "quote_id": "q1",
            "rfq_id": "r1",
            "quote_creator_id": "u2",
            "rfq_creator_id": "u1",
            "order_id": "o1",
            "market_ticker": "M",
            "executed_ts": 1
        }
    }));
}
