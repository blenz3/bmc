//! Async Rust client for the Kalshi prediction market WebSocket API.
//!
//! Two layers:
//!
//! - **Low-level**: [`connection::connect`] returns a raw split sink/stream of
//!   tungstenite messages, and the [`protocol`] module provides typed
//!   serde-(de)serializable command and message enums. Use this when you need
//!   full control.
//! - **High-level**: [`Client`] owns the connection, multiplexes server frames
//!   by subscription id, and exposes one typed [`Subscription`] stream per
//!   `subscribe_*` method. It auto-reconnects with exponential backoff,
//!   replays subscriptions, and surfaces sequence-gap and reconnect events on
//!   a [`tokio::sync::broadcast`] channel.
//!
//! See the `examples/` directory for runnable usage.

pub mod auth;
pub mod connection;
pub mod error;
pub mod protocol;
pub mod subscription;

mod client;

pub use auth::Credentials;
pub use client::{Client, ClientBuilder, Config, ReconnectPolicy};
pub use connection::Environment;
pub use error::{ErrorCode, KalshiError, Result};
pub use subscription::{Subscription, SubscriptionId, SystemEvent};

pub use protocol::channels::{Channel, Side};
pub use protocol::commands::{ClientCommand, UpdateAction};
pub use protocol::messages::{
    CommunicationEvent, EventLifecycle, Fill, FillAction, LifecycleEvent, MarketLifecycleEvent,
    MarketLifecycleEventType, MarketPosition, MultivariateLookup, OrderbookDelta, OrderbookEvent,
    OrderbookSnapshot, OrderGroupUpdate, OrderGroupUpdateType, QuoteAccepted, QuoteCreated,
    QuoteExecuted, RfqCreated, RfqDeleted, ServerMessage, Ticker, Trade, UserOrder,
    UserOrderStatus,
};
