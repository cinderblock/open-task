//! A group of charts that share one hover.
//!
//! Every chart on a page sits on the same log time axis, set for the frame by
//! [`ChartGroup::begin`], so an age means the same moment in all of them. When the
//! pointer is over any chart of a group, the group
//! snaps it to that chart's nearest plotted point and every chart marks that age
//! with a hairline and a dot; charts with a label band read out their value there.
//!
//! A frame goes: [`ChartGroup::begin`] with the number of charts, a
//! [`ChartGroup::build`] for each, [`ChartGroup::snap`], then a
//! [`ChartGroup::paint`] for each. Building every chart before painting any is what
//! lets the chart under the pointer decide the age the others mark.

use std::time::Instant;

use ot_core::{Series, Timeline};
use ot_model::Bytes;
use ot_paint::{Color, DisplayList, Point, Rect};

use crate::format::{self, AgoFields};
use crate::sparkline::{self, Plot, PlotPoint, Side, SparkStyle, TimeAxis};
use crate::theme::Theme;

/// The time axis for a frame: `now_ms` at the right edge (the newest sample when
/// `None`), and the charts filling their width with whatever history `timeline`
/// holds back from there, up to `length_ms`, the history setting.
pub(crate) fn axis_of(timeline: &Timeline, now_ms: Option<i64>, length_ms: i64) -> TimeAxis {
    let s = &timeline.cpu_total;
    let now = now_ms.or_else(|| s.latest().map(|n| n.at_unix_ms));
    let reach = match (now, s.oldest_ms()) {
        (Some(now), Some(oldest)) => (now - oldest).clamp(0, length_ms.max(0)),
        _ => 0,
    };
    TimeAxis::at(now_ms, reach as f32)
}

/// A timeline and the axis a frame charts it on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Charted<'a> {
    pub timeline: &'a Timeline,
    pub axis: TimeAxis,
}

impl Charted<'_> {
    /// `timeline` with its newest sample at the right edge and all of it in view.
    #[cfg(test)]
    pub fn still(timeline: &Timeline) -> Charted<'_> {
        Charted {
            timeline,
            axis: axis_of(timeline, None, i64::MAX),
        }
    }
}

/// The newest sample of `series` and how long after the one before it it came, in
/// milliseconds; `None` before there are two.
pub(crate) fn newest_gap(series: &Series) -> Option<(i64, i64)> {
    let mut h = series.history();
    let newest = h.next()?.mid_ms();
    let before = h.next()?.mid_ms();
    Some((newest, newest - before))
}

/// The moment the charts' right edge shows, run smoothly between samples so the
/// charts scroll rather than step.
///
/// The clock trails the newest sample by a little over the interval between
/// samples, so each new one arrives past the right edge and slides in. It runs at
/// the pace samples arrive: real time live, the playback speed in a replay.
/// Arrival jitter is taken up gradually and a jump (a seek, a change of speed) at
/// once, and the clock never passes the newest sample: a feed that stalls stops at
/// the edge rather than scrolling into nothing.
///
/// Arrivals are timed by the frame that first sees them, so the clock reads the same
/// for the same frames, and a window that was not painting (minimized) still learns
/// the right pace once it does.
#[derive(Debug, Default)]
pub(crate) struct ChartClock {
    /// The newest sample, and the frame that first saw it.
    newest: Option<(i64, Instant)>,
    /// Milliseconds between the newest two samples.
    gap_ms: f64,
    /// Sample time per real time, from the arrivals so far; zero before two.
    rate: f64,
    /// The clock as last read, and when.
    read: Option<(f64, Instant)>,
    /// No read since the history started (or started over): the next one puts the
    /// newest sample at the edge, so the first samples are in view at once, and
    /// the clock eases back to its lag from there.
    fresh: bool,
}

