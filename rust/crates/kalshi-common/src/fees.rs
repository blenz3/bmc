//! Kalshi trading fee math.
//!
//! Per Kalshi's published fee schedule (Feb 2026):
//!
//! ```text
//!   taker_fee_dollars = ceil(0.07 × count × price × (1 − price) × 100) / 100
//!   maker_fee_dollars = ceil(0.0175 × count × price × (1 − price) × 100) / 100   (≈ 25% of taker)
//! ```
//!
//! The `P × (1 − P)` term is the variance of the binary outcome — fees are
//! highest at the 50/50 midpoint and taper to near zero at the extremes.
//!
//! Important properties:
//! - Fees are computed on the *total* trade and then rounded up to the next
//!   cent (a single $0.99 contract rounds up to $0.01 fee).
//! - Settlement is fee-free; you only pay on trade execution.
//! - You pay on both legs of a round-trip.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Taker rate from Kalshi's published formula. Multiplies `P × (1 − P) × count`.
pub const TAKER_FEE_RATE: f64 = 0.07;

/// Maker fees are roughly one-quarter of taker fees.
pub const MAKER_TAKER_RATIO: f64 = 0.25;

/// Whether the fill was the aggressing (taker) side or the passive (maker) side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum FeeKind {
    /// Order that crossed the spread / removed liquidity.
    Taker,
    /// Order that rested in the book / added liquidity.
    Maker,
}

impl FeeKind {
    pub fn rate(self) -> f64 {
        match self {
            FeeKind::Taker => TAKER_FEE_RATE,
            FeeKind::Maker => TAKER_FEE_RATE * MAKER_TAKER_RATIO,
        }
    }
}

/// Compute the total fee for a single fill.
///
/// `price_dollars` is the trade price as a decimal in `[0.0, 1.0]`. `count`
/// is the number of contracts. Returns dollars rounded up to the next cent.
///
/// Returns `0.0` for trivial cases (zero count, zero/one prices).
pub fn fee_dollars(price_dollars: f64, count: u64, kind: FeeKind) -> f64 {
    if count == 0 || price_dollars <= 0.0 || price_dollars >= 1.0 {
        return 0.0;
    }
    let raw_cents = kind.rate() * (count as f64) * price_dollars * (1.0 - price_dollars) * 100.0;
    // Epsilon-aware ceil: an exact integer in math (e.g., 175.0) can land at
    // 175.0000000000001 in f64 because 0.07 isn't representable exactly, and
    // a naive ceil() would push it to 176. Treat anything within 1e-9 of an
    // integer as already at that integer.
    let floored = raw_cents.floor();
    let cents = if raw_cents - floored < 1e-9 {
        floored as i64
    } else {
        raw_cents.ceil() as i64
    };
    cents as f64 / 100.0
}

pub fn taker_fee_dollars(price_dollars: f64, count: u64) -> f64 {
    fee_dollars(price_dollars, count, FeeKind::Taker)
}

pub fn maker_fee_dollars(price_dollars: f64, count: u64) -> f64 {
    fee_dollars(price_dollars, count, FeeKind::Maker)
}

/// Total fees for opening + closing the same position at the same price.
/// `kind` is applied to *both* legs — pass `FeeKind::Taker` for a worst-case
/// market-order round trip, or `FeeKind::Maker` if you'll always rest as a
/// limit on both legs.
pub fn round_trip_fee_dollars(price_dollars: f64, count: u64, kind: FeeKind) -> f64 {
    2.0 * fee_dollars(price_dollars, count, kind)
}

/// Effective edge (in dollars per contract) required to break even on a
/// round-trip taker trade at this price. Useful for "is this strategy even
/// profitable after fees?" sanity checks.
///
/// Returned value is the minimum *price improvement on close vs open* needed
/// to clear costs. Doubles the fee since both legs are charged.
pub fn break_even_edge_dollars(price_dollars: f64, kind: FeeKind) -> f64 {
    round_trip_fee_dollars(price_dollars, 1, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ≈ {b}");
    }

    #[test]
    fn taker_fee_at_fifty_one_contract() {
        // 0.07 × 0.5 × 0.5 = 0.0175 dollars; ceil to next cent = 0.02.
        approx(taker_fee_dollars(0.50, 1), 0.02);
    }

    #[test]
    fn taker_fee_at_fifty_hundred_contracts() {
        // Pinned by Kalshi's published example: 100 × $0.50 → $1.75 total.
        approx(taker_fee_dollars(0.50, 100), 1.75);
    }

    #[test]
    fn taker_fee_tails_round_up_to_penny() {
        // 1 contract at $0.99 → 0.07 × 0.99 × 0.01 = 0.000693 dollars
        // ceil to next cent = $0.01.
        approx(taker_fee_dollars(0.99, 1), 0.01);
        approx(taker_fee_dollars(0.01, 1), 0.01);
    }

    #[test]
    fn taker_fee_tails_at_volume() {
        // 100 contracts at $0.99 → 0.07 × 100 × 0.99 × 0.01 = 0.0693 dollars
        // ceil to next cent = $0.07. Demonstrates that the per-contract floor
        // doesn't apply at scale.
        approx(taker_fee_dollars(0.99, 100), 0.07);
    }

    #[test]
    fn maker_is_one_quarter_of_taker_within_rounding() {
        // Use a count large enough to escape rounding noise.
        let taker = taker_fee_dollars(0.50, 1000);
        let maker = maker_fee_dollars(0.50, 1000);
        // Allow up to 1 cent rounding difference on each.
        let ratio = maker / taker;
        assert!((ratio - 0.25).abs() < 0.001, "maker/taker ratio {ratio} ≠ ~0.25");
    }

    #[test]
    fn zero_count_is_free() {
        approx(taker_fee_dollars(0.50, 0), 0.0);
        approx(maker_fee_dollars(0.50, 0), 0.0);
    }

    #[test]
    fn settlement_prices_are_free() {
        approx(taker_fee_dollars(0.0, 100), 0.0);
        approx(taker_fee_dollars(1.0, 100), 0.0);
    }

    #[test]
    fn round_trip_doubles_single_leg() {
        let single = taker_fee_dollars(0.50, 100);
        let round  = round_trip_fee_dollars(0.50, 100, FeeKind::Taker);
        approx(round, 2.0 * single);
    }

    #[test]
    fn break_even_edge_at_fifty_is_about_four_cents() {
        // Coin-flip round trip at taker rates costs ~$0.04 per contract;
        // strategies need at least that much edge per round trip.
        let edge = break_even_edge_dollars(0.50, FeeKind::Taker);
        approx(edge, 0.04); // 2 × $0.02
    }

    #[test]
    fn fee_at_92_cents_matches_test_order_example() {
        // From the kalshi_test_order.ps1 example: 1 contract at $0.92.
        // 0.07 × 0.92 × 0.08 = 0.005152 dollars → ceil to $0.01.
        approx(taker_fee_dollars(0.92, 1), 0.01);
    }
}
