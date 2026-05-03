//! Verifies serde shapes for orders, positions, balance, and fills.
//! Each test pins a representative JSON sample against the typed struct.

use kalshi_rest::{
    Action, Balance, Fill, MarketPosition, Order, OrderRequest, OrderStatus, OrderType, Positions,
    SelfTradePreventionType, Side, TimeInForce,
};
use serde_json::json;

#[test]
fn order_request_serializes_with_required_fields_only() {
    let req = OrderRequest::buy_yes_limit("KX-EXAMPLE", 56, 10);
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["ticker"], "KX-EXAMPLE");
    assert_eq!(v["side"], "yes");
    assert_eq!(v["action"], "buy");
    assert_eq!(v["count"], 10);
    assert_eq!(v["yes_price"], 56);
    assert!(v.get("client_order_id").is_some(), "client_order_id must always be sent");
    assert!(v["client_order_id"].as_str().unwrap().len() > 0);

    // None-valued fields should not appear on the wire.
    assert!(v.get("no_price").is_none());
    assert!(v.get("post_only").is_none());
    assert!(v.get("time_in_force").is_none());
}

#[test]
fn order_request_with_helpers() {
    let req = OrderRequest::sell_no_limit("KX-EX", 22, 5)
        .with_client_order_id("my-id-123")
        .with_time_in_force(TimeInForce::ImmediateOrCancel)
        .with_post_only(false);
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["client_order_id"], "my-id-123");
    assert_eq!(v["time_in_force"], "immediate_or_cancel");
    assert_eq!(v["post_only"], false);
    assert_eq!(v["side"], "no");
    assert_eq!(v["action"], "sell");
    assert_eq!(v["no_price"], 22);
}

#[test]
fn order_request_distinct_uuids_per_call() {
    let a = OrderRequest::buy_yes_limit("KX", 50, 1);
    let b = OrderRequest::buy_yes_limit("KX", 50, 1);
    assert_ne!(a.client_order_id, b.client_order_id);
}

#[test]
fn order_response_with_string_fixed_point() {
    // Kalshi sometimes serializes numerics as strings (FixedPointDollars).
    let v = json!({
        "order_id": "ord_1",
        "user_id": "u_1",
        "client_order_id": "coid_1",
        "ticker": "KX-FOO",
        "side": "yes",
        "action": "buy",
        "type": "limit",
        "status": "resting",
        "yes_price_dollars": "0.5600",
        "no_price_dollars": "0.4400",
        "fill_count_fp": "0",
        "remaining_count_fp": "10",
        "initial_count_fp": "10",
        "taker_fees_dollars": "0.00",
        "maker_fees_dollars": "0.00",
        "taker_fill_cost_dollars": "0.00",
        "maker_fill_cost_dollars": "0.00",
        "expiration_time": null,
        "created_time": "2026-05-03T12:00:00Z",
        "last_update_time": "2026-05-03T12:00:00Z",
        "self_trade_prevention_type": null,
        "order_group_id": null,
        "cancel_order_on_pause": false,
        "subaccount_number": 0
    });
    let order: Order = serde_json::from_value(v).expect("parse string-form order");
    assert_eq!(order.side, Side::Yes);
    assert_eq!(order.action, Action::Buy);
    assert_eq!(order.order_type, OrderType::Limit);
    assert_eq!(order.status, OrderStatus::Resting);
    assert!((order.yes_price_dollars.unwrap() - 0.56).abs() < 1e-9);
    assert_eq!(order.remaining_count_fp, Some(10));
}

#[test]
fn order_response_with_numeric_fixed_point() {
    let v = json!({
        "order_id": "ord_2",
        "user_id": "u_1",
        "client_order_id": "coid_2",
        "ticker": "KX-FOO",
        "side": "no",
        "action": "sell",
        "type": "limit",
        "status": "executed",
        "yes_price_dollars": 0.50,
        "no_price_dollars": 0.50,
        "fill_count_fp": 5,
        "remaining_count_fp": 0,
        "initial_count_fp": 5,
        "taker_fees_dollars": 0.01,
        "maker_fees_dollars": 0.0,
        "taker_fill_cost_dollars": 2.50,
        "maker_fill_cost_dollars": 0.0,
        "expiration_time": null,
        "created_time": null,
        "last_update_time": null,
        "self_trade_prevention_type": "maker",
        "order_group_id": null,
        "cancel_order_on_pause": false,
        "subaccount_number": null
    });
    let order: Order = serde_json::from_value(v).expect("parse numeric-form order");
    assert_eq!(order.status, OrderStatus::Executed);
    assert_eq!(
        order.self_trade_prevention_type,
        Some(SelfTradePreventionType::Maker)
    );
}

#[test]
fn positions_with_mixed_numeric_styles() {
    let v = json!({
        "market_positions": [
            {
                "ticker": "KX-A",
                "total_traded_dollars": "100.00",
                "position_fp": 10,
                "market_exposure_dollars": 56.0,
                "realized_pnl_dollars": "0.00",
                "fees_paid_dollars": "0.05",
                "last_updated_ts": "2026-05-03T12:00:00Z",
                "resting_orders_count": 0
            }
        ],
        "event_positions": [],
        "cursor": ""
    });
    let p: Positions = serde_json::from_value(v).expect("parse positions");
    assert_eq!(p.market_positions.len(), 1);
    let mp: &MarketPosition = &p.market_positions[0];
    assert_eq!(mp.ticker, "KX-A");
    assert_eq!(mp.position_fp, Some(10));
}

#[test]
fn balance_extra_fields_round_trip() {
    let v = json!({
        "balance_dollars": "1000.00",
        "payout_dollars": "950.00",
        "future_extension": "ok"
    });
    let b: Balance = serde_json::from_value(v).expect("parse balance");
    assert!((b.balance_dollars.unwrap() - 1000.0).abs() < 1e-9);
    assert!(b.extra.contains_key("future_extension"));
}

#[test]
fn fill_parses_minimal() {
    let v = json!({
        "trade_id": "t_1",
        "order_id": "o_1",
        "ticker": "KX-FOO",
        "side": "yes",
        "action": "buy",
        "is_taker": true,
        "count_fp": 1,
        "yes_price_dollars": "0.55",
        "no_price_dollars": "0.45",
        "created_time": "2026-05-03T12:00:00Z"
    });
    let f: Fill = serde_json::from_value(v).expect("parse fill");
    assert_eq!(f.is_taker, true);
    assert_eq!(f.count_fp, Some(1));
}
