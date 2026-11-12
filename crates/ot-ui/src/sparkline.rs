//! Compact time-series chart on a log-scale time axis.
//!
//! The present sits on the right edge and age grows leftward on a log scale: the
//! last seconds get a wide stretch at full resolution, the oldest history is
//! compressed into the left end. The axis spans whatever history is held
//! ([`TimeAxis::at`]), so a chart fills its width from a session's first seconds
//! and the span settles once the history is as long as it is kept.
//!
//! The right edge is a moment on a clock ([`TimeAxis::now_ms`]) that the view runs
//! smoothly between samples, so the charts scroll rather than step: a new sample
//! lands on the right edge, its value easing in from the one before
//! ([`TimeAxis::settle`]), and slides left from there while the line holds its
//! value out to the edge; the oldest leaves past the left edge, clipped. Samples
//! that share a column are grouped by bins anchored in time, not on screen
//! ([`TimeAxis::bin`]), so a summarized stretch keeps its members as it scrolls and
//! the compressed end does not shimmer.
//!
//! Drawing is split in two so several charts can share one hover. [`Plot::build`]
//! places a series' history into columns one DIP wide, keeping each column's
//! minimum, maximum and mean; [`Plot::paint`] draws that as a mean line over a
//! light wash, with a min-max envelope wherever a column summarizes several
//! samples, so a spike twenty minutes ago still shows at the compressed end. The
//! more samples a column summarizes, the more the mean line gives way to that
//! range ([`range_step`]), so the compressed end reads as a band of what happened
//! rather than one line through it. The view snaps the pointer to the nearest column of the chart under it and marks
//! that same age in every chart with [`Plot::paint_crosshair`].

use ot_core::{Seam, SeamKind, Series};
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
/// much longer ones compress logarithmically. Negative ages, samples newer than the
/// moment at the right edge, continue past it linearly, at the log's slope there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeAxis {
    /// The oldest age shown, at the left edge, in milliseconds.
    pub span_ms: f32,
    pub tau_ms: f32,
    /// The moment at the right edge, on the series' clock (Unix milliseconds), that
    /// ages are measured from. `None` puts each series' newest sample there, for a
    /// chart that only moves when a sample arrives.
    pub now_ms: Option<i64>,
    /// How far each series' newest sample has eased in from the one before it,
    /// `0.0` (just arrived, drawn at the value before) to `1.0` (in place).
    pub settle: f32,
}

impl TimeAxis {
    /// One hour, with five seconds as the knee (five samples at the default update
    /// speed): the last 10 s take about 17 % of the width, the last minute 39 %, the
    /// last 10 minutes 73 %.
    pub const DEFAULT: Self = Self {
        span_ms: 3_600_000.0,
        tau_ms: 5_000.0,
        now_ms: None,
        settle: 1.0,
    };

    /// The shortest span a chart shows, only so that a log axis over no time at
    /// all still has a scale. Anything longer is the history held, so a session's
    /// first second already reaches across the chart.
    pub const MIN_SPAN_MS: f32 = 100.0;

    /// The axis with `now_ms` at the right edge and the history reaching back
    /// `reach_ms` from it at the left, so a chart fills its width: at least
    /// [`Self::MIN_SPAN_MS`], with its knee at `tau_ms`.
    #[must_use]
    pub fn at(now_ms: Option<i64>, reach_ms: f32, tau_ms: f32) -> Self {
        Self {
            span_ms: reach_ms.max(Self::MIN_SPAN_MS),
            tau_ms: tau_ms.max(1.0),
            now_ms,
            settle: 1.0,
        }
    }

    /// This axis with the newest samples eased in `settle` of the way.
    #[must_use]
    pub fn settled(self, settle: f32) -> Self {
        Self {
            settle: settle.clamp(0.0, 1.0),
            ..self
        }
    }

    /// How old a sample at `at_ms` is on this axis, given the newest sample of its
    /// series. Negative for a sample newer than the right edge.
    #[must_use]
    pub fn age(self, newest_ms: i64, at_ms: i64) -> f32 {
        (self.now_ms.unwrap_or(newest_ms) - at_ms) as f32
    }

