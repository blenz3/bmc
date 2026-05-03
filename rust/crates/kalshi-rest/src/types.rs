//! Wire types for Kalshi's REST trading API.
//!
//! Naming follows Kalshi's exact field names so serde derive does the right
//! thing without `#[serde(rename = ...)]` clutter.
//!
//! Numeric conventions (carried over from kalshi-ws):
//! - `_dollars` fields are dollar amounts. Kalshi sometimes serializes these
//!   as JSON numbers, sometimes as strings (their `FixedPointDollars` type).
//!   We deserialize both into `f64` via [`f64_or_str`] for ergonomic math.
//! - `_fp` fields are fixed-point integers (whole-contract counts), parsed
//!   from either JSON number or string into `i64`.

use kalshi_ws::Side;
use serde::{Deserialize, Deserializer, Serialize};

// -- Enums --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Resting,
    Canceled,
    Executed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    FillOrKill,
    GoodTillCanceled,
    ImmediateOrCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfTradePreventionType {
    TakerAtCross,
    Maker,
}

// -- Order request ------------------------------------------------------------

/// Body for `POST /portfolio/orders`.
///
/// At least one of (`yes_price`, `no_price`) is required for a limit order;
/// market orders omit price and rely on `buy_max_cost`.
///
/// `client_order_id` is *always* sent. The [`Client::place_order`](crate::Client::place_order)
/// helper auto-generates a UUID v4 if you don't supply one — Kalshi dedupes
/// on this field, so retries are safe.
#[derive(Debug, Clone, Serialize)]
pub struct OrderRequest {
    pub ticker: String,
    pub side: Side,
    pub action: Action,

    pub client_order_id: String,

    /// Whole-contract count. Use `count_fp` for fractional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,

    /// Fixed-point string form of count. Mutually exclusive with `count`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count_fp: Option<String>,

    /// 1..=99. Limit price for the YES side, in cents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yes_price: Option<u8>,

    /// 1..=99. Limit price for the NO side, in cents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_price: Option<u8>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<TimeInForce>,

    /// Maximum cost in cents. Setting this implicitly applies fill-or-kill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_max_cost: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_only: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce_only: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_trade_prevention_type: Option<SelfTradePreventionType>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_group_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_order_on_pause: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_ts: Option<i64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub subaccount: Option<u32>,
}

impl OrderRequest {
    /// Limit order to **buy** YES at a given price (in cents, 1..=99) and contract count.
    pub fn buy_yes_limit(ticker: impl Into<String>, yes_price_cents: u8, count: u64) -> Self {
        Self {
            ticker: ticker.into(),
            side: Side::Yes,
            action: Action::Buy,
            client_order_id: new_client_order_id(),
            count: Some(count),
            yes_price: Some(yes_price_cents),
            ..Self::default_skeleton()
        }
    }

    /// Limit order to **buy** NO at a given price (in cents, 1..=99).
    pub fn buy_no_limit(ticker: impl Into<String>, no_price_cents: u8, count: u64) -> Self {
        Self {
            ticker: ticker.into(),
            side: Side::No,
            action: Action::Buy,
            client_order_id: new_client_order_id(),
            count: Some(count),
            no_price: Some(no_price_cents),
            ..Self::default_skeleton()
        }
    }

    /// Limit order to **sell** an existing YES position.
    pub fn sell_yes_limit(ticker: impl Into<String>, yes_price_cents: u8, count: u64) -> Self {
        Self {
            ticker: ticker.into(),
            side: Side::Yes,
            action: Action::Sell,
            client_order_id: new_client_order_id(),
            count: Some(count),
            yes_price: Some(yes_price_cents),
            ..Self::default_skeleton()
        }
    }

    /// Limit order to **sell** an existing NO position.
    pub fn sell_no_limit(ticker: impl Into<String>, no_price_cents: u8, count: u64) -> Self {
        Self {
            ticker: ticker.into(),
            side: Side::No,
            action: Action::Sell,
            client_order_id: new_client_order_id(),
            count: Some(count),
            no_price: Some(no_price_cents),
            ..Self::default_skeleton()
        }
    }

    pub fn with_client_order_id(mut self, id: impl Into<String>) -> Self {
        self.client_order_id = id.into();
        self
    }

    pub fn with_time_in_force(mut self, tif: TimeInForce) -> Self {
        self.time_in_force = Some(tif);
        self
    }

    pub fn with_post_only(mut self, post_only: bool) -> Self {
        self.post_only = Some(post_only);
        self
    }

    pub fn with_reduce_only(mut self, reduce_only: bool) -> Self {
        self.reduce_only = Some(reduce_only);
        self
    }