impl ChartClock {
    /// Intervals the clock trails the newest sample by: over one, so a sample that
    /// arrives a little late is still past the edge when it does.
    const LAG: f64 = 1.25;
    /// The clock runs faster or slower to take up its difference from where it
    /// should be, by this much more or less a second per second it is off: half a
    /// second off, it runs at twice or half the pace. Never backward.
    const SLEW_MS: f64 = 500.0;
    /// The slowest and fastest it runs while taking up a difference.
    const PACES: (f64, f64) = (0.5, 2.0);
    /// Off by more than this many intervals, the clock jumps instead.
    const JUMP: f64 = 2.0;
    /// Paces past these, one way or the other, are not a feed's.
    const RATES: (f64, f64) = (1.0 / 64.0, 64.0);

    /// The frame at `now` sees `newest_ms` as the newest sample, `gap_ms` after the
    /// one before it.
    pub fn arrive(&mut self, newest_ms: i64, gap_ms: i64, now: Instant) {
        match self.newest {
            Some((prev, _)) if prev == newest_ms => return,
            Some((prev, seen)) if newest_ms > prev => {
                let real = now.saturating_duration_since(seen).as_secs_f64() * 1000.0;
                if real > 0.0 {
                    let rate =
                        ((newest_ms - prev) as f64 / real).clamp(Self::RATES.0, Self::RATES.1);
                    self.rate = if self.rate > 0.0 {
                        f64::midpoint(self.rate, rate)
                    } else {
                        rate
                    };
                }
            }
            // The first sample, or a history that went back (a replay seeking):
            // the pace so far means nothing.
            _ => {
                self.rate = 0.0;
                self.read = None;
                self.fresh = true;
            }
        }
        self.newest = Some((newest_ms, now));
        self.gap_ms = gap_ms.max(0) as f64;
    }

    /// The moment at the right edge for the frame at `now`, or `None` before there
    /// are two samples to pace by.
    pub fn read(&mut self, now: Instant) -> Option<i64> {
        let (newest, seen) = self.newest?;
        if self.gap_ms <= 0.0 {
            self.read = None;
            return None;
        }
        let newest = newest as f64;
        let rate = if self.rate > 0.0 { self.rate } else { 1.0 };
        let since = now.saturating_duration_since(seen).as_secs_f64() * 1000.0;
        let target = newest - self.gap_ms * Self::LAG + rate * since;
        let clock = match self.read {
            Some((was, last)) => {
                let dt = now.saturating_duration_since(last).as_secs_f64() * 1000.0;
                let run = was + rate * dt;
                let off = target - run;
                if off.abs() > self.gap_ms * Self::JUMP {
                    target
                } else {
                    // Faster or slower in proportion to how far off it is, within
                    // the paces, and never past where it should be: a frame long
                    // after the last (a fast replay, a busy machine) must not
                    // overshoot and swing back.
                    let step = rate * dt;
                    let take = (step * off / Self::SLEW_MS)
                        .clamp(step * (Self::PACES.0 - 1.0), step * (Self::PACES.1 - 1.0));
                    run + if off >= 0.0 {
                        take.min(off)
                    } else {
                        take.max(off)
                    }
                }
            }
            None if self.fresh => newest,
            None => target,
        };
        self.fresh = false;
        let clock = clock.min(newest);
        self.read = Some((clock, now));
        Some(clock.round() as i64)
    }

    /// Stop running: the next read starts where the clock should be by then, not
    /// from where it stopped.
    pub fn stop(&mut self) {
        self.read = None;
    }

    /// Whether the clock is running and has not caught up with the newest sample,
    /// so the next frame would show the charts moved.
    #[must_use]
    pub fn moving(&self) -> bool {
        match (self.newest, self.read) {
            (Some((newest, _)), Some((clock, _))) => clock < newest as f64,
            _ => false,
        }
    }
}

/// Height of the time labels (or the hover readout) under a chart that has them.
pub(crate) const AXIS_BAND_H: f32 = 14.0;

/// Writes a value in a chart's units, for its readout.
pub(crate) type ValueFmt = fn(&mut String, f32);

