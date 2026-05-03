use serde::{Deserialize, Serialize};

use super::channels::Channel;

/// A command sent from the client to the Kalshi server.
///
/// Serialized with the `cmd` field as the discriminator, matching Kalshi's wire format:
///
/// ```json
/// { "id": 1, "cmd": "subscribe", "params": { "channels": ["ticker"], ... } }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ClientCommand {
    Subscribe { id: u64, params: SubscribeParams },
    Unsubscribe { id: u64, params: UnsubscribeParams },
    UpdateSubscription { id: u64, params: UpdateSubscriptionParams },
    ListSubscriptions { id: u64 },
}

impl ClientCommand {
    pub fn id(&self) -> u64 {
        match self {
            ClientCommand::Subscribe { id, .. }
            | ClientCommand::Unsubscribe { id, .. }
            | ClientCommand::UpdateSubscription { id, .. }
            | ClientCommand::ListSubscriptions { id } => *id,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubscribeParams {
    pub channels: Vec<Channel>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_ticker: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_tickers: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_ids: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_initial_snapshot: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_ticker_ack: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub shard_factor: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub shard_key: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeParams {
    pub sids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSubscriptionParams {
    pub sid: u64,
    pub action: UpdateAction,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_ticker: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_tickers: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_ids: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_initial_snapshot: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    AddMarkets,
    DeleteMarkets,
    GetSnapshot,
}
