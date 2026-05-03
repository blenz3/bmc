//! High-level client: owns the connection, multiplexes server frames by
//! subscription id, and supervises auto-reconnect with subscription replay.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::auth::Credentials;
use crate::connection::{connect, Environment, WsStream};
use crate::error::{ErrorCode, KalshiError, Result};
use crate::protocol::channels::Channel;
use crate::protocol::commands::{
    ClientCommand, SubscribeParams, UpdateAction, UpdateSubscriptionParams,
};
use crate::protocol::messages::{
    CommunicationEvent, Fill, LifecycleEvent, MarketPosition, MultivariateLookup, OrderGroupUpdate,
    OrderbookEvent, ServerMessage, Ticker, Trade, UserOrder,
};
use crate::subscription::{IdGenerator, Subscription, SubscriptionId, SystemEvent};

// -- Configuration -----------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    pub environment: Environment,
    pub credentials: Option<Credentials>,
    pub reconnect: ReconnectPolicy,
    /// Buffer size for the writer task's command queue.
    pub command_buffer: usize,
    /// Buffer size for the system events broadcast channel.
    pub events_buffer: usize,
    /// Default per-subscription channel buffer when not specified per-subscribe call.
    pub default_subscription_buffer: usize,
    /// How long to wait for a `subscribed` ack before timing out a subscribe call.
    pub request_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            environment: Environment::Production,
            credentials: None,
            reconnect: ReconnectPolicy::default(),
            command_buffer: 256,
            events_buffer: 512,
            default_subscription_buffer: 1024,
            request_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    pub max_attempts: Option<u32>,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub jitter: f32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: None,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            jitter: 0.2,
        }
    }
}

impl ReconnectPolicy {
    fn delay_for(&self, attempt: u32) -> Duration {
        let exp = 2u64.saturating_pow(attempt.min(10));
        let base = self.base_backoff.saturating_mul(exp.min(u32::MAX as u64) as u32);
        let capped = base.min(self.max_backoff);
        let jitter_ms = (capped.as_millis() as f32 * self.jitter) as u64;
        let extra = if jitter_ms == 0 {
            0
        } else {
            // Cheap pseudo-jitter from system clock — we don't need crypto-grade.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0)
                % jitter_ms
        };
        capped + Duration::from_millis(extra)
    }
}

// -- Builder -----------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ClientBuilder {
    config: Config,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn environment(mut self, env: Environment) -> Self {
        self.config.environment = env;
        self
    }

    pub fn credentials(mut self, creds: Credentials) -> Self {
        self.config.credentials = Some(creds);
        self
    }

    pub fn reconnect(mut self, policy: ReconnectPolicy) -> Self {
        self.config.reconnect = policy;
        self
    }

    pub fn request_timeout(mut self, d: Duration) -> Self {
        self.config.request_timeout = d;
        self
    }

    pub fn command_buffer(mut self, n: usize) -> Self {
        self.config.command_buffer = n;
        self
    }

    pub fn default_subscription_buffer(mut self, n: usize) -> Self {
        self.config.default_subscription_buffer = n;
        self
    }

    pub async fn connect(self) -> Result<Client> {
        Client::connect_with(self.config).await
    }
}

// -- Internals ---------------------------------------------------------------

/// Closure-based dispatcher: takes a `ServerMessage` already routed to this sid
/// and forwards a typed payload to the per-subscription mpsc. The closure
/// captures the typed `Sender<T>` at `subscribe_*` time, so each subscription's
/// dispatcher is monomorphized to its channel type.
type Dispatcher = Arc<dyn Fn(&ServerMessage) + Send + Sync>;

#[derive(Clone)]
struct ReplaySpec {
    channel: Channel,
    params: SubscribeParams,
    /// Whether this subscription observed a non-zero seq since the last reset.
    /// Used to flag potential gaps after a forced reconnect-and-replay.
    saw_seq: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
pub(crate) struct SubRegistry {
    sid_to_sub: HashMap<u64, SubscriptionId>,
    sub_to_sid: HashMap<SubscriptionId, u64>,
    specs: HashMap<SubscriptionId, ReplaySpec>,
    last_seq: HashMap<u64, u64>,
    dispatchers: HashMap<SubscriptionId, Dispatcher>,
}

impl SubRegistry {
    fn register(
        &mut self,
        sub_id: SubscriptionId,
        sid: u64,
        spec: ReplaySpec,
        dispatcher: Dispatcher,
    ) {
        self.sid_to_sub.insert(sid, sub_id);
        self.sub_to_sid.insert(sub_id, sid);
        self.specs.insert(sub_id, spec);
        self.dispatchers.insert(sub_id, dispatcher);
    }

