//! A group of charts that share one hover.
//!
//! Every chart on a page sits on the same log time axis, so an age means the same
//! moment in all of them. When the pointer is over any chart of a group, the group
//! snaps it to that chart's nearest plotted point and every chart marks that age
//! with a hairline and a dot; charts with a label band read out their value there.
//!
//! A frame goes: [`ChartGroup::begin`] with the number of charts, a
//! [`ChartGroup::build`] for each, [`ChartGroup::snap`], then a
//! [`ChartGroup::paint`] for each. Building every chart before painting any is what
//! lets the chart under the pointer decide the age the others mark.

use ot_core::Series;
use ot_model::Bytes;
use ot_paint::{Color, DisplayList, Point, Rect};

use crate::format::{self, AgoFields};
use crate::sparkline::{self, Plot, PlotPoint, Side, SparkStyle, TimeAxis};
use crate::theme::Theme;

/// The time axis every chart shares, so one hover lines up across all of them.
pub(crate) const AXIS: TimeAxis = TimeAxis::DEFAULT;
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
    /// The crosshair at `age_ms`, following on from `prev`. The units and the side
    /// change with hysteresis, so a readout keeps its shape and place while the
    /// pointer, or the chart moving under it, jitters around a boundary.
    pub fn follow(prev: Option<Self>, age_ms: f32) -> Self {
        Self {
            age_ms,
            fields: AgoFields::follow(prev.map(|c| c.fields), age_ms),
            side: Side::follow(prev.map(|c| c.side), AXIS.fraction(age_ms)),
        }
    }

    /// How long ago the moment is, into `out`.
    pub fn ago(self, out: &mut String) {
        format::ago(out, self.age_ms, self.fields);
    }
}

#[derive(Debug, Default)]
pub(crate) struct ChartGroup {
    plots: Vec<Plot>,
    /// Each chart's whole area, plot and band, for hit-testing.
    areas: Vec<Rect>,
    /// Each chart's label band; empty for charts without one.
    bands: Vec<Rect>,
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
        self.crosshair = self
            .snapped()
            .or(self.outside)
            .map(|age| Crosshair::follow(prev, age));
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

    /// Start a frame of `n` charts.
    pub fn begin(&mut self, n: usize) {
        self.len = n;
        if self.plots.len() < n {
            self.plots.resize_with(n, Plot::default);
            self.areas.resize(n, Rect::ZERO);
            self.bands.resize(n, Rect::ZERO);
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
        self.plots[i].build(series, plot, max, &AXIS);
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
        plot.paint(dl, style, &AXIS, &mut self.scratch);
        let Some(c) = self.crosshair else {
            if !band.is_empty() {
                sparkline::paint_axis(dl, plot.rect(), band, &AXIS, theme.small, theme.text_dim);
            }
            return None;
        };
        let point = plot.paint_crosshair(dl, c.age_ms, style, &AXIS);
        if !band.is_empty() {
            buf.clear();
            if let Some(p) = &point {
                value(buf, p.mean);
            }
            c.ago(&mut self.ago);
            readout(&mut self.readout, buf, &self.ago, c.side);
            let x = AXIS.x(plot.rect(), c.age_ms);
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
        g.begin(3);
        for (i, a) in areas.iter().enumerate() {
            g.build(i, *a, if i == 0 { AXIS_BAND_H } else { 0.0 }, &s, 100.0);
        }
        g.snap();
        // Over the second chart, near the sample 4 s old.
        let x = AXIS.x(areas[1], 4000.0) + 1.0;
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
        g.begin(1);
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
        const R: Retention = Retention {
            raw: 600,
            tiers: &[Resolution {
                bucket_ms: 10_000,
                capacity: 720,
            }],
        };
        let seconds = 2400;
        let mut series = Series::new(R);
        for t in 0..seconds {
            let v = if t == seconds - 1 - 1500 { 100.0 } else { 10.0 };
            series.push(t * 1000, v);
        }
        let area = Rect::new(0.0, 0.0, 300.0, 60.0);
        let mut group = ChartGroup::default();
        group.begin(1);
        group.build(0, area, AXIS_BAND_H, &series, 100.0);
        group.snap();
        let at = Point::new(AXIS.x(area, 1_500_000.0), 20.0);
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
}