    pub fn with_buy_max_cost_cents(mut self, cents: u64) -> Self {
        self.buy_max_cost = Some(cents);
        self
    }

    pub fn with_subaccount(mut self, subaccount: u32) -> Self {
        self.subaccount = Some(subaccount);
        self
    }

    fn default_skeleton() -> Self {
        Self {
            ticker: String::new(),
            side: Side::Yes,
            action: Action::Buy,
            client_order_id: String::new(),
            count: None,
            count_fp: None,
            yes_price: None,
            no_price: None,
            time_in_force: None,
            buy_max_cost: None,
            post_only: None,
            reduce_only: None,
            self_trade_prevention_type: None,
            order_group_id: None,
            cancel_order_on_pause: None,
            expiration_ts: None,
            subaccount: None,
        }
    }
}

pub fn new_client_order_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// -- Order response -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub user_id: String,
    pub client_order_id: String,
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub status: OrderStatus,

    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub yes_price_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub no_price_dollars: Option<f64>,

    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub fill_count_fp: Option<i64>,
    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub remaining_count_fp: Option<i64>,
    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub initial_count_fp: Option<i64>,

    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub taker_fees_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub maker_fees_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub taker_fill_cost_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub maker_fill_cost_dollars: Option<f64>,

    pub expiration_time: Option<String>,
    pub created_time: Option<String>,
    pub last_update_time: Option<String>,

    pub self_trade_prevention_type: Option<SelfTradePreventionType>,
    pub order_group_id: Option<String>,
    pub cancel_order_on_pause: Option<bool>,
    pub subaccount_number: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListOrdersFilter {
    pub ticker: Option<String>,
    pub event_ticker: Option<String>,
    pub status: Option<OrderStatus>,
    pub min_ts: Option<i64>,
    pub max_ts: Option<i64>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub subaccount: Option<u32>,
}

/// Generic paginated response: items + cursor for the next page.
/// Empty/missing cursor signals "no more pages".
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub cursor: Option<String>,
}

// -- Positions ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPosition {
    pub ticker: String,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub total_traded_dollars: Option<f64>,
    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub position_fp: Option<i64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub market_exposure_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub realized_pnl_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub fees_paid_dollars: Option<f64>,
    pub last_updated_ts: Option<String>,
    pub resting_orders_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPosition {
    pub event_ticker: String,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub total_cost_dollars: Option<f64>,
    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub total_cost_shares_fp: Option<i64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub event_exposure_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub realized_pnl_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub fees_paid_dollars: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Positions {
    #[serde(default)]
    pub market_positions: Vec<MarketPosition>,
    #[serde(default)]
    pub event_positions: Vec<EventPosition>,
    pub cursor: Option<String>,
}

// -- Balance ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    /// Available balance in dollars.
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub balance_dollars: Option<f64>,

    /// Withdrawable balance, after holds.
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub payout_dollars: Option<f64>,

    /// Capture any extra fields Kalshi adds without breaking deserialization.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// -- Fills --------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub trade_id: String,
    pub order_id: String,
    pub ticker: String,
    pub side: Side,
    pub action: Action,
    pub is_taker: bool,

    #[serde(default, deserialize_with = "i64_or_str_opt")]
    pub count_fp: Option<i64>,

    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub yes_price_dollars: Option<f64>,
    #[serde(default, deserialize_with = "f64_or_str_opt")]
    pub no_price_dollars: Option<f64>,

    pub created_time: Option<String>,
}

// -- Number-or-string deserializers ------------------------------------------
//
// Kalshi's `FixedPointDollars` / `FixedPointCount` types serialize as JSON
// strings on some endpoints, plain numbers on others. These helpers tolerate
// either rather than forcing the caller to handle two shapes.

fn f64_or_str_opt<'de, D>(d: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Num(f64),
        Str(String),
    }
    Option::<Either>::deserialize(d)?
        .map(|e| match e {
            Either::Num(n) => Ok(n),
            Either::Str(s) => s.parse::<f64>().map_err(serde::de::Error::custom),
        })
        .transpose()
}

fn i64_or_str_opt<'de, D>(d: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Num(i64),
        Str(String),
    }
    Option::<Either>::deserialize(d)?
        .map(|e| match e {
            Either::Num(n) => Ok(n),
            // Tolerate `"10.00"` style fixed-point by truncating fractional part.
            Either::Str(s) => s
                .split('.')
                .next()
                .unwrap_or("0")
                .parse::<i64>()
                .map_err(serde::de::Error::custom),
        })
        .transpose()
}