    fn remap_sid(&mut self, sub_id: SubscriptionId, new_sid: u64) {
        if let Some(old) = self.sub_to_sid.insert(sub_id, new_sid) {
            self.sid_to_sub.remove(&old);
            self.last_seq.remove(&old);
        }
        self.sid_to_sub.insert(new_sid, sub_id);
    }

    pub(crate) fn deregister(&mut self, sub_id: SubscriptionId) -> Option<u64> {
        self.specs.remove(&sub_id);
        self.dispatchers.remove(&sub_id);
        let sid = self.sub_to_sid.remove(&sub_id)?;
        self.sid_to_sub.remove(&sid);
        self.last_seq.remove(&sid);
        Some(sid)
    }

    fn lookup_by_sid(&self, sid: u64) -> Option<(SubscriptionId, Dispatcher)> {
        let sub = *self.sid_to_sub.get(&sid)?;
        let dispatcher = self.dispatchers.get(&sub).cloned()?;
        Some((sub, dispatcher))
    }

    fn all_specs(&self) -> Vec<(SubscriptionId, ReplaySpec)> {
        self.specs
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }
}

pub(crate) struct Inner {
    config: Config,
    cmd_tx: mpsc::Sender<ClientCommand>,
    registry: Arc<Mutex<SubRegistry>>,
    /// In-flight non-subscribe requests (Ok / Unsubscribed / Error responses
    /// match by `id`). Subscribed acks live in `pending_subscribes` instead
    /// because Kalshi's wire format omits `id` on those.
    inflight: Mutex<HashMap<u64, oneshot::Sender<ServerMessage>>>,
    /// FIFO queue of pending subscribe requests. Each Subscribed ack pops the
    /// front of this queue. Errors that arrive with the original `id` echoed
    /// scan the queue and remove the matching entry.
    pending_subscribes: Mutex<VecDeque<(u64, oneshot::Sender<ServerMessage>)>>,
    events_tx: broadcast::Sender<SystemEvent>,
    ids: Arc<IdGenerator>,
    cancel: CancellationToken,
}

impl Inner {
    fn complete_inflight(&self, req_id: u64, msg: ServerMessage) {
        if let Ok(mut map) = self.inflight.lock() {
            if let Some(tx) = map.remove(&req_id) {
                let _ = tx.send(msg);
            }
        }
    }
}

// -- Client ------------------------------------------------------------------

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    _supervisor: Arc<JoinHandle<()>>,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Convenience: connect with default config to the given environment.
    pub async fn connect(env: Environment) -> Result<Client> {
        Self::builder().environment(env).connect().await
    }

    pub async fn connect_with(config: Config) -> Result<Client> {
        // Initial connection — fail fast if this doesn't work.
        let ws = connect(&config.environment, config.credentials.as_ref()).await?;

        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientCommand>(config.command_buffer);
        let (events_tx, _) = broadcast::channel::<SystemEvent>(config.events_buffer);

        let inner = Arc::new(Inner {
            config: config.clone(),
            cmd_tx: cmd_tx.clone(),
            registry: Arc::new(Mutex::new(SubRegistry::default())),
            inflight: Mutex::new(HashMap::new()),
            pending_subscribes: Mutex::new(VecDeque::new()),
            events_tx: events_tx.clone(),
            ids: Arc::new(IdGenerator::default()),
            cancel: CancellationToken::new(),
        });

        let _ = events_tx.send(SystemEvent::Connected);

        let supervisor = tokio::spawn(supervisor_loop(inner.clone(), ws, cmd_rx));
        Ok(Client {
            inner,
            _supervisor: Arc::new(supervisor),
        })
    }

