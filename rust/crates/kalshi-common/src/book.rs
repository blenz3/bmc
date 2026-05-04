//! Fixed-size order book for Kalshi binary contracts.
//!
//! Kalshi's price space is bounded: at most `100` integer-cent levels for
//! `linear_cent` markets, at most `10_000` deci-cent levels for `deci_cent` /
//! `tapered_deci_cent` markets. That means we can preallocate the *entire*
//! price ladder once and never allocate again on the hot path. Every
//! [`apply_delta`](FixedBook::apply_delta) is O(1) (one array write + one
//! cached-best update), top-of-book lookup is O(1), and the only path that
//! does any real work is when the current best level gets emptied — then we
//! rescan downward to find the new top, which is still cache-friendly
//! (10 000 × 8 bytes = 80 KB scanned in the worst case, comfortably L1-resident).
//!
//! Both sides are stored as flat `Vec<u64>` indexed by price level. Index
//! `i` represents:
//! - **LinearCent** book: price `i` cents (0..=99). Use [`FixedBook::linear_cent`].
//! - **DeciCent** book: price `i` deci-cents (0..=9999, i.e. tenths of a cent).
//!   Use [`FixedBook::deci_cent`].
//!
//! ## Design choices
//!
//! - Sides are tracked separately as YES bids and NO bids (matches Kalshi's
//!   wire format). Use the helpers in [`crate::prob`] to derive YES asks
//!   from NO bids when you want the consolidated view.
//! - Sizes are `u64` non-negative. A delta that would push below zero
//!   saturates at zero, matching exchange semantics.
//! - The cached `best_yes` / `best_no` index avoids an O(n) scan on every
//!   top-of-book read. Maintenance: insert at a higher price → O(1) update.
//!   Remove at the current best → linear scan from that price downward to
//!   find the new top.
//! - No async, no I/O, no serde dependency. Pure arithmetic.

use crate::price::{CENTS_PER_DOLLAR, DECI_CENTS_PER_DOLLAR};

/// Which side of the YES/NO bid book a level belongs to.
///
/// Distinct from [`kalshi_ws::Side`](https://docs.rs/) — that crate hasn't
/// been wired through here to avoid a cyclic dep. They mean the same thing
/// and you can convert via a one-line match at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Yes,
    No,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookError {
    /// `price_idx` is outside the book's price ladder.
    OutOfRange { price_idx: usize, max_exclusive: usize },
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookError::OutOfRange { price_idx, max_exclusive } => write!(
                f,
                "price_idx {price_idx} out of range (must be < {max_exclusive})",
            ),
        }
    }
}

impl std::error::Error for BookError {}

/// Preallocated, fixed-ladder L2 book. See module docs for details.
#[derive(Debug, Clone)]
pub struct FixedBook {
    yes: Vec<u64>,
    no: Vec<u64>,
    best_yes: Option<usize>,
    best_no: Option<usize>,
    /// Number of price units in $1.00 (100 for cents, 10_000 for deci-cents).
    /// Used by [`Self::price_dollars`] to convert indices back to dollars.
    units_per_dollar: usize,
}

impl FixedBook {
    /// Construct a book with `levels` distinct price points (i.e., the full
    /// price ladder has indices `0..levels`). `units_per_dollar` controls the
    /// `index → dollars` conversion via [`Self::price_dollars`].
    pub fn new(levels: usize, units_per_dollar: usize) -> Self {
        Self {
            yes: vec![0; levels],
            no: vec![0; levels],
            best_yes: None,
            best_no: None,
            units_per_dollar,
        }
    }

    /// Book sized for `linear_cent` markets — 100 cent-precision levels (0..=99).
    /// `price_dollars(55) == 0.55`.
    #[inline]
    pub fn linear_cent() -> Self {
        Self::new(CENTS_PER_DOLLAR as usize, CENTS_PER_DOLLAR as usize)
    }

    /// Book sized for `deci_cent` / `tapered_deci_cent` markets — 10_000
    /// deci-cent levels (0..=9999). `price_dollars(5512) == 0.5512`.
    #[inline]
    pub fn deci_cent() -> Self {
        Self::new(DECI_CENTS_PER_DOLLAR as usize, DECI_CENTS_PER_DOLLAR as usize)
    }

    /// Total number of price levels in this book.
    #[inline]
    pub fn levels(&self) -> usize {
        self.yes.len()
    }

    /// Convert a price index back to dollars.
    #[inline]
    pub fn price_dollars(&self, price_idx: usize) -> f64 {
        price_idx as f64 / self.units_per_dollar as f64
    }

