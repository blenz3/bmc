//! Watches a Kalshi market's L2 order book live in the terminal.
//!
//! Discovery → subscription pipeline:
//! 1. REST `/markets?status=open&series_ticker=<S>` to find currently-open
//!    markets for a series (default `KXBTC`).
//! 2. Pick the one with the soonest in-future `close_time` — for high-frequency
//!    series like BTC 15-minute, that's the contract currently trading.
//! 3. Subscribe via kalshi-ws to `orderbook_delta` on that ticker. Apply the
//!    initial snapshot, then mutate the local book on each delta.
//! 4. Re-render the console on every update with ANSI clear-screen.
//!
//! Defaults to the BTC 15-minute series (`KXBTC15M`). Override with
//! `--series-ticker` for other series (`KXBTC` for hourly BTC, `KXETH15M` for
//! 15-min ETH, etc.) or `--ticker` to skip discovery entirely.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use futures_util::StreamExt;
use kalshi_ws::{Client, Environment, OrderbookEvent, Side, Subscription};
use serde::Deserialize;
use tracing::{info, warn};

/// Default targets BTC 15-minute markets (`KXBTC15M`). Other Kalshi BTC series:
/// `KXBTC` (hourly), `KXBTCD` (daily), `KXBTCY` (year-end). For ETH/SOL 15-min
/// substitute `KXETH15M` / `KXSOL15M`.
const DEFAULT_SERIES: &str = "KXBTC15M";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnvArg {
    Production,
    Demo,
}

impl EnvArg {
    fn rest_base_url(self) -> &'static str {
        match self {
            EnvArg::Production => "https://api.elections.kalshi.com",
            EnvArg::Demo => "https://demo-api.kalshi.co",
        }
    }
    fn ws_environment(self) -> Environment {
        match self {
            EnvArg::Production => Environment::Production,
            EnvArg::Demo => Environment::Demo,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "kalshi-book-watch", about, version)]
struct Cli {
    /// Series ticker to discover open markets for. Default targets BTC.
    #[arg(long, default_value = DEFAULT_SERIES)]
    series_ticker: String,

    /// Skip discovery and watch this exact market ticker. Useful when you
    /// already know which contract to follow.
    #[arg(long)]
    ticker: Option<String>,

    #[arg(long, value_enum, default_value_t = EnvArg::Production)]
    env: EnvArg,

    /// Number of price levels to display per side.
    #[arg(long, default_value_t = 10)]
    depth: usize,

    /// Anti-flicker: minimum interval between re-renders, in milliseconds.
    /// Multiple deltas within this window collapse into a single redraw.
    #[arg(long, default_value_t = 50)]
    render_interval_ms: u64,

    #[arg(long, env = "KALSHI_KEY_ID")]
    key_id: Option<String>,

    #[arg(long, env = "KALSHI_KEY_PEM_PATH")]
    key_pem: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,kalshi_book_watch=info")),
        )
        .with_writer(std::io::stderr) // stdout reserved for the rendered book
        .init();

    let cli = Cli::parse();

    let creds = load_credentials(&cli)?;
    let mut builder = Client::builder().environment(cli.env.ws_environment());
    if let Some(c) = creds {
        builder = builder.credentials(c);
    }
    let client = builder
        .connect()
        .await
        .context("connecting to Kalshi WebSocket")?;

    let render = Duration::from_millis(cli.render_interval_ms);
    let user_pinned_ticker = cli.ticker.is_some();

    // Outer loop drives auto-rediscover when a contract closes. For
    // high-frequency series (BTC 15-min) the contract being watched expires
    // every 15 minutes — we transparently roll over to the next one.
    loop {
        let target = match &cli.ticker {
            Some(t) => MarketTarget {
                ticker: t.clone(),
                close_time: None,
            },
            None => discover_latest(&cli)
                .await
                .with_context(|| format!("discovering latest market for {}", cli.series_ticker))?,
        };
        info!(
            "watching market {} (closes {})",
            target.ticker,
            target.close_time.as_deref().unwrap_or("unknown")
        );

        let sub = client
            .subscribe_orderbook(vec![target.ticker.clone()])
            .await
            .context("subscribing to orderbook_delta")?;

        let outcome = run_watcher(&client, target, sub, cli.depth, render).await?;

        match outcome {
            WatcherOutcome::UserExit => break,
            WatcherOutcome::ContractClosed => {
                if user_pinned_ticker {
                    info!("contract closed; --ticker pinned, exiting");
                    break;
                }
                info!("contract closed; rediscovering next window");
                // Brief pause so we don't immediately retry against an empty book.
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            WatcherOutcome::StreamEnded => {
                warn!("subscription stream ended unexpectedly; reconnecting");
                continue;
            }
        }
    }

    client.shutdown();
    Ok(())
}

