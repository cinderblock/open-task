//! Compact time-series chart on a log-scale time axis.
//!
//! The newest sample sits on the right edge and age grows leftward on a log scale:
//! the last seconds get a wide stretch at full resolution, the last hour is
//! compressed into the left end. A young session draws from the right and grows
//! leftward as it ages, the way Task Manager's graphs fill in.
//!
//! Drawing is split in two so several charts can share one hover. [`Plot::build`]
//! places a series' history into columns one DIP wide, keeping each column's
//! minimum, maximum and mean; [`Plot::paint`] draws that as a mean line over a
//! light wash, with a min-max envelope wherever a column summarizes several
//! samples, so a spike twenty minutes ago still shows at the compressed end. The
//! view snaps the pointer to the nearest column of the chart under it and marks
//! that same age in every chart with [`Plot::paint_crosshair`].

use ot_core::Series;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, TextStyle, VAlign};

#[derive(Debug, Clone, Copy)]
pub struct SparkStyle {
    pub line: Color,
    /// Area under the mean line.
    pub wash: Color,
    /// Between the minimum and maximum where a column summarizes several samples.
    pub envelope: Color,
    pub grid: Color,
    pub crosshair: Color,
    pub width: f32,
}

/// Where an age lands across a chart: `ln(1 + age / tau)`, scaled so that `span`
/// reaches the left edge. Ages much shorter than `tau` spread out almost linearly;
/// much longer ones compress logarithmically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeAxis {
    /// The oldest age shown, at the left edge, in milliseconds.
    pub span_ms: f32,
    pub tau_ms: f32,
}

impl TimeAxis {
    /// One hour, with one second as the knee: the last 10 s take about 29 % of the
    /// width, the last minute half, the last 10 minutes 78 %.
    pub const DEFAULT: Self = Self {
        span_ms: 3_600_000.0,
        tau_ms: 1_000.0,
    };

    /// Labeled ages, newest first. Those older than the span are skipped.
    pub const TICKS: [(f32, &'static str); 5] = [
        (0.0, "now"),
        (10_000.0, "10s"),
        (60_000.0, "1m"),
        (600_000.0, "10m"),
        (3_600_000.0, "1h"),
    ];

    fn scale(self) -> f32 {
        (self.span_ms / self.tau_ms).ln_1p()
    }

    /// `0.0` at age zero (the right edge) to `1.0` at the span (the left edge).
    #[must_use]
    pub fn fraction(self, age_ms: f32) -> f32 {
        ((age_ms.max(0.0) / self.tau_ms).ln_1p() / self.scale()).min(1.0)
    }

    /// The x of an age in `rect`. Ages beyond the span land on the left edge.
    #[must_use]
    pub fn x(self, rect: Rect, age_ms: f32) -> f32 {
        rect.right() - rect.w * self.fraction(age_ms)
    }

    /// The age at `x` in `rect`, the inverse of [`TimeAxis::x`].
    #[must_use]
    pub fn age_at(self, rect: Rect, x: f32) -> f32 {
        if rect.w <= 0.0 {
            return 0.0;
        }
        let f = ((rect.right() - x) / rect.w).clamp(0.0, 1.0);
        self.tau_ms * (f * self.scale()).exp_m1()
    }
}

/// Width of one plotted column. Everything that lands in the same column is
/// summarized into one point, so a chart never draws more points than it has DIPs.
const COLUMN_W: f32 = 1.0;

/// One column of a built plot. Values are in the series' own units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlotPoint {
    pub x: f32,
    /// Age of the newest sample in the column, milliseconds before the series'
    /// newest sample.
    pub age_ms: f32,
    pub mean: f32,
    pub min: f32,
    pub max: f32,
    /// Raw samples behind the point. More than one means `min` and `max` bound a
    /// range the line only shows the mean of.
    pub count: u32,
}

