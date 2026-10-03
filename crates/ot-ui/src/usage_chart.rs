//! History: the process table's area drawn as a chart of who has been using the
//! CPU over the last hour, the fourth arrangement beside List, Tree and Map.
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
//! The legend beside it names the bands, top to bottom as they are stacked, with
//! each one's value now, or at the moment under the pointer.

use ot_core::usage::fade;
use ot_core::{ProgramId, Usage};
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

use crate::charts::{self, Crosshair, AXIS_BAND_H};
use crate::format;
use crate::process_rows::contains_ci;
use crate::sparkline::{self, TimeAxis};
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
/// Width of one plotted column, as in [`sparkline`].
const COLUMN_W: f32 = 1.0;
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
    /// Age of the stretch's end, and its length in seconds.
    age_ms: f32,
    secs: f32,
    v: [f32; BANDS],
}

/// One plotted column: each band's thickness there, in the chart's units.
#[derive(Debug, Clone, Copy)]
struct Pt {
    x: f32,
    age_ms: f32,
    v: [f32; BANDS],
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
    legend: Rect,
    switch: [Rect; 2],
    /// Each band's legend row; empty for a band that has none.
    rows: [Rect; BANDS],
    decay: f64,
    /// Scratch, kept so steady-state painting does not allocate.
    weight: Vec<f32>,
    order: Vec<ProgramId>,
    band_of: Vec<u8>,
    history: Vec<Row>,
    poly: Vec<Point>,
    cum: Vec<f32>,
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
        let legend_w = (rect.w * 0.28).clamp(150.0, 240.0).min(body.w * 0.5);
        let (main, legend) = body.split_left((body.w - legend_w - 8.0).max(0.0));
        self.legend = legend.split_left(8.0).1;
        let (axis, plot) = main.split_bottom(AXIS_BAND_H);
        self.plot = plot.split_left(GUTTER_W.min(plot.w)).1;
        self.axis = axis.split_left(GUTTER_W.min(axis.w)).1;

        self.assign_bands(usage);
        self.place(usage);
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
        self.lay_out_legend();
        self.hover_band = self.pointer.and_then(|p| self.band_at(p));
    }

    /// Decide which programs get a band: those that take the most of the chart,
    /// with the ones that already have a band staying unless clearly overtaken.
    fn assign_bands(&mut self, usage: &Usage) {
        let now = usage.now_ms();
        let plot = self.plot;
        self.weight.clear();
        self.weight.resize(usage.programs(), 0.0);
        for f in usage.frames() {
            let age = (now - f.end_ms).max(0) as f32;
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

    /// Sort the history into bands, turn it into the mode's values, and place it
    /// in columns across the plot.
    fn place(&mut self, usage: &Usage) {
        let now = usage.now_ms();
        self.history.clear();
        for f in usage.frames() {
            if f.span_ms <= 0 {
                continue;
            }
            let age_ms = (now - f.end_ms).max(0) as f32;
            let mut v = [0.0; BANDS];
            for &(g, cycles) in f.cycles {
                let band = self
                    .band_of
                    .get(g as usize)
                    .map_or(OTHER, |&b| usize::from(b));
                v[band] += cycles;
            }
            self.history.push(Row {
                age_ms,
                secs: f.span_ms as f32 / 1000.0,
                v,
            });
            // The first stretch past the span carries the bands to the left edge.
            if age_ms >= self.time.span_ms {
                break;
            }
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

        self.points.clear();
        if self.plot.is_empty() {
            return;
        }
        let mut column = i64::MIN;
        let mut secs = 0.0;
        for r in &self.history {
            let x = self.time.x(self.plot, r.age_ms);
            let c = ((self.plot.right() - x) / COLUMN_W).floor() as i64;
            match self.points.last_mut() {
                // Several stretches in one column: their average over time.
                Some(p) if c == column => {
                    let n = secs + r.secs;
                    for (a, b) in p.v.iter_mut().zip(r.v) {
                        *a = (*a * secs + b * r.secs) / n;
                    }
                    secs = n;
                }
                _ => {
                    column = c;
                    secs = r.secs;
                    self.points.push(Pt {
                        x,
                        age_ms: r.age_ms,
                        v: r.v,
                    });
                }
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
        let shown = |b: usize| -> bool {
            if b == OTHER {
                self.points.iter().any(|p| p.v[OTHER] > 0.0)
            } else {
                self.slots[b].is_some()
            }
        };
        let mut y = self.legend.y + LEGEND_ROW_H;
        let mut rows = [Rect::ZERO; BANDS];
        for b in (0..BANDS).rev() {
            if shown(b) && y + LEGEND_ROW_H <= self.legend.bottom() {
                rows[b] = Rect::new(self.legend.x, y, self.legend.w, LEGEND_ROW_H);
                y += LEGEND_ROW_H;
            }
        }
        self.rows = rows;
    }

    /// The age of the plotted column nearest the pointer, while it is over the plot.
    fn pointed_age(&self) -> Option<f32> {
        let p = self.pointer.filter(|p| self.plot.contains(*p))?;
        self.nearest(p.x).map(|i| self.points[i].age_ms)
    }

    /// The age this chart marks on its own account, for charts that share its axis
    /// to mark too.
    #[must_use]
    pub fn hover_age(&self) -> Option<f32> {
        self.pointed_age()
    }

    fn nearest(&self, x: f32) -> Option<usize> {
        (0..self.points.len()).min_by(|&a, &b| {
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
        if !self.plot.contains(p) || self.scale <= 0.0 {
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
        let marked = self.paint_mark(dl, theme, buf);
        self.paint_legend(dl, usage, marked, selected, dim, theme, buf);
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
            // Along the band's lower edge oldest to newest, then back along its
            // upper edge.
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
                    .map(|(p, &c)| Point::new(p.x, self.y(c))),
            );
            let alpha = if dim { DIM_ALPHA } else { 0.9 };
            dl.fill_polygon(poly.iter().copied(), band_color(theme, b).with_alpha(alpha));
            // A hairline of background along the top keeps neighbours apart.
            dl.polyline(poly[upper..].iter().copied(), theme.bg_solid, 1.0);
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
            let label = program.map_or("Everything else", |g| usage.program_name(g));
            dl.label(label, name, theme.small, ink);
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
                DrawCmd::FillPolygon { color, .. } => Some(color),
                _ => None,
            })
            .collect();
        let (a, b) = (band(&c, &u, "a.exe"), band(&c, &u, "b.exe"));
        assert!(fills.contains(&theme.series[a].with_alpha(DIM_ALPHA)));
        assert!(fills.contains(&theme.series[b].with_alpha(0.9)));
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