// -- Discovery ----------------------------------------------------------------

#[derive(Debug, Clone)]
struct MarketTarget {
    ticker: String,
    close_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListMarketsResponse {
    #[serde(default)]
    markets: Vec<DiscoveredMarket>,
}

#[derive(Debug, Deserialize)]
struct DiscoveredMarket {
    ticker: String,
    #[serde(default)]
    close_time: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn discover_latest(cli: &Cli) -> Result<MarketTarget> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("kalshi-book-watch/", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .timeout(Duration::from_secs(15))
        .build()?;

    // We may need to paginate to find all open markets for a busy series like
    // KXBTC, but the first 1000 records (sorted server-side by close_time
    // ascending in practice) almost always contain the active 15-min window.
    let url = format!("{}/trade-api/v2/markets", cli.env.rest_base_url());
    let resp = http
        .get(&url)
        .query(&[
            ("status", "open"),
            ("series_ticker", cli.series_ticker.as_str()),
            ("limit", "1000"),
        ])
        .send()
        .await
        .context("GET /markets")?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("/markets returned non-success: {body}"));
    }
    let body: ListMarketsResponse = resp.json().await.context("parsing /markets response")?;

    if body.markets.is_empty() {
        return Err(anyhow!(
            "no open markets found for series_ticker={}. Try --series-ticker with a different prefix \
             or pass --ticker explicitly.",
            cli.series_ticker
        ));
    }

    let now = SystemTime::now();
    let chosen = body
        .markets
        .iter()
        .filter(|m| m.status.as_deref() != Some("settled"))
        .filter_map(|m| {
            let parsed = m.close_time.as_deref().and_then(parse_iso_to_unix);
            // Skip markets whose close_time is in the past — they're winding down.
            parsed
                .filter(|&close| {
                    let now_ms = now
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    close > now_ms
                })
                .map(|close_ms| (m, close_ms))
        })
        .min_by_key(|(_, close_ms)| *close_ms);

    match chosen {
        Some((m, _)) => Ok(MarketTarget {
            ticker: m.ticker.clone(),
            close_time: m.close_time.clone(),
        }),
        None => {
            warn!("no in-future close_time found; falling back to first listed market");
            let m = &body.markets[0];
            Ok(MarketTarget {
                ticker: m.ticker.clone(),
                close_time: m.close_time.clone(),
            })
        }
    }
}

