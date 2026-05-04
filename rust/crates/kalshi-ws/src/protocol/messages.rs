//! Server-to-client message types.
//!
//! The wire format places a `type` discriminator at the top level of every server
//! frame, so [`ServerMessage`] uses `#[serde(tag = "type")]`. Outer routing fields
//! like `id`, `sid`, and `seq` live on the variant; the channel-specific payload
//! sits in a nested `msg` field.

use serde::{Deserialize, Serialize};

use super::channels::Side;

// Tolerant number-or-string deserializers live in `kalshi_common::serde_num`;
// the same helpers are also used by kalshi-rest for its REST payloads.
use kalshi_common::serde_num as num_serde;

/// All possible server frames, internally tagged on the `type` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Acknowledges a `subscribe` command and assigns a `sid`. Kalshi's
    /// production WS does not echo the request `id` here, so it's modeled as
    /// optional; matching falls back to FIFO over pending subscribe requests.
    Subscribed {
        #[serde(default)]
        id: Option<u64>,
        msg: SubscribedAck,
    },

    /// Generic success ack for `update_subscription`, etc.
    Ok {
        id: u64,
        sid: u64,
        seq: u64,
        #[serde(default)]
        msg: OkAck,
    },

    /// Acknowledges an `unsubscribe` command.
    Unsubscribed { id: u64, sid: u64, seq: u64 },

    /// Server-side error.
    Error {
        #[serde(default)]
        id: Option<u64>,
        msg: ServerError,
    },

    Ticker { sid: u64, msg: Ticker },
    Trade { sid: u64, msg: Trade },
    OrderbookSnapshot { sid: u64, seq: u64, msg: OrderbookSnapshot },
    OrderbookDelta { sid: u64, seq: u64, msg: OrderbookDelta },

    Fill { sid: u64, msg: Fill },
    UserOrder { sid: u64, msg: UserOrder },
    MarketPosition { sid: u64, msg: MarketPosition },

    MarketLifecycleV2 { sid: u64, msg: MarketLifecycleEvent },
    EventLifecycle { sid: u64, msg: EventLifecycle },
    MultivariateMarketLifecycle { sid: u64, msg: MarketLifecycleEvent },
    MultivariateLookup { sid: u64, msg: MultivariateLookup },

    OrderGroupUpdates {
        sid: u64,
        seq: u64,
        msg: OrderGroupUpdate,
    },

    RfqCreated { sid: u64, msg: RfqCreated },
    RfqDeleted { sid: u64, msg: RfqDeleted },
    QuoteCreated { sid: u64, msg: QuoteCreated },
    QuoteAccepted { sid: u64, msg: QuoteAccepted },
    QuoteExecuted { sid: u64, msg: QuoteExecuted },
}

impl ServerMessage {
    /// The subscription id this frame relates to, if any. Control frames like
    /// `Error` may not carry one.
    pub fn sid(&self) -> Option<u64> {
        match self {
            ServerMessage::Subscribed { msg, .. } => Some(msg.sid),
            ServerMessage::Ok { sid, .. }
            | ServerMessage::Unsubscribed { sid, .. }
            | ServerMessage::Ticker { sid, .. }
            | ServerMessage::Trade { sid, .. }
            | ServerMessage::OrderbookSnapshot { sid, .. }
            | ServerMessage::OrderbookDelta { sid, .. }
            | ServerMessage::Fill { sid, .. }
            | ServerMessage::UserOrder { sid, .. }
            | ServerMessage::MarketPosition { sid, .. }
            | ServerMessage::MarketLifecycleV2 { sid, .. }
            | ServerMessage::EventLifecycle { sid, .. }
            | ServerMessage::MultivariateMarketLifecycle { sid, .. }
            | ServerMessage::MultivariateLookup { sid, .. }
            | ServerMessage::OrderGroupUpdates { sid, .. }
            | ServerMessage::RfqCreated { sid, .. }
            | ServerMessage::RfqDeleted { sid, .. }
            | ServerMessage::QuoteCreated { sid, .. }
            | ServerMessage::QuoteAccepted { sid, .. }
            | ServerMessage::QuoteExecuted { sid, .. } => Some(*sid),
            ServerMessage::Error { .. } => None,
        }
    }

