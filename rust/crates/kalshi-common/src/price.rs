//! Price-unit conversion and tick-rounding for Kalshi contracts.
//!
//! Kalshi expresses prices in three forms:
//! - **Dollars** (`f64`, `0.0..=1.0`) — wire format on `_dollars` REST/WS fields.
//! - **Cents** (`u8`, `1..=99`) — what `OrderRequest.yes_price` / `no_price` accept.
//! - **Deci-cents** (`i64`, `1..=9999`) — internal unit that preserves Kalshi's
//!   sub-penny tick precision losslessly.
//!
//! Tick structure varies per market:
//! - `linear_cent`: $0.01 ticks across the whole range
//! - `tapered_deci_cent`: $0.001 ticks below $0.10 and above $0.90; $0.01 in middle
//! - `deci_cent`: $0.001 ticks across the whole range
//!
//! The market's structure is exposed via `price_level_structure` on the REST
//! `Market` object. [`round_to_tick`] snaps an arbitrary dollar price onto
//! the nearest valid tick for the given structure.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub const CENTS_PER_DOLLAR: i64 = 100;
pub const DECI_CENTS_PER_DOLLAR: i64 = 10_000;

/// Smallest valid Kalshi limit price in cents.
pub const MIN_PRICE_CENTS: u8 = 1;
/// Largest valid Kalshi limit price in cents.
pub const MAX_PRICE_CENTS: u8 = 99;

#[inline]
pub fn cents_to_dollars(cents: u8) -> f64 {
    cents as f64 / CENTS_PER_DOLLAR as f64
}

#[inline]
pub fn deci_cents_to_dollars(dc: i64) -> f64 {
    dc as f64 / DECI_CENTS_PER_DOLLAR as f64
}

#[inline]
pub fn dollars_to_deci_cents(dollars: f64) -> i64 {
    (dollars * DECI_CENTS_PER_DOLLAR as f64).round() as i64
}

/// Round dollars to the nearest integer cent and clamp to the valid Kalshi
/// range `[MIN_PRICE_CENTS, MAX_PRICE_CENTS]`. Returns the clamped value
/// regardless of input — call sites that care about lossy clamping should
/// check beforehand using [`is_valid_price_cents`] on the rounded result.
pub fn dollars_to_cents_clamped(dollars: f64) -> u8 {
    let raw = (dollars * CENTS_PER_DOLLAR as f64).round() as i64;
    raw.clamp(MIN_PRICE_CENTS as i64, MAX_PRICE_CENTS as i64) as u8
}

#[inline]
pub fn is_valid_price_cents(cents: u8) -> bool {
    cents >= MIN_PRICE_CENTS && cents <= MAX_PRICE_CENTS
}

/// Per-market tick structure — exposed via `price_level_structure` on the REST
/// `Market` object. Determines the legal price increments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TickStructure {
    /// $0.01 ticks across the entire $0.00–$1.00 range.
    LinearCent,
    /// $0.001 ticks below $0.10 and above $0.90; $0.01 ticks in $0.10–$0.90.
    TaperedDeciCent,
    /// $0.001 ticks across the entire $0.00–$1.00 range.
    DeciCent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    /// Snap to the nearest valid tick.
    Nearest,
    /// Snap toward zero (down for positive prices).
    Down,
    /// Snap away from zero (up for positive prices).
    Up,
}

/// Round `dollars` onto the nearest valid tick for the given structure. Use
/// `Down` for buy bids you want to under-pay, `Up` for ask offers you want
/// to clear at or above market.
pub fn round_to_tick(dollars: f64, structure: TickStructure, mode: RoundingMode) -> f64 {
    let tick_cents = match structure {
        TickStructure::LinearCent => 100,                                       // 100 deci-cents = $0.01
        TickStructure::DeciCent => 10,                                          // 10 deci-cents = $0.001
        TickStructure::TaperedDeciCent => {
            // $0.001 in tails (below $0.10 or above $0.90), $0.01 in middle.
            if dollars < 0.10 || dollars > 0.90 {
                10
            } else {
                100
            }
        }
    };
    let dc = dollars * DECI_CENTS_PER_DOLLAR as f64;
    let snapped = match mode {
        RoundingMode::Nearest => (dc / tick_cents as f64).round() * tick_cents as f64,
        RoundingMode::Down    => (dc / tick_cents as f64).floor() * tick_cents as f64,
        RoundingMode::Up      => (dc / tick_cents as f64).ceil()  * tick_cents as f64,
    };
    snapped / DECI_CENTS_PER_DOLLAR as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ≈ {b}");
    }

    #[test]
    fn cents_dollars_round_trip() {
        for c in 1u8..=99u8 {
            assert_eq!(dollars_to_cents_clamped(cents_to_dollars(c)), c);
        }
    }

    #[test]
    fn deci_cents_round_trip_preserves_subpenny() {
        let dollars = 0.5512;
        let dc = dollars_to_deci_cents(dollars);
        assert_eq!(dc, 5512);
        approx(deci_cents_to_dollars(dc), 0.5512);
    }

    #[test]
    fn dollars_to_cents_clamps() {
        // Above range
        assert_eq!(dollars_to_cents_clamped(1.50), MAX_PRICE_CENTS);
        // Below range
        assert_eq!(dollars_to_cents_clamped(0.00), MIN_PRICE_CENTS);
        // In range
        assert_eq!(dollars_to_cents_clamped(0.55), 55);
        // Sub-penny rounds nearest
        assert_eq!(dollars_to_cents_clamped(0.5512), 55);
        assert_eq!(dollars_to_cents_clamped(0.5550), 56); // round half away from zero
    }

    #[test]
    fn is_valid_price_cents_works() {
        assert!(is_valid_price_cents(1));
        assert!(is_valid_price_cents(99));
        assert!(!is_valid_price_cents(0));
        assert!(!is_valid_price_cents(100));
    }

    #[test]
    fn tick_round_linear_cent() {
        approx(round_to_tick(0.5512, TickStructure::LinearCent, RoundingMode::Nearest), 0.55);
        approx(round_to_tick(0.5550, TickStructure::LinearCent, RoundingMode::Nearest), 0.56);
        approx(round_to_tick(0.5512, TickStructure::LinearCent, RoundingMode::Down), 0.55);
        approx(round_to_tick(0.5512, TickStructure::LinearCent, RoundingMode::Up), 0.56);
    }

    #[test]
    fn tick_round_deci_cent_preserves_subpenny() {
        approx(round_to_tick(0.5512, TickStructure::DeciCent, RoundingMode::Nearest), 0.551);
        approx(round_to_tick(0.5556, TickStructure::DeciCent, RoundingMode::Nearest), 0.556);
        approx(round_to_tick(0.5512, TickStructure::DeciCent, RoundingMode::Down), 0.551);
        approx(round_to_tick(0.5512, TickStructure::DeciCent, RoundingMode::Up), 0.552);
    }

    #[test]
    fn tick_round_tapered_uses_finer_grain_in_tails() {
        // In the tails: $0.001 tick.
        approx(round_to_tick(0.0512, TickStructure::TaperedDeciCent, RoundingMode::Nearest), 0.051);
        approx(round_to_tick(0.9512, TickStructure::TaperedDeciCent, RoundingMode::Nearest), 0.951);
        // In the middle ($0.10–$0.90): $0.01 tick.
        approx(round_to_tick(0.5512, TickStructure::TaperedDeciCent, RoundingMode::Nearest), 0.55);
        approx(round_to_tick(0.5556, TickStructure::TaperedDeciCent, RoundingMode::Nearest), 0.56);
    }
}