    /// Subscribe to the system-events broadcast (reconnects, seq gaps, shutdown).
    pub fn system_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.inner.events_tx.subscribe()
    }

    /// Trigger a graceful shutdown of the supervisor and writer tasks.
    pub fn shutdown(&self) {
        self.inner.cancel.cancel();
        let _ = self.inner.events_tx.send(SystemEvent::ShutDown);
    }

    // ---- Typed subscribe methods ------------------------------------------

    pub async fn subscribe_ticker(&self, market_tickers: Vec<String>) -> Result<Subscription<Ticker>> {
        let params = SubscribeParams {
            market_tickers: opt_vec(&market_tickers),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(Channel::Ticker, params, |msg, tx: &mpsc::Sender<Ticker>| {
            if let ServerMessage::Ticker { msg, .. } = msg {
                let _ = tx.try_send(msg.clone());
            }
        })
        .await
    }

    pub async fn subscribe_trade(&self, market_tickers: Vec<String>) -> Result<Subscription<Trade>> {
        let params = SubscribeParams {
            market_tickers: opt_vec(&market_tickers),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(Channel::Trade, params, |msg, tx: &mpsc::Sender<Trade>| {
            if let ServerMessage::Trade { msg, .. } = msg {
                let _ = tx.try_send(msg.clone());
            }
        })
        .await
    }

    pub async fn subscribe_orderbook(
        &self,
        market_tickers: Vec<String>,
    ) -> Result<Subscription<OrderbookEvent>> {
        let params = SubscribeParams {
            market_tickers: opt_vec(&market_tickers),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(
            Channel::OrderbookDelta,
            params,
            |msg, tx: &mpsc::Sender<OrderbookEvent>| match msg {
                ServerMessage::OrderbookSnapshot { seq, msg, .. } => {
                    let _ = tx.try_send(OrderbookEvent::Snapshot {
                        seq: *seq,
                        snapshot: msg.clone(),
                    });
                }
                ServerMessage::OrderbookDelta { seq, msg, .. } => {
                    let _ = tx.try_send(OrderbookEvent::Delta {
                        seq: *seq,
                        delta: msg.clone(),
                    });
                }
                _ => {}
            },
        )
        .await
    }

    pub async fn subscribe_fill(
        &self,
        market_tickers: Option<Vec<String>>,
    ) -> Result<Subscription<Fill>> {
        let params = SubscribeParams {
            market_tickers: market_tickers.filter(|v| !v.is_empty()),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(Channel::Fill, params, |msg, tx: &mpsc::Sender<Fill>| {
            if let ServerMessage::Fill { msg, .. } = msg {
                let _ = tx.try_send(msg.clone());
            }
        })
        .await
    }

    pub async fn subscribe_user_orders(
        &self,
        market_tickers: Option<Vec<String>>,
    ) -> Result<Subscription<UserOrder>> {
        let params = SubscribeParams {
            market_tickers: market_tickers.filter(|v| !v.is_empty()),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(
            Channel::UserOrders,
            params,
            |msg, tx: &mpsc::Sender<UserOrder>| {
                if let ServerMessage::UserOrder { msg, .. } = msg {
                    let _ = tx.try_send(msg.clone());
                }
            },
        )
        .await
    }

    pub async fn subscribe_market_positions(
        &self,
        market_tickers: Option<Vec<String>>,
    ) -> Result<Subscription<MarketPosition>> {
        let params = SubscribeParams {
            market_tickers: market_tickers.filter(|v| !v.is_empty()),
            ..SubscribeParams::default()
        };
        self.subscribe_typed(
            Channel::MarketPositions,
            params,
            |msg, tx: &mpsc::Sender<MarketPosition>| {
                if let ServerMessage::MarketPosition { msg, .. } = msg {
                    let _ = tx.try_send(msg.clone());
                }
            },
        )
        .await
    }

    pub async fn subscribe_market_lifecycle(&self) -> Result<Subscription<LifecycleEvent>> {
        let params = SubscribeParams::default();
        self.subscribe_typed(
            Channel::MarketLifecycleV2,
            params,
            |msg, tx: &mpsc::Sender<LifecycleEvent>| match msg {
                ServerMessage::MarketLifecycleV2 { msg, .. } => {
                    let _ = tx.try_send(LifecycleEvent::Market(msg.clone()));
                }
                ServerMessage::EventLifecycle { msg, .. } => {
                    let _ = tx.try_send(LifecycleEvent::Event(msg.clone()));
                }
                _ => {}
            },
        )
        .await
    }

    pub async fn subscribe_multivariate_lifecycle(&self) -> Result<Subscription<LifecycleEvent>> {
        let params = SubscribeParams::default();
        self.subscribe_typed(
            Channel::MultivariateMarketLifecycle,
            params,
            |msg, tx: &mpsc::Sender<LifecycleEvent>| match msg {
                ServerMessage::MultivariateMarketLifecycle { msg, .. } => {
                    let _ = tx.try_send(LifecycleEvent::Market(msg.clone()));
                }
                ServerMessage::EventLifecycle { msg, .. } => {
                    let _ = tx.try_send(LifecycleEvent::Event(msg.clone()));
                }
                _ => {}
            },
        )
        .await
    }

    pub async fn subscribe_multivariate(&self) -> Result<Subscription<MultivariateLookup>> {
        let params = SubscribeParams::default();
        self.subscribe_typed(
            Channel::Multivariate,
            params,
            |msg, tx: &mpsc::Sender<MultivariateLookup>| {
                if let ServerMessage::MultivariateLookup { msg, .. } = msg {
                    let _ = tx.try_send(msg.clone());
                }
            },
        )
        .await
    }

    pub async fn subscribe_communications(
        &self,
        shard: Option<(u32, u32)>,
    ) -> Result<Subscription<CommunicationEvent>> {
        let (shard_factor, shard_key) = match shard {
            Some((f, k)) => (Some(f), Some(k)),
            None => (None, None),
        };
        let params = SubscribeParams {
            shard_factor,
            shard_key,
            ..SubscribeParams::default()
        };
        self.subscribe_typed(
            Channel::Communications,
            params,
            |msg, tx: &mpsc::Sender<CommunicationEvent>| match msg {
                ServerMessage::RfqCreated { msg, .. } => {
                    let _ = tx.try_send(CommunicationEvent::RfqCreated(msg.clone()));
                }
                ServerMessage::RfqDeleted { msg, .. } => {
                    let _ = tx.try_send(CommunicationEvent::RfqDeleted(msg.clone()));
                }
                ServerMessage::QuoteCreated { msg, .. } => {
                    let _ = tx.try_send(CommunicationEvent::QuoteCreated(msg.clone()));
                }
                ServerMessage::QuoteAccepted { msg, .. } => {
                    let _ = tx.try_send(CommunicationEvent::QuoteAccepted(msg.clone()));
                }
                ServerMessage::QuoteExecuted { msg, .. } => {
                    let _ = tx.try_send(CommunicationEvent::QuoteExecuted(msg.clone()));
                }
                _ => {}
            },
        )
        .await
    }

    pub async fn subscribe_order_group_updates(&self) -> Result<Subscription<OrderGroupUpdate>> {
        let params = SubscribeParams::default();
        self.subscribe_typed(
            Channel::OrderGroupUpdates,
            params,
            |msg, tx: &mpsc::Sender<OrderGroupUpdate>| {
                if let ServerMessage::OrderGroupUpdates { msg, .. } = msg {
                    let _ = tx.try_send(msg.clone());
                }
            },
        )
        .await
    }

    /// Modify a live subscription (add/remove markets, request snapshot).
    pub async fn update_subscription<T>(
        &self,
        sub: &Subscription<T>,
        action: UpdateAction,
        market_tickers: Option<Vec<String>>,
    ) -> Result<()> {
        let sid = self
            .inner
            .registry
            .lock()
            .ok()
            .and_then(|r| r.sub_to_sid.get(&sub.id).copied())
            .ok_or(KalshiError::SubscriptionClosed)?;
        let req_id = self.inner.ids.next_req();
        let cmd = ClientCommand::UpdateSubscription {
            id: req_id,
            params: UpdateSubscriptionParams {
                sid,
                action,
                market_tickers,
                market_ticker: None,
                market_id: None,
                market_ids: None,
                send_initial_snapshot: None,
            },
        };
        let rx = self.register_inflight(req_id);
        self.send_command(cmd).await?;
        await_ack(rx, self.inner.config.request_timeout).await.map(|_| ())
    }

    // ---- Internal subscribe primitive --------------------------------------

    async fn subscribe_typed<T, F>(
        &self,
        channel: Channel,
        mut params: SubscribeParams,
        forward: F,
    ) -> Result<Subscription<T>>
    where
        T: Send + 'static,
        F: Fn(&ServerMessage, &mpsc::Sender<T>) + Send + Sync + 'static,
    {
        // Auth precheck.
        if channel.requires_auth() && self.inner.config.credentials.is_none() {
            return Err(KalshiError::Auth(format!(
                "channel {channel:?} requires credentials"
            )));
        }
        params.channels = vec![channel];

        let buffer = self.inner.config.default_subscription_buffer;
        let (data_tx, data_rx) = mpsc::channel::<T>(buffer);

        // Dispatcher captures the typed sender; no trait object hierarchy needed.
        let dispatcher: Dispatcher = {
            let data_tx = data_tx.clone();
            Arc::new(move |msg| forward(msg, &data_tx))
        };

        let sub_id = self.inner.ids.next_sub();
        let req_id = self.inner.ids.next_req();
        let saw_seq = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let spec = ReplaySpec {
            channel,
            params: params.clone(),
            saw_seq,
        };

        // Subscribe acks come without an `id` from Kalshi, so we use a FIFO
        // queue rather than the id-keyed inflight map. Errors that DO carry an
        // id back are delivered via the same oneshot — handle_text scans the
        // queue by id when an Error matches a pending subscribe.
        let (ack_tx, ack_rx) = oneshot::channel();
        if let Ok(mut q) = self.inner.pending_subscribes.lock() {
            q.push_back((req_id, ack_tx));
        }
        let send_result = self
            .send_command(ClientCommand::Subscribe { id: req_id, params })
            .await;
        if let Err(e) = send_result {
            // Roll the queue entry back if the send failed; otherwise it would
            // leak and cause off-by-one matching for the next subscribe.
            if let Ok(mut q) = self.inner.pending_subscribes.lock() {
                if let Some(pos) = q.iter().position(|(rid, _)| *rid == req_id) {
                    q.remove(pos);
                }
            }
            return Err(e);
        }
        let ack_result = await_ack(ack_rx, self.inner.config.request_timeout).await;
        // On any non-success outcome (timeout, channel closed, server error)
        // ensure the queue entry is gone.
        if ack_result.is_err() {
            if let Ok(mut q) = self.inner.pending_subscribes.lock() {
                if let Some(pos) = q.iter().position(|(rid, _)| *rid == req_id) {
                    q.remove(pos);
                }
            }
        }
        let ack = ack_result?;
        let sid = match ack {
            ServerMessage::Subscribed { msg, .. } => msg.sid,
            other => {
                return Err(KalshiError::Auth(format!(
                    "expected subscribed ack, got {other:?}"
                )))
            }
        };

        // Atomically register: sid mapping + dispatcher + replay spec.
        if let Ok(mut reg) = self.inner.registry.lock() {
            reg.register(sub_id, sid, spec, dispatcher);
        }

        Ok(Subscription::new(
            sub_id,
            data_rx,
            self.inner.cmd_tx.clone(),
            self.inner.registry.clone(),
            self.inner.ids.clone(),
        ))
    }

    fn register_inflight(&self, req_id: u64) -> oneshot::Receiver<ServerMessage> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut map) = self.inner.inflight.lock() {
            map.insert(req_id, tx);
        }
        rx
    }

    async fn send_command(&self, cmd: ClientCommand) -> Result<()> {
        self.inner
            .cmd_tx
            .send(cmd)
            .await
            .map_err(|_| KalshiError::Shutdown)
    }
}

// -- Helpers -----------------------------------------------------------------

fn opt_vec(v: &[String]) -> Option<Vec<String>> {
    if v.is_empty() {
        None
    } else {
        Some(v.to_vec())
    }
}

async fn await_ack(rx: oneshot::Receiver<ServerMessage>, timeout: Duration) -> Result<ServerMessage> {
    match tokio::time::timeout(timeout, rx).await {
        Err(_) => Err(KalshiError::RequestTimeout),
        Ok(Err(_)) => Err(KalshiError::SubscriptionClosed),
        Ok(Ok(ServerMessage::Error { msg, .. })) => Err(KalshiError::Server {
            code: ErrorCode::from_u8(msg.code),
            msg: msg.msg,
        }),
        Ok(Ok(other)) => Ok(other),
    }
}

// -- Supervisor / multiplexer / writer ---------------------------------------

async fn supervisor_loop(
    inner: Arc<Inner>,
    initial: WsStream,
    mut cmd_rx: mpsc::Receiver<ClientCommand>,
) {
    let mut current = Some(initial);
    let mut attempt: u32 = 0;

    loop {
        if inner.cancel.is_cancelled() {
            break;
        }

        let ws = match current.take() {
            Some(w) => w,
            None => {
                if !inner.config.reconnect.enabled {
                    let _ = inner.events_tx.send(SystemEvent::Disconnected {
                        reason: "reconnect disabled".into(),
                    });
                    break;
                }
                if let Some(max) = inner.config.reconnect.max_attempts {
                    if attempt >= max {
                        let _ = inner.events_tx.send(SystemEvent::Disconnected {
                            reason: format!("exceeded max_attempts={max}"),
                        });
                        break;
                    }
                }
                let delay = inner.config.reconnect.delay_for(attempt);
                debug!(?delay, attempt, "reconnect backoff");
                tokio::select! {
                    _ = sleep(delay) => {}
                    _ = inner.cancel.cancelled() => break,
                }
                match connect(&inner.config.environment, inner.config.credentials.as_ref()).await {
                    Ok(ws) => {
                        attempt = 0;
                        replay_subscriptions(&inner).await;
                        ws
                    }
                    Err(e) => {
                        attempt = attempt.saturating_add(1);
                        warn!("reconnect failed: {e}");
                        continue;
                    }
                }
            }
        };

        let reason = run_session(&inner, ws, &mut cmd_rx).await;
        let _ = inner.events_tx.send(SystemEvent::Disconnected {
            reason: reason.clone(),
        });
        info!("session ended: {reason}");

        if !inner.config.reconnect.enabled {
            break;
        }
    }

    let _ = inner.events_tx.send(SystemEvent::ShutDown);
}

async fn replay_subscriptions(inner: &Arc<Inner>) {
    // Mark sid mappings invalid; preserve specs for re-subscribe.
    let specs = {
        let reg = match inner.registry.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        reg.all_specs()
    };
    for (sub_id, spec) in specs {
        let req_id = inner.ids.next_req();
        let cmd = ClientCommand::Subscribe {
            id: req_id,
            params: SubscribeParams {
                channels: vec![spec.channel],
                ..spec.params.clone()
            },
        };
        // Replay registers in pending_subscribes (FIFO), same as fresh subscribes,
        // because Subscribed acks lack an `id` to match by.
        let (tx, rx) = oneshot::channel();
        if let Ok(mut q) = inner.pending_subscribes.lock() {
            q.push_back((req_id, tx));
        }
        if inner.cmd_tx.send(cmd).await.is_err() {
            warn!("replay: writer dead before subscribe sent");
            if let Ok(mut q) = inner.pending_subscribes.lock() {
                if let Some(pos) = q.iter().position(|(rid, _)| *rid == req_id) {
                    q.remove(pos);
                }
            }
            return;
        }
        match tokio::time::timeout(inner.config.request_timeout, rx).await {
            Ok(Ok(ServerMessage::Subscribed { msg, .. })) => {
                let new_sid = msg.sid;
                if let Ok(mut reg) = inner.registry.lock() {
                    reg.remap_sid(sub_id, new_sid);
                    reg.last_seq.remove(&new_sid);
                }
                let had_seq_gap = spec
                    .saw_seq
                    .swap(false, std::sync::atomic::Ordering::Relaxed);
                let _ = inner.events_tx.send(SystemEvent::Reconnected {
                    sub_id,
                    had_seq_gap,
                });
            }
            Ok(Ok(other)) => warn!("replay: unexpected ack: {other:?}"),
            Ok(Err(_)) => warn!("replay: ack channel closed"),
            Err(_) => {
                warn!("replay: ack timed out for {sub_id}");
                if let Ok(mut q) = inner.pending_subscribes.lock() {
                    if let Some(pos) = q.iter().position(|(rid, _)| *rid == req_id) {
                        q.remove(pos);
                    }
                }
            }
        }
    }
}

async fn run_session(
    inner: &Arc<Inner>,
    ws: WsStream,
    cmd_rx: &mut mpsc::Receiver<ClientCommand>,
) -> String {
    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            _ = inner.cancel.cancelled() => return "shutdown".into(),
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return "command channel closed".into(); };
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => { warn!("serialize cmd: {e}"); continue; }
                };
                if let Err(e) = sink.send(Message::Text(json.into())).await {
                    return format!("send failed: {e}");
                }
            }
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(text))) => handle_text(inner, &text),
                Some(Ok(Message::Binary(_))) => {}
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Frame(_))) => {}
                Some(Ok(Message::Close(c))) => return format!("server closed: {c:?}"),
                Some(Err(e)) => return format!("ws error: {e}"),
                None => return "stream ended".into(),
            }
        }
    }
}