    pub fn seq(&self) -> Option<u64> {
        match self {
            ServerMessage::Ok { seq, .. }
            | ServerMessage::Unsubscribed { seq, .. }
            | ServerMessage::OrderbookSnapshot { seq, .. }
            | ServerMessage::OrderbookDelta { seq, .. }
            | ServerMessage::OrderGroupUpdates { seq, .. } => Some(*seq),
            _ => None,
        }
    }

    /// True for non-data control frames (subscribed/ok/unsubscribed/error).
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            ServerMessage::Subscribed { .. }
                | ServerMessage::Ok { .. }
                | ServerMessage::Unsubscribed { .. }
                | ServerMessage::Error { .. }
        )
    }

    /// Request id for control frames that carry one. `Subscribed` and `Error`
    /// may legitimately have no id on the wire — matching falls back to
    /// FIFO/sid-based lookup elsewhere in the client.
    pub fn request_id(&self) -> Option<u64> {
        match self {
            ServerMessage::Ok { id, .. } | ServerMessage::Unsubscribed { id, .. } => Some(*id),
            ServerMessage::Subscribed { id, .. } => *id,
            ServerMessage::Error { id, .. } => *id,
            _ => None,
        }
    }
}

// -- Control payloads ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribedAck {
    pub channel: String,
    pub sid: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OkAck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_tickers: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerError {
    pub code: u8,
    pub msg: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_ticker: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,
}

// -- Public market data -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub market_ticker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_bid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_ask_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub volume_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub open_interest_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub dollar_volume: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub dollar_open_interest: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub yes_bid_size_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub yes_ask_size_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub last_trade_size_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub ts_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub no_price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub count_fp: i64,
    pub taker_side: Side,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub ts_ms: i64,
}

/// One price level: `[price_dollars, count_fp]`.
pub type PriceLevel = (f64, i64);