    /// Direct read of the size at a given level. Returns `0` for empty levels
    /// or out-of-range indices.
    #[inline]
    pub fn size_at(&self, side: Side, price_idx: usize) -> u64 {
        let arr = self.side_arr(side);
        if price_idx < arr.len() {
            arr[price_idx]
        } else {
            0
        }
    }

    /// Best (highest-price) non-empty bid on this side, as `(price_idx, size)`.
    /// Returns `None` if the side is empty. O(1).
    #[inline]
    pub fn best(&self, side: Side) -> Option<(usize, u64)> {
        let best = match side {
            Side::Yes => self.best_yes,
            Side::No => self.best_no,
        };
        let arr = self.side_arr(side);
        best.map(|i| (i, arr[i]))
    }

    /// Set the size at a specific level (replacing whatever was there).
    /// Updates the cached `best` index.
    pub fn set(&mut self, side: Side, price_idx: usize, size: u64) -> Result<(), BookError> {
        self.bounds_check(price_idx)?;
        let prev = self.side_arr(side)[price_idx];
        self.side_arr_mut(side)[price_idx] = size;
        self.update_best_after_change(side, price_idx, prev, size);
        Ok(())
    }

    /// Apply a delta to the level. Negative deltas reduce; the result
    /// saturates at zero rather than underflowing.
    pub fn apply_delta(
        &mut self,
        side: Side,
        price_idx: usize,
        delta: i64,
    ) -> Result<(), BookError> {
        self.bounds_check(price_idx)?;
        let prev = self.side_arr(side)[price_idx];
        let new_size = (prev as i64).saturating_add(delta).max(0) as u64;
        self.side_arr_mut(side)[price_idx] = new_size;
        self.update_best_after_change(side, price_idx, prev, new_size);
        Ok(())
    }

    /// Replace one entire side from a snapshot. Existing entries on that side
    /// are zeroed first. O(levels + N).
    pub fn replace_side<I>(&mut self, side: Side, levels: I)
    where
        I: IntoIterator<Item = (usize, u64)>,
    {
        self.clear_side(side);
        for (idx, size) in levels {
            // Silently ignore out-of-range entries from a snapshot rather than
            // failing the whole replacement — snapshots are external data.
            if idx < self.side_arr(side).len() {
                self.side_arr_mut(side)[idx] = size;
            }
        }
        // Recompute best for this side from scratch.
        let new_best = scan_top(self.side_arr(side));
        match side {
            Side::Yes => self.best_yes = new_best,
            Side::No => self.best_no = new_best,
        }
    }

    /// Wipe the entire book.
    pub fn clear(&mut self) {
        for x in &mut self.yes {
            *x = 0;
        }
        for x in &mut self.no {
            *x = 0;
        }
        self.best_yes = None;
        self.best_no = None;
    }

    /// Wipe one side.
    pub fn clear_side(&mut self, side: Side) {
        for x in self.side_arr_mut(side) {
            *x = 0;
        }
        match side {
            Side::Yes => self.best_yes = None,
            Side::No => self.best_no = None,
        }
    }

    /// Iterate over non-empty levels on a side, descending by price (best
    /// first). Convenient for top-N rendering and depth calculations.
    pub fn iter(&self, side: Side) -> BookIter<'_> {
        let arr = self.side_arr(side);
        BookIter { arr, next_idx: arr.len() }
    }

    /// Convenience: collect the top `n` levels on a side, descending by price.
    pub fn top_n(&self, side: Side, n: usize) -> Vec<(usize, u64)> {
        self.iter(side).take(n).collect()
    }

    /// Total resting size summed across all levels on a side. O(levels).
    pub fn total_size(&self, side: Side) -> u64 {
        self.side_arr(side).iter().sum()
    }

    // -- Private helpers ------------------------------------------------------

    #[inline]
    fn side_arr(&self, side: Side) -> &[u64] {
        match side {
            Side::Yes => &self.yes,
            Side::No => &self.no,
        }
    }

    #[inline]
    fn side_arr_mut(&mut self, side: Side) -> &mut [u64] {
        match side {
            Side::Yes => &mut self.yes,
            Side::No => &mut self.no,
        }
    }

    #[inline]
    fn bounds_check(&self, price_idx: usize) -> Result<(), BookError> {
        let max = self.yes.len();
        if price_idx >= max {
            Err(BookError::OutOfRange {
                price_idx,
                max_exclusive: max,
            })
        } else {
            Ok(())
        }
    }

    /// Maintain the cached `best` index after a level change. Cases:
    /// - Level went from non-zero → zero AND was the cached best ⇒ rescan downward.
    /// - Level went from zero/lower → non-zero AND idx > current best ⇒ update.
    /// - Otherwise no change.
    ///
    /// Uses direct field access (rather than the side_arr / side_arr_mut
    /// accessors) so the borrow checker can see we're touching different
    /// fields — `arr` is `&self.yes`, `best_ref` is `&mut self.best_yes`,
    /// no overlap.
    fn update_best_after_change(
        &mut self,
        side: Side,
        price_idx: usize,
        prev: u64,
        new_size: u64,
    ) {
        let (arr, best_ref) = match side {
            Side::Yes => (&self.yes[..], &mut self.best_yes),
            Side::No => (&self.no[..], &mut self.best_no),
        };

        if new_size > 0 {
            // Level is now (still) populated. Best can only equal or move up.
            match *best_ref {
                Some(b) if b >= price_idx => {} // unchanged
                _ => *best_ref = Some(price_idx),
            }
        } else if prev > 0 {
            // Level was just emptied. Only need to rescan if it was the top.
            if *best_ref == Some(price_idx) {
                *best_ref = scan_top_below(arr, price_idx);
            }
        }
        // else: prev == 0 && new_size == 0 → no-op write, nothing to update.
    }
}

