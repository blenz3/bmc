//! Async Rust client for the Kalshi REST trading API.
//!
//! Covers order placement / management and read-only portfolio reporting:
//! create, list, get, cancel, decrease, amend orders; query positions,
//! balance, and fills.
//!
//! Auth is mandatory for every endpoint here — Kalshi's portfolio routes
//! require RSA-PSS signed requests. We reuse [`kalshi_ws::Credentials`] so
//! one credential type drives both the WS feed and the REST client.
//!
//! ## Safety posture
//!
//! Order placement is the dangerous side. The client deliberately:
//!
//! - Always populates a `client_order_id`. If the caller doesn't supply one,
//!   we generate a UUID v4. This makes retries safe — Kalshi dedupes on
//!   `client_order_id`, so a retried POST after a network blip won't
//!   double-submit.
//! - Defaults to [`Mode::Live`] only when constructed with [`Environment::Production`].
//!   [`Environment::Demo`] forces a "paper" mode posture; reach for it during
//!   development.
//! - Retries 429 / 5xx with exponential backoff *but never on 2xx-shaped
//!   ambiguity* — once Kalshi accepts an order, we don't re-send.

pub mod client;
pub mod error;
pub mod types;

pub use client::{Client, ClientBuilder, DecreaseAmount, Environment, Mode};
pub use error::{Result, RestError};
pub use types::{
    Action, Balance, EventPosition, Fill, ListOrdersFilter, MarketPosition, Order, OrderRequest,
    OrderStatus, OrderType, Page, Positions, SelfTradePreventionType, TimeInForce,
};

// Re-export shared domain types so callers don't have to depend on kalshi-ws
// just for the Side enum.
pub use kalshi_ws::Side;
pub use kalshi_ws::Credentials;
