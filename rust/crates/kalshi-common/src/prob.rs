//! Mid / spread / favored-side math, plus the YES↔NO complement identities
//! that fall out of the binary-contract structure (`YES + NO = $1.00`).

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Implied YES ask price given the best NO bid. Comes from the cross-match
/// pathway: a NO bidder paying `n` is mathematically offering YES at `1 - n`.
#[inline]
pub fn implied_yes_ask_from_no_bid(no_bid_dollars: f64) -> f64 {
    1.0 - no_bid_dollars
}

/// Implied NO ask price given the best YES bid. Symmetric.
#[inline]
pub fn implied_no_ask_from_yes_bid(yes_bid_dollars: f64) -> f64 {
    1.0 - yes_bid_dollars
}

/// Mid price between bid and ask. Same units as inputs.
#[inline]
pub fn mid(bid_dollars: f64, ask_dollars: f64) -> f64 {
    (bid_dollars + ask_dollars) / 2.0
}

/// Spread (ask − bid). Negative spread indicates a crossed book.
#[inline]
pub fn spread(bid_dollars: f64, ask_dollars: f64) -> f64 {
    ask_dollars - bid_dollars
}

/// Which outcome the market currently favors, derived from the YES mid.
/// `Yes` if YES is at least as likely; `No` if NO is. Caller can treat the
/// `Yes` branch as a tie-break-to-YES.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum FavoredOutcome {
    Yes,
    No,
}

/// Resolve the favored outcome from a YES-side mid price (in dollars). Tie at
/// $0.50 resolves to `Yes`.
#[inline]
pub fn favored_outcome_from_yes_mid(yes_mid_dollars: f64) -> FavoredOutcome {
    if yes_mid_dollars >= 0.5 {
        FavoredOutcome::Yes
    } else {
        FavoredOutcome::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ≈ {b}");
    }

    #[test]
    fn yes_no_complement_is_one_dollar() {
        // For any NO bid in (0, 1), implied YES ask completes to $1.00.
        approx(implied_yes_ask_from_no_bid(0.34) + 0.34, 1.0);
        approx(implied_no_ask_from_yes_bid(0.65) + 0.65, 1.0);
    }

    #[test]
    fn mid_is_arithmetic_mean() {
        approx(mid(0.65, 0.66), 0.655);
        approx(mid(0.10, 0.90), 0.50);
    }

    #[test]
    fn spread_handles_crossed_book() {
        // Normal market: positive spread.
        approx(spread(0.65, 0.66), 0.01);
        // Crossed (best bid above best ask): spread is negative.
        approx(spread(0.66, 0.65), -0.01);
    }

    #[test]
    fn favored_outcome_from_mid() {
        assert_eq!(favored_outcome_from_yes_mid(0.65), FavoredOutcome::Yes);
        assert_eq!(favored_outcome_from_yes_mid(0.30), FavoredOutcome::No);
        // Tie -> Yes.
        assert_eq!(favored_outcome_from_yes_mid(0.50), FavoredOutcome::Yes);
        // Edge cases.
        assert_eq!(favored_outcome_from_yes_mid(0.99), FavoredOutcome::Yes);
        assert_eq!(favored_outcome_from_yes_mid(0.01), FavoredOutcome::No);
    }

    #[test]
    fn yes_mid_from_book_quote() {
        // Reproduces the kalshi_test_order.ps1 favored-side calc.
        // yes_bid 8 cents, yes_ask 10 cents -> mid 9 cents -> NO favored.
        let yes_bid = 0.08;
        let no_bid  = 0.90;
        let yes_ask = implied_yes_ask_from_no_bid(no_bid);
        approx(yes_ask, 0.10);
        let m = mid(yes_bid, yes_ask);
        approx(m, 0.09);
        assert_eq!(favored_outcome_from_yes_mid(m), FavoredOutcome::No);
    }
}