/// A series placed on a time axis, ready to paint and to hit-test. Kept by the view
/// between frames: the buffer is reused, and the hover snaps against the last plot.
#[derive(Debug, Default)]
pub struct Plot {
    rect: Rect,
    max: f32,
    /// Newest (rightmost) first.
    points: Vec<PlotPoint>,
}

impl Plot {
    /// Place `series` into `rect`, scaling values against `max`.
    pub fn build(&mut self, series: &Series, rect: Rect, max: f32, axis: &TimeAxis) {
        self.rect = rect;
        self.max = max;
        self.points.clear();
        let Some(newest) = series.latest() else {
            return;
        };
        if rect.is_empty() {
            return;
        }
        let mut column = i64::MIN;
        for b in series.history() {
            let age = (newest.at_unix_ms - b.mid_ms()).max(0) as f32;
            let x = axis.x(rect, age);
            let c = ((rect.right() - x) / COLUMN_W).floor() as i64;
            match self.points.last_mut() {
                Some(p) if c == column => {
                    let n = p.count + b.count;
                    p.mean = (p.mean * p.count as f32 + b.mean * b.count as f32) / n as f32;
                    p.min = p.min.min(b.min);
                    p.max = p.max.max(b.max);
                    p.count = n;
                }
                _ => {
                    column = c;
                    self.points.push(PlotPoint {
                        x,
                        age_ms: age,
                        mean: b.mean,
                        min: b.min,
                        max: b.max,
                        count: b.count,
                    });
                }
            }
            // The first point past the span sits on the left edge and carries the
            // line all the way there; anything older is off the chart.
            if age >= axis.span_ms {
                break;
            }
        }
    }

    /// The plot area from the last build.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Newest first.
    #[must_use]
    pub fn points(&self) -> &[PlotPoint] {
        &self.points
    }

    fn nearest_index(&self, x: f32) -> Option<usize> {
        (0..self.points.len()).min_by(|&a, &b| {
            (self.points[a].x - x)
                .abs()
                .total_cmp(&(self.points[b].x - x).abs())
        })
    }

    /// The point whose column is nearest `x`, for snapping a pointer.
    #[must_use]
    pub fn nearest_x(&self, x: f32) -> Option<&PlotPoint> {
        self.nearest_index(x).map(|i| &self.points[i])
    }

    /// The point nearest an age, or `None` if the series has nothing there (the age
    /// is older than its history). "There" is within a column, or within half the
    /// gap to a neighbour where the points are sparse (the recent seconds, one
    /// sample every few dozen DIPs).
    #[must_use]
    pub fn at_age(&self, age_ms: f32, axis: &TimeAxis) -> Option<&PlotPoint> {
        let x = axis.x(self.rect, age_ms);
        let i = self.nearest_index(x)?;
        let p = &self.points[i];
        let older = self.points.get(i + 1).map_or(0.0, |q| p.x - q.x);
        let newer = i.checked_sub(1).map_or(0.0, |j| self.points[j].x - p.x);
        let reach = COLUMN_W.max(older.max(newer) * 0.5);
        ((p.x - x).abs() <= reach).then_some(p)
    }

