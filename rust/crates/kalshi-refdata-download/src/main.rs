//! Downloads Kalshi reference data (series, events, markets) and the exchange
//! status snapshot, writing each entity to its own NDJSON file.
//!
//! ```text
//! cargo run -p kalshi-refdata-download -- --out-dir ./refdata --env production
//! ```
//!
//! Auth is optional — Kalshi's read-only reference endpoints don't require it.
//! If `--key-id` and `--key-pem` (or the matching env vars) are supplied, every
//! request gets the standard `kalshi-access-*` headers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use kalshi_ws::Credentials;
use reqwest::{Response, StatusCode};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::time::sleep;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnvArg {
    Production,
    Demo,
}

impl EnvArg {
    fn base_url(self) -> &'static str {
        match self {
            EnvArg::Production => "https://api.elections.kalshi.com",
            EnvArg::Demo => "https://demo-api.kalshi.co",
        }
    }
}

/// Filter values accepted by `/markets?status=` and `/events?status=`. Same enum
/// works for both endpoints. `All` is a sentinel meaning "do not send the
/// status query param" — giving the unfiltered (and much larger) set including
/// settled history.
///
/// `/series` does NOT accept this filter; series is a catalog of recurring
/// templates rather than a lifecycle-bearing entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LifecycleStatus {
    Unopened,
    Open,
    Closed,
    Settled,
    All,
}

impl LifecycleStatus {
    fn as_query_value(self) -> Option<&'static str> {
        match self {
            LifecycleStatus::Unopened => Some("unopened"),
            LifecycleStatus::Open => Some("open"),
            LifecycleStatus::Closed => Some("closed"),
            LifecycleStatus::Settled => Some("settled"),
            LifecycleStatus::All => None,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "kalshi-refdata-download", about, version)]
struct Cli {
    /// Output directory. Created if missing.
    #[arg(long, default_value = "./refdata")]
    out_dir: PathBuf,

    /// Kalshi environment to hit.
    #[arg(long, value_enum, default_value_t = EnvArg::Production)]
    env: EnvArg,

    /// Override the base URL (testing).
    #[arg(long)]
    base_url: Option<String>,

    /// Override page size for every endpoint. When unset, each endpoint uses its
    /// own default: /markets and /series request 1000, /events requests 200
    /// (Kalshi's cap there). Setting this forces all three to the same value.
    #[arg(long)]
    limit: Option<u32>,

    /// Minimum gap between successive requests, in milliseconds. Kalshi rate-limits
    /// per-endpoint; ~250ms (4 rps) is conservative and still completes /markets in
    /// a reasonable time. Lower at your own risk.
    #[arg(long, default_value_t = 250)]
    request_delay_ms: u64,

    /// How many times to retry a single page on 429 / 5xx before giving up.
    #[arg(long, default_value_t = 6)]
    max_retries: u32,

    /// Filter `/markets` by status. Defaults to no filter — pass `open` (the
    /// most common operational case) to skip the much larger settled history.
    #[arg(long, value_enum, default_value_t = LifecycleStatus::All)]
    markets_status: LifecycleStatus,

    /// Filter `/events` by status. Same vocabulary as `--markets-status`.
    /// Defaults to no filter; the script wrapper sets this to `open`.
    /// `/series` has no equivalent filter and is always fetched in full.
    #[arg(long, value_enum, default_value_t = LifecycleStatus::All)]
    events_status: LifecycleStatus,

    /// API key id. Falls back to `KALSHI_KEY_ID` env var.
    #[arg(long, env = "KALSHI_KEY_ID")]
    key_id: Option<String>,

    /// Path to PEM private key. Falls back to `KALSHI_KEY_PEM_PATH` env var.
    #[arg(long, env = "KALSHI_KEY_PEM_PATH")]
    key_pem: Option<PathBuf>,
}