/// Linear scan from the top of `arr` for the highest non-zero index.
fn scan_top(arr: &[u64]) -> Option<usize> {
    for i in (0..arr.len()).rev() {
        if arr[i] > 0 {
            return Some(i);
        }
    }
    None
}

/// Linear scan starting just below `from` (which was just emptied) for the
/// next-highest non-zero index. Returns `None` if no levels remain.
fn scan_top_below(arr: &[u64], from: usize) -> Option<usize> {
    if from == 0 {
        return None;
    }
    for i in (0..from).rev() {
        if arr[i] > 0 {
            return Some(i);
        }
    }
    None
}

/// Iterator over `(price_idx, size)` for non-empty levels, descending by price.
pub struct BookIter<'a> {
    arr: &'a [u64],
    next_idx: usize, // exclusive upper bound for the next .next() call
}

impl<'a> Iterator for BookIter<'a> {
    type Item = (usize, u64);

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_idx > 0 {
            self.next_idx -= 1;
            let v = self.arr[self.next_idx];
            if v > 0 {
                return Some((self.next_idx, v));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_cent_dimensions() {
        let b = FixedBook::linear_cent();
        assert_eq!(b.levels(), 100);
        // price_dollars: 0..=99 maps to $0.00..=$0.99
        assert!((b.price_dollars(0) - 0.0).abs() < 1e-9);
        assert!((b.price_dollars(55) - 0.55).abs() < 1e-9);
        assert!((b.price_dollars(99) - 0.99).abs() < 1e-9);
    }

    #[test]
    fn deci_cent_dimensions() {
        let b = FixedBook::deci_cent();
        assert_eq!(b.levels(), 10_000);
        assert!((b.price_dollars(5512) - 0.5512).abs() < 1e-9);
        assert!((b.price_dollars(10) - 0.001).abs() < 1e-9);
    }

    #[test]
    fn set_get_round_trip() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 65, 100).unwrap();
        b.set(Side::No, 30, 200).unwrap();
        assert_eq!(b.size_at(Side::Yes, 65), 100);
        assert_eq!(b.size_at(Side::No, 30), 200);
        // Untouched levels are zero.
        assert_eq!(b.size_at(Side::Yes, 64), 0);
        assert_eq!(b.size_at(Side::No, 31), 0);
    }

    #[test]
    fn out_of_range_errors() {
        let mut b = FixedBook::linear_cent();
        let err = b.set(Side::Yes, 100, 1).unwrap_err();
        assert_eq!(
            err,
            BookError::OutOfRange {
                price_idx: 100,
                max_exclusive: 100,
            }
        );
    }

    #[test]
    fn delta_accumulates_and_saturates() {
        let mut b = FixedBook::linear_cent();
        b.apply_delta(Side::Yes, 65, 10).unwrap();
        b.apply_delta(Side::Yes, 65, 5).unwrap();
        assert_eq!(b.size_at(Side::Yes, 65), 15);
        b.apply_delta(Side::Yes, 65, -100).unwrap(); // would go to -85
        assert_eq!(b.size_at(Side::Yes, 65), 0); // saturated
    }