    fn y(&self, v: f32) -> f32 {
        let f = if self.max > 0.0 {
            (v / self.max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.rect.bottom() - f * self.rect.h
    }

    /// Grid, wash, envelope and mean line. `scratch` is caller-owned so steady-state
    /// painting does not allocate.
    pub fn paint(
        &self,
        dl: &mut DisplayList,
        style: &SparkStyle,
        axis: &TimeAxis,
        scratch: &mut Vec<Point>,
    ) {
        let rect = self.rect;
        if rect.is_empty() {
            return;
        }
        // Value grid at quarters; time grid at the labeled ages inside the span.
        for q in 1..4 {
            let y = rect.bottom() - rect.h * (q as f32 / 4.0);
            dl.line(
                Point::new(rect.x, y),
                Point::new(rect.right(), y),
                style.grid,
                1.0,
            );
        }
        for (age, _) in TimeAxis::TICKS {
            if age > 0.0 && age < axis.span_ms {
                dl.fill_rect(hairline(axis.x(rect, age), rect), style.grid);
            }
        }

        if self.points.len() < 2 || self.max <= 0.0 {
            return;
        }
        let bottom = rect.bottom();
        let (first, last) = (self.points[0].x, self.points[self.points.len() - 1].x);

        scratch.clear();
        scratch.extend(self.points.iter().map(|p| Point::new(p.x, self.y(p.mean))));
        dl.fill_polygon(
            scratch
                .iter()
                .copied()
                .chain([Point::new(last, bottom), Point::new(first, bottom)]),
            style.wash,
        );

        if self.points.iter().any(|p| p.max > p.min) {
            let upper = self.points.iter().map(|p| Point::new(p.x, self.y(p.max)));
            let lower = self
                .points
                .iter()
                .rev()
                .map(|p| Point::new(p.x, self.y(p.min)));
            dl.fill_polygon(upper.chain(lower), style.envelope);
        }

        dl.polyline(scratch.iter().copied(), style.line, style.width);
    }

    /// Mark `age_ms`: a hairline across the plot and a dot on the mean line.
    /// Returns the point under the line, if the series reaches back that far.
    pub fn paint_crosshair(
        &self,
        dl: &mut DisplayList,
        age_ms: f32,
        style: &SparkStyle,
        axis: &TimeAxis,
    ) -> Option<PlotPoint> {
        if self.rect.is_empty() {
            return None;
        }
        let x = axis.x(self.rect, age_ms);
        dl.fill_rect(hairline(x, self.rect), style.crosshair);
        let p = *self.at_age(age_ms, axis)?;
        let r = DOT_R;
        let c = Point::new(x, self.y(p.mean));
        dl.fill_round_rect(Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r), r, style.line);
        Some(p)
    }
}

/// Radius of the hover marker on the line.
const DOT_R: f32 = 4.0;

/// A one-DIP vertical line at `x` across `rect`, on whole DIPs so it stays crisp.
fn hairline(x: f32, rect: Rect) -> Rect {
    Rect::new(x.floor(), rect.y, 1.0, rect.h)
}

/// The time labels under a chart: each tick centered on its age, the ones at the
/// ends held inside the band.
pub fn paint_axis(
    dl: &mut DisplayList,
    plot: Rect,
    band: Rect,
    axis: &TimeAxis,
    style: TextStyle,
    color: Color,
) {
    for (age, label) in TimeAxis::TICKS {
        if age <= axis.span_ms {
            label_at(
                dl,
                band,
                axis.x(plot, age),
                TICK_LABEL_W,
                label,
                style,
                color,
            );
        }
    }
}

/// Which side of the crosshair a readout sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// How far past the middle of the axis the line has to be before a readout
    /// changes sides, as a fraction of the width.
    const MARGIN: f32 = 0.05;

    /// The side with more room for a line `fraction` of the way from the right edge
    /// ([`TimeAxis::fraction`]). Keeps `prev` until the line is clearly past the
    /// middle, so a line resting there does not swap the readout back and forth.
    #[must_use]
    pub fn follow(prev: Option<Self>, fraction: f32) -> Self {
        match prev {
            Some(Self::Left) if fraction < 0.5 + Self::MARGIN => Self::Left,
            Some(Self::Right) if fraction > 0.5 - Self::MARGIN => Self::Right,
            _ if fraction <= 0.5 => Self::Left,
            _ => Self::Right,
        }
    }
}

