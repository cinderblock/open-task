//! History: the process table's area drawn as a chart of who has been using the
//! CPU over the charts' reach, the fourth arrangement beside List, Tree and Map.
//!
//! The chart is a stack of bands, one per program (every process of one name
//! together, see [`ot_core::Usage`]), on the time axis every other chart uses. It
//! has two readings, switched above it:
//!
//! - **Fading total**: each band is as thick as the program's cycles used, the
//!   total that fades. This is the number the Cycles column and the Map show, over
//!   time: a band swells while its program works and sags once it stops.
//! - **Rate**: each band is as thick as the cycles the program was using a second.
//!   This is the CPU graph cut up by program; the fading total is this, smoothed.
//!
//! Only so many colors can be told apart, so the programs that take the most of the
//! chart get a band and a color each, and the rest are one grey band. A program
//! keeps its color for as long as it keeps a band, and bands change hands only when
//! a newcomer is clearly bigger, so a chart watched live does not recolor itself.
//! The bands are named one of three ways ([`ChartLegend`]):
//!
//! - **In the bands**: each band's name is written inside it, where it is thick
//!   enough for a line of text over the label's whole width. The place is the
//!   stretch where the band is thickest, kept from frame to frame (anchored in
//!   time, so it scrolls with the band) until another stretch is clearly better.
//!   A band too thin anywhere goes unnamed. The band under the pointer reads out
//!   its name and value beside the pointer. The plot takes the whole width.
//! - **Floating**: the legend, with every band's value now or at the moment under
//!   the pointer, over a top corner of the plot: the one the bands rise into
//!   less, and not the one the marked moment is in.
//! - **Beside**: the same legend in a column of its own right of the plot.

use std::collections::VecDeque;

use ot_core::usage::fade;
use ot_core::{ProgramId, Usage};
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

use crate::charts::{self, Crosshair, AXIS_BAND_H};
use crate::format;
use crate::process_rows::contains_ci;
use crate::settings::ChartLegend;
use crate::sparkline::{self, TimeAxis, COLUMN_W};
use crate::theme::Theme;

/// Programs with a band of their own, and the bands in all: those and "everything
/// else", which is the last.
const SERIES: usize = 8;
const BANDS: usize = SERIES + 1;
const OTHER: usize = SERIES;

/// The row above the chart: the switch and what the chart shows.
const HEAD_H: f32 = 26.0;
const SWITCH_W: f32 = 104.0;
/// Room left of the plot for the value scale.
const GUTTER_W: f32 = 46.0;
const LEGEND_ROW_H: f32 = 22.0;
const SWATCH: f32 = 10.0;
/// A legend row's share for its value.
const LEGEND_VALUE_W: f32 = 76.0;
/// How far a floating legend sits in from the plot's edges.
const FLOAT_INSET: f32 = 6.0;
/// A floating legend moves to the other corner only when the bands rise into
/// that one less than this share of how much they rise into its own.
const FLOAT_SWITCH: f32 = 0.6;
/// A band's name inside it: the height it needs clear, the room either side of
/// the text, and the widest a name is given before it is cut short.
const LABEL_H: f32 = 14.0;
const LABEL_PAD: f32 = 4.0;
const LABEL_MAX_W: f32 = 180.0;
/// The text is not measured here (the backends do that), so a name's width is
/// estimated from its length at this share of the font's size a character,
/// generously for the small UI font; a longer name is cut with an ellipsis.
const CHAR_EM: f32 = 0.6;
/// A name stays where it is while that place's clearance is at least this share of
/// the best place's.
const LABEL_KEEP: f32 = 0.75;
/// The band under the pointer's readout: its offset from the pointer.
const CHIP_OFFSET: f32 = 14.0;
/// A program needs this share of the chart to be given a band, and half of it to
/// keep one.
const BAND_SHARE: f32 = 0.005;
/// A program without a band takes one from the smallest that has one only when
/// this share of its own size is still the bigger.
const KEEP: f32 = 0.8;

/// How strongly a band that is not the one in question is drawn: one the search
/// does not match, or any other than the one pointed at.
const DIM_ALPHA: f32 = 0.28;

/// The color of band `b`.
fn band_color(theme: &Theme, b: usize) -> Color {
    theme.series.get(b).copied().unwrap_or(theme.series_other)
}

/// What a band's thickness is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChartMode {
    /// Cycles used, as the total that fades.
    #[default]
    Total,
    /// Cycles used a second.
    Rate,
}

const MODES: [(ChartMode, &str); 2] = [
    (ChartMode::Total, "Fading total"),
    (ChartMode::Rate, "Rate"),
];

/// What a click on the chart asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChartHit {
    /// The switch was pressed; the chart has changed its reading.
    Mode,
    /// A band, or its legend row: select this program.
    Program(ProgramId),
}

/// One stretch of the history, its cycles sorted into bands.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// When the stretch ended, and its length in seconds.
    end_ms: i64,
    secs: f32,
    v: [f32; BANDS],
}

/// One plotted column: each band's thickness there, in the chart's units.
#[derive(Debug, Clone, Copy)]
struct Pt {
    x: f32,
    age_ms: f32,
    /// When the column's newest stretch ended: where it is in time, which unlike
    /// its age does not change as the chart scrolls.
    end_ms: i64,
    v: [f32; BANDS],
}

/// A band's name, placed inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Label {
    rect: Rect,
    /// The column at its right end, by [`Pt::end_ms`], so the next frame can find
    /// the same place.
    anchor_ms: i64,
}

/// The estimated width of `text` in the small UI font, padded either side.
fn label_w(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * CHAR_EM + 2.0 * LABEL_PAD
}

/// Ink that reads on `fill`: near black on a light color, white on a dark one.
fn ink_on(fill: Color) -> Color {
    let luma = 0.2126 * fill.r + 0.7152 * fill.g + 0.0722 * fill.b;
    if luma > 0.5 {
        Color::hex(0x1A_1A_1A)
    } else {
        Color::hex(0xFF_FF_FF)
    }
}

impl Pt {
    fn sum(&self) -> f32 {
        self.v.iter().sum()
    }
}

#[derive(Debug, Default)]
pub(crate) struct UsageChart {
    mode: ChartMode,
    /// The program each colored band belongs to.
    slots: [Option<ProgramId>; SERIES],
    /// Top of the value scale, kept between frames so it does not breathe.
    scale: f32,
    /// Newest first.
    points: Vec<Pt>,
    /// The pointer, wherever it is, and what it is over.
    pointer: Option<Point>,
    crosshair: Option<Crosshair>,
    hover_band: Option<usize>,
    hover_mode: Option<usize>,
    /// Geometry of the last build.
    area: Rect,
    plot: Rect,
    axis: Rect,
    /// The time axis of the last build, shared with the summary charts.
    time: TimeAxis,
    /// How the bands are named, the setting.
    style: ChartLegend,
    /// The legend's place: beside the plot, over it, or empty when the bands are
    /// named inside themselves.
    legend: Rect,
    /// A floating legend is in the plot's top right corner rather than its left.
    float_right: bool,
    /// Each band's name inside it, where it fits.
    labels: [Option<Label>; BANDS],
    switch: [Rect; 2],
    /// Each band's legend row; empty for a band that has none.
    rows: [Rect; BANDS],
    decay: f64,
    /// Scratch, kept so steady-state painting does not allocate.
    weight: Vec<f32>,
    order: Vec<ProgramId>,
    band_of: Vec<u8>,
    history: Vec<Row>,
    /// What the bands and `history` were worked out from: the usage's revision,
    /// the mode and the plot's width. They change with a sample, not with a
    /// frame; the columns are placed again every frame, as the time axis moves.
    rows_key: Option<(u64, ChartMode, u32)>,
    /// The usage's newest moment at the last build.
    now_ms: i64,
    poly: Vec<Point>,
    cum: Vec<f32>,
    /// The sliding window's extremes, for placing the names ([`Self::fit`]).
    window: (VecDeque<usize>, VecDeque<usize>),
    ago: String,
    text: String,
}

