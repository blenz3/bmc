//! Command-line interface for the Kalshi REST trading API.
//!
//! Subcommands map 1:1 to `kalshi_rest::Client` methods. Auth pulls from
//! `KALSHI_KEY_ID` / `KALSHI_KEY_PEM_PATH` env vars (or `--key-id` / `--key-pem`
//! flags). `--paper` flips the client into Mode::Paper, which short-circuits
//! destructive calls with a clear error — useful for dry-running an order
//! before sending for real.
//!
//! All subcommands accept `--json` for machine-readable output (pipe into `jq`).

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use kalshi_rest::{
    Action, Client, Credentials, DecreaseAmount, Environment, ListOrdersFilter, Mode, Order,
    OrderRequest, OrderStatus, RestError, SelfTradePreventionType, Side, TimeInForce,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnvArg {
    Production,
    Demo,
}

impl EnvArg {
    fn into_environment(self) -> Environment {
        match self {
            EnvArg::Production => Environment::Production,
            EnvArg::Demo => Environment::Demo,
        }
    }
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Kalshi environment to hit.
    #[arg(long, value_enum, default_value_t = EnvArg::Production, global = true)]
    env: EnvArg,

    /// Use Mode::Paper — destructive calls (place/cancel/decrease) refuse with
    /// a clear error so you can dry-run order entry without risk.
    #[arg(long, global = true)]
    paper: bool,

    /// API key id. Falls back to `KALSHI_KEY_ID`.
    #[arg(long, env = "KALSHI_KEY_ID", global = true)]
    key_id: Option<String>,

    /// PEM private key path. Falls back to `KALSHI_KEY_PEM_PATH`.
    #[arg(long, env = "KALSHI_KEY_PEM_PATH", global = true)]
    key_pem: Option<PathBuf>,

    /// Output JSON instead of human-readable tables.
    #[arg(long, global = true)]
    json: bool,

    /// Skip the confirmation prompt on destructive ops (place / cancel / decrease).
    #[arg(long, short = 'y', global = true)]
    yes: bool,

    /// Per-request HTTP timeout, in seconds.
    #[arg(long, default_value_t = 15, global = true)]
    timeout_secs: u64,
}

#[derive(Debug, Parser)]
#[command(name = "kalshi-cli", about, version)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show account balance.
    Balance,

    /// List portfolio positions.
    Positions {
        /// Filter to a single market ticker.
        #[arg(long)]
        ticker: Option<String>,
    },

    /// List orders with optional filters. Returns the first page only by default.
    Orders {
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long)]
        event_ticker: Option<String>,
        #[arg(long, value_enum)]
        status: Option<OrderStatusArg>,
        /// Page size (1-1000).
        #[arg(long)]
        limit: Option<u32>,
        /// Pagination cursor for fetching subsequent pages.
        #[arg(long)]
        cursor: Option<String>,
    },

    /// Show one order by id.
    Order {
        order_id: String,
    },

    /// Show recent fills (one page).
    Fills {
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        cursor: Option<String>,
    },

    /// Place a new order. At least one of --yes-price or --no-price is required
    /// for limit orders; market orders use --buy-max-cost.
    Place {
        /// Market ticker, e.g. KXBTC15M-26MAY031715-T87749.99
        #[arg(long)]
        ticker: String,
        /// Which side (yes / no).
        #[arg(long, value_enum)]
        side: SideArg,
        /// buy or sell.
        #[arg(long, value_enum)]
        action: ActionArg,
        /// Whole-contract count.
        #[arg(long)]
        count: u64,
        /// Limit price for the YES side, in cents (1..=99).
        #[arg(long, conflicts_with = "no_price")]
        yes_price: Option<u8>,
        /// Limit price for the NO side, in cents (1..=99).
        #[arg(long, conflicts_with = "yes_price")]
        no_price: Option<u8>,
        /// Time-in-force.
        #[arg(long, value_enum)]
        tif: Option<TifArg>,
        /// Maximum cost in cents (implies fill-or-kill).
        #[arg(long)]
        buy_max_cost: Option<u64>,
        /// post-only: order rests; rejected if it would cross.
        #[arg(long)]
        post_only: bool,
        /// reduce-only: only fills against an existing position.
        #[arg(long)]
        reduce_only: bool,
        /// Self-trade prevention type.
        #[arg(long, value_enum)]
        stp: Option<StpArg>,
        /// Custom client_order_id (default: auto-generated UUID v4).
        #[arg(long)]
        client_order_id: Option<String>,
        /// Subaccount number (0 = primary, 1..32 = others).
        #[arg(long)]
        subaccount: Option<u32>,
    },

    /// Cancel an open order.
    Cancel {
        order_id: String,
    },

    /// Decrease the remaining size of an open order.
    Decrease {
        order_id: String,
        /// Reduce remaining contracts by N.
        #[arg(long, conflicts_with = "to")]
        by: Option<u64>,
        /// Reduce remaining contracts to N (absolute target).
        #[arg(long, conflicts_with = "by")]
        to: Option<u64>,
    },
}

