//! Downloads Kalshi reference data (series, events, markets) and the exchange
//! status snapshot, writing each entity to its own NDJSON file.
//!
//! ```text
//! cargo run -p refdata-downloader -- --out-dir ./refdata --env production
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

#[derive(Debug, Parser)]
#[command(name = "refdata-downloader", about, version)]
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

    /// Page size. Kalshi caps at 200 for /events and 1000 for /markets in practice;
    /// 200 is a safe default that works everywhere.
    #[arg(long, default_value_t = 200)]
    limit: u32,

    /// Minimum gap between successive requests, in milliseconds. Kalshi rate-limits
    /// per-endpoint; ~250ms (4 rps) is conservative and still completes /markets in
    /// a reasonable time. Lower at your own risk.
    #[arg(long, default_value_t = 250)]
    request_delay_ms: u64,

    /// How many times to retry a single page on 429 / 5xx before giving up.
    #[arg(long, default_value_t = 6)]
    max_retries: u32,

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
        .user_agent(concat!("kalshi-refdata-downloader/", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .build()?;

    let rl = RateLimit {
        delay: Duration::from_millis(cli.request_delay_ms),
        max_retries: cli.max_retries,
    };

    // Sequential to keep request load gentle.
    let total_series = paginate(
        &http,
        &base_url,
        "/trade-api/v2/series",
        "series",
        creds.as_ref(),
        cli.limit,
        rl,
        &cli.out_dir.join("series.ndjson"),
    )
    .await
    .context("downloading /series")?;

    let total_events = paginate(
        &http,
        &base_url,
        "/trade-api/v2/events",
        "events",
        creds.as_ref(),
        cli.limit,
        rl,
        &cli.out_dir.join("events.ndjson"),
    )
    .await
    .context("downloading /events")?;

    let total_markets = paginate(
        &http,
        &base_url,
        "/trade-api/v2/markets",
        "markets",
        creds.as_ref(),
        cli.limit,
        rl,
        &cli.out_dir.join("markets.ndjson"),
    )
    .await
    .context("downloading /markets")?;

    fetch_one(
        &http,
        &base_url,
        "/trade-api/v2/exchange/status",
        creds.as_ref(),
        rl,
        &cli.out_dir.join("exchange_status.json"),
    )
    .await
    .context("downloading /exchange/status")?;

    info!(
        "done. series={} events={} markets={} out_dir={}",
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
async fn paginate(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    items_field: &str,
    creds: Option<&Credentials>,
    limit: u32,
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
        let resp = send_with_retry(http, base_url, path, cursor.as_deref(), Some(limit), creds, rl)
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
    let resp = send_with_retry(http, base_url, path, None, None, creds, rl).await?;
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