/// The smallest of 1, 2 and 5 times a power of ten that is at least `v`.
fn nice(v: f32) -> f32 {
    if v <= 0.0 || !v.is_finite() {
        return 1.0;
    }
    let p = 10f32.powf(v.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * p)
        .find(|&s| s >= v)
        .unwrap_or(10.0 * p)
}

impl UsageChart {
    #[cfg(test)]
    pub fn mode(&self) -> ChartMode {
        self.mode
    }

    /// Where the chart was laid out last.
    #[must_use]
    pub fn area(&self) -> Rect {
        self.area
    }

    /// Name the bands this way from the next build on.
    pub fn set_legend(&mut self, style: ChartLegend) {
        if self.style != style {
            self.style = style;
            self.labels = [None; BANDS];
        }
    }

    /// Whether the chart marks a moment, its own or another chart's.
    #[must_use]
    pub fn marking(&self) -> bool {
        self.crosshair.is_some()
    }

    /// Lay the chart out in `rect` and place the history in it on `time`. `outside`
    /// is the age another chart on the same time axis marks, which this one marks
    /// too while the pointer is not over its own plot.
    pub fn build(&mut self, rect: Rect, usage: &Usage, outside: Option<f32>, time: TimeAxis) {
        self.area = rect;
        self.time = time;
        self.decay = usage.decay();
        let (head, body) = rect.split_top(HEAD_H);
        let (_, body) = body.split_top(4.0);
        for (i, r) in self.switch.iter_mut().enumerate() {
            *r = Rect::new(
                head.x + SWITCH_W * i as f32,
                head.y + 2.0,
                SWITCH_W,
                head.h - 4.0,
            );
        }
        // Only a legend beside the plot takes width from it.
        let main = if self.style == ChartLegend::Beside {
            let legend_w = (rect.w * 0.28).clamp(150.0, 240.0).min(body.w * 0.5);
            let (main, legend) = body.split_left((body.w - legend_w - 8.0).max(0.0));
            self.legend = legend.split_left(8.0).1;
            main
        } else {
            body
        };
        let (axis, plot) = main.split_bottom(AXIS_BAND_H);
        self.plot = plot.split_left(GUTTER_W.min(plot.w)).1;
        self.axis = axis.split_left(GUTTER_W.min(axis.w)).1;

        self.now_ms = usage.now_ms();
        let key = (usage.revision(), self.mode, self.plot.w.to_bits());
        if self.rows_key != Some(key) {
            self.assign_bands(usage);
            self.fill_rows(usage);
            self.rows_key = Some(key);
        }
        self.place();
        self.rescale();

        // The moment to mark: under the pointer, or what another chart marks.
        let own = self.pointed_age();
        let prev = self.crosshair;
        self.crosshair = own
            .or(outside)
            .map(|age| Crosshair::follow(prev, age, time));
        self.hover_mode = self
            .pointer
            .and_then(|p| self.switch.iter().position(|r| r.contains(p)));
        // The names inside the bands are placed when painted, in the theme's font.
        match self.style {
            ChartLegend::Inline => self.legend = Rect::ZERO,
            ChartLegend::Floating => self.float_legend(),
            ChartLegend::Beside => {}
        }
        self.lay_out_legend();
        self.hover_band = self.pointer.and_then(|p| self.band_at(p));
    }

    /// Whether the bands are drawn, with what to name: shown when it has a band of
    /// its own, or for everything else, when anything is in it.
    fn shown(&self, b: usize) -> bool {
        if b == OTHER {
            self.points.iter().any(|p| p.v[OTHER] > 0.0)
        } else {
            self.slots[b].is_some()
        }
    }