/// Bundles the request-pacing knobs so they can flow through one parameter.
#[derive(Debug, Clone, Copy)]
struct RateLimit {
    delay: Duration,
    max_retries: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let base_url = cli
        .base_url
        .clone()
        .unwrap_or_else(|| cli.env.base_url().to_string());

    let creds = load_credentials(&cli)?;
    if creds.is_some() {
        info!("auth: using API key {:?}", cli.key_id.as_deref());
    } else {
        info!("auth: anonymous (reference endpoints don't require credentials)");
    }

    fs::create_dir_all(&cli.out_dir)
        .await
        .with_context(|| format!("creating {}", cli.out_dir.display()))?;

    let http = reqwest::Client::builder()
        .user_agent(concat!("kalshi-refdata-download/", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .build()?;

    let rl = RateLimit {
        delay: Duration::from_millis(cli.request_delay_ms),
        max_retries: cli.max_retries,
    };

    // Per-endpoint page-size defaults. /events caps at 200; /markets and /series
    // accept up to 1000 and finish far faster at the larger size.
    let series_limit = cli.limit.unwrap_or(1000);
    let events_limit = cli.limit.unwrap_or(200);
    let markets_limit = cli.limit.unwrap_or(1000);

    // Run all four endpoints concurrently. They paginate independently, each
    // pacing itself; if Kalshi 429s any one stream, that stream's retry layer
    // backs off without stalling the others.
    let creds_ref = creds.as_ref();
    let series_path = cli.out_dir.join("series.ndjson");
    let events_path = cli.out_dir.join("events.ndjson");
    let markets_path = cli.out_dir.join("markets.ndjson");
    let status_path = cli.out_dir.join("exchange_status.json");

    // Build the per-endpoint extra-query filters once. Helper folds `LifecycleStatus`
    // into a query slice and logs the resulting filter for visibility.
    fn status_filter(label: &str, s: LifecycleStatus) -> Vec<(&'static str, &'static str)> {
        match s.as_query_value() {
            Some(v) => {
                info!("{} filter: status={}", label, v);
                vec![("status", v)]
            }
            None => {
                info!("{} filter: none (full history)", label);
                vec![]
            }
        }
    }
    let markets_extra = status_filter("/markets", cli.markets_status);
    let events_extra = status_filter("/events", cli.events_status);
    info!("/series filter: not supported by Kalshi; fetching full catalog");

    let started = std::time::Instant::now();
    let (total_series, total_events, total_markets, _) = tokio::try_join!(
        paginate(
            &http,
            &base_url,
            "/trade-api/v2/series",
            "series",
            creds_ref,
            series_limit,
            &[],
            rl,
            &series_path,
        ),
        paginate(
            &http,
            &base_url,
            "/trade-api/v2/events",
            "events",
            creds_ref,
            events_limit,
            &events_extra,
            rl,
            &events_path,
        ),
        paginate(
            &http,
            &base_url,
            "/trade-api/v2/markets",
            "markets",
            creds_ref,
            markets_limit,
            &markets_extra,
            rl,
            &markets_path,
        ),
        fetch_one(
            &http,
            &base_url,
            "/trade-api/v2/exchange/status",
            creds_ref,
            rl,
            &status_path,
        ),
    )?;

    info!(
        "done in {:.1}s. series={} events={} markets={} out_dir={}",
        started.elapsed().as_secs_f64(),
        total_series,
        total_events,
        total_markets,
        cli.out_dir.display()
    );
    Ok(())
}

fn load_credentials(cli: &Cli) -> Result<Option<Credentials>> {
    match (&cli.key_id, &cli.key_pem) {
        (Some(id), Some(path)) => {
            let creds = Credentials::from_pem_file(id, path)
                .with_context(|| format!("loading key from {}", path.display()))?;
            Ok(Some(creds))
        }
        (None, None) => Ok(None),
        _ => {
            warn!("only one of --key-id / --key-pem provided; skipping auth");
            Ok(None)
        }
    }
}

/// Walks one paginated list endpoint to completion, writing each item as a line of NDJSON.
///
/// `extra_query` lets a caller attach endpoint-specific filters (e.g. `status=open`
/// for `/markets`) without adding more positional params for every future filter.
async fn paginate(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    items_field: &str,
    creds: Option<&Credentials>,
    limit: u32,
    extra_query: &[(&str, &str)],
    rl: RateLimit,
    out_path: &Path,
) -> Result<u64> {
    let file = fs::File::create(out_path)
        .await
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);

    let mut cursor: Option<String> = None;
    let mut total: u64 = 0;
    let mut page: u64 = 0;

    loop {
        let resp = send_with_retry(
            http,
            base_url,
            path,
            cursor.as_deref(),
            Some(limit),
            extra_query,
            creds,
            rl,
        )
        .await?;
        let body: Value = resp.json().await.context("parsing JSON page")?;
        let items = body
            .get(items_field)
            .and_then(|v| v.as_array())
            .with_context(|| format!("missing/non-array `{items_field}` in response"))?;

        for item in items {
            let line = serde_json::to_string(item)?;
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            total += 1;
        }
        page += 1;
        info!(
            "{} page={} items_in_page={} total={}",
            path,
            page,
            items.len(),
            total
        );

        let next = body
            .get("cursor")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }

        // Pace the next request. Skipped on the final page since we just exited.
        sleep(rl.delay).await;
    }

