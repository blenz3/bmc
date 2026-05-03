//! Subscribe to the public `ticker` channel for a few markets and print updates.
//!
//! ```text
//! cargo run --example public_ticker
//! ```

use std::time::Duration;

use futures_util::StreamExt;
use kalshi_ws::{Client, Environment};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = Client::connect(Environment::Demo).await?;

    let markets = vec![
        "KXPRES-2024-DJT".to_string(),
        "KXPRES-2024-KH".to_string(),
    ];
    let mut sub = client.subscribe_ticker(markets).await?;

    println!("subscribed: {}", sub.id);

    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            tick = sub.next() => match tick {
                Some(t) => println!(
                    "{} bid={} ask={} last={} ({}ms)",
                    t.market_ticker, t.yes_bid_dollars, t.yes_ask_dollars, t.price_dollars, t.ts_ms
                ),
                None => break,
            }
        }
    }

    client.shutdown();
    Ok(())
}
