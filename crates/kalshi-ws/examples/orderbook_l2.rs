//! Maintain a local L2 order book by applying `orderbook_delta` events.
//!
//! ```text
//! cargo run --example orderbook_l2 -- KXPRES-2024-DJT
//! ```

use std::collections::BTreeMap;

use futures_util::StreamExt;
use kalshi_ws::{Client, Environment, OrderbookEvent, Side};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let market = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "KXPRES-2024-DJT".to_string());

    let client = Client::connect(Environment::Demo).await?;
    let mut sub = client.subscribe_orderbook(vec![market.clone()]).await?;

    let mut book = Book::default();

    while let Some(evt) = sub.next().await {
        match evt {
            OrderbookEvent::Snapshot { snapshot, .. } => {
                book = Book::default();
                for (price, size) in snapshot.yes_dollars_fp {
                    book.apply_set(Side::Yes, price, size);
                }
                for (price, size) in snapshot.no_dollars_fp {
                    book.apply_set(Side::No, price, size);
                }
            }
            OrderbookEvent::Delta { delta, .. } => {
                book.apply_delta(delta.side, delta.price_dollars, delta.delta_fp);
            }
        }
        if let Some((bid, ask)) = book.top_of_book() {
            println!("{market}  best bid={bid:.4}  best ask={ask:.4}");
        }
    }
    Ok(())
}

#[derive(Default)]
struct Book {
    yes: BTreeMap<i64, i64>, // price in tenths-of-cents (×10000) -> size_fp
    no: BTreeMap<i64, i64>,
}

impl Book {
    fn apply_set(&mut self, side: Side, price: f64, size: i64) {
        let key = price_key(price);
        let map = match side {
            Side::Yes => &mut self.yes,
            Side::No => &mut self.no,
        };
        if size > 0 {
            map.insert(key, size);
        } else {
            map.remove(&key);
        }
    }

    fn apply_delta(&mut self, side: Side, price: f64, delta: i64) {
        let key = price_key(price);
        let map = match side {
            Side::Yes => &mut self.yes,
            Side::No => &mut self.no,
        };
        let entry = map.entry(key).or_insert(0);
        *entry += delta;
        if *entry <= 0 {
            map.remove(&key);
        }
    }

    fn top_of_book(&self) -> Option<(f64, f64)> {
        let best_bid = self.yes.iter().next_back().map(|(k, _)| *k as f64 / 10_000.0)?;
        // Best ask on YES = (1.0 - best NO bid).
        let best_no_bid = self.no.iter().next_back().map(|(k, _)| *k as f64 / 10_000.0)?;
        Some((best_bid, 1.0 - best_no_bid))
    }
}

fn price_key(p: f64) -> i64 {
    (p * 10_000.0).round() as i64
}