fn handle_text(inner: &Arc<Inner>, text: &str) {
    let parsed: ServerMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            warn!(text = %text, "deserialize: {e}");
            return;
        }
    };

    if parsed.is_control() {
        // Subscribed acks live in pending_subscribes regardless of whether the
        // server echoes `id`. If `id` is present, scan the queue and remove
        // that specific entry; otherwise FIFO pop the oldest (Kalshi's actual
        // production behavior).
        if matches!(&parsed, ServerMessage::Subscribed { .. }) {
            let echoed_id = parsed.request_id();
            let popped = if let Some(rid) = echoed_id {
                inner.pending_subscribes.lock().ok().and_then(|mut q| {
                    q.iter()
                        .position(|(qid, _)| *qid == rid)
                        .and_then(|pos| q.remove(pos))
                })
            } else {
                inner
                    .pending_subscribes
                    .lock()
                    .ok()
                    .and_then(|mut q| q.pop_front())
            };
            match popped {
                Some((_req_id, tx)) => {
                    let _ = tx.send(parsed);
                }
                None => warn!("subscribed ack arrived with no pending request"),
            }
            return;
        }

        // Error frames may be a response to a failed subscribe (id echoed back),
        // a failed update_subscription, etc. Try the inflight map first, then
        // fall back to scanning pending_subscribes by id.
        if let ServerMessage::Error { id: Some(id), .. } = &parsed {
            let id = *id;
            // Inflight first.
            let inflight_hit = inner
                .inflight
                .lock()
                .ok()
                .and_then(|mut m| m.remove(&id));
            if let Some(tx) = inflight_hit {
                let _ = tx.send(parsed);
                return;
            }
            // Then pending_subscribes.
            let queued = inner.pending_subscribes.lock().ok().and_then(|mut q| {
                q.iter()
                    .position(|(rid, _)| *rid == id)
                    .and_then(|pos| q.remove(pos))
            });
            if let Some((_, tx)) = queued {
                let _ = tx.send(parsed);
                return;
            }
        }

        // Ok / Unsubscribed / Error without id: id-keyed inflight only.
        if let Some(req_id) = parsed.request_id() {
            inner.complete_inflight(req_id, parsed);
            return;
        }
        if let ServerMessage::Error { msg, .. } = &parsed {
            warn!(code = msg.code, "server error: {}", msg.msg);
        }
        return;
    }

    // Data frame: route by sid.
    let Some(sid) = parsed.sid() else { return; };

    // Sequence gap detection (mutates registry).
    if let Some(seq) = parsed.seq() {
        if let Ok(mut reg) = inner.registry.lock() {
            let prev = reg.last_seq.insert(sid, seq);
            if let Some(prev) = prev {
                let expected = prev + 1;
                if seq != expected {
                    if let Some(sub_id) = reg.sid_to_sub.get(&sid).copied() {
                        let _ = inner.events_tx.send(SystemEvent::SeqGap {
                            sub_id,
                            expected,
                            got: seq,
                        });
                    }
                }
            }
            if let Some(sub_id) = reg.sid_to_sub.get(&sid).copied() {
                if let Some(spec) = reg.specs.get(&sub_id) {
                    spec.saw_seq
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    let dispatcher = match inner.registry.lock() {
        Ok(reg) => reg.lookup_by_sid(sid).map(|(_, d)| d),
        Err(_) => None,
    };
    if let Some(d) = dispatcher {
        d(&parsed);
    }
}