/// The moment every chart of a group marks, and how readouts show it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Crosshair {
    /// The pointer snapped to the nearest plotted point of the chart under it.
    pub age_ms: f32,
    /// The units the time is written in.
    pub fields: AgoFields,
    /// The side of the line readouts sit on.
    pub side: Side,
}

impl Crosshair {
    /// The crosshair at `age_ms` on `axis`, following on from `prev`. The units and
    /// the side change with hysteresis, so a readout keeps its shape and place while
    /// the pointer, or the chart moving under it, jitters around a boundary.
    pub fn follow(prev: Option<Self>, age_ms: f32, axis: TimeAxis) -> Self {
        Self {
            age_ms,
            fields: AgoFields::follow(prev.map(|c| c.fields), age_ms),
            side: Side::follow(prev.map(|c| c.side), axis.fraction(age_ms)),
        }
    }

    /// How long ago the moment is, into `out`.
    pub fn ago(self, out: &mut String) {
        format::ago(out, self.age_ms, self.fields);
    }
}

#[derive(Debug, Default)]
pub(crate) struct ChartGroup {
    /// The time axis of the current frame, the same for every chart in the group.
    axis: TimeAxis,
    plots: Vec<Plot>,
    /// Each chart's whole area, plot and band, for hit-testing.
    areas: Vec<Rect>,
    /// Each chart's label band; empty for charts without one.
    bands: Vec<Rect>,
    /// Each chart's look as last painted, for [`ChartGroup::repaint`].
    styles: Vec<Option<SparkStyle>>,
    /// Charts in the current frame. The vectors only grow, so a page that switches
    /// between few and many charts does not reallocate.
    len: usize,
    /// The pointer over a chart: which one, and its x.
    pointer: Option<(usize, f32)>,
    /// What every chart marks while the pointer is over one of them.
    crosshair: Option<Crosshair>,
    /// An age a chart outside the group asks this group to mark too, while the
    /// pointer is over that chart rather than one of these.
    outside: Option<f32>,
    scratch: Vec<Point>,
    ago: String,
    readout: String,
}

impl ChartGroup {
    /// Chart `i` as last built.
    #[must_use]
    pub fn plot(&self, i: usize) -> &Plot {
        &self.plots[i]
    }

    /// The time axis of the current frame.
    #[cfg(test)]
    #[must_use]
    pub fn axis(&self) -> TimeAxis {
        self.axis
    }

    /// Charts in the current frame.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The age the crosshair marks, while the pointer is over a chart.
    #[cfg(test)]
    #[must_use]
    pub fn hover_age(&self) -> Option<f32> {
        self.crosshair.map(|c| c.age_ms)
    }

    /// The crosshair, while the pointer is over a chart.
    #[must_use]
    pub fn crosshair(&self) -> Option<Crosshair> {
        self.crosshair
    }

    fn snapped(&self) -> Option<f32> {
        let (i, x) = self.pointer?;
        if i >= self.len {
            return None;
        }
        self.plots[i].nearest_x(x).map(|p| p.age_ms)
    }

    /// Move the crosshair to where the pointer snaps now. Leaving the charts
    /// forgets it, so the next hover starts fresh.
    fn resnap(&mut self) {
        let prev = self.crosshair;
        let axis = self.axis;
        self.crosshair = self
            .snapped()
            .or(self.outside)
            .map(|age| Crosshair::follow(prev, age, axis));
    }

    /// Mark `age_ms` in every chart of the group on behalf of a chart outside it
    /// that shares the time axis and has the pointer; `None` when it no longer
    /// does. The group's own pointer wins. Returns whether the crosshair moved.
    pub fn mark(&mut self, age_ms: Option<f32>) -> bool {
        self.outside = age_ms;
        let before = self.crosshair;
        self.resnap();
        self.crosshair != before
    }

    /// Whether the pointer is over one of the group's own charts.
    #[must_use]
    pub fn pointed(&self) -> bool {
        self.pointer.is_some_and(|(i, _)| i < self.len)
    }