    /// The name band `b` is labeled with.
    fn band_name<'u>(&self, usage: &'u Usage, b: usize) -> &'u str {
        self.slots
            .get(b)
            .copied()
            .flatten()
            .map_or("Everything else", |g| usage.program_name(g))
    }

    /// Place the floating legend over a top corner of the plot: the one the bands
    /// rise into less, kept until the other is clearly better, and away from the
    /// marked moment unless the pointer is on the legend itself.
    fn float_legend(&mut self) {
        let plot = self.plot;
        let rows = (0..BANDS).filter(|&b| self.shown(b)).count();
        let w = (plot.w * 0.28).clamp(150.0, 220.0).min(plot.w * 0.5);
        let h = (LEGEND_ROW_H * (rows + 1) as f32 + 4.0).min(plot.h - 2.0 * FLOAT_INSET);
        if w <= 0.0 || h <= LEGEND_ROW_H {
            self.legend = Rect::ZERO;
            return;
        }
        let y = plot.y + FLOAT_INSET;
        let left = Rect::new(plot.x + FLOAT_INSET, y, w, h);
        let right = Rect::new(plot.right() - FLOAT_INSET - w, y, w, h);
        // How far the top of the stack rises into a corner, summed across it.
        let covered = |r: Rect| -> f32 {
            self.points
                .iter()
                .filter(|p| p.x >= r.x && p.x <= r.right())
                .map(|p| (r.bottom() - self.y(p.sum())).max(0.0))
                .sum()
        };
        let (here, there) = if self.float_right {
            (right, left)
        } else {
            (left, right)
        };
        let mut flip = covered(there) < covered(here) * FLOAT_SWITCH;
        let on_legend = self.pointer.is_some_and(|p| here.contains(p));
        if let Some(c) = self.crosshair.filter(|_| !on_legend) {
            let x = self.time.x(plot, c.age_ms);
            let near = |r: Rect| x >= r.x - FLOAT_INSET && x <= r.right() + FLOAT_INSET;
            if near(here) && !near(there) {
                flip = true;
            } else if near(there) {
                flip = false;
            }
        }
        if flip {
            self.float_right = !self.float_right;
        }
        self.legend = if self.float_right { right } else { left };
    }

    /// Find each band the place its name fits best: the stretch, as wide as the
    /// name, over which the band leaves the most height clear. A name keeps last
    /// frame's place while that is still nearly as good, so names do not hop about
    /// as the chart scrolls.
    fn place_labels(&mut self, usage: &Usage, font: f32) {
        let prev = std::mem::replace(&mut self.labels, [None; BANDS]);
        let n = self.points.len();
        if n < 2 || self.scale <= 0.0 || self.plot.is_empty() {
            return;
        }
        let mut cum = std::mem::take(&mut self.cum);
        let mut window = std::mem::take(&mut self.window);
        cum.clear();
        cum.resize(n, 0.0);
        for (b, prev) in prev.iter().enumerate() {
            let w = label_w(self.band_name(usage, b), font).min(LABEL_MAX_W);
            if self.shown(b) && w < self.plot.w {
                let anchor = prev.map(|l| l.anchor_ms);
                self.labels[b] = self.fit(b, w, &cum, anchor, &mut window);
            }
            for (p, c) in self.points.iter().zip(&mut cum) {
                *c += p.v[b];
            }
        }
        self.cum = cum;
        self.window = window;
    }

    /// Where `band`'s name, `width` wide, fits best, if anywhere: `below` is the
    /// sum of the bands under it at each column, `anchor` the column it was at last
    /// frame.
    ///
    /// Columns are newest (rightmost) first. For each column as the name's right
    /// end, the window runs to the first column at or past its left end, which
    /// bounds the band between them; the clear height is the window's highest lower
    /// edge less its lowest upper edge, each kept by a monotonic queue so the sweep
    /// is linear in the columns.
    fn fit(
        &self,
        band: usize,
        width: f32,
        below: &[f32],
        anchor: Option<i64>,
        window: &mut (VecDeque<usize>, VecDeque<usize>),
    ) -> Option<Label> {
        // A place: its clear height, its right end's column, and the band's upper
        // and lower edges over it, on screen, where y grows downward.
        type Place = (f32, usize, f32, f32);
        let (floors, ceils) = window;
        floors.clear();
        ceils.clear();
        let lower = |i: usize| self.y(below[i]);
        let upper = |i: usize| self.y(below[i] + self.points[i].v[band]);
        let count = self.points.len();
        let mut best: Option<Place> = None;
        let mut kept: Option<Place> = None;
        let mut next = 0;
        for i in 0..count {
            while floors.front().is_some_and(|&j| j < i) {
                floors.pop_front();
            }
            while ceils.front().is_some_and(|&j| j < i) {
                ceils.pop_front();
            }
            let right = self.points[i].x;
            let left = right - width;
            if left < self.plot.x {
                break;
            }
            let reached = |next: usize| next > i && self.points[next - 1].x <= left;
            while next < count && !reached(next) {
                // `floors` keeps the window's highest lower edge (least y) at its
                // front, `ceils` its lowest upper edge (greatest y).
                let y = lower(next);
                while floors.back().is_some_and(|&j| lower(j) >= y) {
                    floors.pop_back();
                }
                floors.push_back(next);
                let y = upper(next);
                while ceils.back().is_some_and(|&j| upper(j) <= y) {
                    ceils.pop_back();
                }
                ceils.push_back(next);
                next += 1;
            }
            // The history does not reach back past the name's left end.
            if !reached(next) {
                break;
            }
            if right > self.plot.right() {
                continue;
            }
            let (Some(&floor), Some(&ceil)) = (floors.front(), ceils.front()) else {
                continue;
            };
            let (top, bottom) = (upper(ceil), lower(floor));
            let here = (bottom - top, i, top, bottom);
            if best.is_none_or(|best| here.0 > best.0) {
                best = Some(here);
            }
            if anchor == Some(self.points[i].end_ms) {
                kept = Some(here);
            }
        }
        let pick = match (kept, best) {
            (Some(kept), Some(best)) if kept.0 >= LABEL_H && kept.0 >= best.0 * LABEL_KEEP => kept,
            (_, best) => best?,
        };
        let (clear, i, top, bottom) = pick;
        (clear >= LABEL_H).then(|| {
            let p = &self.points[i];
            let mid = f32::midpoint(top, bottom);
            Label {
                rect: Rect::new(p.x - width, mid - LABEL_H * 0.5, width, LABEL_H),
                anchor_ms: p.end_ms,
            }
        })
    }

    /// Decide which programs get a band: those that take the most of the chart,
    /// with the ones that already have a band staying unless clearly overtaken.
    fn assign_bands(&mut self, usage: &Usage) {
        let now = usage.now_ms();
        let plot = self.plot;
        self.weight.clear();
        self.weight.resize(usage.programs(), 0.0);
        for f in usage.frames() {
            let age = self.time.age(now, f.end_ms);
            if age >= self.time.span_ms || f.span_ms <= 0 {
                break;
            }
            // A band's area here: its rate times the width the stretch gets.
            let w = self.time.x(plot, age) - self.time.x(plot, age + f.span_ms as f32);
            let per = w / f.span_ms as f32;
            for &(g, cycles) in f.cycles {
                if let Some(slot) = self.weight.get_mut(g as usize) {
                    *slot += cycles * per;
                }
            }
        }
        let weight = &self.weight;
        let of = |g: ProgramId| weight.get(g as usize).copied().unwrap_or(0.0);
        let floor = weight.iter().sum::<f32>() * BAND_SHARE;
        self.order.clear();
        self.order.extend(
            (0..weight.len())
                .filter(|&g| weight[g] > floor)
                .map(|g| g as ProgramId),
        );
        self.order
            .sort_unstable_by(|&a, &b| of(b).total_cmp(&of(a)).then(a.cmp(&b)));
        for slot in &mut self.slots {
            if slot.is_some_and(|g| of(g) <= floor * 0.5) {
                *slot = None;
            }
        }
        // Biggest first: into a free band, or in place of the smallest program
        // that has one, if clearly bigger than it.
        for &g in self.order.iter().take(SERIES) {
            if self.slots.contains(&Some(g)) {
                continue;
            }
            if let Some(free) = self.slots.iter_mut().find(|s| s.is_none()) {
                *free = Some(g);
                continue;
            }
            let smallest = self
                .slots
                .iter_mut()
                .min_by(|a, b| of(a.unwrap_or(0)).total_cmp(&of(b.unwrap_or(0))));
            match smallest {
                Some(slot) if of(g) * KEEP > of(slot.unwrap_or(0)) => *slot = Some(g),
                _ => break,
            }
        }
        self.band_of.clear();
        self.band_of.resize(usage.programs(), OTHER as u8);
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(b) = slot.and_then(|g| self.band_of.get_mut(g as usize)) {
                *b = i as u8;
            }
        }
    }

    /// Sort the history into bands and turn it into the mode's values: once a
    /// sample, since neither depends on where the time axis is.
    fn fill_rows(&mut self, usage: &Usage) {
        self.history.clear();
        for f in usage.frames() {
            if f.span_ms <= 0 {
                continue;
            }
            let mut v = [0.0; BANDS];
            for &(g, cycles) in f.cycles {
                let band = self
                    .band_of
                    .get(g as usize)
                    .map_or(OTHER, |&b| usize::from(b));
                v[band] += cycles;
            }
            self.history.push(Row {
                end_ms: f.end_ms,
                secs: f.span_ms as f32 / 1000.0,
                v,
            });
        }
        match self.mode {
            ChartMode::Rate => {
                for r in &mut self.history {
                    for v in &mut r.v {
                        *v /= r.secs;
                    }
                }
            }
            // The same sum the totals are, run over the history oldest first.
            // Fading is the same for every program, so "everything else" fades as
            // one.
            ChartMode::Total => {
                let mut total = [0.0f64; BANDS];
                for r in self.history.iter_mut().rev() {
                    let (kept, arrived) = fade(self.decay, f64::from(r.secs));
                    for (t, v) in total.iter_mut().zip(&mut r.v) {
                        *t = *t * kept + f64::from(*v) * arrived;
                        *v = *t as f32;
                    }
                }
            }
        }
    }

    /// Place the rows in columns across the plot, on this frame's time axis.
    fn place(&mut self) {
        self.points.clear();
        if self.plot.is_empty() {
            return;
        }
        // Grouped by bins anchored in time, as the line charts are, so a stretch
        // keeps its column's company as the chart scrolls; the newest stretch
        // slides in past the right edge and the oldest out past the left, clipped.
        let mut bin = (0, i64::MIN);
        let mut secs = 0.0;
        for r in &self.history {
            let age_ms = self.time.age(self.now_ms, r.end_ms);
            let x = self.time.x_unclamped(self.plot, age_ms);
            let key = self.time.bin(self.plot.w, age_ms, r.end_ms);
            match self.points.last_mut() {
                // Several stretches in one column: their average over time.
                Some(p) if key == bin => {
                    let n = secs + r.secs;
                    for (a, b) in p.v.iter_mut().zip(r.v) {
                        *a = (*a * secs + b * r.secs) / n;
                    }
                    secs = n;
                }
                _ => {
                    bin = key;
                    secs = r.secs;
                    self.points.push(Pt {
                        x,
                        age_ms,
                        end_ms: r.end_ms,
                        v: r.v,
                    });
                }
            }
            // The first stretch past the span carries the bands to the left edge.
            if age_ms >= self.time.span_ms {
                break;
            }
        }
    }

    /// Keep the value scale until the chart outgrows it or shrinks well below it.
    fn rescale(&mut self) {
        let max = self.points.iter().map(Pt::sum).fold(0.0, f32::max);
        if self.scale <= 0.0 || max > self.scale || max < self.scale * 0.3 {
            self.scale = nice(max * 1.1);
        }
    }

    /// The legend's rows, top to bottom as the bands are stacked.
    fn lay_out_legend(&mut self) {
        let mut y = self.legend.y + LEGEND_ROW_H;
        let mut rows = [Rect::ZERO; BANDS];
        if self.legend.is_empty() {
            self.rows = rows;
            return;
        }
        for b in (0..BANDS).rev() {
            if self.shown(b) && y + LEGEND_ROW_H <= self.legend.bottom() {
                rows[b] = Rect::new(self.legend.x, y, self.legend.w, LEGEND_ROW_H);
                y += LEGEND_ROW_H;
            }
        }
        self.rows = rows;
    }

    /// The age of the plotted column nearest the pointer, while it is over the plot.
    fn pointed_age(&self) -> Option<f32> {
        let p = self
            .pointer
            .filter(|p| self.plot.contains(*p) && !self.over_legend(*p))?;
        self.nearest(p.x).map(|i| self.points[i].age_ms)
    }

    /// Whether `p` is on a legend floating over the plot, which hides what is under
    /// it from the pointer.
    fn over_legend(&self, p: Point) -> bool {
        self.style == ChartLegend::Floating && self.legend.contains(p)
    }

    /// The age this chart marks on its own account, for charts that share its axis
    /// to mark too.
    #[must_use]
    pub fn hover_age(&self) -> Option<f32> {
        self.pointed_age()
    }

    /// The column nearest `x` of those inside the plot.
    fn nearest(&self, x: f32) -> Option<usize> {
        let inside = self.plot.x..=self.plot.right();
        (0..self.points.len())
            .filter(|&i| inside.contains(&self.points[i].x))
            .min_by(|&a, &b| {
                (self.points[a].x - x)
                    .abs()
                    .total_cmp(&(self.points[b].x - x).abs())
            })
    }

    /// The column at an age, if the history reaches back that far.
    fn at_age(&self, age_ms: f32) -> Option<&Pt> {
        let oldest = self.points.last()?;
        let x = self.time.x(self.plot, age_ms);
        (x >= oldest.x - COLUMN_W)
            .then(|| self.nearest(x))
            .flatten()
            .map(|i| &self.points[i])
    }

    fn y(&self, v: f32) -> f32 {
        let f = if self.scale > 0.0 {
            (v / self.scale).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.plot.bottom() - f * self.plot.h
    }

    /// The band under `p`: in the plot, the one drawn there; in the legend, the
    /// row's.
    fn band_at(&self, p: Point) -> Option<usize> {
        if let Some(b) = self.rows.iter().position(|r| r.contains(p)) {
            return Some(b);
        }
        if !self.plot.contains(p) || self.over_legend(p) || self.scale <= 0.0 {
            return None;
        }
        // Between the two columns around the pointer, as the bands are drawn.
        let newer = self.points.iter().rposition(|q| q.x >= p.x)?;
        let near = &self.points[newer];
        let older = self.points.get(newer + 1);
        // Left of the oldest column there is nothing drawn.
        if older.is_none() && p.x < near.x - COLUMN_W {
            return None;
        }
        let toward_older = match older {
            Some(far) if near.x > far.x => (near.x - p.x) / (near.x - far.x),
            _ => 0.0,
        };
        let far = older.unwrap_or(near);
        let value = (self.plot.bottom() - p.y) / self.plot.h * self.scale;
        let mut top = 0.0;
        for band in 0..BANDS {
            top += near.v[band] + (far.v[band] - near.v[band]) * toward_older;
            if value < top {
                return Some(band);
            }
        }
        None
    }

    /// The program whose band, or legend row, is under `p`.
    #[must_use]
    pub fn program_at(&self, p: Point) -> Option<ProgramId> {
        self.band_at(p)
            .and_then(|b| self.slots.get(b).copied().flatten())
    }

    /// Follow the pointer, `None` when it left. Returns whether to paint again.
    pub fn set_pointer(&mut self, at: Option<Point>) -> bool {
        let inside = |p: Option<Point>| p.is_some_and(|p| self.area.contains(p));
        let repaint = inside(at) || inside(self.pointer);
        self.pointer = at;
        repaint
    }

    /// A left click at `p`.
    pub fn click(&mut self, p: Point) -> Option<ChartHit> {
        if let Some(i) = self.switch.iter().position(|r| r.contains(p)) {
            if self.mode != MODES[i].0 {
                self.mode = MODES[i].0;
                // The two readings are in different units.
                self.scale = 0.0;
            }
            return Some(ChartHit::Mode);
        }
        self.program_at(p).map(ChartHit::Program)
    }

    fn value(&self, out: &mut String, v: f32) {
        format::cycles(out, f64::from(v));
        if self.mode == ChartMode::Rate {
            out.push_str("/s");
        }
    }

    /// Draw the chart as last built. `needle` is the search, lower-cased: programs
    /// it does not match are dimmed. `selected` is the selected process's program.
    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        usage: &Usage,
        needle: &str,
        selected: Option<ProgramId>,
        theme: &Theme,
        buf: &mut String,
    ) {
        self.paint_head(dl, theme, buf);
        self.paint_grid(dl, theme, buf);
        let mut dim = [false; BANDS];
        for (b, dim) in dim.iter_mut().enumerate() {
            let unmatched = !needle.is_empty()
                && self
                    .slots
                    .get(b)
                    .copied()
                    .flatten()
                    .is_none_or(|g| !contains_ci(usage.program_name(g), needle));
            *dim = unmatched || self.hover_band.is_some_and(|h| h != b);
        }
        self.paint_bands(dl, dim, theme);
        if self.style == ChartLegend::Inline {
            self.place_labels(usage, theme.small.size);
            self.paint_labels(dl, usage, selected, dim, theme);
        }
        let marked = self.paint_mark(dl, theme, buf);
        match self.style {
            ChartLegend::Inline => self.paint_chip(dl, usage, marked, theme, buf),
            ChartLegend::Floating if !self.legend.is_empty() => {
                // A backing that keeps the names legible over the bands, and
                // lets a little of them show through.
                dl.fill_round_rect(
                    self.legend,
                    theme.card_radius,
                    theme.bg_solid.with_alpha(0.88),
                );
                dl.stroke_round_rect(self.legend, theme.card_radius, theme.surface_border, 1.0);
                self.paint_legend(dl, usage, marked, selected, dim, theme, buf);
            }
            ChartLegend::Floating => {}
            ChartLegend::Beside => self.paint_legend(dl, usage, marked, selected, dim, theme, buf),
        }
    }

    /// Each band's name inside it, where it was placed; the selected program's
    /// outlined, as its legend row would be.
    fn paint_labels(
        &self,
        dl: &mut DisplayList,
        usage: &Usage,
        selected: Option<ProgramId>,
        dim: [bool; BANDS],
        theme: &Theme,
    ) {
        dl.push_clip(self.plot);
        for (b, label) in self.labels.iter().enumerate() {
            let Some(label) = label else {
                continue;
            };
            let program = self.slots.get(b).copied().flatten();
            if program.is_some() && program == selected {
                dl.stroke_round_rect(label.rect.inset(0.5, 0.5), 3.0, theme.accent, 1.0);
            }
            let ink = ink_on(band_color(theme, b));
            let ink = if dim[b] { ink.with_alpha(0.45) } else { ink };
            dl.text(
                self.band_name(usage, b),
                label.rect.inset(LABEL_PAD, 0.0),
                theme.small,
                ink,
                HAlign::Left,
                VAlign::Middle,
                true,
            );
        }
        dl.pop_clip();
    }

    /// Beside the pointer, the band under it: its color, name and value at the
    /// marked moment, the readout a legend row would give.
    fn paint_chip(
        &self,
        dl: &mut DisplayList,
        usage: &Usage,
        marked: Option<Pt>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (Some(band), Some(column), Some(pointer)) = (self.hover_band, marked, self.pointer)
        else {
            return;
        };
        // The value first: formatting it starts the buffer afresh.
        self.value(buf, column.v[band]);
        buf.insert_str(0, "  ");
        buf.insert_str(0, self.band_name(usage, band));
        let plot = self.plot;
        let width = (label_w(buf, theme.small.size) + SWATCH + 6.0).min(plot.w);
        let height = LEGEND_ROW_H;
        // Below right of the pointer, or wherever that keeps it in the plot.
        let mut left = pointer.x + CHIP_OFFSET;
        if left + width > plot.right() {
            left = pointer.x - CHIP_OFFSET - width;
        }
        let mut top = pointer.y + CHIP_OFFSET;
        if top + height > plot.bottom() {
            top = pointer.y - CHIP_OFFSET - height;
        }
        let left = left.clamp(plot.x, (plot.right() - width).max(plot.x));
        let top = top.clamp(plot.y, (plot.bottom() - height).max(plot.y));
        let chip = Rect::new(left, top, width, height);
        dl.fill_round_rect(chip, theme.card_radius, theme.bg_solid.with_alpha(0.92));
        dl.stroke_round_rect(chip, theme.card_radius, theme.surface_border, 1.0);
        let inner = chip.inset(LABEL_PAD, 0.0);
        let swatch = Rect::new(inner.x, inner.center().y - SWATCH * 0.5, SWATCH, SWATCH);
        dl.fill_round_rect(swatch, 2.0, band_color(theme, band));
        let (_, text) = inner.split_left(SWATCH + 6.0);
        dl.label(buf, text, theme.small, theme.text);
    }

    /// The plot's background: the value grid at quarters, with the scale beside the
    /// top and the middle, and the time grid at the labeled ages.
    fn paint_grid(&self, dl: &mut DisplayList, theme: &Theme, buf: &mut String) {
        let plot = self.plot;
        dl.fill_rect(plot, theme.surface);
        for q in 1..4 {
            let y = plot.bottom() - plot.h * (q as f32 / 4.0);
            dl.fill_rect(Rect::new(plot.x, y.floor(), plot.w, 1.0), theme.grid);
        }
        for (age, _) in TimeAxis::TICKS {
            if age > 0.0 && age < self.time.span_ms {
                let x = self.time.x(plot, age).floor();
                dl.fill_rect(Rect::new(x, plot.y, 1.0, plot.h), theme.grid);
            }
        }
        for (f, y) in [(1.0, plot.y), (0.5, plot.y + plot.h * 0.5 - 7.0)] {
            self.value(buf, self.scale * f);
            dl.text(
                buf,
                Rect::new(plot.x - GUTTER_W, y, GUTTER_W - 6.0, 14.0),
                theme.small,
                theme.text_dim,
                HAlign::Right,
                VAlign::Middle,
                false,
            );
        }
    }

    /// The bands, stacked from the first up, "everything else" on top.
    fn paint_bands(&mut self, dl: &mut DisplayList, dim: [bool; BANDS], theme: &Theme) {
        let plot = self.plot;
        if self.points.len() < 2 {
            dl.text(
                "The history starts with the next sample",
                plot,
                theme.cell,
                theme.text_dim,
                HAlign::Center,
                VAlign::Middle,
                true,
            );
            return;
        }
        dl.push_clip(plot);
        let mut cum = std::mem::take(&mut self.cum);
        let mut poly = std::mem::take(&mut self.poly);
        cum.clear();
        cum.resize(self.points.len(), 0.0);
        for (b, &dim) in dim.iter().enumerate() {
            if self.points.iter().all(|p| p.v[b] <= 0.0) {
                continue;
            }
            // The band's lower edge, then its upper, each oldest (leftmost) first;
            // the points are newest first.
            poly.clear();
            poly.extend(
                self.points
                    .iter()
                    .zip(&cum)
                    .rev()
                    .map(|(p, &c)| Point::new(p.x, self.y(c))),
            );
            for (p, c) in self.points.iter().zip(&mut cum) {
                *c += p.v[b];
            }
            let upper = poly.len();
            poly.extend(
                self.points
                    .iter()
                    .zip(&cum)
                    .rev()
                    .map(|(p, &c)| Point::new(p.x, self.y(c))),
            );
            let alpha = if dim { DIM_ALPHA } else { 0.9 };
            let (lower, top) = poly.split_at(upper);
            dl.band(
                top.iter().copied(),
                lower.iter().copied(),
                band_color(theme, b).with_alpha(alpha),
            );
            // A hairline of background along the top keeps neighbours apart.
            dl.graph(top.iter().copied(), theme.bg_solid, 1.0);
        }
        self.cum = cum;
        self.poly = poly;
        dl.pop_clip();
    }

    /// Under the plot: the time labels, or while a moment is marked, the line
    /// through the plot and its readout. Returns the column at the marked moment,
    /// if the history reaches back that far.
    fn paint_mark(&mut self, dl: &mut DisplayList, theme: &Theme, buf: &mut String) -> Option<Pt> {
        let plot = self.plot;
        let Some(c) = self.crosshair else {
            sparkline::paint_axis(dl, plot, self.axis, &self.time, theme.small, theme.text_dim);
            return None;
        };
        let x = self.time.x(plot, c.age_ms);
        dl.fill_rect(Rect::new(x.floor(), plot.y, 1.0, plot.h), theme.crosshair);
        let marked = self.at_age(c.age_ms).copied();
        let mut ago = std::mem::take(&mut self.ago);
        let mut text = std::mem::take(&mut self.text);
        c.ago(&mut ago);
        buf.clear();
        if let Some(p) = marked {
            self.value(buf, p.sum());
        }
        charts::readout(&mut text, buf, &ago, c.side);
        sparkline::paint_readout(dl, self.axis, x, c.side, &text, theme.small, theme.text);
        self.ago = ago;
        self.text = text;
        marked
    }

    /// The switch, and a line on what the chart shows.
    fn paint_head(&self, dl: &mut DisplayList, theme: &Theme, buf: &mut String) {
        let group = Rect::new(
            self.switch[0].x,
            self.switch[0].y,
            SWITCH_W * MODES.len() as f32,
            self.switch[0].h,
        );
        dl.fill_round_rect(group, theme.card_radius, theme.surface);
        dl.stroke_rect(group, theme.surface_border, 1.0);
        for (i, (mode, label)) in MODES.iter().enumerate() {
            let r = self.switch[i];
            let active = *mode == self.mode;
            let fill = if active {
                Some(theme.button_active)
            } else if self.hover_mode == Some(i) {
                Some(theme.button_hover)
            } else {
                None
            };
            if let Some(c) = fill {
                dl.fill_round_rect(r.inset(2.0, 2.0), theme.card_radius - 1.0, c);
            }
            let ink = if active { theme.text } else { theme.text_dim };
            dl.text(
                label,
                r,
                theme.header,
                ink,
                HAlign::Center,
                VAlign::Middle,
                false,
            );
        }
        buf.clear();
        match self.mode {
            ChartMode::Total => {
                use std::fmt::Write as _;
                let _ = write!(
                    buf,
                    "Cycles each program has used, fading {:.0}% a second (halving in {:.0} s). \
                     Processes of one name are one band.",
                    self.decay * 100.0,
                    ot_core::usage::half_life(self.decay)
                );
            }
            ChartMode::Rate => buf.push_str(
                "Cycles each program was using a second. Processes of one name are one band.",
            ),
        }
        let caption = Rect::new(
            group.right() + theme.pad * 1.5,
            self.switch[0].y,
            (self.area.right() - group.right() - theme.pad * 1.5).max(0.0),
            self.switch[0].h,
        );
        dl.label(buf, caption, theme.small, theme.text_dim);
    }

    /// The legend: the moment it reads and the sum of the bands there, then a row
    /// per band.
    #[allow(clippy::too_many_arguments)]
    fn paint_legend(
        &self,
        dl: &mut DisplayList,
        usage: &Usage,
        marked: Option<Pt>,
        selected: Option<ProgramId>,
        dim: [bool; BANDS],
        theme: &Theme,
        buf: &mut String,
    ) {
        let legend = self.legend;
        if legend.is_empty() {
            return;
        }
        // With a moment marked beyond the history, there are no values to give.
        let point = match (self.crosshair, marked) {
            (Some(_), marked) => marked,
            (None, _) => self.points.first().copied(),
        };
        let head = Rect::new(legend.x, legend.y, legend.w, LEGEND_ROW_H).inset(6.0, 0.0);
        let (when, sum) = head.split_left((head.w - LEGEND_VALUE_W).max(0.0));
        match self.crosshair {
            Some(_) => dl.label(&self.ago, when, theme.small, theme.text_dim),
            None => dl.label("now", when, theme.small, theme.text_dim),
        }
        buf.clear();
        if let Some(p) = point {
            self.value(buf, p.sum());
        }
        dl.text(
            buf,
            sum,
            theme.small,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            false,
        );

        for (b, &row) in self.rows.iter().enumerate() {
            if row.is_empty() {
                continue;
            }
            let program = self.slots.get(b).copied().flatten();
            if self.hover_band == Some(b) {
                dl.fill_round_rect(row, theme.card_radius - 1.0, theme.button_hover);
            }
            if program.is_some() && program == selected {
                dl.stroke_round_rect(
                    row.inset(0.5, 0.5),
                    theme.card_radius - 1.0,
                    theme.accent,
                    1.0,
                );
            }
            let inner = row.inset(6.0, 0.0);
            let swatch = Rect::new(inner.x, inner.center().y - SWATCH * 0.5, SWATCH, SWATCH);
            let faded = dim[b];
            dl.fill_round_rect(
                swatch,
                2.0,
                band_color(theme, b).with_alpha(if faded { DIM_ALPHA } else { 1.0 }),
            );
            let (_, text) = inner.split_left(SWATCH + 6.0);
            let (name, value) = text.split_left((text.w - LEGEND_VALUE_W).max(0.0));
            let ink = if faded { theme.text_dim } else { theme.text };
            dl.label(self.band_name(usage, b), name, theme.small, ink);
            buf.clear();
            if let Some(p) = point {
                self.value(buf, p.v[b]);
            }
            dl.text(
                buf,
                value,
                theme.small,
                ink,
                HAlign::Right,
                VAlign::Middle,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_rows::tests::proc;
    use ot_core::Snapshot;
    use ot_model::process::ProcessStatic;
    use ot_model::Tick;
    use ot_paint::DrawCmd;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    const G: u64 = 1_000_000_000;
    const RECT: Rect = Rect::new(0.0, 0.0, 900.0, 400.0);

    /// A session of `secs` seconds sampled once a second, where `rate(name, s)` is
    /// the G cycles program `name` uses in second `s`.
    fn session(names: &[&str], secs: u64, rate: impl Fn(usize, u64) -> u64) -> Usage {
        let mut u = Usage::new(0.05);
        let mut used = vec![0u64; names.len()];
        for s in 0..=secs {
            let procs = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    if s > 0 {
                        used[i] += rate(i, s) * G;
                    }
                    let mut p = proc(i as u32 + 1, None, 0.0);
                    p.statics = Arc::new(ProcessStatic {
                        name: (*name).to_owned(),
                        ..(*p.statics).clone()
                    });
                    p.cycles = 100 * G + used[i];
                    p
                })
                .collect();
            u.observe(&Snapshot {
                tick: Tick(s + 1),
                taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + s)),
                interval: Duration::from_secs(1),
                processes: procs,
                ..Default::default()
            });
        }
        u
    }

    fn painted(c: &mut UsageChart, u: &Usage, needle: &str) -> DisplayList {
        let mut dl = DisplayList::new();
        c.build(RECT, u, None, TimeAxis::DEFAULT);
        c.paint(&mut dl, u, needle, None, &Theme::dark(), &mut String::new());
        assert_eq!(dl.clip_depth(), 0);
        dl
    }

    fn texts(dl: &DisplayList) -> Vec<String> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    fn band(c: &UsageChart, u: &Usage, name: &str) -> usize {
        let g = u.program(name).unwrap();
        c.slots.iter().position(|s| *s == Some(g)).unwrap()
    }

    #[test]
    fn nice_scales_step_through_one_two_five() {
        for (v, want) in [(0.0, 1.0), (0.7, 1.0), (1.2, 2.0), (4.1, 5.0), (5.5, 10.0)] {
            assert!((nice(v) - want).abs() < 1e-6, "{v}");
        }
        assert!((nice(3.0e10) / 5.0e10 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn the_rate_reading_is_cycles_a_second_per_program() {
        let u = session(&["a.exe", "b.exe"], 30, |i, _| if i == 0 { 3 } else { 1 });
        let mut c = UsageChart::default();
        c.set_legend(ChartLegend::Beside);
        assert_eq!(c.click(Point::new(0.0, 0.0)), None, "nothing built yet");
        let _ = painted(&mut c, &u, "");
        let rate = c.switch[1].center();
        assert_eq!(c.click(rate), Some(ChartHit::Mode));
        assert_eq!(c.mode(), ChartMode::Rate);
        let dl = painted(&mut c, &u, "");
        let (a, b) = (band(&c, &u, "a.exe"), band(&c, &u, "b.exe"));
        assert_eq!(c.points.len(), 30);
        for p in &c.points {
            assert!((p.v[a] / (3.0 * G as f32) - 1.0).abs() < 1e-4);
            assert!((p.v[b] / G as f32 - 1.0).abs() < 1e-4);
        }
        assert!(
            (c.points[0].x - c.plot.right()).abs() < 1e-3,
            "now on the right"
        );
        // 4 G a second at most: a scale of 5 G, labeled at the top and the middle.
        assert!((c.scale / 5.0e9 - 1.0).abs() < 1e-5);
        let strings = texts(&dl);
        for want in [
            "5.00 G/s", "2.50 G/s", "a.exe", "b.exe", "3.00 G/s", "4.00 G/s", "now",
        ] {
            assert!(strings.iter().any(|s| s == want), "{want}: {strings:?}");
        }
        assert!(!strings.iter().any(|s| s == "Everything else"));
    }

    #[test]
    fn the_fading_total_reading_matches_the_totals() {
        // a works throughout; b worked for ten seconds and stopped a minute ago.
        let u = session(&["a.exe", "b.exe"], 120, |i, s| match i {
            0 => 2,
            _ if (50..60).contains(&s) => 6,
            _ => 0,
        });
        let mut c = UsageChart::default();
        let dl = painted(&mut c, &u, "");
        assert_eq!(c.mode(), ChartMode::Total);
        let (a, b) = (band(&c, &u, "a.exe"), band(&c, &u, "b.exe"));
        // The newest column is what the table's Cycles column says now.
        let used = |pid: u32| u.used(ot_model::ProcessKey::new(pid, 1)).unwrap() as f32;
        let now = c.points[0];
        assert!(
            (now.v[a] / used(1) - 1.0).abs() < 1e-3,
            "{} {}",
            now.v[a],
            used(1)
        );
        assert!((now.v[b] / used(2) - 1.0).abs() < 1e-3);
        // b's band was at its thickest when it stopped, and has sagged since.
        let peak = c.points.iter().map(|p| p.v[b]).fold(0.0, f32::max);
        let at_stop = c.at_age(61_000.0).unwrap().v[b];
        assert!((peak - at_stop).abs() < peak * 1e-3);
        assert!(now.v[b] < peak * 0.06);
        assert!(texts(&dl).iter().any(|s| s
            .starts_with("Cycles each program has used, fading 5% a second (halving in 14 s).")));
    }

    #[test]
    fn programs_past_the_eighth_are_everything_else_and_colors_stay_put() {
        let names: Vec<String> = (0..12).map(|i| format!("p{i}.exe")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        // Program i uses 12 - i G a second: p0 the most.
        let u = session(&names, 40, |i, _| 12 - i as u64);
        let mut c = UsageChart::default();
        let dl = painted(&mut c, &u, "");
        assert!(c.slots.iter().all(Option::is_some));
        for name in &names[..8] {
            let _ = band(&c, &u, name);
        }
        assert!(c.points[0].v[OTHER] > 0.0);
        assert!(texts(&dl).iter().any(|s| s == "Everything else"));
        let before = c.slots;

        // p9 pulls just ahead of p7: not enough to take its band.
        let u = session(&names, 40, |i, _| if i == 9 { 6 } else { 12 - i as u64 });
        let _ = painted(&mut c, &u, "");
        assert_eq!(c.slots, before, "a near tie changes nothing");
        // Well ahead, it takes the band of the smallest, and nothing else moves.
        let u = session(&names, 40, |i, _| if i == 9 { 30 } else { 12 - i as u64 });
        let _ = painted(&mut c, &u, "");
        let moved: Vec<usize> = (0..SERIES).filter(|&i| c.slots[i] != before[i]).collect();
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(c.slots[moved[0]], u.program("p9.exe"));
        assert_eq!(before[moved[0]], u.program("p7.exe"));
    }

    #[test]
    #[allow(clippy::many_single_char_names)] // two programs, a chart, a point
    fn the_pointer_marks_a_moment_and_picks_a_band() {
        let u = session(&["a.exe", "b.exe"], 60, |i, _| if i == 0 { 3 } else { 1 });
        let mut c = UsageChart::default();
        c.set_legend(ChartLegend::Beside);
        let _ = painted(&mut c, &u, "");
        let (a, b) = (band(&c, &u, "a.exe"), band(&c, &u, "b.exe"));
        // Ten seconds ago, a third of the way up a's band.
        let x = c.time.x(c.plot, 10_000.0);
        let at = c.at_age(10_000.0).copied().unwrap();
        let lower = if a < b { 0.0 } else { at.v[b] };
        let y = c.y(lower + at.v[a] / 3.0);
        assert!(c.set_pointer(Some(Point::new(x, y))));
        let dl = painted(&mut c, &u, "");
        assert_eq!(c.hover_age(), Some(10_000.0));
        assert_eq!(c.hover_band, Some(a));
        assert_eq!(c.program_at(Point::new(x, y)), u.program("a.exe"));
        assert_eq!(
            c.click(Point::new(x, y)),
            u.program("a.exe").map(ChartHit::Program)
        );
        let strings = texts(&dl);
        assert!(strings.iter().any(|s| s == "10s ago"), "{strings:?}");
        assert!(!strings.iter().any(|s| s == "now"));
        // Above the stack there is no band; the moment is still marked.
        assert!(c.set_pointer(Some(Point::new(x, c.plot.y + 1.0))));
        let _ = painted(&mut c, &u, "");
        assert_eq!(c.hover_band, None);
        assert_eq!(c.hover_age(), Some(10_000.0));
        // A legend row stands for its band.
        let row = c.rows[b].center();
        assert_eq!(c.program_at(row), u.program("b.exe"));
        // Off the chart: nothing marked, unless another chart marks a moment.
        assert!(c.set_pointer(None));
        assert!(!c.set_pointer(None));
        c.build(RECT, &u, Some(30_000.0), TimeAxis::DEFAULT);
        assert_eq!(c.hover_age(), None);
        assert_eq!(c.crosshair.map(|c| c.age_ms), Some(30_000.0));
    }

    #[test]
    fn a_search_dims_the_programs_it_does_not_match() {
        let u = session(&["a.exe", "b.exe"], 30, |_, _| 1);
        let mut c = UsageChart::default();
        let dl = painted(&mut c, &u, "b.ex");
        let theme = Theme::dark();
        let fills: Vec<ot_paint::Color> = dl
            .cmds()
            .iter()
            .filter_map(|cmd| match *cmd {
                DrawCmd::Band { color, .. } => Some(color),
                _ => None,
            })
            .collect();
        let (a, b) = (band(&c, &u, "a.exe"), band(&c, &u, "b.exe"));
        assert!(fills.contains(&theme.series[a].with_alpha(DIM_ALPHA)));
        assert!(fills.contains(&theme.series[b].with_alpha(0.9)));
    }

    /// The y range band `b` covers at column `i`, top first, on screen.
    fn band_span(c: &UsageChart, b: usize, i: usize) -> (f32, f32) {
        let below: f32 = c.points[i].v[..b].iter().sum();
        (c.y(below + c.points[i].v[b]), c.y(below))
    }

    #[test]
    fn names_in_the_bands_free_the_width_and_sit_inside_their_bands() {
        // a is thick throughout; b is thick for its first half, then a sliver.
        let u = session(&["a.exe", "b.exe", "c.exe"], 120, |i, s| match i {
            0 => 6,
            1 if s < 60 => 4,
            1 => 0,
            _ => 1,
        });
        assert_eq!(
            UsageChart::default().style,
            ChartLegend::Inline,
            "in the bands unless set"
        );
        let mut c = UsageChart {
            mode: ChartMode::Rate,
            ..Default::default()
        };
        let dl = painted(&mut c, &u, "");
        assert!(
            (c.plot.right() - RECT.right()).abs() < 1e-3,
            "the plot takes the width"
        );
        assert!(c.rows.iter().all(Rect::is_empty), "no legend rows");
        let strings = texts(&dl);
        for name in ["a.exe", "b.exe", "c.exe"] {
            assert!(strings.iter().any(|s| s == name), "{name}: {strings:?}");
        }
        for name in ["a.exe", "b.exe"] {
            let b = band(&c, &u, name);
            let l = c.labels[b].expect(name);
            assert!(c.plot.contains(Point::new(l.rect.x, l.rect.y)));
            assert!(l.rect.right() <= c.plot.right() + 1e-3);
            // Inside the band at every column under it.
            for (i, p) in c.points.iter().enumerate() {
                if p.x >= l.rect.x && p.x <= l.rect.right() {
                    let (top, bottom) = band_span(&c, b, i);
                    assert!(
                        l.rect.y >= top - 0.5 && l.rect.bottom() <= bottom + 0.5,
                        "{name}"
                    );
                }
            }
        }
        // b's name is where b was thick: over a minute ago.
        let b = band(&c, &u, "b.exe");
        let at = c.labels[b].unwrap().rect.right();
        assert!(c.time.age_at(c.plot, at) > 60_000.0);
    }

    #[test]
    fn a_band_too_thin_for_a_line_of_text_goes_unnamed() {
        let u = session(
            &["big.exe", "tiny.exe"],
            60,
            |i, _| if i == 0 { 100 } else { 1 },
        );
        let mut c = UsageChart {
            mode: ChartMode::Rate,
            ..Default::default()
        };
        let dl = painted(&mut c, &u, "");
        assert!(c.labels[band(&c, &u, "big.exe")].is_some());
        assert!(c.labels[band(&c, &u, "tiny.exe")].is_none());
        assert!(!texts(&dl).iter().any(|s| s == "tiny.exe"));
    }

    #[test]
    fn a_name_keeps_its_place_in_time_as_the_chart_scrolls() {
        let u = session(&["a.exe", "b.exe"], 120, |i, _| if i == 0 { 3 } else { 2 });
        let newest = u.now_ms();
        let mut c = UsageChart::default();
        let mut placed = Vec::new();
        for step in 0..8 {
            let axis = TimeAxis::at(Some(newest + step * 125), 120_000.0, 5_000.0);
            c.build(RECT, &u, None, axis);
            c.paint(
                &mut DisplayList::new(),
                &u,
                "",
                None,
                &Theme::dark(),
                &mut String::new(),
            );
            placed.push(c.labels[band(&c, &u, "a.exe")].unwrap());
        }
        assert!(placed.windows(2).all(|w| w[0].anchor_ms == w[1].anchor_ms));
        assert!(
            placed[7].rect.x < placed[0].rect.x,
            "it scrolls with the band"
        );
    }

    #[test]
    fn the_band_under_the_pointer_reads_out_its_name_and_value() {
        let u = session(&["a.exe", "b.exe"], 60, |i, _| if i == 0 { 3 } else { 1 });
        let mut c = UsageChart::default();
        let _ = painted(&mut c, &u, "");
        let a = band(&c, &u, "a.exe");
        let x = c.time.x(c.plot, 10_000.0);
        let (top, bottom) = band_span(&c, a, c.nearest(x).unwrap());
        assert!(c.set_pointer(Some(Point::new(x, f32::midpoint(top, bottom)))));
        let dl = painted(&mut c, &u, "");
        assert_eq!(c.hover_band, Some(a));
        let strings = texts(&dl);
        assert!(
            strings
                .iter()
                .any(|s| s.starts_with("a.exe  ") && s.ends_with(" G")),
            "{strings:?}"
        );
    }

    #[test]
    fn a_floating_legend_sits_over_the_freer_corner_and_out_of_the_marks_way() {
        // The bands rise high on the old (left) side and stay low on the new.
        let u = session(&["a.exe", "b.exe"], 300, |i, s| match (i, s) {
            (0, s) if s < 150 => 8,
            _ => 1,
        });
        let mut c = UsageChart {
            mode: ChartMode::Rate,
            style: ChartLegend::Floating,
            ..Default::default()
        };
        // The axis spans the session, as the app's does.
        let axis = TimeAxis::at(None, 300_000.0, 5_000.0);
        let paint = |c: &mut UsageChart, outside: Option<f32>| {
            let mut dl = DisplayList::new();
            c.build(RECT, &u, outside, axis);
            c.paint(&mut dl, &u, "", None, &Theme::dark(), &mut String::new());
            dl
        };
        let dl = paint(&mut c, None);
        assert!(
            (c.plot.right() - RECT.right()).abs() < 1e-3,
            "the plot takes the width"
        );
        assert!(c.plot.contains(c.legend.center()), "over the plot");
        assert!(c.float_right, "over the corner the bands leave free");
        assert!(texts(&dl).iter().any(|s| s == "a.exe"));
        // Its rows still pick their program; the plot under it is hidden.
        let b = band(&c, &u, "b.exe");
        assert_eq!(c.program_at(c.rows[b].center()), u.program("b.exe"));
        let below = Point::new(c.legend.center().x, c.legend.bottom() - 2.0);
        assert!(c.over_legend(below));
        assert!(c.set_pointer(Some(below)));
        let _ = paint(&mut c, None);
        assert_eq!(c.hover_age(), None, "the legend is not the plot");

        // Another chart marks a moment under it: it moves to the other corner.
        assert!(c.set_pointer(None));
        let age = c.time.age_at(c.plot, c.legend.center().x);
        let _ = paint(&mut c, Some(age));
        assert!(!c.float_right);
        assert!(!c
            .legend
            .contains(Point::new(c.time.x(c.plot, age), c.legend.center().y)));
    }

    #[test]
    fn a_session_with_one_sample_says_the_history_is_coming() {
        let u = session(&["a.exe"], 0, |_, _| 1);
        let mut c = UsageChart::default();
        let dl = painted(&mut c, &u, "");
        assert!(c.points.is_empty());
        assert!(texts(&dl)
            .iter()
            .any(|s| s == "The history starts with the next sample"));
    }
}