/// Parses an RFC 3339 / ISO 8601 timestamp into Unix milliseconds. Tolerant of
/// trailing `Z` and fractional seconds; returns None on anything weird so the
/// caller can fall back rather than crash.
fn parse_iso_to_unix(s: &str) -> Option<i64> {
    // Manual parse to avoid pulling in chrono. Format: YYYY-MM-DDTHH:MM:SS[.fff]Z
    // Extract date and time parts, compute days-since-epoch, multiply.
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_iter = date.split('-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: u32 = date_iter.next()?.parse().ok()?;
    let day: u32 = date_iter.next()?.parse().ok()?;
    let mut time_iter = time.split(':');
    let hour: i64 = time_iter.next()?.parse().ok()?;
    let minute: i64 = time_iter.next()?.parse().ok()?;
    let sec_str = time_iter.next()?;
    let second: f64 = sec_str.parse().ok()?;

    // Days-from-epoch via the Howard Hinnant algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    let days_from_epoch = era * 146097 + doe as i64 - 719468;

    let secs = days_from_epoch * 86400 + hour * 3600 + minute * 60;
    Some(secs * 1000 + (second * 1000.0) as i64)
}

// -- Credentials --------------------------------------------------------------

fn load_credentials(cli: &Cli) -> Result<Option<kalshi_ws::Credentials>> {
    match (&cli.key_id, &cli.key_pem) {
        (Some(id), Some(path)) => {
            let creds = kalshi_ws::Credentials::from_pem_file(id, path)
                .with_context(|| format!("loading key from {}", path.display()))?;
            Ok(Some(creds))
        }
        _ => Ok(None),
    }
}

// -- Order book + rendering ---------------------------------------------------
//
// Backed by `kalshi_common::book::FixedBook` — a preallocated 10_000-level
// flat-array L2 book. Apply/delta is O(1), top-of-book lookup is O(1) via a
// cached best index, and the only sub-O(1) path (rescan after the top is
// emptied) is a contiguous walk over an L1-resident array.

use kalshi_common::book::{FixedBook, Side as BookSide};
use kalshi_common::DECI_CENTS_PER_DOLLAR;
use kalshi_ws::UpdateAction;

/// Returned by `Book::apply` when a delta's seq number doesn't equal
/// `last_seq + 1`. A gap means we missed at least one delta, so the local
/// book is out of sync with the server -- caller should request a fresh
/// snapshot to resync.
#[derive(Debug, Clone, Copy)]
struct SeqGap {
    expected: u64,
    got: u64,
}

/// Internal price unit: 1/10000 of a dollar. Sub-penny markets resolve at
/// $0.001 ticks; storing in deci-cents preserves that losslessly.
const PRICE_DENOM: i64 = DECI_CENTS_PER_DOLLAR;

struct Book {
    inner: FixedBook,
    last_seq: Option<u64>,
    last_update_ms: Option<i64>,
    /// Cumulative count of seq-number gaps observed. Each gap means we lost
    /// at least one delta and the local book diverged from the server until
    /// the next snapshot arrives.
    gaps_observed: u64,
    snapshots_applied: u64,
    deltas_applied: u64,
}

impl Book {
    fn new() -> Self {
        Self {
            // 10_000 deci-cent levels covers both `linear_cent` (uses every
            // 100th level) and `deci_cent` / `tapered_deci_cent` (uses every
            // 10th or every 1st level). 160 KB memory, fits in L2.
            inner: FixedBook::deci_cent(),
            last_seq: None,
            last_update_ms: None,
            gaps_observed: 0,
            snapshots_applied: 0,
            deltas_applied: 0,
        }
    }

    /// Applies an event and returns `Some(SeqGap)` if the event's seq number
    /// indicates we missed at least one earlier delta. Snapshots reset the
    /// baseline (no gap reported) since they're authoritative.
    fn apply(&mut self, evt: &OrderbookEvent) -> Option<SeqGap> {
        let new_seq = match evt {
            OrderbookEvent::Snapshot { seq, .. } => *seq,
            OrderbookEvent::Delta { seq, .. } => *seq,
        };

        // Gap detection: only deltas (not snapshots) must follow last_seq + 1.
        let gap = match (evt, self.last_seq) {
            (OrderbookEvent::Delta { .. }, Some(prev)) if new_seq != prev + 1 => {
                self.gaps_observed += 1;
                Some(SeqGap {
                    expected: prev + 1,
                    got: new_seq,
                })
            }
            _ => None,
        };

        match evt {
            OrderbookEvent::Snapshot { snapshot, .. } => {
                self.inner.replace_side(
                    BookSide::Yes,
                    snapshot
                        .yes_dollars_fp
                        .iter()
                        .map(|(price, size)| (price_key(*price) as usize, (*size).max(0) as u64)),
                );
                self.inner.replace_side(
                    BookSide::No,
                    snapshot
                        .no_dollars_fp
                        .iter()
                        .map(|(price, size)| (price_key(*price) as usize, (*size).max(0) as u64)),
                );
                self.last_update_ms = snapshot.ts_ms;
                self.snapshots_applied += 1;
                // Log the post-snapshot top of book so we can tell whether a
                // crossed view comes from the snapshot itself (Kalshi-side
                // weirdness) or from accumulated delta state.
                let yb = self.best_yes_bid();
                let nb = self.best_no_bid();
                let crossed = match (yb, nb) {
                    (Some(y), Some(n)) => (y + n).saturating_sub(PRICE_DENOM).max(0),
                    _ => 0,
                };
                info!(
                    seq = new_seq,
                    yes_levels = snapshot.yes_dollars_fp.len(),
                    no_levels = snapshot.no_dollars_fp.len(),
                    best_yes_bid_dc = ?yb,
                    best_no_bid_dc = ?nb,
                    cross_dc = crossed,
                    "snapshot applied"
                );
            }
            OrderbookEvent::Delta { delta, .. } => {
                let idx = price_key(delta.price_dollars);
                if let Err(e) = self.inner.apply_delta(
                    book_side(delta.side),
                    idx as usize,
                    delta.delta_fp,
                ) {
                    warn!(
                        seq = new_seq,
                        price = delta.price_dollars,
                        idx,
                        "delta dropped: {e}"
                    );
                }
                self.last_update_ms = delta.ts_ms;
                self.deltas_applied += 1;
            }
        }
        self.last_seq = Some(new_seq);
        gap
    }

    /// YES-equivalent asks derived from NO bids
    /// (`ask_price = PRICE_DENOM - no_bid_price`), ascending by price (best/lowest first),
    /// capped to `depth`. Filters out asks that don't exceed the best YES bid
    /// (those are crossed entries — see comment in [`Self::unified_bids`]).
    fn unified_asks(&self, depth: usize) -> Vec<(i64, i64)> {
        let best_yes_bid = self.best_yes_bid().unwrap_or(0);
        // FixedBook.iter() yields (idx, size) descending by price index.
        // For NO: descending NO price → ascending implied YES ask price (since ask = DENOM - no_p).
        // So filter, then transform — best (lowest) ask comes out first.
        self.inner
            .iter(BookSide::No)
            .filter(|(no_p, _)| (PRICE_DENOM - *no_p as i64) > best_yes_bid)
            .take(depth)
            .map(|(no_p, s)| (PRICE_DENOM - no_p as i64, s as i64))
            .collect()
    }

    /// YES bids descending by price (best/highest first), capped to `depth`.
    /// `best_ask_cap` filters out bids that would cross the displayed asks.
    fn unified_bids(&self, depth: usize, best_ask_cap: Option<i64>) -> Vec<(i64, i64)> {
        let upper = best_ask_cap.unwrap_or(PRICE_DENOM + 1);
        self.inner
            .iter(BookSide::Yes)
            .filter(|(p, _)| (*p as i64) < upper)
            .take(depth)
            .map(|(p, s)| (p as i64, s as i64))
            .collect()
    }

    fn best_yes_bid(&self) -> Option<i64> {
        self.inner.best(BookSide::Yes).map(|(idx, _)| idx as i64)
    }

    fn best_no_bid(&self) -> Option<i64> {
        self.inner.best(BookSide::No).map(|(idx, _)| idx as i64)
    }
}

#[inline]
fn book_side(s: Side) -> BookSide {
    match s {
        Side::Yes => BookSide::Yes,
        Side::No => BookSide::No,
    }
}

fn price_key(p: f64) -> i64 {
    (p * PRICE_DENOM as f64).round() as i64
}

/// How `run_watcher` returned. Lets `main` decide whether to rediscover, retry,
/// or exit.
#[derive(Debug)]
enum WatcherOutcome {
    /// User pressed Ctrl+C — exit cleanly.
    UserExit,
    /// `target.close_time` has passed; the watched contract is over. The caller
    /// should rediscover the next window.
    ContractClosed,
    /// The subscription stream ended (Kalshi closed it or the supervisor gave
    /// up). Caller may want to resubscribe.
    StreamEnded,
}

async fn run_watcher(
    client: &Client,
    target: MarketTarget,
    mut sub: Subscription<OrderbookEvent>,
    depth: usize,
    render_interval: Duration,
) -> Result<WatcherOutcome> {
    let mut book = Book::new();
    let mut last_render = std::time::Instant::now()
        .checked_sub(render_interval)
        .unwrap_or_else(std::time::Instant::now);
    let mut last_event_at: Option<std::time::Instant> = None;
    let mut received = 0u64;
    // Throttle resync requests so a sustained gap doesn't spam the server.
    let mut last_resync_request: Option<std::time::Instant> = None;
    const RESYNC_COOLDOWN: Duration = Duration::from_secs(5);

    let close_at_ms = target.close_time.as_deref().and_then(parse_iso_to_unix);

    // Heartbeat tick keeps the screen alive even when no events arrive — the
    // "last update Xs ago" timestamp keeps moving — and lets us check
    // `close_time` without depending on event arrival.
    let mut heartbeat = tokio::time::interval(Duration::from_millis(500));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nshutting down on Ctrl+C");
                return Ok(WatcherOutcome::UserExit);
            }
            _ = heartbeat.tick() => {
                if let Some(close_ms) = close_at_ms {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    // 2-second grace so settlement-edge frames have a chance
                    // to land before we tear down the subscription.
                    if now_ms > close_ms + 2_000 {
                        return Ok(WatcherOutcome::ContractClosed);
                    }
                }
                render(&target, &book, depth, received, last_event_at, close_at_ms);
                last_render = std::time::Instant::now();
            }
            evt = sub.next() => {
                let Some(evt) = evt else {
                    return Ok(WatcherOutcome::StreamEnded);
                };
                let gap = book.apply(&evt);
                received += 1;
                last_event_at = Some(std::time::Instant::now());

                // Seq gap means we missed a delta; the local book has diverged
                // from the server. Request a fresh snapshot to resync. Throttle
                // so a sustained gap stream doesn't spam the server.
                if let Some(g) = gap {
                    let now = std::time::Instant::now();
                    let cooldown_passed = last_resync_request
                        .map(|t| now.duration_since(t) >= RESYNC_COOLDOWN)
                        .unwrap_or(true);
                    if cooldown_passed {
                        warn!(
                            expected = g.expected,
                            got = g.got,
                            gap = g.got.saturating_sub(g.expected),
                            "seq gap; requesting fresh snapshot"
                        );
                        last_resync_request = Some(now);
                        if let Err(e) = client
                            .update_subscription(&sub, UpdateAction::GetSnapshot, None)
                            .await
                        {
                            warn!("snapshot resync request failed: {e}");
                        }
                    } else {
                        warn!(
                            expected = g.expected,
                            got = g.got,
                            "seq gap (within resync cooldown -- not re-requesting yet)"
                        );
                    }
                }

                if last_render.elapsed() >= render_interval {
                    render(&target, &book, depth, received, last_event_at, close_at_ms);
                    last_render = std::time::Instant::now();
                }
            }
        }
    }
}

