//! REST client: builds requests, signs them with `Credentials::signed_headers`,
//! retries 429/5xx with exponential backoff, deserializes responses.

use std::time::Duration;

use kalshi_ws::Credentials;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::error::{RestError, Result};
use crate::types::{
    Balance, Fill, ListOrdersFilter, Order, OrderRequest, Page, Positions,
};

/// Where to send requests. `Production` hits real Kalshi; `Demo` hits the
/// sandbox; `Custom` is for tests and self-hosted relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Environment {
    Production,
    Demo,
    /// Custom base URL (no trailing slash). Used for tests against an
    /// in-process mock server.
    Custom(String),
}

impl Default for Environment {
    fn default() -> Self {
        Environment::Production
    }
}

impl Environment {
    pub fn base_url(&self) -> &str {
        match self {
            Environment::Production => "https://api.elections.kalshi.com",
            Environment::Demo => "https://demo-api.kalshi.co",
            Environment::Custom(url) => url,
        }
    }
}

/// Operating mode. `Live` allows destructive calls (place / cancel / amend);
/// `Paper` refuses them at the client layer with [`RestError::PaperRefused`].
///
/// Default is derived from the environment: Production ⇒ Live, Demo ⇒ Live
/// (sandbox is paper-equivalent), Custom ⇒ Live. To force a hard local
/// guard regardless of environment, set [`ClientBuilder::mode`] explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Paper,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: Environment,
    pub credentials: Credentials,
    pub mode: Mode,
    pub max_retries: u32,
    pub base_backoff: Duration,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ClientBuilder {
    environment: Environment,
    credentials: Option<Credentials>,
    mode: Option<Mode>,
    max_retries: u32,
    base_backoff: Duration,
    request_timeout: Duration,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            environment: Environment::Production,
            credentials: None,
            mode: None,
            max_retries: 5,
            base_backoff: Duration::from_millis(500),
            request_timeout: Duration::from_secs(15),
        }
    }
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn environment(mut self, env: Environment) -> Self {
        self.environment = env;
        self
    }

    pub fn credentials(mut self, creds: Credentials) -> Self {
        self.credentials = Some(creds);
        self
    }

    pub fn mode(mut self, mode: Mode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    pub fn base_backoff(mut self, d: Duration) -> Self {
        self.base_backoff = d;
        self
    }

    pub fn request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn build(self) -> Result<Client> {
        let credentials = self
            .credentials
            .ok_or_else(|| RestError::Auth("credentials not set".into()))?;
        let mode = self.mode.unwrap_or(Mode::Live);
        let http = reqwest::Client::builder()
            .user_agent(concat!("kalshi-rest/", env!("CARGO_PKG_VERSION")))
            .gzip(true)
            .timeout(self.request_timeout)
            .build()?;
        Ok(Client {
            http,
            config: Config {
                environment: self.environment,
                credentials,
                mode,
                max_retries: self.max_retries,
                base_backoff: self.base_backoff,
                request_timeout: self.request_timeout,
            },
        })
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    config: Config,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    pub fn mode(&self) -> Mode {
        self.config.mode
    }

    pub fn environment(&self) -> &Environment {
        &self.config.environment
    }

    // -- Orders --------------------------------------------------------------

    /// Place an order. Always sends a `client_order_id` (auto-generated if not
    /// set on `req`) so retries from the caller are safe — Kalshi dedupes on
    /// `client_order_id`.
    pub async fn place_order(&self, mut req: OrderRequest) -> Result<Order> {
        self.guard_paper("place_order")?;
        if req.client_order_id.is_empty() {
            req.client_order_id = crate::types::new_client_order_id();
        }
        debug!(
            ticker = %req.ticker,
            client_order_id = %req.client_order_id,
            "placing order"
        );
        let wrapper: OrderResponse = self
            .send_json(Method::POST, "/trade-api/v2/portfolio/orders", Some(&req))
            .await?;
        Ok(wrapper.order)
    }

    /// Cancel an open order by id. Idempotent on Kalshi's side — calling on
    /// an already-canceled or executed order returns the order in its terminal state.
    pub async fn cancel_order(&self, order_id: &str) -> Result<Order> {
        self.guard_paper("cancel_order")?;
        let path = format!("/trade-api/v2/portfolio/orders/{order_id}");
        let wrapper: OrderResponse = self
            .send_json::<(), _>(Method::DELETE, &path, None)
            .await?;
        Ok(wrapper.order)
    }

    /// Decrease the remaining size of an open order without canceling it.
    ///
    /// Kalshi's endpoint expects `{ "reduce_by_count": N }` or `{ "reduce_to_count": N }`.
    /// Caller picks one via [`DecreaseAmount`].
    pub async fn decrease_order(
        &self,
        order_id: &str,
        amount: DecreaseAmount,
    ) -> Result<Order> {
        self.guard_paper("decrease_order")?;
        let path = format!("/trade-api/v2/portfolio/orders/{order_id}/decrease");
        let body = match amount {
            DecreaseAmount::ReduceBy(n) => serde_json::json!({ "reduce_by_count": n }),
            DecreaseAmount::ReduceTo(n) => serde_json::json!({ "reduce_to_count": n }),
        };
        let wrapper: OrderResponse = self
            .send_json(Method::POST, &path, Some(&body))
            .await?;
        Ok(wrapper.order)
    }

    /// Fetch one order by id.
    pub async fn get_order(&self, order_id: &str) -> Result<Order> {
        let path = format!("/trade-api/v2/portfolio/orders/{order_id}");
        let wrapper: OrderResponse = self
            .send_json::<(), _>(Method::GET, &path, None)
            .await?;
        Ok(wrapper.order)
    }

    /// List orders. Pass [`ListOrdersFilter::default()`] for the unfiltered first page;
    /// keep calling with the returned cursor until it's `None`.
    pub async fn list_orders(&self, filter: &ListOrdersFilter) -> Result<Page<Order>> {
        let mut query: Vec<(&'static str, String)> = Vec::new();
        if let Some(t) = &filter.ticker {
            query.push(("ticker", t.clone()));
        }
        if let Some(t) = &filter.event_ticker {
            query.push(("event_ticker", t.clone()));
        }
        if let Some(s) = filter.status {
            query.push((
                "status",
                serde_json::to_value(s)?
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
        if let Some(t) = filter.min_ts {
            query.push(("min_ts", t.to_string()));
        }
        if let Some(t) = filter.max_ts {
            query.push(("max_ts", t.to_string()));
        }
        if let Some(l) = filter.limit {
            query.push(("limit", l.to_string()));
        }
        if let Some(c) = &filter.cursor {
            query.push(("cursor", c.clone()));
        }
        if let Some(s) = filter.subaccount {
            query.push(("subaccount", s.to_string()));
        }
        let body: ListOrdersResponse = self
            .send_query(Method::GET, "/trade-api/v2/portfolio/orders", &query)
            .await?;
        Ok(Page {
            items: body.orders,
            cursor: body.cursor.filter(|s| !s.is_empty()),
        })
    }

    // -- Portfolio reads -----------------------------------------------------

    pub async fn get_positions(&self, ticker: Option<&str>) -> Result<Positions> {
        let mut query: Vec<(&'static str, String)> = Vec::new();
        if let Some(t) = ticker {
            query.push(("ticker", t.to_string()));
        }
        self.send_query(Method::GET, "/trade-api/v2/portfolio/positions", &query)
            .await
    }

    pub async fn get_balance(&self) -> Result<Balance> {
        self.send_json::<(), _>(Method::GET, "/trade-api/v2/portfolio/balance", None)
            .await
    }

    /// Fetch the next page of historical fills. Pass `cursor: None` for the first page.
    pub async fn get_fills(
        &self,
        ticker: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Fill>> {
        let mut query: Vec<(&'static str, String)> = Vec::new();
        if let Some(t) = ticker {
            query.push(("ticker", t.to_string()));
        }
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        if let Some(l) = limit {
            query.push(("limit", l.to_string()));
        }
        let body: ListFillsResponse = self
            .send_query(Method::GET, "/trade-api/v2/portfolio/fills", &query)
            .await?;
        Ok(Page {
            items: body.fills,
            cursor: body.cursor.filter(|s| !s.is_empty()),
        })
    }

    // -- Internal: signed request + retry ------------------------------------

    fn guard_paper(&self, action: &'static str) -> Result<()> {
        if self.config.mode == Mode::Paper {
            return Err(RestError::PaperRefused { action });
        }
        Ok(())
    }

    async fn send_json<B, R>(&self, method: Method, path: &str, body: Option<&B>) -> Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        self.send::<B, R>(method, path, &[], body).await
    }

    async fn send_query<R>(
        &self,
        method: Method,
        path: &str,
        query: &[(&'static str, String)],
    ) -> Result<R>
    where
        R: DeserializeOwned,
    {
        self.send::<(), R>(method, path, query, None).await
    }

    async fn send<B, R>(
        &self,
        method: Method,
        path: &str,
        query: &[(&'static str, String)],
        body: Option<&B>,
    ) -> Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.config.environment.base_url(), path);
        let mut attempt: u32 = 0;
        loop {
            let mut req = self.http.request(method.clone(), &url);
            if !query.is_empty() {
                req = req.query(query);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            // Re-sign every attempt — signatures are timestamp-bound.
            for (name, value) in self.config.credentials.signed_headers(method.as_str(), path) {
                req = req.header(name, value);
            }

            let resp = req.send().await?;
            let status = resp.status();
            if status.is_success() {
                let bytes = resp.bytes().await?;
                return serde_json::from_slice::<R>(&bytes).map_err(RestError::from);
            }

            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if !retryable || attempt >= self.config.max_retries {
                let body_text = resp.text().await.unwrap_or_default();
                return Err(RestError::Server {
                    status: status.as_u16(),
                    body: body_text,
                });
            }

            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            let backoff = retry_after.unwrap_or_else(|| {
                self.config
                    .base_backoff
                    .saturating_mul(1u32 << attempt.min(5))
            });
            warn!(
                "{} {} -> {} (attempt {}/{}), backing off {:?}",
                method,
                path,
                status,
                attempt + 1,
                self.config.max_retries,
                backoff
            );
            // Drop the response body so the connection can be reused.
            drop(resp);
            sleep(backoff).await;
            attempt += 1;
        }
    }
}

/// Helper for [`Client::decrease_order`].
#[derive(Debug, Clone, Copy)]
pub enum DecreaseAmount {
    /// Reduce remaining count by this many contracts.
    ReduceBy(u64),
    /// Reduce remaining count *to* this many contracts (absolute target).
    ReduceTo(u64),
}

// -- Response wrappers --------------------------------------------------------

#[derive(serde::Deserialize)]
struct OrderResponse {
    order: Order,
}

#[derive(serde::Deserialize)]
struct ListOrdersResponse {
    #[serde(default)]
    orders: Vec<Order>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize)]
struct ListFillsResponse {
    #[serde(default)]
    fills: Vec<Fill>,
    cursor: Option<String>,
}

