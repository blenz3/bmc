//! `Subscription<T>` — a typed `Stream` of channel events with best-effort
//! `Drop`-based unsubscribe, plus the [`SystemEvent`] enum used by [`Client::system_events`].

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::protocol::commands::{ClientCommand, UnsubscribeParams};

/// Stable client-side handle for a subscription. Survives reconnects (the server-assigned
/// `sid` may change, but `SubscriptionId` does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(pub u64);

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sub#{}", self.0)
    }
}

/// Generates fresh `SubscriptionId`s and request `id`s.
#[derive(Debug, Default)]
pub(crate) struct IdGenerator {
    sub_id: AtomicU64,
    req_id: AtomicU64,
}

impl IdGenerator {
    pub fn next_sub(&self) -> SubscriptionId {
        SubscriptionId(self.sub_id.fetch_add(1, Ordering::Relaxed))
    }
    pub fn next_req(&self) -> u64 {
        self.req_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Out-of-band events the client emits onto a broadcast channel for observability
/// and gap detection.
#[derive(Debug, Clone)]
pub enum SystemEvent {
    /// First successful connection.
    Connected,
    /// Disconnect detected; supervisor will attempt to reconnect.
    Disconnected { reason: String },
    /// Reconnect succeeded and a particular subscription has been re-established.
    /// `had_seq_gap` indicates the supervisor saw a non-zero seq counter before
    /// disconnect — i.e. some messages may have been lost.
    Reconnected {
        sub_id: SubscriptionId,
        had_seq_gap: bool,
    },
    /// A monotonically-increasing seq number jumped — server-side messages were lost.
    SeqGap {
        sub_id: SubscriptionId,
        expected: u64,
        got: u64,
    },
    /// The client has shut down. No further events will be emitted.
    ShutDown,
}

/// A typed stream of one channel's events, scoped to a single subscription.
///
/// Backed by an **unbounded** mpsc — the multiplexer's dispatcher always
/// successfully enqueues the next event regardless of consumer speed, so
/// there's no silent-drop path. If the consumer permanently stalls, memory
/// will grow with the inflow rate; in practice the supervisor's connection
/// timeouts catch a stuck consumer well before that becomes a problem.
///
/// Dropping the `Subscription` issues a best-effort `unsubscribe` to the server.
pub struct Subscription<T> {
    inner: UnboundedReceiverStream<T>,
    pub id: SubscriptionId,
    pub(crate) cmd_tx: mpsc::Sender<ClientCommand>,
    pub(crate) registry: Arc<Mutex<crate::client::SubRegistry>>,
    pub(crate) ids: Arc<IdGenerator>,
}

impl<T> Subscription<T> {
    pub(crate) fn new(
        id: SubscriptionId,
        rx: mpsc::UnboundedReceiver<T>,
        cmd_tx: mpsc::Sender<ClientCommand>,
        registry: Arc<Mutex<crate::client::SubRegistry>>,
        ids: Arc<IdGenerator>,
    ) -> Self {
        Self {
            inner: UnboundedReceiverStream::new(rx),
            id,
            cmd_tx,
            registry,
            ids,
        }
    }
}

impl<T> Stream for Subscription<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        // Take the current server-assigned sid (if any) and unregister synchronously.
        let sid = {
            if let Ok(mut reg) = self.registry.lock() {
                reg.deregister(self.id)
            } else {
                None
            }
        };
        if let Some(sid) = sid {
            let req_id = self.ids.next_req();
            let cmd = ClientCommand::Unsubscribe {
                id: req_id,
                params: UnsubscribeParams { sids: vec![sid] },
            };
            // Best-effort. If the writer task is gone or the channel is full, the
            // server will GC the subscription on disconnect anyway.
            let _ = self.cmd_tx.try_send(cmd);
        }
    }
}