fn render(
    target: &MarketTarget,
    book: &Book,
    depth: usize,
    received: u64,
    last_event_at: Option<std::time::Instant>,
    close_at_ms: Option<i64>,
) {
    // Move cursor home + clear from there. Avoids the full-screen-clear flicker
    // on terminals that don't double-buffer.
    print!("\x1B[H\x1B[2J");

    println!("=== {} ===", target.ticker);
    if let Some(close) = &target.close_time {
        let countdown = close_at_ms
            .and_then(|ms| {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_millis() as i64;
                Some(ms - now_ms)
            })
            .map(|delta_ms| {
                if delta_ms < 0 {
                    "(closed)".to_string()
                } else {
                    let s = delta_ms / 1000;
                    format!("(closes in {}m {:02}s)", s / 60, s % 60)
                }
            })
            .unwrap_or_default();
        println!("closes {close}  {countdown}");
    }
    let freshness = last_event_at
        .map(|t| format!("last event {:.1}s ago", t.elapsed().as_secs_f64()))
        .unwrap_or_else(|| "no events yet".into());
    let gap_marker = if book.gaps_observed > 0 {
        format!("   gaps {}", book.gaps_observed)
    } else {
        String::new()
    };
    println!(
        "seq {:?}   recv {} (snap {} / delta {}){}   {}",
        book.last_seq,
        received,
        book.snapshots_applied,
        book.deltas_applied,
        gap_marker,
        freshness
    );
    println!();

    // Unified YES-centric book, filtered to non-crossing entries:
    //   asks (top of section, descending price → best ask just above the divider)
    //   ─── mid line ───
    //   bids (descending price → best bid just below the divider)
    //
    // Asks are derived from NO bids: ask_price_cents = 100 - no_bid_price_cents,
    // restricted to those strictly above best_yes_bid. Bids are restricted to
    // those strictly below the resulting best ask.
    let asks = book.unified_asks(depth);
    let best_ask = asks.first().map(|&(p, _)| p);
    let bids = book.unified_bids(depth, best_ask);
    let best_bid = bids.first().map(|&(p, _)| p);

    println!("{:>14} {:>12}", "price", "size");
    println!("{}", "-".repeat(28));
    // Print asks reversed: highest of the top-N at top, best ask closest to mid.
    for (price, size) in asks.iter().rev() {
        println!("{:>14} {:>12}    ASK", fmt_price(*price), size);
    }

    match (best_bid, best_ask) {
        (Some(b), Some(a)) => {
            let denom = PRICE_DENOM as f64;
            let mid = (b + a) as f64 / 2.0 / denom;
            let spread = (a - b) as f64 / denom;
            println!(
                "{}  mid ${:.4}  spread ${:.4}",
                "─".repeat(14),
                mid,
                spread
            );
        }
        _ => println!("{}  (one-sided)", "─".repeat(14)),
    }

    for (price, size) in bids.iter() {
        println!("{:>14} {:>12}    BID", fmt_price(*price), size);
    }

    // Surface that entries were hidden as crossed liquidity, so the user knows
    // the displayed book isn't the entire raw orderbook.
    if let (Some(yt), Some(nt)) = (book.best_yes_bid(), book.best_no_bid()) {
        if yt + nt > PRICE_DENOM {
            let crossed_no: u64 = book
                .inner
                .iter(BookSide::No)
                .filter(|(p, _)| (PRICE_DENOM - *p as i64) <= yt)
                .map(|(_, s)| s)
                .sum();
            let crossed_yes: u64 = book
                .inner
                .iter(BookSide::Yes)
                .filter(|(p, _)| best_ask.map_or(false, |a| (*p as i64) >= a))
                .map(|(_, s)| s)
                .sum();
            let crossed_size = crossed_no + crossed_yes;
            println!(
                "(crossed: yes_bid_top + no_bid_top = ${:.4}, hidden depth ≈ {} contracts)",
                (yt + nt) as f64 / PRICE_DENOM as f64,
                crossed_size
            );
        }
    }

    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn fmt_price(ticks: i64) -> String {
    format!("${:.4}", ticks as f64 / PRICE_DENOM as f64)
}
