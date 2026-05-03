//! Subscribe to the authenticated `fill` and `user_orders` channels.
//!
//! Set `KALSHI_KEY_ID` and `KALSHI_KEY_PEM_PATH` before running:
//!
//! ```text
//! $env:KALSHI_KEY_ID = "..."
//! $env:KALSHI_KEY_PEM_PATH = "C:\path\to\key.pem"
//! cargo run --example private_fills
//! ```

use futures_util::StreamExt;
use kalshi_ws::{Client, Credentials, Environment};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let key_id = std::env::var("KALSHI_KEY_ID").expect("KALSHI_KEY_ID not set");
    let pem_path = std::env::var("KALSHI_KEY_PEM_PATH").expect("KALSHI_KEY_PEM_PATH not set");
    let creds = Credentials::from_pem_file(key_id, pem_path)?;

    let client = Client::builder()
        .environment(Environment::Demo)
        .credentials(creds)
        .connect()
        .await?;

    let mut fills = client.subscribe_fill(None).await?;
    let mut orders = client.subscribe_user_orders(None).await?;
    let mut events = client.system_events();

    println!("subscribed; waiting for fills/orders/events...");

    loop {
        tokio::select! {
            f = fills.next() => match f {
                Some(fill) => println!("FILL  {} {:?} {}@{}",
                    fill.market_ticker, fill.action, fill.count_fp, fill.yes_price_dollars),
                None => break,
            },
            o = orders.next() => match o {
                Some(order) => println!("ORDER {} {:?} side={:?} remaining_fp={}",
                    order.ticker, order.status, order.side, order.remaining_count_fp),
                None => break,
            },
            ev = events.recv() => match ev {
                Ok(ev) => eprintln!("[system] {ev:?}"),
                Err(_) => break,
            }
        }
    }
    Ok(())
}
