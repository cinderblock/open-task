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

/// A count of processor clock cycles: `850 k`, `12.3 M`, `1.20 G`, `68.2 G`,
/// `1.40 T`. Three significant figures, decimal prefixes.
pub fn cycles(out: &mut String, cycles: f64) {
    const UNITS: [&str; 5] = ["", " k", " M", " G", " T"];
    out.clear();
    let mut v = cycles.max(0.0);
    let mut u = 0;
    while v >= 999.5 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    let _ = if u == 0 || v >= 99.95 {
        write!(out, "{v:.0}{}", UNITS[u])
    } else if v >= 9.995 {
        write!(out, "{v:.1}{}", UNITS[u])
    } else {
        write!(out, "{v:.2}{}", UNITS[u])
    };
}

/// [`cycles`] for a table cell: under a million (well under a millisecond of work)
/// is left empty, so a column of idle processes reads as quiet.
pub fn cycles_cell(out: &mut String, n: f64) {
    if n < 1.0e6 {
        out.clear();
    } else {
        cycles(out, n);
    }
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

/// Task Manager's up time: `d:hh:mm:ss`, e.g. `3:04:12:09`.
pub fn uptime(out: &mut String, secs: u64) {
    out.clear();
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    let _ = write!(out, "{d}:{h:02}:{m:02}:{s:02}");
}

/// A network rate in bits per second, decimal like every link speed: `850 Kbps`,
/// `5.60 Mbps`, `1.00 Gbps`. Takes bits.
pub fn bits(out: &mut String, bits_per_sec: f64) {
    const UNITS: [&str; 5] = ["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    out.clear();
    let mut v = bits_per_sec.max(0.0);
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    let _ = if u == 0 || v >= 100.0 {
        write!(out, "{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        write!(out, "{v:.1} {}", UNITS[u])
    } else {
        write!(out, "{v:.2} {}", UNITS[u])
    };
}

/// A byte rate: `1.50 MB/s`, `0 B/s`.
pub fn bytes_per_sec(out: &mut String, b: Bytes) {
    bytes(out, b);
    out.push_str("/s");
}

/// A short duration in milliseconds: `0.4 ms`, `12 ms`.
pub fn ms(out: &mut String, ms: f32) {
    out.clear();
    let _ = if ms < 10.0 {
        write!(out, "{ms:.1} ms")
    } else {
        write!(out, "{ms:.0} ms")
    };
}

/// A clock speed: `3.61 GHz`, or `800 MHz` below one gigahertz.
pub fn clock(out: &mut String, hz: ot_model::Hertz) {
    out.clear();
    let mhz = hz.as_mhz();
    let _ = if mhz >= 1000.0 {
        write!(out, "{:.2} GHz", mhz / 1000.0)
    } else {
        write!(out, "{mhz:.0} MHz")
    };
}

/// A unit of an [`ago`] time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
}

impl TimeUnit {
    const ALL: [Self; 3] = [Self::Seconds, Self::Minutes, Self::Hours];

    const fn ms(self) -> f32 {
        match self {
            Self::Seconds => 1_000.0,
            Self::Minutes => 60_000.0,
            Self::Hours => 3_600_000.0,
        }
    }

    const fn symbol(self) -> char {
        match self {
            Self::Seconds => 's',
            Self::Minutes => 'm',
            Self::Hours => 'h',
        }
    }

    /// The coarsest unit that is at least 1 in `ms`: what a time leads with.
    fn leading(ms: f32) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|u| ms >= u.ms())
            .unwrap_or(Self::Seconds)
    }

    /// The smallest unit a time of `ms` shows: seconds up to ten minutes, minutes
    /// up to two hours, then whole hours.
    fn smallest(ms: f32) -> Self {
        if ms < 600_000.0 {
            Self::Seconds
        } else if ms < 7_200_000.0 {
            Self::Minutes
        } else {
            Self::Hours
        }
    }
}

/// A boundary crossed going older at an age T is only crossed back below
/// `HYSTERESIS * T`.
const HYSTERESIS: f32 = 0.8;

/// Which units an [`ago`] time shows, from its leading unit down to its smallest.
///
/// A readout follows the pointer through ages, and a pointer resting near a
/// boundary (or a chart moving under it) jitters across it. So the units change
/// with hysteresis: [`AgoFields::follow`] moves to coarser units as soon as the age
/// reaches a boundary, and back only once it is well below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgoFields {
    leading: TimeUnit,
    smallest: TimeUnit,
}

