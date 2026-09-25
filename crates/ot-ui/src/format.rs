//! Number formatting into a caller-owned buffer.
//!
//! Every function clears `out` and writes one value. Painting a 1000-row table means
//! formatting thousands of cells per frame; reusing one `String` keeps that free of
//! allocation.

use std::fmt::Write as _;

use ot_model::Bytes;

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// `1.23 GB`, `45.6 MB`, `789 KB`, `12 B`. Three significant figures.
pub fn bytes(out: &mut String, b: Bytes) {
    out.clear();
    let raw = b.get();
    let mut v = raw as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let _ = if u == 0 {
        write!(out, "{raw} B")
    } else if v >= 100.0 {
        write!(out, "{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        write!(out, "{v:.1} {}", UNITS[u])
    } else {
        write!(out, "{v:.2} {}", UNITS[u])
    };
}

/// Like [`bytes`] with `/s` appended, from a per-interval count. Zero is rendered as
/// an empty string so a column of idle processes reads as quiet rather than noisy.
pub fn rate(out: &mut String, per_interval: Bytes, interval_secs: f32) {
    out.clear();
    if per_interval.get() == 0 || interval_secs <= 0.0 {
        return;
    }
    let per_sec = (per_interval.get() as f64 / f64::from(interval_secs)) as u64;
    bytes(out, Bytes(per_sec));
    out.push_str("/s");
}

/// `0.3`, `12`, `100`, `1,234`. One decimal below ten, none above.
pub fn percent(out: &mut String, p: f32) {
    out.clear();
    let _ = if p < 10.0 {
        write!(out, "{p:.1}")
    } else {
        write!(out, "{p:.0}")
    };
}

/// Plain integer.
pub fn count(out: &mut String, n: u32) {
    out.clear();
    let _ = write!(out, "{n}");
}

/// `52.3 / 63.8 GB` with a shared unit chosen from the larger value.
pub fn bytes_of(out: &mut String, used: Bytes, total: Bytes) {
    out.clear();
    let mut scale = total.get().max(used.get()) as f64;
    let mut u = 0;
    while scale >= 1024.0 && u < UNITS.len() - 1 {
        scale /= 1024.0;
        u += 1;
    }
    let div = 1024f64.powi(i32::try_from(u).unwrap_or(i32::MAX));
    let a = used.get() as f64 / div;
    let b = total.get() as f64 / div;
    let _ = write!(out, "{a:.1} / {b:.1} {}", UNITS[u]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: impl FnOnce(&mut String)) -> String {
        let mut b = String::new();
        f(&mut b);
        b
    }

    #[test]
    fn bytes_picks_three_significant_figures() {
        assert_eq!(s(|b| bytes(b, Bytes(12))), "12 B");
        assert_eq!(s(|b| bytes(b, Bytes(1536))), "1.50 KB");
        assert_eq!(
            s(|b| bytes(b, Bytes(45 * 1024 * 1024 + 600 * 1024))),
            "45.6 MB"
        );
        assert_eq!(s(|b| bytes(b, Bytes(789 * 1024 * 1024))), "789 MB");
        assert_eq!(s(|b| bytes(b, Bytes(1_320_702_444))), "1.23 GB");
    }

    #[test]
    fn rate_blanks_zero() {
        assert_eq!(s(|b| rate(b, Bytes(0), 1.0)), "");
        assert_eq!(s(|b| rate(b, Bytes(2048), 2.0)), "1.00 KB/s");
    }

    #[test]
    fn percent_precision_steps_at_ten() {
        assert_eq!(s(|b| percent(b, 0.34)), "0.3");
        assert_eq!(s(|b| percent(b, 9.96)), "10.0");
        assert_eq!(s(|b| percent(b, 12.4)), "12");
        assert_eq!(s(|b| percent(b, 100.0)), "100");
    }

    #[test]
    fn bytes_of_shares_a_unit() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(
            s(|b| bytes_of(b, Bytes(52 * gb + gb / 3), Bytes(64 * gb))),
            "52.3 / 64.0 GB"
        );
    }
}