    writer.flush().await?;
    info!("{} -> {} ({} records)", path, out_path.display(), total);
    Ok(total)
}

/// Fetches a single non-paginated endpoint and writes the response body verbatim.
async fn fetch_one(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    creds: Option<&Credentials>,
    rl: RateLimit,
    out_path: &Path,
) -> Result<()> {
    let resp = send_with_retry(http, base_url, path, None, None, &[], creds, rl).await?;
    let bytes = resp.bytes().await?;

    let mut file = fs::File::create(out_path)
        .await
        .with_context(|| format!("creating {}", out_path.display()))?;
    file.write_all(&bytes).await?;
    file.flush().await?;
    info!("{} -> {} ({} bytes)", path, out_path.display(), bytes.len());
    Ok(())
}

/// Sends one request, retrying on 429 (Too Many Requests) and 5xx with backoff.
/// Honours the `Retry-After` header when present (seconds; HTTP-date form is
/// uncommon for Kalshi and not parsed).
async fn send_with_retry(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    cursor: Option<&str>,
    limit: Option<u32>,
    extra_query: &[(&str, &str)],
    creds: Option<&Credentials>,
    rl: RateLimit,
) -> Result<Response> {
    let mut attempt: u32 = 0;
    loop {
        let mut req = http.get(format!("{base_url}{path}"));
        if let Some(l) = limit {
            req = req.query(&[("limit", l.to_string())]);
        }
        if let Some(c) = cursor {
            req = req.query(&[("cursor", c)]);
        }
        if !extra_query.is_empty() {
            req = req.query(extra_query);
        }
        if let Some(creds) = creds {
            // Re-sign every attempt — signatures embed a timestamp the server
            // validates against its clock.
            for (name, value) in creds.signed_headers("GET", path) {
                req = req.header(name, value);
            }
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("GET {base_url}{path}"))?;
        let status = resp.status();

        if status.is_success() {
            return Ok(resp);
        }

        let retryable =
            status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
        if !retryable || attempt >= rl.max_retries {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("{path} returned {status}: {body}");
        }

        // Honour Retry-After (seconds). Otherwise exponential backoff: 1, 2, 4, 8, 16, 32 s.
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs);
        let backoff = retry_after
            .unwrap_or_else(|| Duration::from_secs(1u64 << attempt.min(5)));
        warn!(
            "{path} -> {status} (attempt {}/{}), backing off {:?}",
            attempt + 1,
            rl.max_retries,
            backoff
        );
        sleep(backoff).await;
        attempt += 1;
    }
}
