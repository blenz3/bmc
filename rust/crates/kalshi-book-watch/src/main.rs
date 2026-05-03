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

use std::collections::BTreeMap;
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

    let creds = load_credentials(&cli)?;
    let mut builder = Client::builder().environment(cli.env.ws_environment());
    if let Some(c) = creds {
        builder = builder.credentials(c);
    }
    let client = builder
        .connect()
        .await
        .context("connecting to Kalshi WebSocket")?;

    let sub = client
        .subscribe_orderbook(vec![target.ticker.clone()])
        .await
        .context("subscribing to orderbook_delta")?;

    let render = Duration::from_millis(cli.render_interval_ms);
    run_watcher(target, sub, cli.depth, render).await?;

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

/// Internal price unit: 1/10000 of a dollar (one "deci-cent" tick) so the book
/// represents Kalshi's full sub-penny precision losslessly. The wire format
/// supports up to 4 decimal places — `linear_cent` markets use $0.01 ticks
/// (multiples of 100 here), `deci_cent` and `tapered_deci_cent` markets use
/// $0.001 ticks (multiples of 10).
const PRICE_DENOM: i64 = 10_000;

#[derive(Default)]
struct Book {
    /// Price (in deci-cents, 1..=PRICE_DENOM-1) → size in fixed-point contracts.
    yes: BTreeMap<i64, i64>,
    no: BTreeMap<i64, i64>,
    last_seq: Option<u64>,
    last_update_ms: Option<i64>,
}

impl Book {
    fn apply(&mut self, evt: &OrderbookEvent) {
        match evt {
            OrderbookEvent::Snapshot { seq, snapshot } => {
                self.yes.clear();
                self.no.clear();
                for (price, size) in &snapshot.yes_dollars_fp {
                    self.set(Side::Yes, *price, *size);
                }
                for (price, size) in &snapshot.no_dollars_fp {
                    self.set(Side::No, *price, *size);
                }
                self.last_seq = Some(*seq);
                self.last_update_ms = snapshot.ts_ms;
            }
            OrderbookEvent::Delta { seq, delta } => {
                self.delta(delta.side, delta.price_dollars, delta.delta_fp);
                self.last_seq = Some(*seq);
                self.last_update_ms = delta.ts_ms;
            }
        }
    }

    fn set(&mut self, side: Side, price: f64, size: i64) {
        let key = price_key(price);
        let map = self.side_mut(side);
        if size > 0 {
            map.insert(key, size);
        } else {
            map.remove(&key);
        }
    }

    fn delta(&mut self, side: Side, price: f64, delta: i64) {
        let key = price_key(price);
        let map = self.side_mut(side);
        let entry = map.entry(key).or_insert(0);
        *entry += delta;
        if *entry <= 0 {
            map.remove(&key);
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<i64, i64> {
        match side {
            Side::Yes => &mut self.yes,
            Side::No => &mut self.no,
        }
    }

    /// YES-equivalent asks derived from NO bids
    /// (`ask_price = PRICE_DENOM - no_bid_price`), in ascending price order
    /// (best/lowest first), capped to `depth`.
    ///
    /// Filters out asks that don't strictly exceed the best YES bid. Without
    /// this filter, deep standing NO bids at e.g. $0.99 appear as YES asks at
    /// $0.01 — visually below the YES bid book, which inverts the display.
    /// Such entries represent crossed liquidity (the matching engine should
    /// have already filled them, or they're protected by self-trade prevention,
    /// or they're conditional / iceberg orders that don't auto-cross).
    fn unified_asks(&self, depth: usize) -> Vec<(i64, i64)> {
        let best_yes_bid = self.best_yes_bid().unwrap_or(0);
        self.no
            .iter()
            .filter(|(&no_p, _)| (PRICE_DENOM - no_p) > best_yes_bid)
            // Keys ascending → after filter, .rev() iterates descending no_p,
            // i.e., ascending YES ask price. Best (lowest) ask first.
            .rev()
            .take(depth)
            .map(|(&no_p, &s)| (PRICE_DENOM - no_p, s))
            .collect()
    }

    /// YES bids in descending price order (best/highest first), capped to `depth`.
    /// `best_ask_cap` filters out bids that would cross the displayed asks (any
    /// such bid is an obviously stale / self-protected entry).
    fn unified_bids(&self, depth: usize, best_ask_cap: Option<i64>) -> Vec<(i64, i64)> {
        let upper = best_ask_cap.unwrap_or(PRICE_DENOM + 1); // > any valid price
        self.yes
            .iter()
            .filter(|(&p, _)| p < upper)
            .rev()
            .take(depth)
            .map(|(&p, &s)| (p, s))
            .collect()
    }

    fn best_yes_bid(&self) -> Option<i64> {
        self.yes.keys().next_back().copied()
    }
}

fn price_key(p: f64) -> i64 {
    (p * PRICE_DENOM as f64).round() as i64
}

async fn run_watcher(
    target: MarketTarget,
    mut sub: Subscription<OrderbookEvent>,
    depth: usize,
    render_interval: Duration,
) -> Result<()> {
    let mut book = Book::default();
    let mut last_render = std::time::Instant::now() - render_interval;
    let mut received = 0u64;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nshutting down on Ctrl+C");
                return Ok(());
            }
            evt = sub.next() => {
                let Some(evt) = evt else {
                    return Err(anyhow!("subscription stream ended"));
                };
                book.apply(&evt);
                received += 1;
                if last_render.elapsed() >= render_interval {
                    render(&target, &book, depth, received);
                    last_render = std::time::Instant::now();
                }
            }
        }
    }
}

fn render(target: &MarketTarget, book: &Book, depth: usize, received: u64) {
    // Move cursor home + clear from there. Avoids the full-screen-clear flicker
    // on terminals that don't double-buffer.
    print!("\x1B[H\x1B[2J");

    println!("=== {} ===", target.ticker);
    if let Some(close) = &target.close_time {
        println!("closes {close}");
    }
    println!("seq {:?}   updates received {}", book.last_seq, received);
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
    let raw_no_top = book.no.keys().next_back().copied();
    let raw_yes_top = book.yes.keys().next_back().copied();
    if let (Some(yt), Some(nt)) = (raw_yes_top, raw_no_top) {
        if yt + nt > PRICE_DENOM {
            let crossed_size: i64 = book
                .no
                .iter()
                .filter(|(&p, _)| (PRICE_DENOM - p) <= yt)
                .map(|(_, &s)| s)
                .sum::<i64>()
                + book
                    .yes
                    .iter()
                    .filter(|(&p, _)| best_ask.map_or(false, |a| p >= a))
                    .map(|(_, &s)| s)
                    .sum::<i64>();
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
