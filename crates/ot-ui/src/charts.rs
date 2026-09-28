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

use crate::format;
use crate::sparkline::{self, Plot, PlotPoint, SparkStyle, TimeAxis};
use crate::theme::Theme;

/// The time axis every chart shares, so one hover lines up across all of them.
pub(crate) const AXIS: TimeAxis = TimeAxis::DEFAULT;
/// Height of the time labels (or the hover readout) under a chart that has them.
pub(crate) const AXIS_BAND_H: f32 = 14.0;

/// Writes a value in a chart's units, for its readout.
pub(crate) type ValueFmt = fn(&mut String, f32);

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
    /// The age every chart marks: the pointer snapped to the nearest plotted point
    /// of the chart under it.
    hover_age: Option<f32>,
    scratch: Vec<Point>,
    readout: String,
}

impl ChartGroup {
    /// Chart `i` as last built.
    #[must_use]
    pub fn plot(&self, i: usize) -> &Plot {
        &self.plots[i]
    }

    /// Charts in the current frame.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The age the crosshair marks, while the pointer is over a chart.
    #[must_use]
    pub fn hover_age(&self) -> Option<f32> {
        self.hover_age
    }

    fn snapped(&self) -> Option<f32> {
        let (i, x) = self.pointer?;
        if i >= self.len {
            return None;
        }
        self.plots[i].nearest_x(x).map(|p| p.age_ms)
    }

    /// Follow the pointer, `None` when it left. Returns whether the crosshair moved.
    pub fn hover(&mut self, at: Option<Point>) -> bool {
        let areas = &self.areas[..self.len];
        self.pointer = at.and_then(|p| areas.iter().position(|r| r.contains(p)).map(|i| (i, p.x)));
        let age = self.snapped();
        std::mem::replace(&mut self.hover_age, age) != age
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
        self.hover_age = self.snapped();
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
        let Some(age) = self.hover_age else {
            if !band.is_empty() {
                sparkline::paint_axis(dl, plot.rect(), band, &AXIS, theme.small, theme.text_dim);
            }
            return None;
        };
        let point = plot.paint_crosshair(dl, age, style, &AXIS);
        if !band.is_empty() {
            readout(&mut self.readout, buf, value, point.as_ref(), age);
            let x = AXIS.x(plot.rect(), age);
            sparkline::paint_readout(dl, band, x, &self.readout, theme.small, theme.text);
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

/// What a chart says at the crosshair: `34% · 12 s ago`. Where the point summarizes
/// several samples and their peak reads differently from their mean, both:
/// `12% avg · 80% peak · 25 min ago`, so a spike the envelope shows is also named.
pub(crate) fn readout(
    out: &mut String,
    tmp: &mut String,
    value: ValueFmt,
    point: Option<&PlotPoint>,
    age_ms: f32,
) {
    out.clear();
    if let Some(p) = point {
        value(tmp, p.mean);
        out.push_str(tmp);
        let mean_len = out.len();
        value(tmp, p.max);
        if p.count > 1 && tmp.as_str() != &out[..mean_len] {
            out.push_str(" avg \u{b7} ");
            out.push_str(tmp);
            out.push_str(" peak");
        }
        out.push_str(" \u{b7} ");
    }
    format::ago(tmp, age_ms);
    out.push_str(tmp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_core::Retention;

    #[test]
    fn a_summarized_point_names_its_peak() {
        let mut out = String::new();
        let mut tmp = String::new();
        let p = PlotPoint {
            x: 0.0,
            age_ms: 1_500_000.0,
            mean: 12.0,
            min: 3.0,
            max: 80.0,
            count: 60,
        };
        readout(&mut out, &mut tmp, percent_value, Some(&p), p.age_ms);
        assert_eq!(out, "12% avg · 80% peak · 25 min ago");
        // A summary whose peak reads the same as its mean does not repeat it.
        let flat = PlotPoint { max: 12.2, ..p };
        readout(&mut out, &mut tmp, percent_value, Some(&flat), p.age_ms);
        assert_eq!(out, "12% · 25 min ago");
        readout(&mut out, &mut tmp, percent_value, None, 4000.0);
        assert_eq!(out, "4 s ago");
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
        let texts: Vec<&str> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                ot_paint::DrawCmd::Text(t) => Some(dl.str(t.text)),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            ["25% · 4 s ago"],
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
}