impl AgoFields {
    /// The units for an age of `ms` with no readout before it.
    #[must_use]
    pub fn of(ms: f32) -> Self {
        Self::follow(None, ms)
    }

    /// The units for an age of `ms`, keeping those of `prev` (the readout just
    /// shown) while the age is within the hysteresis of their boundaries.
    #[must_use]
    pub fn follow(prev: Option<Self>, ms: f32) -> Self {
        let ms = ms.max(0.0);
        // A unit is allowed if it is what `ms` asks for, what a slightly older age
        // would ask for, or anything between.
        let keep = |prev: Option<TimeUnit>, unit: fn(f32) -> TimeUnit, ms: f32| {
            let (now, older) = (unit(ms), unit(ms / HYSTERESIS));
            prev.map_or(now, |p| p.clamp(now, older))
        };
        let smallest = keep(prev.map(|p| p.smallest), TimeUnit::smallest, ms);
        // The leading unit goes by the age as written, rounded to the smallest unit,
        // so 59.6 s reads `1m 00s` rather than `60s`.
        let written = (ms / smallest.ms()).round() * smallest.ms();
        let leading = keep(prev.map(|p| p.leading), TimeUnit::leading, written).max(smallest);
        Self { leading, smallest }
    }
}

/// How long ago, for a chart readout, in the units `fields` names: `now`,
/// `␣9s ago`, `2m 05s ago`, `␣8m ago`, `1h 05m ago`, `␣2h ago`.
///
/// Written to keep its width while the units stay the same: units after the
/// leading one are zero-padded (and shown when zero), and a lone unit is padded to
/// two digits with a figure space (`␣`, U+2007), which is as wide as a digit in
/// tabular figures.
pub fn ago(out: &mut String, ms: f32, fields: AgoFields) {
    out.clear();
    let AgoFields { leading, smallest } = fields;
    let n = (ms.max(0.0) / smallest.ms()).round() as u64;
    if n == 0 {
        out.push_str("now");
        return;
    }
    for unit in TimeUnit::ALL.into_iter().rev() {
        if unit > leading || unit < smallest {
            continue;
        }
        let v = n / (unit.ms() / smallest.ms()) as u64;
        let _ = if unit < leading {
            write!(out, " {:02}", v % 60)
        } else if leading == smallest {
            write!(out, "{v:\u{2007}>2}")
        } else {
            write!(out, "{v}")
        };
        out.push(unit.symbol());
    }
    out.push_str(" ago");
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
    fn cycles_use_decimal_prefixes_and_three_figures() {
        assert_eq!(s(|b| cycles(b, 0.0)), "0");
        assert_eq!(s(|b| cycles(b, 850_000.0)), "850 k");
        assert_eq!(s(|b| cycles(b, 12_340_000.0)), "12.3 M");
        assert_eq!(s(|b| cycles(b, 1.2e9)), "1.20 G");
        assert_eq!(s(|b| cycles(b, 68.2e9)), "68.2 G");
        assert_eq!(s(|b| cycles(b, 999.7e9)), "1.00 T");
        assert_eq!(s(|b| cycles(b, 1.4e12)), "1.40 T");
        // A table cell stays empty until there is something to say.
        assert_eq!(s(|b| cycles_cell(b, 400_000.0)), "");
        assert_eq!(s(|b| cycles_cell(b, 2.5e6)), "2.50 M");
    }

    #[test]
    fn bytes_of_shares_a_unit() {
        let gb = 1024 * 1024 * 1024;
        assert_eq!(
            s(|b| bytes_of(b, Bytes(52 * gb + gb / 3), Bytes(64 * gb))),
            "52.3 / 64.0 GB"
        );
    }

    #[test]
    fn rates_and_durations() {
        assert_eq!(s(|b| bits(b, 0.0)), "0 bps");
        assert_eq!(s(|b| bits(b, 850_000.0)), "850 Kbps");
        assert_eq!(s(|b| bits(b, 5_600_000.0)), "5.60 Mbps");
        assert_eq!(s(|b| bits(b, 1e9)), "1.00 Gbps");
        assert_eq!(s(|b| bytes_per_sec(b, Bytes(1536))), "1.50 KB/s");
        assert_eq!(s(|b| ms(b, 0.42)), "0.4 ms");
        assert_eq!(s(|b| ms(b, 12.3)), "12 ms");
    }

    #[test]
    fn uptime_and_clock() {
        assert_eq!(s(|b| uptime(b, 0)), "0:00:00:00");
        assert_eq!(
            s(|b| uptime(b, 3 * 86_400 + 4 * 3600 + 12 * 60 + 9)),
            "3:04:12:09"
        );
        assert_eq!(s(|b| clock(b, ot_model::Hertz::from_mhz(1608))), "1.61 GHz");
        assert_eq!(s(|b| clock(b, ot_model::Hertz::from_mhz(800))), "800 MHz");
    }

    /// An age written as a fresh readout would write it.
    fn fresh(ms: f32) -> String {
        s(|b| ago(b, ms, AgoFields::of(ms)))
    }

    #[test]
    fn ago_coarsens_with_age_and_keeps_zeros() {
        assert_eq!(fresh(0.0), "now");
        assert_eq!(fresh(400.0), "now");
        assert_eq!(fresh(4_000.0), "\u{2007}4s ago");
        assert_eq!(fresh(12_300.0), "12s ago");
        assert_eq!(fresh(59_600.0), "1m 00s ago", "rounds up into minutes");
        assert_eq!(fresh(120_000.0), "2m 00s ago", "seconds show at zero");
        assert_eq!(fresh(135_000.0), "2m 15s ago");
        assert_eq!(fresh(545_000.0), "9m 05s ago");
        assert_eq!(fresh(600_000.0), "10m ago", "no seconds from ten minutes");
        assert_eq!(
            fresh(1_490_000.0),
            "25m ago",
            "rounded to the smallest unit"
        );
        assert_eq!(fresh(3_900_000.0), "1h 05m ago");
        assert_eq!(
            fresh(7_200_000.0),
            "\u{2007}2h ago",
            "no minutes from two hours"
        );
        assert_eq!(fresh(45_000_000.0), "13h ago");
    }

    #[test]
    fn ago_keeps_its_width_while_its_units_stay() {
        // Tabular digits and the figure space are all one width, so the same
        // shape with the digits blanked means the same width on screen.
        let shape = |t: String| t.replace(|c: char| c.is_ascii_digit(), "\u{2007}");
        for (a, b) in [
            (1_000.0, 45_000.0),
            (61_000.0, 599_000.0),
            (600_000.0, 3_540_000.0),
            (3_600_000.0, 7_100_000.0),
            (7_200_000.0, 90_000_000.0),
        ] {
            assert_eq!(shape(fresh(a)), shape(fresh(b)), "{a} vs {b}");
        }
    }

    #[test]
    fn ago_units_change_with_hysteresis() {
        const MIN: f32 = 60_000.0;
        const H: f32 = 60.0 * MIN;
        let walk = |ages: &[f32]| {
            let mut fields = None;
            ages.iter()
                .map(|&ms| {
                    let f = AgoFields::follow(fields, ms);
                    fields = Some(f);
                    s(|b| ago(b, ms, f))
                })
                .collect::<Vec<_>>()
        };
        // Seconds go at ten minutes and only come back below eight.
        assert_eq!(
            walk(&[9.9 * MIN, 10.0 * MIN, 9.9 * MIN, 8.5 * MIN, 7.9 * MIN]),
            [
                "9m 54s ago",
                "10m ago",
                "10m ago",
                "\u{2007}9m ago",
                "7m 54s ago"
            ]
        );
        // Minutes go at two hours and come back below 96 minutes.
        assert_eq!(
            walk(&[119.0 * MIN, 2.0 * H, 1.7 * H, 95.0 * MIN]),
            [
                "1h 59m ago",
                "\u{2007}2h ago",
                "\u{2007}2h ago",
                "1h 35m ago"
            ]
        );
        // The leading unit holds on the way back too: a column near the hour
        // boundary that sawtooths across it keeps one shape.
        assert_eq!(
            walk(&[59.0 * MIN, 60.0 * MIN, 58.0 * MIN, 60.0 * MIN, 47.0 * MIN]),
            [
                "59m ago",
                "1h 00m ago",
                "0h 58m ago",
                "1h 00m ago",
                "47m ago"
            ]
        );
        assert_eq!(
            walk(&[55_000.0, 61_000.0, 50_000.0, 47_000.0]),
            ["55s ago", "1m 01s ago", "0m 50s ago", "47s ago"]
        );
        // A jump goes straight to the units of where it lands.
        assert_eq!(
            walk(&[3.0 * H, 5_000.0, 30.0 * MIN]),
            ["\u{2007}3h ago", "\u{2007}5s ago", "30m ago"]
        );
    }
}