    #[test]
    fn best_updates_on_higher_insert() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 50, 10).unwrap();
        assert_eq!(b.best(Side::Yes), Some((50, 10)));
        b.set(Side::Yes, 65, 5).unwrap();
        assert_eq!(b.best(Side::Yes), Some((65, 5))); // moved up
        b.set(Side::Yes, 60, 20).unwrap();
        assert_eq!(b.best(Side::Yes), Some((65, 5))); // unchanged
    }

    #[test]
    fn best_rescans_when_top_removed() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 50, 10).unwrap();
        b.set(Side::Yes, 60, 20).unwrap();
        b.set(Side::Yes, 65, 5).unwrap();
        assert_eq!(b.best(Side::Yes), Some((65, 5)));
        b.set(Side::Yes, 65, 0).unwrap(); // remove top
        assert_eq!(b.best(Side::Yes), Some((60, 20))); // next-highest
        b.apply_delta(Side::Yes, 60, -20).unwrap(); // remove top via delta
        assert_eq!(b.best(Side::Yes), Some((50, 10)));
        b.set(Side::Yes, 50, 0).unwrap();
        assert_eq!(b.best(Side::Yes), None);
    }

    #[test]
    fn best_unchanged_on_unrelated_removal() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 65, 5).unwrap();
        b.set(Side::Yes, 60, 20).unwrap();
        b.set(Side::Yes, 60, 0).unwrap(); // not the top
        assert_eq!(b.best(Side::Yes), Some((65, 5))); // best unchanged
    }

    #[test]
    fn iter_descends_by_price() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 50, 10).unwrap();
        b.set(Side::Yes, 65, 5).unwrap();
        b.set(Side::Yes, 60, 20).unwrap();
        let levels: Vec<_> = b.iter(Side::Yes).collect();
        assert_eq!(levels, vec![(65, 5), (60, 20), (50, 10)]);
    }

    #[test]
    fn top_n_truncates() {
        let mut b = FixedBook::linear_cent();
        for (i, sz) in [(40, 1), (50, 2), (60, 3), (70, 4)] {
            b.set(Side::Yes, i, sz).unwrap();
        }
        let top2 = b.top_n(Side::Yes, 2);
        assert_eq!(top2, vec![(70, 4), (60, 3)]);
    }

    #[test]
    fn replace_side_swaps_atomically() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 50, 10).unwrap();
        b.set(Side::Yes, 60, 20).unwrap();
        b.replace_side(Side::Yes, [(70, 5), (80, 15)]);
        assert_eq!(b.size_at(Side::Yes, 50), 0); // wiped
        assert_eq!(b.size_at(Side::Yes, 60), 0);
        assert_eq!(b.size_at(Side::Yes, 70), 5);
        assert_eq!(b.size_at(Side::Yes, 80), 15);
        assert_eq!(b.best(Side::Yes), Some((80, 15)));
    }

    #[test]
    fn replace_side_silently_drops_oob_entries() {
        let mut b = FixedBook::linear_cent();
        b.replace_side(Side::Yes, [(50, 10), (200, 999), (75, 3)]);
        assert_eq!(b.size_at(Side::Yes, 50), 10);
        assert_eq!(b.size_at(Side::Yes, 75), 3);
        assert_eq!(b.best(Side::Yes), Some((75, 3))); // 200 ignored
    }

    #[test]
    fn clear_resets_everything() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 65, 10).unwrap();
        b.set(Side::No, 35, 20).unwrap();
        b.clear();
        assert_eq!(b.best(Side::Yes), None);
        assert_eq!(b.best(Side::No), None);
        assert_eq!(b.total_size(Side::Yes), 0);
        assert_eq!(b.total_size(Side::No), 0);
    }

    #[test]
    fn yes_and_no_sides_independent() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 65, 100).unwrap();
        b.set(Side::No, 30, 200).unwrap();
        assert_eq!(b.best(Side::Yes), Some((65, 100)));
        assert_eq!(b.best(Side::No), Some((30, 200)));
        b.clear_side(Side::Yes);
        assert_eq!(b.best(Side::Yes), None);
        assert_eq!(b.best(Side::No), Some((30, 200))); // unaffected
    }

    #[test]
    fn deci_cent_handles_subpenny_levels() {
        let mut b = FixedBook::deci_cent();
        b.set(Side::Yes, 5512, 100).unwrap(); // $0.5512
        b.set(Side::Yes, 5500, 50).unwrap(); // $0.5500
        b.set(Side::Yes, 5523, 200).unwrap(); // $0.5523
        assert_eq!(b.best(Side::Yes), Some((5523, 200)));
        let levels: Vec<_> = b.iter(Side::Yes).collect();
        assert_eq!(levels, vec![(5523, 200), (5512, 100), (5500, 50)]);
    }

    #[test]
    fn total_size_sums_levels() {
        let mut b = FixedBook::linear_cent();
        b.set(Side::Yes, 50, 10).unwrap();
        b.set(Side::Yes, 60, 20).unwrap();
        b.set(Side::Yes, 70, 30).unwrap();
        assert_eq!(b.total_size(Side::Yes), 60);
        assert_eq!(b.total_size(Side::No), 0);
    }
}