// -- Wrapper enums (clap-friendly) → kalshi_rest types --------------------------

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SideArg {
    Yes,
    No,
}

impl From<SideArg> for Side {
    fn from(s: SideArg) -> Self {
        match s {
            SideArg::Yes => Side::Yes,
            SideArg::No => Side::No,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ActionArg {
    Buy,
    Sell,
}

impl From<ActionArg> for Action {
    fn from(a: ActionArg) -> Self {
        match a {
            ActionArg::Buy => Action::Buy,
            ActionArg::Sell => Action::Sell,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OrderStatusArg {
    Resting,
    Canceled,
    Executed,
}

impl From<OrderStatusArg> for OrderStatus {
    fn from(s: OrderStatusArg) -> Self {
        match s {
            OrderStatusArg::Resting => OrderStatus::Resting,
            OrderStatusArg::Canceled => OrderStatus::Canceled,
            OrderStatusArg::Executed => OrderStatus::Executed,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TifArg {
    /// fill_or_kill — match in full immediately or cancel.
    Fok,
    /// good_till_canceled — rest until filled or canceled.
    Gtc,
    /// immediate_or_cancel — fill what you can immediately, cancel the rest.
    Ioc,
}

impl From<TifArg> for TimeInForce {
    fn from(t: TifArg) -> Self {
        match t {
            TifArg::Fok => TimeInForce::FillOrKill,
            TifArg::Gtc => TimeInForce::GoodTillCanceled,
            TifArg::Ioc => TimeInForce::ImmediateOrCancel,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StpArg {
    /// taker_at_cross — incoming taker side cancels.
    Taker,
    /// maker — resting maker side cancels.
    Maker,
}

impl From<StpArg> for SelfTradePreventionType {
    fn from(s: StpArg) -> Self {
        match s {
            StpArg::Taker => SelfTradePreventionType::TakerAtCross,
            StpArg::Maker => SelfTradePreventionType::Maker,
        }
    }
}

// -- Main ---------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,kalshi_cli=info")),
        )
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let client = build_client(&cli.common)?;

    match cli.command {
        Command::Balance => cmd_balance(&client, &cli.common).await,
        Command::Positions { ticker } => cmd_positions(&client, &cli.common, ticker.as_deref()).await,
        Command::Orders {
            ticker,
            event_ticker,
            status,
            limit,
            cursor,
        } => {
            let filter = ListOrdersFilter {
                ticker,
                event_ticker,
                status: status.map(Into::into),
                limit,
                cursor,
                ..ListOrdersFilter::default()
            };
            cmd_orders(&client, &cli.common, &filter).await
        }
        Command::Order { order_id } => cmd_order(&client, &cli.common, &order_id).await,
        Command::Fills {
            ticker,
            limit,
            cursor,
        } => cmd_fills(&client, &cli.common, ticker.as_deref(), limit, cursor.as_deref()).await,
        Command::Place {
            ticker,
            side,
            action,
            count,
            yes_price,
            no_price,
            tif,
            buy_max_cost,
            post_only,
            reduce_only,
            stp,
            client_order_id,
            subaccount,
        } => {
            let mut req = OrderRequest {
                ticker,
                side: side.into(),
                action: action.into(),
                client_order_id: client_order_id.unwrap_or_else(kalshi_rest::types::new_client_order_id),
                count: Some(count),
                count_fp: None,
                yes_price,
                no_price,
                time_in_force: tif.map(Into::into),
                buy_max_cost,
                post_only: if post_only { Some(true) } else { None },
                reduce_only: if reduce_only { Some(true) } else { None },
                self_trade_prevention_type: stp.map(Into::into),
                order_group_id: None,
                cancel_order_on_pause: None,
                expiration_ts: None,
                subaccount,
            };
            // If neither price nor max_cost set, that's an error.
            if req.yes_price.is_none() && req.no_price.is_none() && req.buy_max_cost.is_none() {
                return Err(anyhow!(
                    "must specify one of --yes-price, --no-price, or --buy-max-cost"
                ));
            }
            cmd_place(&client, &cli.common, &mut req).await
        }
        Command::Cancel { order_id } => cmd_cancel(&client, &cli.common, &order_id).await,
        Command::Decrease { order_id, by, to } => {
            let amount = match (by, to) {
                (Some(n), None) => DecreaseAmount::ReduceBy(n),
                (None, Some(n)) => DecreaseAmount::ReduceTo(n),
                (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents both"),
                (None, None) => return Err(anyhow!("specify --by N or --to N")),
            };
            cmd_decrease(&client, &cli.common, &order_id, amount).await
        }
    }
}

fn build_client(c: &CommonArgs) -> Result<Client> {
    let key_id = c
        .key_id
        .clone()
        .ok_or_else(|| anyhow!("--key-id or KALSHI_KEY_ID required"))?;
    let key_pem = c
        .key_pem
        .clone()
        .ok_or_else(|| anyhow!("--key-pem or KALSHI_KEY_PEM_PATH required"))?;
    let creds = Credentials::from_pem_file(&key_id, &key_pem)
        .with_context(|| format!("loading PEM key from {}", key_pem.display()))?;
    let mode = if c.paper { Mode::Paper } else { Mode::Live };
    let client = Client::builder()
        .environment(c.env.into_environment())
        .credentials(creds)
        .mode(mode)
        .request_timeout(Duration::from_secs(c.timeout_secs))
        .build()?;
    Ok(client)
}

// -- Subcommand handlers ------------------------------------------------------

async fn cmd_balance(client: &Client, common: &CommonArgs) -> Result<()> {
    let bal = client.get_balance().await?;
    if common.json {
        println!("{}", serde_json::to_string_pretty(&bal)?);
        return Ok(());
    }
    println!(
        "Balance:       {}",
        bal.balance_dollars
            .map(|v| format!("${:.2}", v))
            .unwrap_or_else(|| "<none>".to_string())
    );
    println!(
        "Withdrawable:  {}",
        bal.payout_dollars
            .map(|v| format!("${:.2}", v))
            .unwrap_or_else(|| "<none>".to_string())
    );
    if !bal.extra.is_empty() {
        println!("Extra:");
        for (k, v) in &bal.extra {
            println!("  {k}: {v}");
        }
    }
    Ok(())
}

async fn cmd_positions(client: &Client, common: &CommonArgs, ticker: Option<&str>) -> Result<()> {
    let positions = client.get_positions(ticker).await?;
    if common.json {
        println!("{}", serde_json::to_string_pretty(&positions)?);
        return Ok(());
    }
    if positions.market_positions.is_empty() && positions.event_positions.is_empty() {
        println!("(no positions)");
        return Ok(());
    }
    println!(
        "{:<40} {:>10} {:>12} {:>14} {:>10}",
        "TICKER", "POS", "COST $", "REALIZED PNL", "FEES $"
    );
    for p in &positions.market_positions {
        println!(
            "{:<40} {:>10} {:>12} {:>14} {:>10}",
            truncate(&p.ticker, 40),
            p.position_fp.map(|n| n.to_string()).unwrap_or_default(),
            p.market_exposure_dollars
                .map(|v| format!("{:.2}", v))
                .unwrap_or_default(),
            p.realized_pnl_dollars
                .map(|v| format!("{:.2}", v))
                .unwrap_or_default(),
            p.fees_paid_dollars
                .map(|v| format!("{:.2}", v))
                .unwrap_or_default(),
        );
    }
    if !positions.event_positions.is_empty() {
        println!();
        println!(
            "{:<40} {:>14} {:>14} {:>10}",
            "EVENT", "EXPOSURE $", "REALIZED $", "FEES $"
        );
        for p in &positions.event_positions {
            println!(
                "{:<40} {:>14} {:>14} {:>10}",
                truncate(&p.event_ticker, 40),
                p.event_exposure_dollars
                    .map(|v| format!("{:.2}", v))
                    .unwrap_or_default(),
                p.realized_pnl_dollars
                    .map(|v| format!("{:.2}", v))
                    .unwrap_or_default(),
                p.fees_paid_dollars
                    .map(|v| format!("{:.2}", v))
                    .unwrap_or_default(),
            );
        }
    }
    Ok(())
}

async fn cmd_orders(client: &Client, common: &CommonArgs, filter: &ListOrdersFilter) -> Result<()> {
    let page = client.list_orders(filter).await?;
    if common.json {
        let body = serde_json::json!({
            "orders": &page.items,
            "cursor": &page.cursor,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no orders)");
        return Ok(());
    }
    println!(
        "{:<22} {:<35} {:<5} {:<5} {:<8} {:>9} {:>10}",
        "ORDER_ID", "TICKER", "SIDE", "ACT", "STATUS", "PRICE", "FILL/INIT"
    );
    for o in &page.items {
        print_order_row(o);
    }
    if let Some(cursor) = &page.cursor {
        println!();
        println!("(next cursor: {cursor})");
    }
    Ok(())
}

async fn cmd_order(client: &Client, common: &CommonArgs, order_id: &str) -> Result<()> {
    let o = client.get_order(order_id).await?;
    if common.json {
        println!("{}", serde_json::to_string_pretty(&o)?);
        return Ok(());
    }
    print_order_detailed(&o);
    Ok(())
}

async fn cmd_fills(
    client: &Client,
    common: &CommonArgs,
    ticker: Option<&str>,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<()> {
    let page = client.get_fills(ticker, cursor, limit).await?;
    if common.json {
        let body = serde_json::json!({
            "fills": &page.items,
            "cursor": &page.cursor,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no fills)");
        return Ok(());
    }
    println!(
        "{:<24} {:<22} {:<35} {:<5} {:<5} {:>9} {:>8} {:<25}",
        "TRADE_ID", "ORDER_ID", "TICKER", "SIDE", "ACT", "YES_PX", "COUNT", "TIME"
    );
    for f in &page.items {
        println!(
            "{:<24} {:<22} {:<35} {:<5} {:<5} {:>9} {:>8} {:<25}",
            truncate(&f.trade_id, 24),
            truncate(&f.order_id, 22),
            truncate(&f.ticker, 35),
            format!("{:?}", f.side).to_lowercase(),
            format!("{:?}", f.action).to_lowercase(),
            f.yes_price_dollars
                .map(|v| format!("${:.2}", v))
                .unwrap_or_default(),
            f.count_fp.map(|n| n.to_string()).unwrap_or_default(),
            f.created_time.as_deref().unwrap_or(""),
        );
    }
    if let Some(cursor) = &page.cursor {
        println!();
        println!("(next cursor: {cursor})");
    }
    Ok(())
}

async fn cmd_place(client: &Client, common: &CommonArgs, req: &mut OrderRequest) -> Result<()> {
    print_pending_order(req);
    if !common.yes && !confirm("Send this order?")? {
        eprintln!("aborted");
        return Ok(());
    }
    let result = client.place_order(req.clone()).await;
    match result {
        Ok(o) => {
            if common.json {
                println!("{}", serde_json::to_string_pretty(&o)?);
            } else {
                println!("Placed:");
                print_order_detailed(&o);
            }
            Ok(())
        }
        Err(RestError::PaperRefused { action }) => {
            eprintln!(
                "[paper mode] would have called {} — no order sent. The constructed request was printed above.",
                action
            );
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn cmd_cancel(client: &Client, common: &CommonArgs, order_id: &str) -> Result<()> {
    if !common.yes {
        // Fetch current state for context — costs one extra API call but the
        // safety win is worth it.
        match client.get_order(order_id).await {
            Ok(current) => {
                println!("Order to cancel:");
                print_order_detailed(&current);
            }
            Err(e) => {
                eprintln!("(failed to look up current state: {e})");
                println!("Cancel order: {order_id}");
            }
        }
        if !confirm("Cancel?")? {
            eprintln!("aborted");
            return Ok(());
        }
    }
    let result = client.cancel_order(order_id).await;
    match result {
        Ok(o) => {
            if common.json {
                println!("{}", serde_json::to_string_pretty(&o)?);
            } else {
                println!("Canceled:");
                print_order_detailed(&o);
            }
            Ok(())
        }
        Err(RestError::PaperRefused { action }) => {
            eprintln!("[paper mode] would have called {action} — no cancel sent");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn cmd_decrease(
    client: &Client,
    common: &CommonArgs,
    order_id: &str,
    amount: DecreaseAmount,
) -> Result<()> {
    let amount_label = match amount {
        DecreaseAmount::ReduceBy(n) => format!("reduce by {n}"),
        DecreaseAmount::ReduceTo(n) => format!("reduce to {n}"),
    };
    if !common.yes {
        match client.get_order(order_id).await {
            Ok(current) => {
                println!("Order to {amount_label}:");
                print_order_detailed(&current);
            }
            Err(e) => {
                eprintln!("(failed to look up current state: {e})");
                println!("Decrease ({amount_label}): {order_id}");
            }
        }
        if !confirm("Apply?")? {
            eprintln!("aborted");
            return Ok(());
        }
    }
    let result = client.decrease_order(order_id, amount).await;
    match result {
        Ok(o) => {
            if common.json {
                println!("{}", serde_json::to_string_pretty(&o)?);
            } else {
                println!("Decreased:");
                print_order_detailed(&o);
            }
            Ok(())
        }
        Err(RestError::PaperRefused { action }) => {
            eprintln!("[paper mode] would have called {action} — no change sent");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

// -- Output helpers ------------------------------------------------------------

fn print_order_row(o: &Order) {
    let price = match (o.yes_price_dollars, o.no_price_dollars) {
        (Some(y), _) if y > 0.0 => format!("YES ${:.2}", y),
        (_, Some(n)) if n > 0.0 => format!("NO  ${:.2}", n),
        _ => "-".to_string(),
    };
    let fill_init = format!(
        "{}/{}",
        o.fill_count_fp.unwrap_or(0),
        o.initial_count_fp.unwrap_or(0)
    );
    println!(
        "{:<22} {:<35} {:<5} {:<5} {:<8} {:>9} {:>10}",
        truncate(&o.order_id, 22),
        truncate(&o.ticker, 35),
        format!("{:?}", o.side).to_lowercase(),
        format!("{:?}", o.action).to_lowercase(),
        format!("{:?}", o.status).to_lowercase(),
        price,
        fill_init,
    );
}

fn print_order_detailed(o: &Order) {
    println!("  order_id:           {}", o.order_id);
    println!("  client_order_id:    {}", o.client_order_id);
    println!("  ticker:             {}", o.ticker);
    println!(
        "  side / action:      {:?} {:?}",
        o.side, o.action
    );
    println!("  type:               {:?}", o.order_type);
    println!("  status:             {:?}", o.status);
    if let Some(v) = o.yes_price_dollars {
        println!("  yes_price:          ${:.4}", v);
    }
    if let Some(v) = o.no_price_dollars {
        println!("  no_price:           ${:.4}", v);
    }
    println!(
        "  count fill/remaining/initial: {}/{}/{}",
        o.fill_count_fp.unwrap_or(0),
        o.remaining_count_fp.unwrap_or(0),
        o.initial_count_fp.unwrap_or(0),
    );
    if let Some(t) = &o.created_time {
        println!("  created:            {t}");
    }
    if let Some(t) = &o.last_update_time {
        println!("  last_update:        {t}");
    }
    if let Some(stp) = o.self_trade_prevention_type {
        println!("  stp:                {:?}", stp);
    }
}

fn print_pending_order(req: &OrderRequest) {
    println!("Order to place:");
    println!("  ticker:             {}", req.ticker);
    println!("  side / action:      {:?} {:?}", req.side, req.action);
    println!("  client_order_id:    {}", req.client_order_id);
    if let Some(c) = req.count {
        println!("  count:              {} contracts", c);
    }
    if let Some(p) = req.yes_price {
        println!("  yes_price:          {} cents (${:.2})", p, p as f64 / 100.0);
    }
    if let Some(p) = req.no_price {
        println!("  no_price:           {} cents (${:.2})", p, p as f64 / 100.0);
    }
    if let Some(t) = req.time_in_force {
        println!("  time_in_force:      {:?}", t);
    }
    if let Some(c) = req.buy_max_cost {
        println!("  buy_max_cost:       {} cents (${:.2})", c, c as f64 / 100.0);
    }
    if matches!(req.post_only, Some(true)) {
        println!("  post_only:          true");
    }
    if matches!(req.reduce_only, Some(true)) {
        println!("  reduce_only:        true");
    }
    if let Some(stp) = req.self_trade_prevention_type {
        println!("  stp:                {:?}", stp);
    }
    if let Some(s) = req.subaccount {
        println!("  subaccount:         {s}");
    }
    // Estimate worst-case cost for the trader's awareness.
    if req.action == Action::Buy {
        let cents = req
            .yes_price
            .or(req.no_price)
            .map(|p| p as u64)
            .or(req.buy_max_cost);
        if let (Some(c), Some(n)) = (cents, req.count) {
            let total = (c * n) as f64 / 100.0;
            println!("  est. cost:          ~${:.2}", total);
        }
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt} [y/N]: ");
    io::stderr().flush().ok();
    let mut buf = String::new();
    io::stdin()
        .lock()
        .read_line(&mut buf)
        .context("reading confirmation")?;
    let answer = buf.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}
