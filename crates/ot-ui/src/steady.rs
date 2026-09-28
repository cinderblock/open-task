//! Keeping the process table calm while its numbers move.
//!
//! Each snapshot moves most rows' numbers a little, and sorting by them exactly
//! makes the table shuffle all the time: rows at 0.8 % and 1.1 % CPU trade places
//! on noise, and the row you were about to click moves away. [`Steady`] gives every
//! row a sort key with hysteresis. A key follows its row's value only once the value
//! leaves a dead band around the key, and while the order is held (the pointer is
//! over the table) keys do not move at all.
//!
//! The table then sorts exactly by these keys. That is still a total order, which a
//! comparator that treated "close" values as equal would not be, and it bounds the
//! disorder: two rows can only be shown out of order when each one's value is within
//! the other's key's dead band.

use std::collections::HashMap;

use crate::table::RowId;

/// A column's dead band: a key takes a new value only when the value differs from
/// it by more than `max(abs, rel * |key|)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Band {
    pub abs: f64,
    pub rel: f64,
}

impl Band {
    fn width(self, key: f64) -> f64 {
        self.abs.max(self.rel * key.abs())
    }
}

/// Sticky sort keys for one sort column and arrangement at a time.
#[derive(Debug, Default)]
pub(crate) struct Steady {
    /// The column and arrangement (tree or not) the keys were taken for. Keys for
    /// anything else are not used, and are replaced by exact values on the next
    /// update.
    basis: Option<(usize, bool)>,
    keys: HashMap<RowId, f64>,
    /// Spare map for the next update, so rows that are gone drop out without a
    /// per-update allocation.
    next: HashMap<RowId, f64>,
    held: bool,
}

impl Steady {
    /// What `id` sorts by in column `col` of the list (`tree` false) or the tree:
    /// its sticky key when there is one for that basis, else `value`, its exact
    /// value.
    #[must_use]
    pub fn key(&self, id: RowId, col: usize, tree: bool, value: f64) -> f64 {
        if self.basis == Some((col, tree)) {
            self.keys.get(&id).copied().unwrap_or(value)
        } else {
            value
        }
    }

    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }

    /// Freeze the keys, or let them follow again. Returns whether that changed.
    pub fn set_held(&mut self, on: bool) -> bool {
        std::mem::replace(&mut self.held, on) != on
    }

    /// Fold in the current value of every row that has one in `col`. A row seen
    /// for the first time, and every row after the column or arrangement changed,
    /// takes its exact value; otherwise a key moves only past `band` (never while
    /// held). Without a band (text columns) the keys are the values. Rows not in
    /// `values` are forgotten.
    pub fn update(
        &mut self,
        col: usize,
        tree: bool,
        band: Option<Band>,
        values: impl IntoIterator<Item = (RowId, f64)>,
    ) {
        let fresh = self.basis != Some((col, tree));
        self.basis = Some((col, tree));
        self.next.clear();
        if let Some(band) = band {
            for (id, v) in values {
                let k = match self.keys.get(&id) {
                    Some(&old) if !fresh && v.is_finite() && old.is_finite() => {
                        if self.held || (v - old).abs() <= band.width(old) {
                            old
                        } else {
                            v
                        }
                    }
                    _ => v,
                };
                self.next.insert(id, k);
            }
        }
        std::mem::swap(&mut self.keys, &mut self.next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU: Band = Band {
        abs: 1.0,
        rel: 0.15,
    };

    fn id(n: u64) -> RowId {
        RowId(n)
    }

    #[test]
    fn keys_move_only_past_the_dead_band() {
        let mut s = Steady::default();
        s.update(3, false, Some(CPU), [(id(1), 5.0), (id(2), 40.0)]);
        assert!(
            (s.key(id(1), 3, false, 5.0) - 5.0).abs() < 1e-9,
            "first sight is exact"
        );
        // Within the band (1 point at 5 %, 6 points at 40 %): keys stay.
        s.update(3, false, Some(CPU), [(id(1), 5.9), (id(2), 45.0)]);
        assert!((s.key(id(1), 3, false, 5.9) - 5.0).abs() < 1e-9);
        assert!((s.key(id(2), 3, false, 45.0) - 40.0).abs() < 1e-9);
        // Past it: the key takes the new value.
        s.update(3, false, Some(CPU), [(id(1), 6.2), (id(2), 47.0)]);
        assert!((s.key(id(1), 3, false, 6.2) - 6.2).abs() < 1e-9);
        assert!((s.key(id(2), 3, false, 47.0) - 47.0).abs() < 1e-9);
    }

    #[test]
    fn holding_freezes_known_keys_but_takes_new_rows() {
        let mut s = Steady::default();
        s.update(3, false, Some(CPU), [(id(1), 5.0)]);
        assert!(s.set_held(true));
        assert!(!s.set_held(true), "already held");
        s.update(3, false, Some(CPU), [(id(1), 90.0), (id(2), 30.0)]);
        assert!((s.key(id(1), 3, false, 90.0) - 5.0).abs() < 1e-9);
        assert!((s.key(id(2), 3, false, 30.0) - 30.0).abs() < 1e-9);
        assert!(s.set_held(false));
        s.update(3, false, Some(CPU), [(id(1), 90.0), (id(2), 30.0)]);
        assert!((s.key(id(1), 3, false, 90.0) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn a_new_basis_starts_exact_even_while_held() {
        let mut s = Steady::default();
        s.update(3, false, Some(CPU), [(id(1), 5.0)]);
        s.set_held(true);
        // Another column, or the tree: the old keys mean nothing there.
        assert!((s.key(id(1), 4, false, 123.0) - 123.0).abs() < 1e-9);
        assert!((s.key(id(1), 3, true, 7.0) - 7.0).abs() < 1e-9);
        s.update(3, true, Some(CPU), [(id(1), 7.0)]);
        assert!((s.key(id(1), 3, true, 7.0) - 7.0).abs() < 1e-9);
        s.update(3, true, Some(CPU), [(id(1), 50.0)]);
        assert!(
            (s.key(id(1), 3, true, 50.0) - 7.0).abs() < 1e-9,
            "held again"
        );
    }

    #[test]
    fn gone_rows_are_forgotten_and_pinned_rows_stay_exact() {
        let mut s = Steady::default();
        s.update(3, false, Some(CPU), [(id(1), 5.0), (id(2), f64::INFINITY)]);
        s.update(3, false, Some(CPU), [(id(2), f64::INFINITY)]);
        assert!(
            (s.key(id(1), 3, false, 50.0) - 50.0).abs() < 1e-9,
            "forgotten"
        );
        assert!(s.key(id(2), 3, false, f64::INFINITY).is_infinite());
        // No band: keys are the values.
        s.update(0, false, None, [(id(1), 1.0)]);
        assert!((s.key(id(1), 0, false, 2.0) - 2.0).abs() < 1e-9);
    }
}
