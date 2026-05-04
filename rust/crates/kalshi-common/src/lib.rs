//! Shared utilities for Kalshi: fee math, price-unit conversion, and the
//! identities that fall out of the YES + NO = $1.00 binary-contract structure.
//!
//! Pure `std`. Adding `serde` is gated behind the `serde` feature.
//!
//! ## What's here vs. what isn't
//!
//! - **[`fees`]** — round-trip cost calculator and the per-trade fee formula
//!   Kalshi publishes (`ceil(0.07 × C × P × (1 − P) × 100) / 100`).
//! - **[`price`]** — conversion between dollars (`f64`), cents (`u8` 1..=99),
//!   and deci-cents (`i64`, the sub-penny precision the wire uses), plus
//!   tick-rounding for the three Kalshi tick structures.
//! - **[`prob`]** — mid / spread computation and the YES↔NO complement
//!   identities.
//!
//! Deliberately NOT here:
//! - The `Side` enum (lives in `kalshi-ws::protocol::channels` and is
//!   re-exported through `kalshi-rest`; pulling it down here would force a
//!   cyclic-feeling dep). When that becomes painful, move it.
//! - Anything that requires HTTP, async, or real I/O. This crate is pure
//!   arithmetic so it can be used from backtests and tests freely.

pub mod book;
pub mod fees;
pub mod price;
pub mod prob;

pub use book::{BookError, BookIter, FixedBook, Side as BookSide};
pub use fees::{
    fee_dollars, maker_fee_dollars, round_trip_fee_dollars, taker_fee_dollars, FeeKind,
    MAKER_TAKER_RATIO, TAKER_FEE_RATE,
};
pub use price::{
    cents_to_dollars, deci_cents_to_dollars, dollars_to_cents_clamped, dollars_to_deci_cents,
    is_valid_price_cents, round_to_tick, RoundingMode, TickStructure, CENTS_PER_DOLLAR,
    DECI_CENTS_PER_DOLLAR, MAX_PRICE_CENTS, MIN_PRICE_CENTS,
};
pub use prob::{
    implied_no_ask_from_yes_bid, implied_yes_ask_from_no_bid, mid, spread, FavoredOutcome,
    favored_outcome_from_yes_mid,
};