    /// Follow the pointer, `None` when it left. Returns whether the crosshair moved.
    pub fn hover(&mut self, at: Option<Point>) -> bool {
        let areas = &self.areas[..self.len];
        self.pointer = at.and_then(|p| areas.iter().position(|r| r.contains(p)).map(|i| (i, p.x)));
        let before = self.crosshair;
        self.resnap();
        self.crosshair != before
    }

    /// Start a frame of `n` charts on `axis`.
    pub fn begin(&mut self, n: usize, axis: TimeAxis) {
        self.axis = axis;
        self.len = n;
        if self.plots.len() < n {
            self.plots.resize_with(n, Plot::default);
            self.areas.resize(n, Rect::ZERO);
            self.bands.resize(n, Rect::ZERO);
            self.styles.resize(n, None);
        }
    }

    /// Move the charts to a new frame's axis, keeping where they are and how they
    /// are scaled: a frame where only the clock moved.
    pub fn set_axis(&mut self, axis: TimeAxis) {
        self.axis = axis;
    }

    /// Whether the group marks a moment, for the pointer or for a chart outside it.
    #[must_use]
    pub fn marking(&self) -> bool {
        self.crosshair.is_some()
    }

    /// Paint chart `i`'s line and time labels again from `series` on the group's
    /// axis, where it was last built and as it was last painted: the layer
    /// [`ChartGroup::paint`] marked, for a frame where only the clock moved (so no
    /// moment is marked).
    pub fn repaint(&mut self, i: usize, dl: &mut DisplayList, series: &Series, theme: &Theme) {
        let Some(style) = self.styles.get(i).copied().flatten() else {
            return;
        };
        let plot = &mut self.plots[i];
        let (rect, max) = (plot.rect(), plot.max());
        plot.build(series, rect, max, &self.axis);
        plot.paint(dl, &style, &self.axis, &mut self.scratch);
        let band = self.bands[i];
        if !band.is_empty() {
            sparkline::paint_axis(dl, rect, band, &self.axis, theme.small, theme.text_dim);
        }
    }

    /// Place chart `i` in `area`, scaled against `max`, with a label band `band_h`
    /// tall along its bottom (zero for none).
    pub fn build(&mut self, i: usize, area: Rect, band_h: f32, series: &Series, max: f32) {
        self.areas[i] = area;
        let (band, plot) = if band_h > 0.0 {
            area.split_bottom(band_h)
        } else {
            (Rect::ZERO, area)
        };
        self.bands[i] = band;
        self.plots[i].build(series, plot, max, &self.axis);
    }

    /// After every chart is built: new samples slide under a pointer that stays
    /// put, and the line should sit on one of them.
    pub fn snap(&mut self) {
        self.resnap();
    }

    /// Draw chart `i`: its series, then either its time labels or, while hovered,
    /// the crosshair and the readout. Returns the point under the crosshair.
    pub fn paint(
        &mut self,
        i: usize,
        dl: &mut DisplayList,
        style: &SparkStyle,
        value: ValueFmt,
        theme: &Theme,
        buf: &mut String,
    ) -> Option<PlotPoint> {
        let plot = &self.plots[i];
        let band = self.bands[i];
        self.styles[i] = Some(*style);
        // The line and the time labels move with the clock (the labels while the
        // span grows): a layer of their own, so a frame where nothing else changed
        // paints just that ([`ChartGroup::repaint`]).
        dl.begin_layer(i as u32);
        plot.paint(dl, style, &self.axis, &mut self.scratch);
        let Some(c) = self.crosshair else {
            if !band.is_empty() {
                sparkline::paint_axis(
                    dl,
                    plot.rect(),
                    band,
                    &self.axis,
                    theme.small,
                    theme.text_dim,
                );
            }
            dl.end_layer();
            return None;
        };
        dl.end_layer();
        let point = plot.paint_crosshair(dl, c.age_ms, style, &self.axis);
        if !band.is_empty() {
            buf.clear();
            if let Some(p) = &point {
                value(buf, p.mean);
            }
            c.ago(&mut self.ago);
            readout(&mut self.readout, buf, &self.ago, c.side);
            let x = self.axis.x(plot.rect(), c.age_ms);
            sparkline::paint_readout(dl, band, x, c.side, &self.readout, theme.small, theme.text);
        }
        point
    }
}