/// A readout under a chart, flush against the crosshair at `x` on `side`, so it
/// stays next to the line all the way to either edge.
pub fn paint_readout(
    dl: &mut DisplayList,
    band: Rect,
    x: f32,
    side: Side,
    text: &str,
    style: TextStyle,
    color: Color,
) {
    let (rect, align) = match side {
        Side::Left => {
            let right = (x - READOUT_GAP).min(band.right());
            (
                Rect::new(band.x, band.y, (right - band.x).max(0.0), band.h),
                HAlign::Right,
            )
        }
        Side::Right => {
            let left = (x + READOUT_GAP).max(band.x);
            (
                Rect::new(left, band.y, (band.right() - left).max(0.0), band.h),
                HAlign::Left,
            )
        }
    };
    dl.text(text, rect, style, color, align, VAlign::Middle, true);
}

const TICK_LABEL_W: f32 = 40.0;
/// Space between the crosshair and its readout.
const READOUT_GAP: f32 = 5.0;

/// Text centered on `x` in a box `w` wide, or pushed against an edge of `band` (and
/// aligned to it) where centering would spill out.
fn label_at(
    dl: &mut DisplayList,
    band: Rect,
    x: f32,
    w: f32,
    text: &str,
    style: TextStyle,
    color: Color,
) {
    let w = w.min(band.w);
    let (left, align) = if x - w * 0.5 < band.x {
        (band.x, HAlign::Left)
    } else if x + w * 0.5 > band.right() {
        (band.right() - w, HAlign::Right)
    } else {
        (x - w * 0.5, HAlign::Center)
    };
    dl.text(
        text,
        Rect::new(left, band.y, w, band.h),
        style,
        color,
        align,
        VAlign::Middle,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_core::{Resolution, Retention};
    use ot_paint::DrawCmd;

    fn style() -> SparkStyle {
        SparkStyle {
            line: Color::WHITE,
            wash: Color::WHITE.with_alpha(0.1),
            envelope: Color::WHITE.with_alpha(0.2),
            grid: Color::WHITE.with_alpha(0.05),
            crosshair: Color::WHITE.with_alpha(0.3),
            width: 1.0,
        }
    }

    const AXIS: TimeAxis = TimeAxis::DEFAULT;

    fn polyline_points(dl: &DisplayList) -> Vec<Point> {
        dl.cmds()
            .iter()
            .find_map(|c| match *c {
                DrawCmd::Polyline { points, .. } => Some(dl.points(points).to_vec()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn polygons(dl: &DisplayList) -> Vec<Vec<Point>> {
        dl.cmds()
            .iter()
            .filter_map(|c| match *c {
                DrawCmd::FillPolygon { points, .. } => Some(dl.points(points).to_vec()),
                _ => None,
            })
            .collect()
    }

    /// One sample a second ending at t = `n` - 1 s.
    fn per_second(retention: Retention, values: impl IntoIterator<Item = f32>) -> Series {
        let mut s = Series::new(retention);
        for (i, v) in values.into_iter().enumerate() {
            s.push(i64::try_from(i).unwrap() * 1000, v);
        }
        s
    }

    fn plot(series: &Series, rect: Rect, max: f32) -> Plot {
        let mut p = Plot::default();
        p.build(series, rect, max, &AXIS);
        p
    }

    #[test]
    fn the_axis_is_logarithmic_and_invertible() {
        let r = Rect::new(0.0, 0.0, 1000.0, 50.0);
        assert!(
            (AXIS.x(r, 0.0) - 1000.0).abs() < 1e-3,
            "now on the right edge"
        );
        assert!(
            AXIS.x(r, AXIS.span_ms).abs() < 1e-3,
            "the span on the left edge"
        );
        assert!(AXIS.x(r, 10.0 * AXIS.span_ms).abs() < 1e-3, "older clamps");
        // The documented proportions of the default axis.
        let from_right = |age: f32| 1.0 - AXIS.x(r, age) / 1000.0;
        assert!((from_right(10_000.0) - 0.29).abs() < 0.01);
        assert!((from_right(60_000.0) - 0.50).abs() < 0.01);
        assert!((from_right(600_000.0) - 0.78).abs() < 0.01);
        for age in [0.0, 500.0, 7_000.0, 90_000.0, 2_000_000.0] {
            let back = AXIS.age_at(r, AXIS.x(r, age));
            assert!((back - age).abs() <= age * 1e-3 + 1.0, "{age} -> {back}");
        }
    }

    #[test]
    fn newest_sample_lands_on_the_right_edge_and_ages_spread_leftward() {
        let s = per_second(Retention::raw(100), [50.0; 10]);
        let rect = Rect::new(0.0, 0.0, 200.0, 50.0);
        let p = plot(&s, rect, 100.0);
        assert_eq!(p.points().len(), 10);
        assert!((p.points()[0].x - 200.0).abs() < 1e-3);
        assert!((p.points()[3].age_ms - 3000.0).abs() < 1e-3);
        assert!((p.points()[3].x - AXIS.x(rect, 3000.0)).abs() < 1e-3);
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &AXIS, &mut Vec::new());
        let pts = polyline_points(&dl);
        assert_eq!(pts.len(), 10);
        assert!((pts[0].y - 25.0).abs() < 1e-3, "50% of 50 DIP tall is y=25");
        // Gaps shrink with age: log, not linear.
        assert!(pts[0].x - pts[1].x > pts[8].x - pts[9].x);
    }

    #[test]
    fn a_column_never_holds_two_points_and_spikes_survive_in_the_envelope() {
        // Two hours at 1 Hz with one spike 40 minutes ago; raw holds 10 minutes,
        // 10 s buckets the rest.
        const R: Retention = Retention {
            raw: 600,
            tiers: &[Resolution {
                bucket_ms: 10_000,
                capacity: 720,
            }],
        };
        let n = 7200;
        let spike = n - 1 - 2400;
        let s = per_second(R, (0..n).map(|i| if i == spike { 100.0 } else { 10.0 }));
        let rect = Rect::new(0.0, 0.0, 300.0, 60.0);
        let p = plot(&s, rect, 100.0);
        assert!(p.points().len() <= 301, "{}", p.points().len());
        assert!(
            p.points().windows(2).all(|w| w[0].x - w[1].x > 0.0),
            "strictly right to left"
        );
        assert!(
            p.points().last().unwrap().x.abs() < 1e-3,
            "two hours of history reach the left edge"
        );
        let hit = p
            .points()
            .iter()
            .find(|q| q.max >= 100.0)
            .expect("the spike's column");
        assert!(hit.count > 1 && hit.mean < 50.0, "summarized, not the line");
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &AXIS, &mut Vec::new());
        let polys = polygons(&dl);
        assert_eq!(polys.len(), 2, "wash and envelope");
        assert!(
            polys[1].iter().any(|q| q.y.abs() < 1e-3),
            "the envelope reaches the top at the spike"
        );
        assert!(
            polyline_points(&dl).iter().all(|q| q.y > 20.0),
            "the mean line does not"
        );
    }

    #[test]
    fn too_few_samples_draws_only_the_grid() {
        let s = per_second(Retention::raw(10), [1.0]);
        let mut dl = DisplayList::new();
        plot(&s, Rect::new(0.0, 0.0, 100.0, 10.0), 1.0).paint(
            &mut dl,
            &style(),
            &AXIS,
            &mut Vec::new(),
        );
        assert!(dl
            .cmds()
            .iter()
            .all(|c| matches!(c, DrawCmd::Line { .. } | DrawCmd::FillRect { .. })));
    }

    #[test]
    fn snapping_and_the_crosshair() {
        let s = per_second(Retention::raw(100), (0..30).map(|i| i as f32));
        let rect = Rect::new(0.0, 0.0, 400.0, 50.0);
        let p = plot(&s, rect, 100.0);
        // A pointer a little right of the 5 s sample snaps to it.
        let x5 = AXIS.x(rect, 5000.0);
        let snapped = p.nearest_x(x5 + 2.0).unwrap();
        assert!((snapped.age_ms - 5000.0).abs() < 1e-3);
        assert!(
            (snapped.mean - 24.0).abs() < 1e-3,
            "t = 24 s is 5 s before 29 s"
        );

        let mut dl = DisplayList::new();
        let got = p.paint_crosshair(&mut dl, 5000.0, &style(), &AXIS).unwrap();
        assert!((got.mean - 24.0).abs() < 1e-3);
        let line = dl.cmds().iter().find_map(|c| match *c {
            DrawCmd::FillRect { rect, .. } => Some(rect),
            _ => None,
        });
        assert_eq!(line, Some(Rect::new(x5.floor(), 0.0, 1.0, 50.0)));
        assert!(dl
            .cmds()
            .iter()
            .any(|c| matches!(c, DrawCmd::FillRoundRect { .. })));

        // An age with no history behind it: the line, but no dot and no point.
        let mut dl = DisplayList::new();
        assert!(p
            .paint_crosshair(&mut dl, 600_000.0, &style(), &AXIS)
            .is_none());
        assert_eq!(dl.cmds().len(), 1);
    }

    #[test]
    fn labels_stay_inside_the_band() {
        let plot = Rect::new(10.0, 0.0, 300.0, 50.0);
        let band = Rect::new(10.0, 50.0, 300.0, 14.0);
        let mut dl = DisplayList::new();
        paint_axis(
            &mut dl,
            plot,
            band,
            &AXIS,
            TextStyle::default(),
            Color::WHITE,
        );
        let texts: Vec<(String, Rect, HAlign)> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some((dl.str(t.text).to_owned(), t.rect, t.halign)),
                _ => None,
            })
            .collect();
        let names: Vec<&str> = texts.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(names, ["now", "10s", "1m", "10m", "1h"]);
        for (_, r, _) in &texts {
            assert!(r.x >= band.x && r.right() <= band.right() + 1e-3, "{r:?}");
        }
        assert_eq!(texts[0].2, HAlign::Right, "now hugs the right edge");
        assert_eq!(texts[4].2, HAlign::Left, "1h hugs the left edge");
        assert_eq!(texts[2].2, HAlign::Center);
    }

    #[test]
    fn the_readout_sits_against_the_line() {
        let band = Rect::new(0.0, 50.0, 300.0, 14.0);
        let placed = |x: f32, side: Side| {
            let mut dl = DisplayList::new();
            paint_readout(
                &mut dl,
                band,
                x,
                side,
                "35% · 5s ago",
                TextStyle::default(),
                Color::WHITE,
            );
            match dl.cmds() {
                [DrawCmd::Text(t)] => (t.rect, t.halign),
                other => panic!("{other:?}"),
            }
        };
        // To the left of the line, ending just short of it.
        let (r, a) = placed(290.0, Side::Left);
        assert_eq!(a, HAlign::Right);
        assert!((r.right() - (290.0 - READOUT_GAP)).abs() < 1e-3 && (r.x - band.x).abs() < 1e-3);
        // To the right of it.
        let (r, a) = placed(20.0, Side::Right);
        assert_eq!(a, HAlign::Left);
        assert!(
            (r.x - (20.0 + READOUT_GAP)).abs() < 1e-3 && (r.right() - band.right()).abs() < 1e-3
        );
    }

    #[test]
    fn the_readout_changes_sides_only_clearly_past_the_middle() {
        use Side::{Left, Right};
        // A fresh readout takes the roomier side: left of a line in the newer half.
        assert_eq!(Side::follow(None, 0.3), Side::Left);
        assert_eq!(Side::follow(None, 0.51), Side::Right);
        // Once placed, it stays through the middle.
        let walk = |start: Side, fractions: &[f32]| {
            let mut side = start;
            fractions
                .iter()
                .map(|&f| {
                    side = Side::follow(Some(side), f);
                    side
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            walk(Left, &[0.49, 0.51, 0.49, 0.54, 0.56, 0.51, 0.46, 0.44]),
            [Left, Left, Left, Left, Right, Right, Right, Left]
        );
    }
}
