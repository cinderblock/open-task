//! Squarified treemap layout (Bruls, Huizing and van Wijk, "Squarified Treemaps",
//! 2000).
//!
//! Areas are proportional to values, and the rectangles are kept as close to
//! square as the values allow: items are laid out in rows along the shorter side of
//! what is left, and a row takes one more item only while that does not make its
//! worst aspect ratio worse. Squares are what an eye compares well and what a label
//! fits in; the strip layouts that are simpler to write produce slivers.

use ot_paint::Rect;

/// Lay `values` out in `rect`, one rectangle per value, in the same order, their
/// areas proportional to the values. For the squarest result pass the values largest
/// first. Values that are not positive get an empty rectangle. Allocates nothing
/// once `out` has grown to size.
pub fn squarify(values: &[f64], rect: Rect, out: &mut Vec<Rect>) {
    out.clear();
    out.resize(values.len(), Rect::new(rect.x, rect.y, 0.0, 0.0));
    let total: f64 = values.iter().filter(|v| **v > 0.0).sum();
    if total <= 0.0 || rect.is_empty() {
        return;
    }
    let scale = f64::from(rect.w) * f64::from(rect.h) / total;
    let n = values.len();
    let next = |mut i: usize| {
        while i < n && values[i] <= 0.0 {
            i += 1;
        }
        i
    };

    let mut free = rect;
    let mut first = next(0);
    while first < n {
        let side = f64::from(free.w.min(free.h));
        // Grow the row while its worst aspect ratio improves.
        let a0 = values[first] * scale;
        let (mut sum, mut lo, mut hi) = (a0, a0, a0);
        let mut worst = worst_ratio(sum, lo, hi, side);
        let mut last = first;
        let mut candidate = next(first + 1);
        while candidate < n {
            let a = values[candidate] * scale;
            let w = worst_ratio(sum + a, lo.min(a), hi.max(a), side);
            if w > worst {
                break;
            }
            (worst, sum, lo, hi, last) = (w, sum + a, lo.min(a), hi.max(a), candidate);
            candidate = next(candidate + 1);
        }
        free = place_row(values, first, last, scale, sum, free, out);
        first = next(last + 1);
    }
}

/// The worst aspect ratio in a row of total area `sum` along a side `side` long,
/// whose smallest and largest items are `lo` and `hi` (from the paper).
fn worst_ratio(sum: f64, lo: f64, hi: f64, side: f64) -> f64 {
    let (s2, w2) = (sum * sum, side * side);
    (w2 * hi / s2).max(s2 / (w2 * lo))
}

/// Lay the positive values from `first` to `last` as one row along the shorter
/// side of `free`, and return what is left. The row's last item takes whatever
/// rounding left, so the row ends exactly at the edge.
fn place_row(
    values: &[f64],
    first: usize,
    last: usize,
    scale: f64,
    sum: f64,
    free: Rect,
    out: &mut [Rect],
) -> Rect {
    let row = (first..=last).filter(|&i| values[i] > 0.0);
    if free.w >= free.h {
        // A column down the left edge.
        let w = (sum / f64::from(free.h)) as f32;
        let mut y = free.y;
        for i in row {
            let h = if i == last {
                free.bottom() - y
            } else {
                (values[i] * scale / f64::from(w)) as f32
            };
            out[i] = Rect::new(free.x, y, w, h);
            y += h;
        }
        Rect::new(free.x + w, free.y, (free.w - w).max(0.0), free.h)
    } else {
        // A row across the top.
        let h = (sum / f64::from(free.w)) as f32;
        let mut x = free.x;
        for i in row {
            let w = if i == last {
                free.right() - x
            } else {
                (values[i] * scale / f64::from(h)) as f32
            };
            out[i] = Rect::new(x, free.y, w, h);
            x += w;
        }
        Rect::new(free.x, free.y + h, free.w, (free.h - h).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &Rect) -> f64 {
        f64::from(r.w) * f64::from(r.h)
    }

    #[test]
    fn areas_follow_the_values_and_fill_the_rect() {
        // The example from the paper: 6, 6, 4, 3, 2, 2, 1 in a 6 x 4 rectangle.
        let values = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rect = Rect::new(0.0, 0.0, 6.0, 4.0);
        let mut out = Vec::new();
        squarify(&values, rect, &mut out);
        let total: f64 = out.iter().map(area).sum();
        assert!((total - 24.0).abs() < 1e-3, "{total}");
        for (v, r) in values.iter().zip(&out) {
            assert!((area(r) - v).abs() < 1e-3, "{v}: {r:?}");
            assert!(r.x >= -1e-4 && r.right() <= 6.0 + 1e-4, "{r:?}");
            assert!(r.y >= -1e-4 && r.bottom() <= 4.0 + 1e-4, "{r:?}");
        }
        // The paper's first row: the two sixes stacked down the left, 3 wide.
        assert!((out[0].w - 3.0).abs() < 1e-4 && (out[1].w - 3.0).abs() < 1e-4);
        // Nothing overlaps.
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let o = out[i].intersect(&out[j]);
                assert!(area(&o) < 1e-6, "{i} and {j} overlap: {o:?}");
            }
        }
    }

    #[test]
    fn rectangles_stay_near_square() {
        let values: Vec<f64> = (1..=20).rev().map(f64::from).collect();
        let mut out = Vec::new();
        squarify(&values, Rect::new(0.0, 0.0, 400.0, 300.0), &mut out);
        let worst = out
            .iter()
            .map(|r| (r.w / r.h).max(r.h / r.w))
            .fold(0.0f32, f32::max);
        assert!(worst < 4.0, "worst aspect ratio {worst}");
    }

    #[test]
    fn empty_and_zero_values_get_empty_rectangles() {
        let mut out = Vec::new();
        squarify(
            &[0.0, 5.0, -1.0, 5.0],
            Rect::new(10.0, 10.0, 100.0, 50.0),
            &mut out,
        );
        assert_eq!(out.len(), 4);
        assert!(out[0].is_empty() && out[2].is_empty());
        assert!((area(&out[1]) - 2500.0).abs() < 1e-2);
        squarify(&[0.0], Rect::new(0.0, 0.0, 10.0, 10.0), &mut out);
        assert!(out[0].is_empty());
        squarify(&[], Rect::new(0.0, 0.0, 10.0, 10.0), &mut out);
        assert!(out.is_empty(), "{out:?}");
    }
}