/// Tolerant deserialization for `[price, size]` price-level arrays. Reuses
/// the [`num_serde`] number-or-string parsers for each tuple element.
mod price_levels_serde {
    use kalshi_common::serde_num::{to_f64, to_i64, NumOrStr};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, D>(d: D) -> Result<Vec<(f64, i64)>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Vec<(NumOrStr, NumOrStr)> = Vec::deserialize(d)?;
        raw.into_iter()
            .map(|(p, s)| {
                let price = to_f64::<D::Error>(p)?;
                let size = to_i64::<D::Error>(s)?;
                Ok((price, size))
            })
            .collect()
    }

    #[allow(clippy::ptr_arg)]
    pub fn serialize<S>(levels: &Vec<(f64, i64)>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        levels.serialize(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookSnapshot {
    pub market_ticker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,
    #[serde(default, with = "price_levels_serde")]
    pub yes_dollars_fp: Vec<PriceLevel>,
    #[serde(default, with = "price_levels_serde")]
    pub no_dollars_fp: Vec<PriceLevel>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookDelta {
    pub market_ticker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub delta_fp: i64,
    pub side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subaccount: Option<String>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub ts_ms: Option<i64>,
}

/// Combined orderbook stream: one snapshot followed by deltas.
#[derive(Debug, Clone)]
pub enum OrderbookEvent {
    Snapshot { seq: u64, snapshot: OrderbookSnapshot },
    Delta { seq: u64, delta: OrderbookDelta },
}

// -- Private user data --------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub trade_id: String,
    pub order_id: String,
    pub market_ticker: String,
    pub is_taker: bool,
    pub side: Side,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub count_fp: i64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub fee_cost: f64,
    pub action: FillAction,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub ts_ms: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub post_position_fp: i64,
    pub purchased_side: Side,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subaccount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FillAction {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOrder {
    pub order_id: String,
    pub user_id: String,
    pub ticker: String,
    pub status: UserOrderStatus,
    pub side: Side,
    pub is_yes: bool,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_price_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub fill_count_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub remaining_count_fp: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub initial_count_fp: i64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub taker_fill_cost_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub maker_fill_cost_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub taker_fees_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub maker_fees_dollars: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub created_ts_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_trade_prevention_type: Option<String>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub last_updated_ts_ms: Option<i64>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub expiration_ts_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subaccount_number: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserOrderStatus {
    Resting,
    Canceled,
    Executed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPosition {
    pub user_id: String,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub position_fp: i64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub position_cost_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub realized_pnl_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub fees_paid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub position_fee_cost_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub volume_fp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subaccount: Option<String>,
}

// -- Lifecycle events ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketLifecycleEvent {
    pub event_type: MarketLifecycleEventType,
    pub market_ticker: String,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub open_ts: Option<i64>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub close_ts: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub determination_ts: Option<i64>,
    #[serde(default, deserialize_with = "num_serde::as_f64_opt")]
    pub settlement_value: Option<f64>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub settled_ts: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_deactivated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_level_structure: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketLifecycleEventType {
    Created,
    Activated,
    Deactivated,
    CloseDateUpdated,
    Determined,
    Settled,
    PriceLevelStructureUpdated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLifecycle {
    pub event_ticker: String,
    pub title: String,
    pub subtitle: String,
    pub collateral_return_type: String,
    pub series_ticker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultivariateLookup {
    pub collection_ticker: String,
    pub event_ticker: String,
    pub market_ticker: String,
    pub selected_markets: Vec<SelectedMarket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedMarket {
    pub event_ticker: String,
    pub market_ticker: String,
    pub side: Side,
}

// -- Order group updates ------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderGroupUpdate {
    pub event_type: OrderGroupUpdateType,
    pub order_group_id: String,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub contracts_limit_fp: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderGroupUpdateType {
    Created,
    Triggered,
    Reset,
    Deleted,
    LimitUpdated,
}

// -- Communications channel (RFQs / quotes) -----------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqCreated {
    pub id: String,
    pub creator_id: String,
    pub market_ticker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_ticker: Option<String>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub contracts_count_fp: Option<i64>,
    #[serde(default, deserialize_with = "num_serde::as_f64_opt")]
    pub yes_bid_dollars: Option<f64>,
    #[serde(default, deserialize_with = "num_serde::as_f64_opt")]
    pub no_bid_dollars: Option<f64>,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub created_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqDeleted {
    pub id: String,
    pub creator_id: String,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub created_ts: i64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub deleted_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteCreated {
    pub quote_id: String,
    pub rfq_id: String,
    pub quote_creator_id: String,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_bid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub no_bid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub created_ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteAccepted {
    pub quote_id: String,
    pub rfq_id: String,
    pub quote_creator_id: String,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub yes_bid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_f64")]
    pub no_bid_dollars: f64,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub created_ts: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_side: Option<Side>,
    #[serde(default, deserialize_with = "num_serde::as_i64_opt")]
    pub contracts_accepted_fp: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteExecuted {
    pub quote_id: String,
    pub rfq_id: String,
    pub quote_creator_id: String,
    pub rfq_creator_id: String,
    pub order_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    pub market_ticker: String,
    #[serde(deserialize_with = "num_serde::as_i64")]
    pub executed_ts: i64,
}

/// Unified stream of all communications-channel events.
#[derive(Debug, Clone)]
pub enum CommunicationEvent {
    RfqCreated(RfqCreated),
    RfqDeleted(RfqDeleted),
    QuoteCreated(QuoteCreated),
    QuoteAccepted(QuoteAccepted),
    QuoteExecuted(QuoteExecuted),
}

/// Unified stream for the `market_lifecycle_v2` channel — both market and event events.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    Market(MarketLifecycleEvent),
    Event(EventLifecycle),
}