/// The standard look for a series drawn in `color`.
pub(crate) fn style(color: Color, theme: &Theme) -> SparkStyle {
    SparkStyle {
        line: color,
        wash: color.with_alpha(0.12),
        envelope: color.with_alpha(0.2),
        grid: theme.grid,
        crosshair: theme.crosshair,
        width: 1.5,
    }
}

pub(crate) fn percent_value(out: &mut String, v: f32) {
    format::percent(out, v);
    out.push('%');
}

pub(crate) fn bytes_value(out: &mut String, v: f32) {
    format::bytes(out, Bytes(v.max(0.0) as u64));
}

/// A series of bytes per second, shown as such.
pub(crate) fn byte_rate_value(out: &mut String, v: f32) {
    format::bytes_per_sec(out, Bytes(v.max(0.0) as u64));
}

/// A series of bytes per second, shown in bits per second, as networks are.
pub(crate) fn bit_rate_value(out: &mut String, v: f32) {
    format::bits(out, f64::from(v.max(0.0)) * 8.0);
}

/// What a chart says at the crosshair: its value at the dot and how long ago, the
/// time nearer the line. Left of the line `34% · 12s ago`, right of it
/// `2m 05s ago · 34%`. The time keeps its width while its units stay the same, so
/// neither part moves as the pointer does. Just the time where the chart has no
/// value (`value` empty).
pub(crate) fn readout(out: &mut String, value: &str, ago: &str, side: Side) {
    out.clear();
    let (first, second) = match side {
        Side::Left => (value, ago),
        Side::Right => (ago, value),
    };
    out.push_str(first);
    if !value.is_empty() {
        out.push_str(" \u{b7} ");
    }
    out.push_str(second);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_core::{Resolution, Retention};

    #[test]
    fn the_time_sits_next_to_the_line() {
        let mut out = String::new();
        readout(&mut out, "12%", "25m ago", Side::Left);
        assert_eq!(out, "12% · 25m ago");
        readout(&mut out, "12%", "25m ago", Side::Right);
        assert_eq!(out, "25m ago · 12%");
        readout(&mut out, "", "\u{2007}4s ago", Side::Right);
        assert_eq!(out, "\u{2007}4s ago");
    }

    fn ramp() -> Series {
        let mut s = Series::new(Retention::raw(100));
        for t in 0..30 {
            s.push(t * 1000, t as f32);
        }
        s
    }

    #[test]
    fn a_pointer_over_one_chart_marks_every_chart() {
        let s = ramp();
        let mut g = ChartGroup::default();
        let areas = [
            Rect::new(0.0, 0.0, 300.0, 60.0),
            Rect::new(0.0, 100.0, 300.0, 60.0),
            Rect::new(0.0, 200.0, 150.0, 30.0),
        ];
        g.begin(3, TimeAxis::DEFAULT);
        for (i, a) in areas.iter().enumerate() {
            g.build(i, *a, if i == 0 { AXIS_BAND_H } else { 0.0 }, &s, 100.0);
        }
        g.snap();
        // Over the second chart, near the sample 4 s old.
        let x = TimeAxis::DEFAULT.x(areas[1], 4000.0) + 1.0;
        assert!(g.hover(Some(Point::new(x, 130.0))));
        assert_eq!(g.hover_age(), Some(4000.0));
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        let st = style(theme.cpu, &theme);
        for i in 0..3 {
            let p = g.paint(i, &mut dl, &st, percent_value, &theme, &mut buf);
            assert!((p.expect("every chart has that age").mean - 25.0).abs() < 1e-3);
        }
        assert_eq!(
            texts(&dl),
            ["25% · \u{2007}4s ago"],
            "only the chart with a band reads out"
        );

        // Fewer charts next frame: a pointer on a chart that is gone marks nothing.
        g.begin(1, TimeAxis::DEFAULT);
        g.build(0, areas[0], AXIS_BAND_H, &s, 100.0);
        g.snap();
        assert_eq!(g.hover_age(), None);
        assert!(!g.hover(Some(Point::new(x, 130.0))), "outside every chart");
        assert!(g.hover(Some(Point::new(290.0, 20.0))));
        assert!(g.hover(None));
        assert_eq!(g.hover_age(), None);
    }

    fn texts(dl: &DisplayList) -> Vec<&str> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                ot_paint::DrawCmd::Text(t) => Some(dl.str(t.text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_summarized_point_reads_out_one_value() {
        // Forty minutes at 1 Hz with a spike 25 minutes ago, where a column
        // summarizes several 10 s buckets.
        let r = Retention {
            raw: 600,
            tiers: vec![Resolution {
                bucket_ms: 10_000,
                capacity: 720,
            }],
        };
        let seconds = 2400;
        let mut series = Series::new(r);
        for t in 0..seconds {
            let v = if t == seconds - 1 - 1500 { 100.0 } else { 10.0 };
            series.push(t * 1000, v);
        }
        let area = Rect::new(0.0, 0.0, 300.0, 60.0);
        let mut group = ChartGroup::default();
        group.begin(1, TimeAxis::DEFAULT);
        group.build(0, area, AXIS_BAND_H, &series, 100.0);
        group.snap();
        let at = Point::new(TimeAxis::DEFAULT.x(area, 1_500_000.0), 20.0);
        assert!(group.hover(Some(at)));
        let crosshair = group.crosshair().expect("over the chart");
        assert_eq!(
            crosshair.side,
            Side::Right,
            "the older half reads out on the right"
        );

        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        let st = style(theme.cpu, &theme);
        let point = group.paint(0, &mut dl, &st, percent_value, &theme, &mut buf);
        assert!(point.expect("history reaches back that far").count > 1);
        let mut ago = String::new();
        crosshair.ago(&mut ago);
        assert!(ago.ends_with("m ago") && !ago.contains('s'), "{ago}");
        let texts = texts(&dl);
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].starts_with(&format!("{ago} \u{b7} ")) && texts[0].matches('%').count() == 1,
            "the time next to the line, then one value: {texts:?}"
        );
    }

    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Read the clock every 16 ms from `from` to `to` (milliseconds after `t0`),
    /// with a sample `gap` ms of sample time apart arriving every `every` ms of
    /// real time; the clock must never run backward. Returns the last reading.
    fn run_frames(
        c: &mut ChartClock,
        t0: Instant,
        (from, to): (u64, u64),
        (gap, every): (i64, u64),
        first: i64,
    ) -> i64 {
        let mut prev = i64::MIN;
        let mut arrived = from / every;
        for t in (from..=to).step_by(16) {
            if t / every > arrived {
                arrived = t / every;
                c.arrive(first + arrived.cast_signed() * gap, gap, t0 + ms(t));
            }
            let now = c.read(t0 + ms(t)).unwrap();
            assert!(now >= prev, "backward at {t} ms: {prev} -> {now}");
            prev = now;
        }
        prev
    }

    #[test]
    fn the_clock_starts_at_the_newest_sample_and_eases_back_to_its_lag() {
        let t0 = Instant::now();
        let mut c = ChartClock::default();
        assert_eq!(c.read(t0), None, "nothing to show yet");
        // The first samples are in view at once, the newest at the edge, and the
        // clock waits there for the next.
        c.arrive(10_000, 1000, t0);
        assert_eq!(c.read(t0), Some(10_000));
        assert_eq!(c.read(t0 + ms(500)), Some(10_000));
        assert!(!c.moving(), "nothing moves until the next sample");
        // Samples on time from here: it falls back to trailing the newest by a
        // sample and a quarter, slowing down rather than running backward.
        let _ = run_frames(&mut c, t0, (516, 7_996), (1000, 1000), 10_000);
        c.arrive(18_000, 1000, t0 + ms(8_000));
        let settled = c.read(t0 + ms(8_000)).unwrap();
        // Within a frame: the earlier arrivals were timed by the frames that saw them.
        assert!((settled - (18_000 - 1250)).abs() <= 16, "{settled}");
        assert!(c.moving());
        // Settled, it runs in real time, frame by frame.
        let next = c.read(t0 + ms(8_016)).unwrap();
        assert!(
            (15..=17).contains(&(next - settled)),
            "{} ms in 16",
            next - settled
        );
    }

    #[test]
    fn the_clock_takes_up_jitter_gradually_and_waits_at_a_stalled_feed() {
        let t0 = Instant::now();
        let mut c = ChartClock::default();
        c.arrive(10_000, 1000, t0);
        let _ = c.read(t0);
        // The next sample 100 ms late: the clock does not leap to make it up.
        let mut prev = c.read(t0 + ms(1000)).unwrap();
        for f in 1..=60 {
            let at = t0 + ms(1000 + 16 * f);
            if f == 7 {
                c.arrive(11_000, 1000, at);
            }
            let now = c.read(at).unwrap();
            let step = now - prev;
            assert!((0..=40).contains(&step), "frame {f}: a step of {step} ms");
            prev = now;
        }
        // No more samples: the clock stops at the newest rather than run past it.
        for f in 0..400 {
            let _ = c.read(t0 + ms(2000 + 16 * f));
        }
        assert_eq!(c.read(t0 + ms(9000)), Some(11_000));
        assert!(!c.moving(), "nothing left to show moving");
    }

    #[test]
    fn the_clock_runs_at_the_feeds_pace_and_starts_over_when_it_goes_back() {
        let t0 = Instant::now();
        let mut c = ChartClock::default();
        // A replay at 4x: a second of samples every 250 ms.
        c.arrive(10_000, 1000, t0);
        let _ = c.read(t0);
        let last = run_frames(&mut c, t0, (16, 6_000), (1000, 250), 10_000);
        let next = c.read(t0 + ms(6_100)).unwrap();
        // About 400 ms: arrivals timed by 16 ms frames are 64 ms of samples off.
        assert!(
            (340..=460).contains(&(next - last)),
            "{} ms in 100",
            next - last
        );
        // Seeking back: the pace so far means nothing, and the newest is shown at
        // once again.
        c.arrive(3_000, 1000, t0 + ms(6_200));
        assert_eq!(c.read(t0 + ms(6_200)), Some(3_000));
        // Stopped (paused, or a page without charts): it resumes where it should
        // be by then, not where it stopped.
        c.arrive(4_000, 1000, t0 + ms(7_200));
        let _ = c.read(t0 + ms(7_200));
        c.stop();
        assert!(!c.moving());
        let resumed = c.read(t0 + ms(7_300)).unwrap();
        assert!((resumed - (4_000 - 1250 + 100)).abs() <= 1, "{resumed}");
    }

    #[test]
    fn the_axis_reaches_from_the_clock_back_to_the_oldest_sample_held() {
        let mut tl = Timeline::new(Retention::raw(100));
        for i in 0..30 {
            tl.cpu_total.push(10_000 + i * 1000, 5.0);
        }
        // Still: the newest sample at the edge, everything held in view.
        let still = axis_of(&tl, None, 3_600_000);
        assert_eq!((still.now_ms, still.span_ms), (None, 29_000.0));
        // Running: from the clock, a little behind the newest.
        let running = axis_of(&tl, Some(37_750), 3_600_000);
        assert_eq!((running.now_ms, running.span_ms), (Some(37_750), 27_750.0));
        // Never longer than the history setting.
        assert_eq!(axis_of(&tl, Some(37_750), 20_000).span_ms, 20_000.0);
        assert_eq!(newest_gap(&tl.cpu_total), Some((39_000, 1000)));
    }
}