    /// Labeled ages, newest first. Those older than the span are skipped, and on a
    /// chart too narrow for them all, the least important ([`Self::TICK_ORDER`])
    /// give way ([`Self::ticks`]).
    pub const TICKS: [(f32, &'static str); 11] = [
        (0.0, "now"),
        (1_000.0, "1s"),
        (5_000.0, "5s"),
        (10_000.0, "10s"),
        (20_000.0, "20s"),
        (60_000.0, "1m"),
        (120_000.0, "2m"),
        (600_000.0, "10m"),
        (3_600_000.0, "1h"),
        (21_600_000.0, "6h"),
        (86_400_000.0, "1d"),
    ];

    /// [`Self::TICKS`] by importance: the decades first, then the marks between.
    const TICK_ORDER: [usize; 11] = [0, 3, 5, 7, 8, 9, 10, 6, 4, 2, 1];

    /// Which of [`Self::TICKS`] a plot `width` wide marks, as a bit per tick: those
    /// within the span whose labels fit without crowding the more important ones.
    /// The grid and the labels both follow it, so every line has its label.
    #[must_use]
    pub fn ticks(self, width: f32) -> u16 {
        let rect = Rect::new(0.0, 0.0, width, 0.0);
        let mut shown = 0u16;
        // The labels placed so far, left and right.
        let mut taken = [(0.0f32, 0.0f32); Self::TICKS.len()];
        let mut n = 0;
        for i in Self::TICK_ORDER {
            let (age, label) = Self::TICKS[i];
            if age > self.span_ms {
                continue;
            }
            let (left, w) = label_box(rect, self.x(rect, age), tick_label_w(label));
            let clear = taken[..n]
                .iter()
                .all(|&(l, r)| left + w + TICK_GAP <= l || r + TICK_GAP <= left);
            if clear {
                taken[n] = (left, left + w);
                n += 1;
                shown |= 1 << i;
            }
        }
        shown
    }

    /// The marked ticks of [`Self::ticks`], newest first.
    pub fn marked(self, width: f32) -> impl Iterator<Item = (f32, &'static str)> {
        let shown = self.ticks(width);
        Self::TICKS
            .into_iter()
            .enumerate()
            .filter(move |&(i, _)| shown & (1 << i) != 0)
            .map(|(_, t)| t)
    }

    fn scale(self) -> f32 {
        (self.span_ms / self.tau_ms).ln_1p()
    }

    /// `0.0` at age zero (the right edge) to `1.0` at the span (the left edge), and
    /// on past either end: negative for negative ages, over one beyond the span.
    fn unclamped(self, age_ms: f32) -> f32 {
        let t = age_ms / self.tau_ms;
        let f = if t >= 0.0 { t.ln_1p() } else { t };
        f / self.scale()
    }

    /// `0.0` at age zero (the right edge) to `1.0` at the span (the left edge).
    #[must_use]
    pub fn fraction(self, age_ms: f32) -> f32 {
        self.unclamped(age_ms).clamp(0.0, 1.0)
    }

    /// The x of an age in `rect`. Ages beyond the span land on the left edge, and
    /// negative ones on the right.
    #[must_use]
    pub fn x(self, rect: Rect, age_ms: f32) -> f32 {
        rect.right() - rect.w * self.fraction(age_ms)
    }

    /// The x of an age in `rect`, past its edges where the age is off the axis, for
    /// a line that is clipped to `rect` rather than flattened against its edges.
    #[must_use]
    pub fn x_unclamped(self, rect: Rect, age_ms: f32) -> f32 {
        rect.right() - rect.w * self.unclamped(age_ms)
    }

    /// How many milliseconds one DIP spans at `age_ms` on a chart `width` wide: the
    /// inverse of the axis's slope there.
    #[must_use]
    pub fn ms_per_dip(self, width: f32, age_ms: f32) -> f32 {
        if width <= 0.0 {
            return f32::INFINITY;
        }
        (age_ms.max(0.0) + self.tau_ms) * self.scale() / width
    }

    /// The bin a sample at `at_ms`, `age_ms` old, is summarized in on a chart
    /// `width` wide: a power-of-two number of milliseconds, at least a column wide
    /// at that age, aligned to the clock. A sample keeps its bin as the chart
    /// scrolls, until the columns around it grow wider than the bin.
    #[must_use]
    pub fn bin(self, width: f32, age_ms: f32, at_ms: i64) -> (i64, i64) {
        let column = (self.ms_per_dip(width, age_ms) * COLUMN_W).ceil();
        let size = (column.clamp(1.0, MAX_BIN_MS) as u64)
            .next_power_of_two()
            .cast_signed();
        (size, at_ms.div_euclid(size))
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

impl Default for TimeAxis {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Width of one plotted column. Samples in the same column-wide time bin are
/// summarized into one point, so a chart draws at most about one point a DIP.
pub(crate) const COLUMN_W: f32 = 1.0;
/// The widest bin, about twelve days.
const MAX_BIN_MS: f32 = 1_073_741_824.0;

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
    /// How far the line has given way to a range here: the [`range_step`] of the
    /// busiest column from here to the newest, so it only grows toward the left
    /// and the line fades once rather than flickering where a column now and then
    /// catches two samples.
    pub step: usize,
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
        let mut bin = (0, i64::MIN);
        for b in series.history() {
            let at = b.mid_ms();
            let age = axis.age(newest.at_unix_ms, at);
            let x = axis.x_unclamped(rect, age);
            let key = axis.bin(rect.w, age, at);
            // The first point past the span keeps its own place, past the left
            // edge, so the line reaches that edge.
            let past = age >= axis.span_ms;
            match self.points.last_mut() {
                Some(p) if key == bin && !past => {
                    let n = p.count + b.count;
                    p.mean = (p.mean * p.count as f32 + b.mean * b.count as f32) / n as f32;
                    p.min = p.min.min(b.min);
                    p.max = p.max.max(b.max);
                    p.count = n;
                }
                _ => {
                    bin = key;
                    self.points.push(PlotPoint {
                        x,
                        age_ms: age,
                        mean: b.mean,
                        min: b.min,
                        max: b.max,
                        count: b.count,
                        step: 0,
                    });
                }
            }
            // Anything older than the first point past the span is off the chart.
            if past {
                break;
            }
        }
        let mut step = 0;
        for p in &mut self.points {
            step = step.max(range_step(p.count));
            p.step = step;
        }
        // The newest sample eases in from the value before it.
        if let [newest, before, ..] = &mut self.points[..] {
            if axis.settle < 1.0 && newest.count == 1 {
                let v = before.mean + (newest.mean - before.mean) * axis.settle;
                (newest.mean, newest.min, newest.max) = (v, v, v);
            }
        }
    }

    /// The plot area from the last build.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// The value at the top of the plot, from the last build.
    #[must_use]
    pub fn max(&self) -> f32 {
        self.max
    }

    /// Newest first.
    #[must_use]
    pub fn points(&self) -> &[PlotPoint] {
        &self.points
    }

    /// The point nearest `x` of those inside the plot: one sliding in past the
    /// right edge or out past the left is drawn, clipped, but not pointed at.
    fn nearest_index(&self, x: f32) -> Option<usize> {
        let inside = self.rect.x..=self.rect.right();
        (0..self.points.len())
            .filter(|&i| inside.contains(&self.points[i].x))
            .min_by(|&a, &b| {
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
        self.index_at_age(age_ms, axis).map(|i| &self.points[i])
    }

    /// The index of [`Plot::at_age`]'s point.
    fn index_at_age(&self, age_ms: f32, axis: &TimeAxis) -> Option<usize> {
        let x = axis.x(self.rect, age_ms);
        let i = self.nearest_index(x)?;
        let p = &self.points[i];
        let older = self.points.get(i + 1).map_or(0.0, |q| p.x - q.x);
        let newer = i.checked_sub(1).map_or(0.0, |j| self.points[j].x - p.x);
        let reach = COLUMN_W.max(older.max(newer) * 0.5);
        ((p.x - x).abs() <= reach).then_some(i)
    }

    fn y(&self, v: f32) -> f32 {
        let f = if self.max > 0.0 {
            (v / self.max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.rect.bottom() - f * self.rect.h
    }

    /// Grid, wash, envelope and mean line: the line and wash through the smoothed
    /// curve ([`smooth`]), the envelope's edges through every lowest and highest
    /// value ([`smooth_through`]), so a spike keeps its height there. `scratch` is caller-owned so steady-state
    /// painting does not allocate.
    pub fn paint(
        &self,
        dl: &mut DisplayList,
        style: &SparkStyle,
        axis: &TimeAxis,
        scratch: &mut Scratch,
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
        for (age, _) in axis.marked(rect.w) {
            if age > 0.0 && age < axis.span_ms {
                dl.fill_rect(hairline(axis.x(rect, age), rect), style.grid);
            }
        }

        if self.points.len() < 2 || self.max <= 0.0 {
            return;
        }
        let bottom = rect.bottom();
        // Chart shapes go oldest (leftmost) first; the points are newest first.
        let n = self.points.len();
        // The oldest leaves past the left edge while it slides out, and a sample
        // that came a little early lands just past the right one.
        dl.push_clip(rect);

        let s = scratch;
        s.ctrl.clear();
        s.ctrl.extend(
            self.points
                .iter()
                .rev()
                .map(|p| Point::new(p.x, self.y(p.mean))),
        );
        // On a running clock the newest sample is a moment old: the line holds
        // its value from there to the edge until the next one lands on it.
        let head = s.ctrl[n - 1];
        if axis.now_ms.is_some() && head.x < rect.right() {
            s.ctrl.push(Point::new(rect.right(), head.y));
        }
        smooth(&s.ctrl, &mut s.line, &mut s.knots);
        let (first, last) = (s.line[0].x, s.line[s.line.len() - 1].x);
        dl.band(
            s.line.iter().copied(),
            [Point::new(first, bottom), Point::new(last, bottom)],
            style.wash,
        );
        if self.points.iter().any(|p| p.step > 0) {
            s.ctrl.clear();
            s.ctrl.extend(
                self.points
                    .iter()
                    .rev()
                    .map(|p| Point::new(p.x, self.y(p.max))),
            );
            smooth_through(&s.ctrl, &mut s.upper, &mut s.edge_knots);
            s.ctrl.clear();
            s.ctrl.extend(
                self.points
                    .iter()
                    .rev()
                    .map(|p| Point::new(p.x, self.y(p.min))),
            );
            smooth_through(&s.ctrl, &mut s.lower, &mut s.edge_knots);
        }

        // Runs of segments that summarize about as many samples, oldest first; a
        // segment takes the step of its older end. Each run is a line and an
        // envelope of its own, so toward the compressed end the line fades and the
        // range strengthens, step by step. A run is the curve from its first
        // point's knot to its last's; the oldest run takes the curve from its
        // start, the newest to its end (the hold).
        let segment = |k: usize| self.points[n - 1 - k].step;
        let mut start = 0;
        while start < n - 1 {
            let step = segment(start);
            let mut end = start + 1;
            while end < n - 1 && segment(end) == step {
                end += 1;
            }
            let span = |knots: &[usize], len: usize| {
                let from = if start == 0 { 0 } else { knots[start] };
                let to = if end == n - 1 { len - 1 } else { knots[end] };
                from..to + 1
            };
            if step > 0 {
                let r = span(&s.edge_knots, s.upper.len());
                let a = (style.envelope.a * RANGE_ENVELOPE[step]).min(1.0);
                dl.band(
                    s.upper[r.clone()].iter().copied(),
                    s.lower[r].iter().copied(),
                    style.envelope.with_alpha(a),
                );
            }
            dl.graph(
                s.line[span(&s.knots, s.line.len())].iter().copied(),
                style.line.with_alpha(style.line.a * RANGE_LINE[step]),
                style.width,
            );
            start = end;
        }
        dl.pop_clip();
    }

    /// Where the drawn curve passes point `i` (newest first): its knot, a blend
    /// of it with its neighbours ([`smooth`]), on a running clock with the hold
    /// after the newest.
    fn knot_y(&self, i: usize) -> f32 {
        let y = |j: usize| self.y(self.points[j].mean);
        let older = y((i + 1).min(self.points.len() - 1));
        let newer = i.checked_sub(1).map_or(y(i), y);
        (older + 4.0 * y(i) + newer) / 6.0
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
        let at = self.index_at_age(age_ms, axis)?;
        let p = self.points[at];
        let r = DOT_R;
        // On the drawn curve; the readout gives the value measured.
        let c = Point::new(x, self.knot_y(at));
        dl.fill_round_rect(Rect::new(c.x - r, c.y - r, 2.0 * r, 2.0 * r), r, style.line);
        Some(p)
    }
}

/// Buffers [`Plot::paint`] draws through, kept by the caller so steady-state
/// painting does not allocate.
#[derive(Debug, Default)]
pub struct Scratch {
    /// Control points, for one curve at a time.
    ctrl: Vec<Point>,
    /// The mean line's curve, and each point's knot on it.
    line: Vec<Point>,
    knots: Vec<usize>,
    /// The envelope's edges' curves, and each point's knot on them.
    upper: Vec<Point>,
    lower: Vec<Point>,
    edge_knots: Vec<usize>,
}

impl Scratch {
    /// The curves of a band's two edges (control points each ascending in x):
    /// [`Scratch::top`] and [`Scratch::bottom`] until the next call.
    pub fn band(&mut self, top: &[Point], bottom: &[Point]) {
        smooth(top, &mut self.upper, &mut self.edge_knots);
        smooth(bottom, &mut self.lower, &mut self.edge_knots);
    }

    /// The top edge's curve from the last [`Scratch::band`].
    #[must_use]
    pub fn top(&self) -> &[Point] {
        &self.upper
    }

    /// The bottom edge's curve from the last [`Scratch::band`].
    #[must_use]
    pub fn bottom(&self) -> &[Point] {
        &self.lower
    }
}

/// About how far apart, in DIPs along x, [`smooth`] places a curve's points.
const SMOOTH_STEP: f32 = 2.0;
/// The most pieces a span between two control points is drawn in.
const SMOOTH_MAX_PIECES: f32 = 24.0;

/// The uniform cubic B-spline over `ctrl` (ascending in x), its ends clamped to
/// the first and last points, as points about [`SMOOTH_STEP`] apart into `out`;
/// `knots[k]` is the index in `out` of the curve's point for `ctrl[k]`.
///
/// The curve has no corners and no jumps in its slope, and passes not through a
/// point but through its knot, a sixth of each neighbour and four sixths of it:
/// a gentle low-pass that takes the zigzag of samples taken a second apart out
/// of the line. It stays within its control points' hull, so it never overshoots
/// (a CPU line does not dip below zero) and x only ever grows; a lone spike is
/// drawn at about two thirds of its height. It is linear in the points, so the
/// curves of stacked edges stay stacked.
pub fn smooth(ctrl: &[Point], out: &mut Vec<Point>, knots: &mut Vec<usize>) {
    out.clear();
    knots.clear();
    let n = ctrl.len();
    if n < 2 {
        out.extend_from_slice(ctrl);
        knots.extend(0..n);
        return;
    }
    // The control points with the ends tripled, so the curve starts and ends on
    // them.
    let q = |j: usize| ctrl[j.saturating_sub(2).min(n - 1)];
    out.push(ctrl[0]);
    // Segment `s` is shaped by `q(s)..=q(s + 3)`; it starts at the knot of
    // `ctrl[s - 1]`.
    for s in 0..=n {
        if s > 0 {
            knots.push(out.len() - 1);
        }
        let (p0, p1, p2, p3) = (q(s), q(s + 1), q(s + 2), q(s + 3));
        let pieces = ((p2.x - p1.x).abs() / SMOOTH_STEP)
            .ceil()
            .clamp(1.0, SMOOTH_MAX_PIECES) as u32;
        for i in 1..=pieces {
            let t = i as f32 / pieces as f32;
            let (t2, t3) = (t * t, t * t * t);
            let u = 1.0 - t;
            let b0 = u * u * u / 6.0;
            let b1 = (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0;
            let b2 = (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0;
            let b3 = t3 / 6.0;
            out.push(Point::new(
                b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x,
                b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y,
            ));
        }
    }
    // Exactly on the last point, not a rounding off it: the edge.
    if let Some(end) = out.last_mut() {
        *end = ctrl[n - 1];
    }
}

/// A smooth curve through every point of `ctrl` (ascending in x), as points
/// about [`SMOOTH_STEP`] apart into `out`; `knots[k]` is the index in `out` of
/// `ctrl[k]`.
///
/// Piecewise cubic with Fritsch-Carlson slopes: no corners, and monotone between
/// neighbours, so it never overshoots them. Unlike [`smooth`] it does not
/// low-pass: it is for the envelope, whose edges say how high and how low the
/// value went.
pub fn smooth_through(ctrl: &[Point], out: &mut Vec<Point>, knots: &mut Vec<usize>) {
    out.clear();
    knots.clear();
    let count = ctrl.len();
    if count < 3 {
        out.extend_from_slice(ctrl);
        knots.extend(0..count);
        return;
    }
    let width = |k: usize| ctrl[k + 1].x - ctrl[k].x;
    let secant = |k: usize| {
        let w = width(k);
        if w > 0.0 {
            (ctrl[k + 1].y - ctrl[k].y) / w
        } else {
            0.0
        }
    };
    // The slope at each point: zero at a peak or a trough, else the weighted
    // harmonic mean of the two sides' (Fritsch and Butland), which keeps each
    // piece monotone.
    let slope = |k: usize| {
        if k == 0 {
            return secant(0);
        }
        if k == count - 1 {
            return secant(count - 2);
        }
        let (before, after) = (secant(k - 1), secant(k));
        if before * after <= 0.0 {
            return 0.0;
        }
        let (w0, w1) = (width(k - 1), width(k));
        3.0 * (w0 + w1) / ((2.0 * w1 + w0) / before + (w1 + 2.0 * w0) / after)
    };
    out.push(ctrl[0]);
    knots.push(0);
    let mut leaving = slope(0);
    for k in 0..count - 1 {
        let (from, to, w) = (ctrl[k], ctrl[k + 1], width(k));
        let arriving = slope(k + 1);
        let pieces = (w.abs() / SMOOTH_STEP).ceil().clamp(1.0, SMOOTH_MAX_PIECES) as u32;
        for piece in 1..=pieces {
            let t = piece as f32 / pieces as f32;
            let (t2, t3) = (t * t, t * t * t);
            let y = (2.0 * t3 - 3.0 * t2 + 1.0) * from.y
                + (t3 - 2.0 * t2 + t) * w * leaving
                + (-2.0 * t3 + 3.0 * t2) * to.y
                + (t3 - t2) * w * arriving;
            out.push(Point::new(from.x + t * w, y));
        }
        // Exactly through the point.
        let last = out.len() - 1;
        out[last] = to;
        knots.push(last);
        leaving = arriving;
    }
}

/// Radius of the hover marker on the line.
const DOT_R: f32 = 4.0;

/// How far a column of `count` samples has given way from a line to a range:
/// `0` for a single sample, then a step per doubling, up to `4` (16 or more).
#[must_use]
pub fn range_step(count: u32) -> usize {
    (count.max(1).ilog2() as usize).min(RANGE_LINE.len() - 1)
}

/// The mean line's opacity at each [`range_step`], as a share of its color's.
const RANGE_LINE: [f32; 5] = [1.0, 0.75, 0.55, 0.4, 0.3];
/// The min-max envelope's opacity at each [`range_step`], as a multiple of its
/// color's.
const RANGE_ENVELOPE: [f32; 5] = [1.0, 1.4, 1.8, 2.2, 2.6];

/// A one-DIP vertical line at `x` across `rect`, on whole DIPs so it stays crisp.
fn hairline(x: f32, rect: Rect) -> Rect {
    Rect::new(x.floor(), rect.y, 1.0, rect.h)
}

/// A seam's dashes: this long, this far apart (start to start), in DIPs.
const SEAM_DASH: f32 = 3.0;
const SEAM_PERIOD: f32 = 6.0;
/// A gap narrower than this on screen is one dashed line at its middle; a wider
/// one gets a line at each end and a faint wash between.
const SEAM_SPLIT_W: f32 = 3.0;
/// How near a seam, in DIPs, the crosshair counts as on it.
const SEAM_REACH: f32 = 3.0;

/// Where on `rect` the gap of `seam` lies, left and right: from its first sample
/// after the restart (the newer, right) to its last before (left), the ages taken
/// against `newest_ms` as [`TimeAxis::age`] does. Unclamped: off either edge for
/// a seam scrolling in or out.
fn seam_span(seam: &Seam, rect: Rect, axis: &TimeAxis, newest_ms: i64) -> (f32, f32) {
    let left = axis.x_unclamped(rect, axis.age(newest_ms, seam.from_ms));
    let right = axis.x_unclamped(rect, axis.age(newest_ms, seam.end_ms()));
    (left.min(right), left.max(right))
}

/// Mark where restarts joined the record: a dashed hairline down `rect` at each
/// gap, at both ends of one wide enough to show, with a faint `wash` between.
/// Clipped to `rect`.
pub fn paint_seams(
    dl: &mut DisplayList,
    rect: Rect,
    axis: &TimeAxis,
    newest_ms: i64,
    seams: &[Seam],
    (line, wash): (Color, Color),
) {
    if rect.is_empty() {
        return;
    }
    let dashed = |dl: &mut DisplayList, x: f32| {
        if x < rect.x || x > rect.right() {
            return;
        }
        let x = x.floor().min(rect.right() - 1.0);
        let mut y = rect.y;
        while y < rect.bottom() {
            let h = SEAM_DASH.min(rect.bottom() - y);
            dl.fill_rect(Rect::new(x, y, 1.0, h), line);
            y += SEAM_PERIOD;
        }
    };
    for seam in seams {
        let (left, right) = seam_span(seam, rect, axis, newest_ms);
        if right < rect.x || left > rect.right() {
            continue;
        }
        if right - left < SEAM_SPLIT_W {
            dashed(dl, f32::midpoint(left, right));
            continue;
        }
        let (l, r) = (left.max(rect.x), right.min(rect.right()));
        dl.fill_rect(Rect::new(l, rect.y, r - l, rect.h), wash);
        dashed(dl, left);
        dashed(dl, right);
    }
}

/// The seam the crosshair at `age_ms` is on, if any: within a few DIPs of its
/// gap. The newest wins where two meet.
#[must_use]
pub fn seam_at<'a>(
    seams: &'a [Seam],
    rect: Rect,
    axis: &TimeAxis,
    newest_ms: i64,
    age_ms: f32,
) -> Option<&'a Seam> {
    if rect.is_empty() {
        return None;
    }
    let x = axis.x(rect, age_ms);
    seams.iter().rev().find(|s| {
        let (left, right) = seam_span(s, rect, axis, newest_ms);
        x >= left - SEAM_REACH && x <= right + SEAM_REACH
    })
}

/// What a readout says on a seam, into `out`: `Updated v0.21.0 → v0.21.1`,
/// `Reinstalled v0.21.0`, or `Restarted as administrator`.
pub fn seam_label(out: &mut String, seam: &Seam) {
    out.clear();
    match &seam.kind {
        SeamKind::Update { from, to } if from == to => {
            out.push_str("Reinstalled v");
            out.push_str(to);
        }
        SeamKind::Update { from, to } => {
            out.push_str("Updated v");
            out.push_str(from);
            out.push_str(" \u{2192} v");
            out.push_str(to);
        }
        SeamKind::Elevated => out.push_str("Restarted as administrator"),
    }
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
    for (age, label) in axis.marked(plot.w) {
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
/// About how wide a character of a tick label is in the small UI font (11 DIP,
/// tabular digits), generously: the text is measured by the backends, not here.
const TICK_CHAR_W: f32 = 6.6;
/// The least space between two tick labels.
const TICK_GAP: f32 = 8.0;
/// Space between the crosshair and its readout.
const READOUT_GAP: f32 = 5.0;

/// About how wide a tick label's text is.
fn tick_label_w(text: &str) -> f32 {
    text.chars().count() as f32 * TICK_CHAR_W
}

/// Where text `w` wide lands for [`label_at`] at `x` in `band`: its left and width.
fn label_box(band: Rect, x: f32, w: f32) -> (f32, f32) {
    let w = w.min(band.w);
    let left = if x - TICK_LABEL_W.min(band.w) * 0.5 < band.x {
        band.x
    } else if x + TICK_LABEL_W.min(band.w) * 0.5 > band.right() {
        band.right() - w
    } else {
        x - w * 0.5
    };
    (left, w)
}

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

    /// Every line drawn, oldest first, with its color.
    fn graphs(dl: &DisplayList) -> Vec<(Color, Vec<Point>)> {
        dl.cmds()
            .iter()
            .filter_map(|c| match *c {
                DrawCmd::Graph { points, color, .. } => Some((color, dl.points(points).to_vec())),
                _ => None,
            })
            .collect()
    }

    /// The points of the line, its runs joined, oldest first.
    fn polyline_points(dl: &DisplayList) -> Vec<Point> {
        let mut out: Vec<Point> = Vec::new();
        for (_, pts) in graphs(dl) {
            let skip = usize::from(out.last() == pts.first());
            out.extend(&pts[skip..]);
        }
        out
    }

    fn polygons(dl: &DisplayList) -> Vec<Vec<Point>> {
        dl.cmds()
            .iter()
            .filter_map(|c| match *c {
                DrawCmd::Band { top, bottom, .. } => {
                    Some([dl.points(top), dl.points(bottom)].concat())
                }
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
        assert!((from_right(10_000.0) - 0.17).abs() < 0.01);
        assert!((from_right(60_000.0) - 0.39).abs() < 0.01);
        assert!((from_right(600_000.0) - 0.73).abs() < 0.01);
        // A wider knee gives the last seconds less and the last minutes more.
        let wide = TimeAxis::at(None, AXIS.span_ms, 4.0 * AXIS.tau_ms);
        let share = |a: TimeAxis, from: f32, to: f32| a.fraction(to) - a.fraction(from);
        assert!(share(wide, 0.0, 10_000.0) < share(AXIS, 0.0, 10_000.0));
        assert!(share(wide, 60_000.0, 600_000.0) > share(AXIS, 60_000.0, 600_000.0));
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
        p.paint(&mut dl, &style(), &AXIS, &mut Scratch::default());
        let pts = polyline_points(&dl);
        // Oldest first, from the oldest sample to the newest, on the edge.
        assert!(pts.windows(2).all(|w| w[0].x < w[1].x));
        assert_eq!(pts[0], Point::new(p.points()[9].x, 25.0));
        assert_eq!(pts[pts.len() - 1], Point::new(200.0, 25.0));
        assert!(
            pts.iter().all(|q| (q.y - 25.0).abs() < 1e-3),
            "50% of 50 DIP tall is y=25 all along"
        );
        // Gaps shrink with age: log, not linear.
        let x = |i: usize| p.points()[i].x;
        assert!(x(0) - x(1) > x(8) - x(9));
    }

    #[test]
    fn a_column_never_holds_two_points_and_spikes_survive_in_the_envelope() {
        // Two hours at 1 Hz with one spike 40 minutes ago; raw holds 10 minutes,
        // 10 s buckets the rest.
        let r = Retention {
            raw: 600,
            tiers: vec![Resolution {
                bucket_ms: 10_000,
                capacity: 720,
            }],
        };
        let n = 7200;
        let spike = n - 1 - 2400;
        let s = per_second(r, (0..n).map(|i| if i == spike { 100.0 } else { 10.0 }));
        let rect = Rect::new(0.0, 0.0, 300.0, 60.0);
        let p = plot(&s, rect, 100.0);
        assert!(p.points().len() <= 301, "{}", p.points().len());
        assert!(
            p.points().windows(2).all(|w| w[0].x - w[1].x > 0.0),
            "strictly right to left"
        );
        assert!(
            p.points().last().unwrap().x <= rect.x,
            "two hours of history reach the left edge"
        );
        let hit = p
            .points()
            .iter()
            .find(|q| q.max >= 100.0)
            .expect("the spike's column");
        assert!(hit.count > 1 && hit.mean < 50.0, "summarized, not the line");
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &AXIS, &mut Scratch::default());
        let polys = polygons(&dl);
        assert!(polys.len() >= 2, "wash and envelope");
        assert!(
            polys[1..].iter().flatten().any(|q| q.y.abs() < 1e-3),
            "the envelope reaches the top at the spike"
        );
        assert!(
            polyline_points(&dl).iter().all(|q| q.y > 20.0),
            "the mean line does not"
        );

        // Toward the compressed end the line gives way to the range, step by
        // step: fainter lines, stronger envelopes, the newest at full strength.
        let lines = graphs(&dl);
        let alphas: Vec<f32> = lines.iter().map(|(c, _)| c.a).collect();
        assert!(alphas.len() > 2, "{alphas:?}");
        assert!(
            alphas.windows(2).all(|w| w[0] <= w[1]),
            "fading toward the left: {alphas:?}"
        );
        assert!((alphas[alphas.len() - 1] - 1.0).abs() < 1e-6);
        assert!(alphas[0] < 0.5, "{alphas:?}");
        // The runs meet: each starts where the one before it ended.
        assert!(lines.windows(2).all(|w| w[0].1.last() == w[1].1.first()));
    }

    #[test]
    fn a_line_turns_to_a_range_a_step_per_doubling() {
        let steps: Vec<usize> = [0, 1, 2, 3, 4, 7, 8, 15, 16, 1000]
            .into_iter()
            .map(range_step)
            .collect();
        assert_eq!(steps, [0, 0, 1, 1, 2, 2, 3, 3, 4, 4]);
    }

    #[test]
    fn on_a_running_clock_the_line_holds_its_newest_value_to_the_edge() {
        // Samples to 9 s; the clock 400 ms on.
        let s = per_second(Retention::raw(100), (0..10).map(|i| i as f32 * 10.0));
        let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        let axis = running(9_400, 9_400.0);
        let mut p = Plot::default();
        p.build(&s, rect, 100.0, &axis);
        assert!(p.points()[0].x < rect.right(), "the newest is 400 ms old");
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &axis, &mut Scratch::default());
        let pts = polyline_points(&dl);
        // The curve ends on the edge at the newest value (90% is y=10), and
        // settles toward it from the newest sample on.
        assert_eq!(pts[pts.len() - 1], Point::new(rect.right(), 10.0));
        let held: Vec<f32> = pts
            .iter()
            .filter(|q| q.x >= p.points()[0].x)
            .map(|q| q.y)
            .collect();
        assert!(held.len() > 2, "{held:?}");
        assert!(
            held.windows(2).all(|w| w[1] <= w[0] + 1e-4) && held[0] < 12.0,
            "{held:?}"
        );
        // The hold is drawn, not pointed at.
        assert_eq!(p.nearest_x(rect.right()).unwrap().age_ms, 400.0);

        // Still, the newest sample is the edge, and the curve ends on it.
        let p = plot(&s, rect, 100.0);
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &AXIS, &mut Scratch::default());
        let pts = polyline_points(&dl);
        assert_eq!(pts[pts.len() - 1], Point::new(p.points()[0].x, 10.0));
    }

    /// Control points `ys` one every `dx` DIPs from x = 0.
    fn ctrl(dx: f32, ys: &[f32]) -> Vec<Point> {
        ys.iter()
            .enumerate()
            .map(|(i, &y)| Point::new(i as f32 * dx, y))
            .collect()
    }

    #[test]
    fn the_curve_is_smooth_takes_out_the_zigzag_and_never_overshoots() {
        // A sample-to-sample zigzag, as aliasing draws it.
        let zig: Vec<f32> = (0..12)
            .map(|i| if i % 2 == 0 { 0.0 } else { 100.0 })
            .collect();
        let pts = ctrl(20.0, &zig);
        let (mut out, mut knots) = (Vec::new(), Vec::new());
        smooth(&pts, &mut out, &mut knots);
        assert_eq!(knots.len(), pts.len());
        assert_eq!(
            (out[0], out[out.len() - 1]),
            (pts[0], pts[11]),
            "clamped ends"
        );
        assert!(out.windows(2).all(|w| w[0].x < w[1].x), "x only grows");
        assert!(
            out.iter().all(|q| (0.0..=100.0).contains(&q.y)),
            "no overshoot"
        );
        // Away from the ends, a sixth-four sixths-a sixth blend at each knot: the
        // zigzag is down to a third of its swing.
        for k in 2..10 {
            let want = if k % 2 == 0 {
                100.0 / 6.0 * 2.0
            } else {
                100.0 / 6.0 * 4.0
            };
            assert!(
                (out[knots[k]].y - want).abs() < 1e-3,
                "{k}: {:?}",
                out[knots[k]]
            );
        }
        // No corners: the slope turns by little from one piece to the next.
        let slopes: Vec<f32> = out
            .windows(2)
            .map(|w| (w[1].y - w[0].y) / (w[1].x - w[0].x))
            .collect();
        let turn = slopes
            .windows(2)
            .skip(12)
            .take(slopes.len() - 24)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(turn < 1.5, "a corner: the slope turns by {turn}");
    }

    #[test]
    fn the_envelope_curve_passes_through_every_extreme_without_overshoot() {
        let ys = [10.0, 10.0, 90.0, 20.0, 25.0, 25.0, 60.0];
        let pts = ctrl(10.0, &ys);
        let (mut out, mut knots) = (Vec::new(), Vec::new());
        smooth_through(&pts, &mut out, &mut knots);
        for (k, p) in pts.iter().enumerate() {
            assert_eq!(out[knots[k]], *p, "through point {k}");
        }
        // Between neighbours it stays between them: the spike keeps its height
        // and nothing bulges past it.
        for k in 0..pts.len() - 1 {
            let (lo, hi) = (ys[k].min(ys[k + 1]), ys[k].max(ys[k + 1]));
            for q in &out[knots[k]..=knots[k + 1]] {
                assert!((lo - 1e-3..=hi + 1e-3).contains(&q.y), "{k}: {q:?}");
            }
        }
    }

    #[test]
    fn a_new_sample_eases_in_from_the_one_before() {
        let s = per_second(Retention::raw(100), [10.0, 10.0, 10.0, 90.0]);
        let rect = Rect::new(0.0, 0.0, 200.0, 100.0);
        let value = |settle: f32| {
            let mut p = Plot::default();
            p.build(&s, rect, 100.0, &running(3_000, 3_000.0).settled(settle));
            p.points()[0].mean
        };
        assert!(
            (value(0.0) - 10.0).abs() < 1e-3,
            "drawn at the value before"
        );
        assert!((value(0.5) - 50.0).abs() < 1e-3);
        assert!((value(1.0) - 90.0).abs() < 1e-3, "in place");
    }

    #[test]
    fn too_few_samples_draws_only_the_grid() {
        let s = per_second(Retention::raw(10), [1.0]);
        let mut dl = DisplayList::new();
        plot(&s, Rect::new(0.0, 0.0, 100.0, 10.0), 1.0).paint(
            &mut dl,
            &style(),
            &AXIS,
            &mut Scratch::default(),
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
        assert_eq!(names, ["now", "10s", "1m", "2m", "10m", "1h"]);
        for (_, r, _) in &texts {
            assert!(r.x >= band.x && r.right() <= band.right() + 1e-3, "{r:?}");
        }
        assert_eq!(texts[0].2, HAlign::Right, "now hugs the right edge");
        assert_eq!(texts[5].2, HAlign::Left, "1h hugs the left edge");
        assert_eq!(texts[2].2, HAlign::Center);
    }

    #[test]
    fn a_wide_chart_marks_every_tick_and_a_narrow_one_the_decades() {
        let names = |w: f32| -> Vec<&str> { AXIS.marked(w).map(|(_, l)| l).collect() };
        assert_eq!(
            names(1600.0),
            ["now", "1s", "5s", "10s", "20s", "1m", "2m", "10m", "1h"]
        );
        assert_eq!(names(120.0), ["now", "1m", "10m", "1h"]);
        // Whatever the width, no two labels crowd each other.
        for width in [60.0, 120.0, 300.0, 700.0, 1000.0, 1600.0] {
            let rect = Rect::new(0.0, 0.0, width, 0.0);
            let mut boxes: Vec<(f32, f32)> = AXIS
                .marked(width)
                .map(|(age, label)| label_box(rect, AXIS.x(rect, age), tick_label_w(label)))
                .collect();
            boxes.sort_by(|one, other| one.0.total_cmp(&other.0));
            for pair in boxes.windows(2) {
                let ((left, w), (next, _)) = (pair[0], pair[1]);
                assert!(left + w + TICK_GAP <= next + 1e-3, "{width}: {boxes:?}");
            }
        }
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

    /// The default axis with `now_ms` at the right edge.
    fn running(now_ms: i64, span_ms: f32) -> TimeAxis {
        TimeAxis {
            span_ms,
            now_ms: Some(now_ms),
            ..AXIS
        }
    }

    #[test]
    fn past_the_right_edge_the_axis_runs_on_linearly() {
        let r = Rect::new(0.0, 0.0, 1000.0, 50.0);
        let x = |age: f32| AXIS.x_unclamped(r, age);
        assert!((x(0.0) - 1000.0).abs() < 1e-3);
        assert!(
            x(-500.0) > 1000.0 && x(-1500.0) > x(-500.0),
            "newer is further right"
        );
        // The slope matches on both sides of the edge.
        let (left, right) = (x(1.0) - x(0.0), x(0.0) - x(-1.0));
        assert!(
            (left - right).abs() < 1e-3 * right.abs(),
            "{left} vs {right}"
        );
        assert!(
            x(2.0 * AXIS.span_ms) < 0.0,
            "older runs on past the left edge"
        );
        assert_eq!(
            AXIS.x(r, -500.0),
            1000.0,
            "the clamped x stays on the chart"
        );
        assert!(AXIS.ms_per_dip(r.w, 0.0) < AXIS.ms_per_dip(r.w, 60_000.0));
    }

    #[test]
    fn a_running_clock_slides_samples_in_past_the_right_edge_clipped() {
        let s = per_second(Retention::raw(100), (0..30).map(|i| i as f32));
        let rect = Rect::new(0.0, 0.0, 400.0, 50.0);
        // The right edge 1.25 s before the newest sample.
        let axis = running(27_750, 20_000.0);
        let mut p = Plot::default();
        p.build(&s, rect, 100.0, &axis);
        let newest = p.points()[0];
        assert!(newest.age_ms < 0.0 && newest.x > rect.right(), "{newest:?}");
        assert!(
            p.points()[1].x > rect.right(),
            "29 s and 28 s are still to come"
        );
        assert!(p.points()[2].x <= rect.right(), "27 s has slid in");
        assert!(
            p.points().last().unwrap().x < rect.x,
            "the oldest in view slides out"
        );
        // The pointer only finds what is in view.
        assert!(p.nearest_x(rect.right()).unwrap().x <= rect.right());
        // Drawn inside a clip, so neither end spills out of the chart.
        let mut dl = DisplayList::new();
        p.paint(&mut dl, &style(), &axis, &mut Scratch::default());
        let cmds = dl.cmds();
        let open = cmds
            .iter()
            .position(|c| matches!(c, DrawCmd::PushClip(r) if *r == rect))
            .expect("a clip to the plot");
        let line = cmds
            .iter()
            .position(|c| matches!(c, DrawCmd::Graph { .. }))
            .unwrap();
        let close = cmds
            .iter()
            .position(|c| matches!(c, DrawCmd::PopClip))
            .unwrap();
        assert!(open < line && line < close);
    }

    #[test]
    fn summarized_columns_keep_their_samples_as_the_chart_scrolls() {
        // Two hours at 1 Hz, varying, on a narrow chart: the old end summarizes many
        // samples a column. A frame later the clock has moved 16 ms; a column's
        // summary should be the same samples, only a hair further left.
        let r = Retention {
            raw: 600,
            tiers: vec![Resolution {
                bucket_ms: 10_000,
                capacity: 720,
            }],
        };
        let s = per_second(r, (0..7200).map(|i| ((i * 37) % 100) as f32));
        let rect = Rect::new(0.0, 0.0, 300.0, 60.0);
        let build = |now: i64| {
            let mut p = Plot::default();
            p.build(&s, rect, 100.0, &running(now, 3_600_000.0));
            p.points().to_vec()
        };
        let (a, b) = (build(7_198_000), build(7_198_016));
        let summarized = |pts: &[PlotPoint]| {
            pts.iter()
                .filter(|p| p.count > 1)
                .map(|p| {
                    let key = (p.count, p.mean.to_bits(), p.min.to_bits(), p.max.to_bits());
                    (key, p.x)
                })
                .collect::<std::collections::HashMap<_, _>>()
        };
        let (a, b) = (summarized(&a), summarized(&b));
        assert!(a.len() > 50, "the old end is summarized: {}", a.len());
        // A column regroups only where a sample crosses into a wider bin, at a
        // place or two along the chart; every other keeps its samples.
        let kept: Vec<(f32, f32)> = a
            .iter()
            .filter_map(|(k, &x)| b.get(k).map(|&y| (x, y)))
            .collect();
        assert!(
            kept.len() + 2 >= a.len(),
            "{} of {} kept",
            kept.len(),
            a.len()
        );
        for (x, y) in kept {
            assert!(y <= x && x - y < 0.1, "{x} -> {y}");
        }
    }

    fn seam(from_ms: i64, to_ms: Option<i64>, kind: SeamKind) -> Seam {
        Seam {
            from_ms,
            to_ms,
            kind,
        }
    }

    #[test]
    fn a_seam_reads_as_what_happened() {
        let mut out = String::from("stale");
        let update = |from: &str, to: &str| SeamKind::Update {
            from: from.into(),
            to: to.into(),
        };
        seam_label(&mut out, &seam(0, None, update("0.21.0", "0.22.0")));
        assert_eq!(out, "Updated v0.21.0 \u{2192} v0.22.0");
        seam_label(&mut out, &seam(0, None, update("0.21.0", "0.21.0")));
        assert_eq!(out, "Reinstalled v0.21.0");
        seam_label(&mut out, &seam(0, None, SeamKind::Elevated));
        assert_eq!(out, "Restarted as administrator");
    }

    #[test]
    fn a_narrow_gap_is_one_dashed_line_and_one_off_the_chart_is_none() {
        let rect = Rect::new(10.0, 0.0, 300.0, 60.0);
        let axis = TimeAxis::DEFAULT;
        let line = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let wash = Color::rgba(0.0, 1.0, 0.0, 1.0);
        // Half a second, half an hour ago: well under a DIP there.
        let old = seam(
            3_600_000 - 1_800_000,
            Some(3_600_000 - 1_799_500),
            SeamKind::Elevated,
        );
        // Older than the span: off the left edge.
        let gone = seam(-10_000, Some(-9_000), SeamKind::Elevated);
        // Still open, the newest sample its start: at the right edge.
        let open = seam(3_600_000, None, SeamKind::Elevated);
        let seams = [gone, old, open];
        let mut dl = DisplayList::new();
        paint_seams(&mut dl, rect, &axis, 3_600_000, &seams, (line, wash));
        let mut xs = Vec::new();
        for c in dl.cmds() {
            match c {
                DrawCmd::FillRect { rect: r, color } if *color == line => {
                    assert!(r.y >= rect.y && r.bottom() <= rect.bottom());
                    if !xs.contains(&r.x) {
                        xs.push(r.x);
                    }
                }
                DrawCmd::FillRect { color, .. } if *color == wash => panic!("no wash"),
                _ => {}
            }
        }
        let mid = f32::midpoint(axis.x(rect, 1_800_000.0), axis.x(rect, 1_799_500.0)).floor();
        assert_eq!(xs, [mid, rect.right() - 1.0]);
        // The crosshair on the old seam finds it, and on the open one, the newest.
        let at = seam_at(&seams, rect, &axis, 3_600_000, 1_799_750.0);
        assert_eq!(at, Some(&seams[1]));
        assert_eq!(
            seam_at(&seams, rect, &axis, 3_600_000, 0.0),
            Some(&seams[2])
        );
        assert_eq!(seam_at(&seams, rect, &axis, 3_600_000, 60_000.0), None);
    }
}
