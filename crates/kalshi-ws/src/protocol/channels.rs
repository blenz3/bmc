use serde::{Deserialize, Serialize};

/// Kalshi WebSocket channel names. Serialized as the exact strings the server expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Ticker,
    Trade,
    OrderbookDelta,
    Fill,
    UserOrders,
    MarketPositions,
    MarketLifecycleV2,
    MultivariateMarketLifecycle,
    Multivariate,
    Communications,
    OrderGroupUpdates,
}

impl Channel {
    /// Whether subscribing to this channel requires authenticated headers.
    pub fn requires_auth(self) -> bool {
        matches!(
            self,
            Channel::Fill
                | Channel::UserOrders
                | Channel::MarketPositions
                | Channel::Communications
                | Channel::OrderGroupUpdates
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuySell {
    Buy,
    Sell,
}
