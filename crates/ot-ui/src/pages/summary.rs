//! The Summary page: the headline number of every other page on one screen, as
//! TMOG's Summary view. The CPU, memory, the disks, the network, the GPUs and the
//! battery each get a card with their numbers and, where one tells the story, a
//! mini chart of the history held; then the busiest processes, right now and by
//! the fading cycle totals; and who is signed in, what runs, and for how long.
//!
//! The cards sit in a grid that reflows with the width: two columns when wide,
//! one when narrow, each card going under whichever column is shorter. Every chart
//! shares one hover line through [`ChartGroup`]. A frame allocates nothing beyond
//! the caller's `buf`: the card plan, the chart slots, the fact lines and the
//! process rows live in vectors and strings the page keeps between frames.

use std::fmt::Write as _;
use std::ops::Range;
use std::time::UNIX_EPOCH;

use ot_core::{Retention, Series, Snapshot, Timeline};
use ot_model::battery::BatteryState;
use ot_model::service::ServiceState;
use ot_model::system::SystemFacts;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

use crate::charts::{self, ChartGroup, ValueFmt};
use crate::format;
use crate::pages::PageOutcome;
use crate::perf;
use crate::sparkline::TimeAxis;
use crate::theme::Theme;
use crate::view::{MouseButton, Reaction, UiEvent};

/// Two columns of cards from this page width up.
const TWO_COLUMNS_AT: f32 = 760.0;
/// A card's title line.
const TITLE_H: f32 = 24.0;
/// A card's headline number, under the title.
const HEAD_H: f32 = 30.0;
/// One label-and-value line, and one process row.
const LINE_H: f32 = 19.0;
const LABEL_W: f32 = 124.0;
/// A mini chart takes this share of a card's width, up to the cap.
const CHART_SHARE: f32 = 0.38;
const CHART_MAX_W: f32 = 220.0;
const CHART_MIN_H: f32 = 64.0;
const CHART_MAX_H: f32 = 96.0;
/// Processes listed by CPU and by cycles.
const TOP_N: usize = 5;
/// Room for a process row's value.
const VALUE_W: f32 = 72.0;
/// The two process lists sit side by side from this text width up.
const TOP_SIDE_BY_SIDE_AT: f32 = 400.0;
/// Adapters at rest are left out when there are more than this many.
const QUIET_ADAPTERS_FROM: usize = 4;
/// How far a notch of the wheel scrolls the page.
const WHEEL_STEP: f32 = 48.0;

/// What the page is painted from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Inputs<'a> {
    pub snap: &'a Snapshot,
    pub timeline: &'a Timeline,
    /// The time axis of the frame, shared with every other chart.
    pub axis: TimeAxis,
    /// Each process's fading cycle total, parallel to the snapshot's process list.
    pub cycles: &'a [f64],
    /// The System page's facts, once read.
    pub facts: Option<&'a SystemFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardKind {
    Cpu,
    Memory,
    Disks,
    Network,
    Gpus,
    Battery,
    Top,
    System,
}

impl CardKind {
    /// In reading order, which is also the order the columns are filled in.
    const ALL: [Self; 8] = [
        Self::Cpu,
        Self::Memory,
        Self::Disks,
        Self::Network,
        Self::Gpus,
        Self::Battery,
        Self::Top,
        Self::System,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Disks => "Disks",
            Self::Network => "Network",
            Self::Gpus => "GPU",
            Self::Battery => "Battery",
            Self::Top => "Top processes",
            Self::System => "System",
        }
    }

    /// Whether the snapshot has anything for this card.
    fn present(self, snap: &Snapshot) -> bool {
        match self {
            Self::Cpu | Self::Memory | Self::System => true,
            Self::Disks => !snap.disks.is_empty(),
            Self::Network => !snap.adapters.is_empty(),
            Self::Gpus => snap.gpus.iter().any(|g| !g.info.software),
            Self::Battery => snap.battery.is_some(),
            Self::Top => snap.processes.iter().any(|p| p.key().pid != 0),
        }
    }
}

/// A mini chart's series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Cpu,
    Memory,
    /// The busiest disk's active time, by disk number.
    DiskActive(u32),
    /// The busiest adapter's receive rate, by adapter id.
    Rx(u64),
}

impl Line {
    fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Cpu => theme.cpu,
            Self::Memory => theme.memory,
            Self::DiskActive(_) => theme.disk,
            Self::Rx(_) => theme.network,
        }
    }

    fn value_fmt(self) -> ValueFmt {
        match self {
            Self::Cpu | Self::DiskActive(_) => charts::percent_value,
            Self::Memory => charts::bytes_value,
            Self::Rx(_) => charts::bit_rate_value,
        }
    }
}

fn series(tl: &Timeline, line: Line) -> Option<&Series> {
    match line {
        Line::Cpu => Some(&tl.cpu_total),
        Line::Memory => Some(&tl.mem_in_use),
        Line::DiskActive(n) => tl.disk(n).map(|d| &d.active),
        Line::Rx(id) => tl.adapter(id).map(|a| &a.rx),
    }
}

/// The chart a card shows, and its full scale, if it has one.
fn chart_of(kind: CardKind, snap: &Snapshot, tl: &Timeline, span_ms: f32) -> Option<(Line, f32)> {
    match kind {
        CardKind::Cpu => Some((Line::Cpu, 100.0)),
        CardKind::Memory => Some((Line::Memory, snap.memory.total.get() as f32)),
        CardKind::Disks => snap
            .disks
            .iter()
            .max_by(|a, b| a.active.get().total_cmp(&b.active.get()))
            .map(|d| (Line::DiskActive(d.info.number), 100.0)),
        CardKind::Network => snap
            .adapters
            .iter()
            .max_by_key(|a| a.rx_per_sec.get() + a.tx_per_sec.get())
            .map(|a| {
                let id = a.info.id;
                (Line::Rx(id), perf::net_scale(tl, id, span_ms))
            }),
        CardKind::Gpus | CardKind::Battery | CardKind::Top | CardKind::System => None,
    }
}

/// One chart of this frame: what it draws, where, and on what scale.
#[derive(Debug, Clone, Copy)]
struct Slot {
    line: Line,
    area: Rect,
    max: f32,
}

/// One card of this frame, and the index of its chart's slot.
#[derive(Debug, Clone, Copy)]
struct Card {
    kind: CardKind,
    rect: Rect,
    chart: Option<usize>,
}

/// Which of the two process lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rank {
    Cpu,
    Cycles,
}

/// The `TOP_N` highest scores offered, highest first. Scores of zero are not
/// kept: an idle process is not a top process.
#[derive(Debug, Clone, Copy)]
struct Top {
    items: [(f64, usize); TOP_N],
    len: usize,
}

impl Top {
    const fn new() -> Self {
        Self {
            items: [(0.0, 0); TOP_N],
            len: 0,
        }
    }

    fn push(&mut self, score: f64, index: usize) {
        if score <= 0.0 || (self.len == TOP_N && score <= self.items[TOP_N - 1].0) {
            return;
        }
        let mut i = self.len.min(TOP_N - 1);
        while i > 0 && self.items[i - 1].0 < score {
            self.items[i] = self.items[i - 1];
            i -= 1;
        }
        self.items[i] = (score, index);
        self.len = (self.len + 1).min(TOP_N);
    }

    fn iter(&self) -> impl Iterator<Item = (f64, usize)> + '_ {
        self.items[..self.len].iter().copied()
    }
}

#[derive(Debug)]
pub(crate) struct SummaryPage {
    charts: ChartGroup,
    slots: Vec<Slot>,
    cards: Vec<Card>,
    /// The Top processes card's rows: each PID and where it was painted.
    rows: Vec<(u32, Rect)>,
    hover_row: Option<u32>,
    /// The fact lines of the card being painted, as label and value ranges into
    /// `store`.
    lines: Vec<(Range<usize>, Range<usize>)>,
    store: String,
    tmp: String,
    /// The grid's viewport, how far it is scrolled, and how tall its content is.
    view: Rect,
    scroll: f32,
    content_h: f32,
    /// Stands in for a device's series before the timeline has one.
    empty: Series,
    /// The time axis of the frame being painted.
    axis: TimeAxis,
}

impl Default for SummaryPage {
    fn default() -> Self {
        Self {
            charts: ChartGroup::default(),
            slots: Vec::with_capacity(4),
            cards: Vec::with_capacity(CardKind::ALL.len()),
            rows: Vec::with_capacity(2 * TOP_N),
            hover_row: None,
            lines: Vec::with_capacity(16),
            store: String::with_capacity(512),
            tmp: String::with_capacity(32),
            view: Rect::ZERO,
            scroll: 0.0,
            content_h: 0.0,
            empty: Series::new(Retention::raw(1)),
            axis: TimeAxis::DEFAULT,
        }
    }
}

impl SummaryPage {
    /// Whether a chart marks a moment (the pointer is over one).
    pub(crate) fn marking(&self) -> bool {
        self.charts.marking()
    }

    /// Start a frame where only the clock moved: the charts stay where the last
    /// paint put them, on `axis`.
    pub(crate) fn tick(&mut self, axis: TimeAxis) {
        self.axis = axis;
        self.charts.set_axis(axis);
    }

    /// Paint chart `i`'s line again from `tl`, for a frame where only the clock
    /// moved; see [`ChartGroup::repaint`].
    pub(crate) fn repaint_chart(
        &mut self,
        i: usize,
        dl: &mut DisplayList,
        tl: &Timeline,
        theme: &Theme,
    ) {
        let Some(slot) = self.slots.get(i) else {
            return;
        };
        let series = series(tl, slot.line).unwrap_or(&self.empty);
        self.charts.repaint(i, dl, series, theme);
    }

    #[cfg(test)]
    pub fn charts(&self) -> &ChartGroup {
        &self.charts
    }

    /// Where each card was painted last, in reading order.
    #[cfg(test)]
    pub fn card_rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.cards.iter().map(|c| c.rect)
    }

    /// Where a process's row was painted last, if it is listed.
    #[cfg(test)]
    pub fn row_rect(&self, pid: u32) -> Option<Rect> {
        self.rows.iter().find(|(p, _)| *p == pid).map(|(_, r)| *r)
    }

    pub fn handle(&mut self, ev: UiEvent) -> PageOutcome {
        match ev {
            UiEvent::MouseMove(p) => {
                let crosshair = self.charts.hover(Some(p));
                let row = self.row_at(p);
                let moved = std::mem::replace(&mut self.hover_row, row) != row;
                Reaction::painted(crosshair || moved).into()
            }
            UiEvent::MouseLeave => {
                let crosshair = self.charts.hover(None);
                let had = self.hover_row.take().is_some();
                Reaction::painted(crosshair || had).into()
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => match self.row_at(at) {
                Some(pid) => PageOutcome::SelectPid(pid),
                None => Reaction::NONE.into(),
            },
            UiEvent::Wheel {
                at,
                lines,
                horizontal: false,
            } if self.view.contains(at) => {
                let max = (self.content_h - self.view.h).max(0.0);
                let next = (self.scroll + lines * WHEEL_STEP).clamp(0.0, max);
                let changed = (next - self.scroll).abs() > f32::EPSILON;
                self.scroll = next;
                Reaction::painted(changed).into()
            }
            UiEvent::Resize(_) => Reaction::REPAINT.into(),
            _ => Reaction::NONE.into(),
        }
    }

    fn row_at(&self, p: Point) -> Option<u32> {
        if !self.view.contains(p) {
            return None;
        }
        self.rows
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(pid, _)| *pid)
    }

    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        inputs: Inputs,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (title_line, body) = rect.split_top(theme.toolbar_h);
        dl.label("Summary", title_line, theme.title, theme.text);
        let (_, view) = body.split_top(theme.gap * 0.5);
        self.view = view;
        self.rows.clear();
        self.axis = inputs.axis;
        if inputs.snap.is_empty() {
            self.cards.clear();
            self.slots.clear();
            self.content_h = 0.0;
            self.charts.begin(0, self.axis);
            self.charts.snap();
            let (line, _) = view.split_top(LINE_H);
            dl.label(
                "Waiting for the first sample\u{2026}",
                line,
                theme.cell,
                theme.text_dim,
            );
            return;
        }

        // Plan every card and build every chart before painting any, so the hover
        // can snap to the chart under the pointer and the others mark that moment.
        self.plan(view, inputs, theme, buf);
        self.charts.begin(self.slots.len(), self.axis);
        for (i, s) in self.slots.iter().enumerate() {
            let series = series(inputs.timeline, s.line).unwrap_or(&self.empty);
            self.charts.build(i, s.area, 0.0, series, s.max);
        }
        self.charts.snap();

        dl.push_clip(view);
        for i in 0..self.cards.len() {
            let card = self.cards[i];
            if card.rect.bottom() < view.y || card.rect.y > view.bottom() {
                continue;
            }
            self.paint_card(card, dl, inputs, theme, buf);
        }
        dl.pop_clip();

        let max = (self.content_h - view.h).max(0.0);
        if max > 0.0 {
            let track = Rect::new(view.right() - 4.0, view.y, 4.0, view.h);
            let thumb_h = (track.h * view.h / self.content_h).max(20.0);
            let thumb_y = track.y + (track.h - thumb_h) * (self.scroll / max);
            dl.fill_round_rect(
                Rect::new(track.x, thumb_y, track.w, thumb_h),
                2.0,
                theme.scrollbar,
            );
        }
    }

    /// Lay the cards out in `view`: one or two columns by its width, each card
    /// under the shorter column, scrolled; and plan their charts.
    fn plan(&mut self, view: Rect, inputs: Inputs, theme: &Theme, buf: &mut String) {
        self.cards.clear();
        self.slots.clear();
        let gap = theme.gap;
        let cols = if view.w >= TWO_COLUMNS_AT { 2 } else { 1 };
        let col_w = ((view.w - gap * (cols - 1) as f32) / cols as f32).max(0.0);
        let mut col_y = [0.0f32; 2];
        let span = self.axis.span_ms;
        for kind in CardKind::ALL {
            if !kind.present(inputs.snap) {
                continue;
            }
            let chart = chart_of(kind, inputs.snap, inputs.timeline, span);
            let h = self.card_height(kind, inputs, col_w, chart.is_some(), theme, buf);
            let col = usize::from(cols == 2 && col_y[1] < col_y[0]);
            let rect = Rect::new(col as f32 * (col_w + gap), col_y[col], col_w, h);
            col_y[col] += h + gap;
            let chart = chart.map(|(line, max)| {
                let area = chart_area(rect, theme);
                self.slots.push(Slot { line, area, max });
                self.slots.len() - 1
            });
            self.cards.push(Card { kind, rect, chart });
        }
        self.content_h = (col_y[0].max(col_y[1]) - gap).max(0.0);

        let max_scroll = (self.content_h - view.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let (dx, dy) = (view.x, view.y - self.scroll);
        for c in &mut self.cards {
            c.rect = c.rect.offset(dx, dy);
        }
        for s in &mut self.slots {
            s.area = s.area.offset(dx, dy);
        }
    }

    /// How tall a card `w` wide comes out: its title, then its headline and fact
    /// lines (or the process lists), or its chart if that is taller.
    fn card_height(
        &mut self,
        kind: CardKind,
        inputs: Inputs,
        w: f32,
        has_chart: bool,
        theme: &Theme,
        buf: &mut String,
    ) -> f32 {
        let inner_w = (w - 4.0 * theme.pad).max(0.0);
        let body = if kind == CardKind::Top {
            top_height(inner_w, theme)
        } else {
            let head = if self.headline(kind, inputs.snap, buf) {
                HEAD_H
            } else {
                0.0
            };
            self.fill_lines(kind, inputs, buf);
            let text = head + self.lines.len() as f32 * LINE_H;
            if has_chart {
                text.max(CHART_MIN_H)
            } else {
                text
            }
        };
        2.0 * theme.pad + TITLE_H + body
    }

    fn paint_card(
        &mut self,
        card: Card,
        dl: &mut DisplayList,
        inputs: Inputs,
        theme: &Theme,
        buf: &mut String,
    ) {
        dl.fill_round_rect(card.rect, theme.card_radius, theme.surface);
        dl.stroke_rect(card.rect, theme.surface_border, 1.0);
        let inner = card.rect.inset(theme.pad * 2.0, theme.pad);
        let (title, body) = inner.split_top(TITLE_H);
        dl.label(card.kind.title(), title, theme.title, theme.text);

        let text = match card.chart {
            Some(i) => {
                let Slot { line, area, .. } = self.slots[i];
                let style = charts::style(line.color(theme), theme);
                self.charts
                    .paint(i, dl, &style, line.value_fmt(), theme, buf);
                dl.stroke_rect(area, line.color(theme).with_alpha(0.45), 1.0);
                body.split_left((body.w - area.w - theme.gap).max(0.0)).0
            }
            None => body,
        };
        if card.kind == CardKind::Top {
            self.paint_top(dl, text, inputs, theme, buf);
            return;
        }

        let mut y = text.y;
        if self.headline(card.kind, inputs.snap, buf) {
            let r = Rect::new(text.x, y, text.w, HEAD_H);
            dl.label(buf, r, theme.stat, theme.text);
            y += HEAD_H;
        }
        self.fill_lines(card.kind, inputs, buf);
        for (l, v) in &self.lines {
            let r = Rect::new(text.x, y, text.w, LINE_H);
            let (lr, vr) = r.split_left(LABEL_W.min(r.w));
            dl.label(&self.store[l.clone()], lr, theme.cell, theme.text_dim);
            dl.label(&self.store[v.clone()], vr, theme.cell, theme.text);
            y += LINE_H;
        }
    }

    /// The two process lists: the five by CPU right now, the five by the fading
    /// cycle totals; side by side when there is room, one over the other when not.
    fn paint_top(
        &mut self,
        dl: &mut DisplayList,
        area: Rect,
        inputs: Inputs,
        theme: &Theme,
        buf: &mut String,
    ) {
        let mut by_cpu = Top::new();
        let mut by_cycles = Top::new();
        for (i, p) in inputs.snap.processes.iter().enumerate() {
            if p.key().pid == 0 {
                continue;
            }
            by_cpu.push(f64::from(p.cpu.get()), i);
            by_cycles.push(inputs.cycles.get(i).copied().unwrap_or(0.0), i);
        }
        let (left, right) = if area.w >= TOP_SIDE_BY_SIDE_AT {
            let (l, r) = area.split_left(((area.w - theme.gap) * 0.5).max(0.0));
            (l, r.split_left(theme.gap.min(r.w)).1)
        } else {
            let (l, rest) = area.split_top(list_height());
            (l, rest.split_top(theme.gap.min(rest.h)).1)
        };
        self.paint_list(dl, left, Rank::Cpu, &by_cpu, inputs, theme, buf);
        self.paint_list(dl, right, Rank::Cycles, &by_cycles, inputs, theme, buf);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_list(
        &mut self,
        dl: &mut DisplayList,
        area: Rect,
        rank: Rank,
        top: &Top,
        inputs: Inputs,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (caption, rows) = area.split_top(LINE_H);
        let title = match rank {
            Rank::Cpu => "By CPU",
            Rank::Cycles => "By cycles",
        };
        dl.label(title, caption, theme.small, theme.text_dim);
        let mut y = rows.y;
        for (score, i) in top.iter() {
            let p = &inputs.snap.processes[i];
            let pid = p.key().pid;
            let r = Rect::new(rows.x, y, rows.w, LINE_H);
            if self.hover_row == Some(pid) {
                dl.fill_round_rect(r, 3.0, theme.row_hover);
            }
            match rank {
                Rank::Cpu => charts::percent_value(buf, score as f32),
                Rank::Cycles => format::cycles(buf, score),
            }
            let (name, value) = r.split_left((r.w - VALUE_W).max(0.0));
            dl.label(p.name(), name, theme.cell, theme.text);
            dl.text(
                buf,
                value,
                theme.cell_num,
                theme.text_dim,
                HAlign::Right,
                VAlign::Middle,
                false,
            );
            self.rows.push((pid, r));
            y += LINE_H;
        }
    }

    /// A card's headline into `buf`; whether it has one.
    fn headline(&mut self, kind: CardKind, snap: &Snapshot, buf: &mut String) -> bool {
        match kind {
            CardKind::Cpu => {
                charts::percent_value(buf, snap.cpu.total.get());
                if let Some(hz) = perf::current_clock(snap) {
                    format::clock(&mut self.tmp, hz);
                    let _ = write!(buf, "  {}", self.tmp);
                }
                true
            }
            CardKind::Memory => {
                let m = &snap.memory;
                format::bytes_of(buf, m.in_use(), m.total);
                if m.total.get() > 0 {
                    let pct = m.in_use().get() as f64 / m.total.get() as f64 * 100.0;
                    let _ = write!(buf, " ({:.0}%)", pct.round());
                }
                true
            }
            CardKind::Battery => {
                let Some(b) = &snap.battery else {
                    return false;
                };
                buf.clear();
                if let Some(c) = b.charge {
                    let _ = write!(buf, "{c:.0}%");
                }
                let state = b.state.label();
                if !state.is_empty() {
                    if !buf.is_empty() {
                        buf.push_str("  ");
                    }
                    buf.push_str(state);
                }
                !buf.is_empty()
            }
            CardKind::Disks
            | CardKind::Network
            | CardKind::Gpus
            | CardKind::Top
            | CardKind::System => false,
        }
    }

    /// Add a fact line: `label`, then `value`.
    fn line(&mut self, label: &str, value: &str) {
        let l = self.store.len()..self.store.len() + label.len();
        self.store.push_str(label);
        let v = self.store.len()..self.store.len() + value.len();
        self.store.push_str(value);
        self.lines.push((l, v));
    }

    /// A card's fact lines, into `lines` and `store`. `buf` is scratch.
    #[allow(clippy::too_many_lines)]
    fn fill_lines(&mut self, kind: CardKind, inputs: Inputs, buf: &mut String) {
        self.lines.clear();
        self.store.clear();
        let snap = inputs.snap;
        match kind {
            CardKind::Cpu => {
                let (mut procs, mut threads, mut handles) = (0u32, 0u32, 0u32);
                for p in snap.processes.iter().filter(|p| p.key().pid != 0) {
                    procs += 1;
                    threads = threads.saturating_add(p.threads);
                    handles = handles.saturating_add(p.handles);
                }
                format::count(buf, procs);
                self.line("Processes", buf);
                format::count(buf, threads);
                self.line("Threads", buf);
                format::count(buf, handles);
                self.line("Handles", buf);
                if uptime(snap, buf) {
                    self.line("Up time", buf);
                }
                if let Some(w) = snap.cpu.package_power {
                    format::watts(buf, w.0);
                    self.line("Package power", buf);
                }
                if let Some(c) = snap.cpu.hotspot_celsius {
                    buf.clear();
                    let _ = write!(buf, "{c:.0} \u{b0}C");
                    self.line("Temperature", buf);
                }
            }
            CardKind::Memory => {
                let m = &snap.memory;
                format::bytes_of(buf, m.committed, m.commit_limit);
                self.line("Committed", buf);
                format::bytes(buf, m.cached);
                self.line("Cached", buf);
            }
            CardKind::Disks => {
                for d in &snap.disks {
                    charts::percent_value(buf, d.active.get());
                    format::bytes_per_sec(&mut self.tmp, d.read_per_sec);
                    let _ = write!(buf, " \u{b7} R {}", self.tmp);
                    format::bytes_per_sec(&mut self.tmp, d.write_per_sec);
                    let _ = write!(buf, " \u{b7} W {}", self.tmp);
                    self.line(&d.info.name, buf);
                }
            }
            CardKind::Network => {
                let leave_quiet_out = snap.adapters.len() > QUIET_ADAPTERS_FROM;
                for a in &snap.adapters {
                    let busy = a.rx_per_sec.get() + a.tx_per_sec.get() > 0;
                    if leave_quiet_out && !busy {
                        continue;
                    }
                    charts::bit_rate_value(&mut self.tmp, a.rx_per_sec.get() as f32);
                    buf.clear();
                    let _ = write!(buf, "\u{2193} {}  ", self.tmp);
                    charts::bit_rate_value(&mut self.tmp, a.tx_per_sec.get() as f32);
                    let _ = write!(buf, "\u{2191} {}", self.tmp);
                    self.line(&a.info.name, buf);
                }
                if self.lines.is_empty() {
                    buf.clear();
                    let _ = write!(buf, "{} adapters", snap.adapters.len());
                    self.line("No traffic", buf);
                }
            }
            CardKind::Gpus => {
                for g in snap.gpus.iter().filter(|g| !g.info.software) {
                    charts::percent_value(buf, g.utilization.get());
                    match g.info.dedicated_total.filter(|t| t.get() > 0) {
                        Some(total) => format::bytes_of(&mut self.tmp, g.dedicated_used, total),
                        None => format::bytes(&mut self.tmp, g.dedicated_used),
                    }
                    let _ = write!(buf, "  {}", self.tmp);
                    self.line(&g.info.name, buf);
                }
            }
            CardKind::Battery => {
                let Some(b) = &snap.battery else {
                    return;
                };
                let charging = b.state == BatteryState::Charging;
                if let Some(w) = b.rate {
                    format::watts(buf, w.0);
                    let label = if charging {
                        "Charging at"
                    } else {
                        "Power draw"
                    };
                    self.line(label, buf);
                }
                if let Some(t) = b.time_left {
                    format::hms(buf, t.as_secs());
                    let label = if charging {
                        "Time to full"
                    } else {
                        "Time left"
                    };
                    self.line(label, buf);
                }
                if let Some(ac) = b.ac_power {
                    let source = if ac { "Plugged in" } else { "On battery" };
                    self.line("Power source", source);
                }
            }
            CardKind::Top => {}
            CardKind::System => {
                let users = snap.sessions.iter().filter(|s| s.user.is_some()).count();
                format::count64(buf, users as u64);
                self.line("Users signed in", buf);
                let running = snap
                    .services
                    .iter()
                    .filter(|s| s.state == ServiceState::Running)
                    .count();
                format::count64(buf, running as u64);
                self.line("Services running", buf);
                if uptime(snap, buf) {
                    self.line("Up time", buf);
                }
                if let Some(f) = inputs.facts {
                    if let Some(e) = f.os_name.as_deref().filter(|e| !e.is_empty()) {
                        self.line("Edition", e);
                    }
                    if let Some(b) = f.os_build.as_deref().filter(|b| !b.is_empty()) {
                        self.line("Build", b);
                    }
                    if let Some(c) = f.computer_name.as_deref().filter(|c| !c.is_empty()) {
                        self.line("Computer name", c);
                    }
                }
            }
        }
    }
}

/// A card's chart: the right part of its body, at most `CHART_MAX_H` tall.
fn chart_area(card: Rect, theme: &Theme) -> Rect {
    let inner = card.inset(theme.pad * 2.0, theme.pad);
    let (_, body) = inner.split_top(TITLE_H);
    let w = (inner.w * CHART_SHARE).clamp(0.0, CHART_MAX_W);
    Rect::new(body.right() - w, body.y, w, body.h.min(CHART_MAX_H))
}

/// One process list: its caption and `TOP_N` rows.
fn list_height() -> f32 {
    (TOP_N + 1) as f32 * LINE_H
}

/// The Top processes card's body: the two lists side by side, or stacked.
fn top_height(inner_w: f32, theme: &Theme) -> f32 {
    if inner_w >= TOP_SIDE_BY_SIDE_AT {
        list_height()
    } else {
        2.0 * list_height() + theme.gap
    }
}

/// Up time into `buf`; false when the boot time is unknown.
fn uptime(snap: &Snapshot, buf: &mut String) -> bool {
    let now = snap
        .taken_at
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    match (now, snap.hardware.boot_unix_ms) {
        (Some(now), Some(boot)) => {
            format::uptime(buf, u64::try_from((now - boot) / 1000).unwrap_or(0));
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::tests::{adapter, battery, disk, gpu, timeline_of, Devices};
    use ot_model::session::{SessionInfo, SessionState};
    use ot_model::Watts;
    use ot_paint::DrawCmd;

    const WIDE: Rect = Rect::new(0.0, 0.0, 1000.0, 720.0);
    const NARROW: Rect = Rect::new(0.0, 0.0, 520.0, 400.0);

    /// A machine with one of everything, signed in once, with two cycle totals.
    fn machine() -> (Timeline, Snapshot) {
        let (tl, mut s) = timeline_of(
            2,
            &Devices {
                disks: vec![disk(0)],
                adapters: vec![adapter(7, "Ethernet")],
                gpus: vec![gpu(3)],
                battery: Some(battery()),
                ..Devices::default()
            },
        );
        s.sessions.push(SessionInfo {
            id: 1,
            user: Some("BOX\\cam".to_owned()),
            station: "Console".to_owned(),
            state: SessionState::Active,
            client: None,
            current: true,
        });
        s.cpu.package_power = Some(Watts(45.0));
        s.cpu.hotspot_celsius = Some(72.4);
        (tl, s)
    }

    /// Cycle totals for the three test processes (PIDs 0, 4, 8).
    const CYCLES: [f64; 3] = [0.0, 5.0e9, 2.0e9];

    fn paint_in(page: &mut SummaryPage, rect: Rect, tl: &Timeline, s: &Snapshot) -> DisplayList {
        let facts = SystemFacts {
            os_name: Some("Windows 11 Pro".to_owned()),
            os_build: Some("26100.1".to_owned()),
            ..SystemFacts::default()
        };
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            rect,
            Inputs {
                snap: s,
                timeline: tl,
                axis: charts::axis_of(tl, None, i64::MAX),
                cycles: &CYCLES,
                facts: Some(&facts),
            },
            &Theme::dark(),
            &mut buf,
        );
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

    fn has(t: &[String], want: &str) -> bool {
        t.iter().any(|s| s == want)
    }

    fn line_colors(dl: &DisplayList) -> Vec<Color> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Polyline { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_card_shows_its_headline_numbers() {
        let (tl, s) = machine();
        let mut page = SummaryPage::default();
        let dl = paint_in(&mut page, WIDE, &tl, &s);
        let t = texts(&dl);
        for want in [
            "Summary",
            "CPU",
            "25%",
            "Processes",
            "2",
            "Threads",
            "Handles",
            "Up time",
            // 1020 s after a boot at the epoch.
            "0:00:17:00",
            "Package power",
            "45.0 W",
            "Temperature",
            "72 \u{b0}C",
            "Memory",
            "10.0 / 16.0 GB (63%)",
            "Committed",
            "12.0 / 20.0 GB",
            "Cached",
            "4.00 GB",
            "Disks",
            "Disk 0 (C:)",
            "12% \u{b7} R 3.00 MB/s \u{b7} W 1.00 MB/s",
            "Network",
            "Ethernet",
            "\u{2193} 5.60 Mbps  \u{2191} 100 Kbps",
            "GPU",
            "GPU 3",
            "42%  2.0 / 8.0 GB",
            "Battery",
            "80%  Discharging",
            "Power draw",
            "12.5 W",
            "Time left",
            "3:10:00",
            "Power source",
            "On battery",
            "Top processes",
            "By CPU",
            "p8.exe",
            "2.0%",
            "p4.exe",
            "1.0%",
            "By cycles",
            "5.00 G",
            "2.00 G",
            "System",
            "Users signed in",
            "1",
            "Services running",
            "0",
            "Edition",
            "Windows 11 Pro",
            "Build",
            "26100.1",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        assert!(!has(&t, "Speed"), "no clock reported, no speed");
        assert!(!has(&t, "p0.exe"), "the idle process is not a top process");
        // CPU, memory, the busiest disk and the busiest adapter get a chart each.
        let theme = Theme::dark();
        assert_eq!(
            line_colors(&dl),
            [theme.cpu, theme.memory, theme.disk, theme.network]
        );
        assert_eq!(page.charts().len(), 4);
        // The System card is painted whether or not the facts have been read.
        let mut dl = DisplayList::new();
        page.paint(
            &mut dl,
            WIDE,
            Inputs {
                snap: &s,
                timeline: &tl,
                axis: charts::axis_of(&tl, None, i64::MAX),
                cycles: &CYCLES,
                facts: None,
            },
            &theme,
            &mut String::new(),
        );
        let t = texts(&dl);
        assert!(has(&t, "Users signed in") && !has(&t, "Edition"), "{t:?}");
    }

    #[test]
    fn cards_without_a_device_are_left_out() {
        let (tl, s) = timeline_of(2, &Devices::default());
        let mut page = SummaryPage::default();
        let t = texts(&paint_in(&mut page, WIDE, &tl, &s));
        for title in ["CPU", "Memory", "Top processes", "System"] {
            assert!(has(&t, title), "{title} missing from {t:?}");
        }
        for title in ["Disks", "Network", "GPU", "Battery"] {
            assert!(!has(&t, title), "{title} should not be in {t:?}");
        }
        assert_eq!(page.charts().len(), 2);
        assert!(!has(&t, "Package power"), "unknown facts are left out");

        // Before the first sample there is nothing to summarize.
        let empty = Snapshot::default();
        let t = texts(&paint_in(&mut page, WIDE, &tl, &empty));
        assert_eq!(t, ["Summary", "Waiting for the first sample\u{2026}"]);
        assert_eq!(page.charts().len(), 0);
    }

    #[test]
    fn quiet_adapters_are_left_out_when_there_are_many() {
        let mut adapters: Vec<_> = (0..6).map(|i| adapter(i, "vEthernet")).collect();
        for a in &mut adapters[1..] {
            a.rx_per_sec = ot_model::Bytes(0);
            a.tx_per_sec = ot_model::Bytes(0);
        }
        adapters[0].info = std::sync::Arc::new(ot_model::device::AdapterInfo {
            name: "Busy".to_owned(),
            ..(*adapters[0].info).clone()
        });
        let (tl, s) = timeline_of(
            2,
            &Devices {
                adapters,
                ..Devices::default()
            },
        );
        let mut page = SummaryPage::default();
        let t = texts(&paint_in(&mut page, WIDE, &tl, &s));
        assert!(has(&t, "Busy"), "{t:?}");
        assert!(!has(&t, "vEthernet"), "the five idle ones: {t:?}");

        // Four or fewer are all listed, idle or not.
        let adapters: Vec<_> = (0..3)
            .map(|i| {
                let mut a = adapter(i, "Idle");
                a.rx_per_sec = ot_model::Bytes(0);
                a.tx_per_sec = ot_model::Bytes(0);
                a
            })
            .collect();
        let (tl, s) = timeline_of(
            2,
            &Devices {
                adapters,
                ..Devices::default()
            },
        );
        let t = texts(&paint_in(&mut page, WIDE, &tl, &s));
        assert_eq!(t.iter().filter(|s| *s == "Idle").count(), 3, "{t:?}");
    }

    #[test]
    fn the_grid_reflows_with_the_width_and_scrolls_when_tall() {
        let (tl, s) = machine();
        let mut page = SummaryPage::default();
        let _ = paint_in(&mut page, WIDE, &tl, &s);
        let wide: Vec<Rect> = page.card_rects().collect();
        assert_eq!(wide.len(), 8, "every card is present");
        let xs: std::collections::BTreeSet<i32> = wide.iter().map(|r| r.x as i32).collect();
        assert_eq!(xs.len(), 2, "two columns: {wide:?}");
        assert!(
            wide.iter().all(|r| r.bottom() <= WIDE.bottom() + 1e-3),
            "everything fits when wide: {wide:?}"
        );
        for pair in wide.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let overlap = a.intersect(&b);
            assert!(overlap.is_empty(), "{a:?} overlaps {b:?}");
        }

        let _ = paint_in(&mut page, NARROW, &tl, &s);
        let narrow: Vec<Rect> = page.card_rects().collect();
        let xs: std::collections::BTreeSet<i32> = narrow.iter().map(|r| r.x as i32).collect();
        assert_eq!(xs.len(), 1, "one column: {narrow:?}");
        for pair in narrow.windows(2) {
            assert!(pair[1].y >= pair[0].bottom(), "stacked: {narrow:?}");
        }
        let last = *narrow.last().unwrap();
        assert!(last.bottom() > NARROW.bottom(), "overflows when narrow");
        let first_y = narrow[0].y;
        let r = page.handle(UiEvent::Wheel {
            at: NARROW.center(),
            lines: 3.0,
            horizontal: false,
        });
        assert_eq!(r, PageOutcome::Reaction(Reaction::REPAINT));
        let _ = paint_in(&mut page, NARROW, &tl, &s);
        let scrolled = page.card_rects().next().unwrap();
        assert!((first_y - scrolled.y - 3.0 * WHEEL_STEP).abs() < 1e-3);
        // Scrolling never runs past the end, and wide again there is no scroll.
        page.handle(UiEvent::Wheel {
            at: NARROW.center(),
            lines: 1000.0,
            horizontal: false,
        });
        let _ = paint_in(&mut page, NARROW, &tl, &s);
        let last = page.card_rects().last().unwrap();
        assert!((last.bottom() - NARROW.bottom()).abs() < 1e-3, "{last:?}");
        let _ = paint_in(&mut page, WIDE, &tl, &s);
        assert!((page.card_rects().next().unwrap().y - wide[0].y).abs() < 1e-3);
    }

    #[test]
    fn a_click_on_a_top_process_goes_to_it() {
        let (tl, s) = machine();
        let mut page = SummaryPage::default();
        let _ = paint_in(&mut page, WIDE, &tl, &s);
        let row = page.row_rect(8).expect("p8 leads by CPU");
        assert!(page.row_rect(0).is_none(), "the idle process is not listed");
        let r = page.handle(UiEvent::MouseMove(row.center()));
        assert_eq!(
            r,
            PageOutcome::Reaction(Reaction::REPAINT),
            "hover highlight"
        );
        let r = page.handle(UiEvent::MouseMove(row.center()));
        assert_eq!(r, PageOutcome::Reaction(Reaction::NONE), "same row");
        let dl = paint_in(&mut page, WIDE, &tl, &s);
        let theme = Theme::dark();
        let highlighted = dl
            .cmds()
            .iter()
            .filter(
                |c| matches!(c, DrawCmd::FillRoundRect { color, .. } if *color == theme.row_hover),
            )
            .count();
        assert_eq!(highlighted, 2, "p8 is in both lists, so both rows light up");
        assert_eq!(
            page.handle(UiEvent::MouseDown {
                at: row.center(),
                button: MouseButton::Left
            }),
            PageOutcome::SelectPid(8)
        );
        assert_eq!(
            page.handle(UiEvent::MouseDown {
                at: Point::new(WIDE.right() - 1.0, WIDE.bottom() - 1.0),
                button: MouseButton::Left
            }),
            PageOutcome::Reaction(Reaction::NONE),
            "a click elsewhere does nothing"
        );
        assert_eq!(
            page.handle(UiEvent::MouseLeave),
            PageOutcome::Reaction(Reaction::REPAINT)
        );
    }

    #[test]
    fn hovering_one_chart_marks_them_all() {
        let (tl, s) = machine();
        let mut page = SummaryPage::default();
        let _ = paint_in(&mut page, WIDE, &tl, &s);
        let cpu = page.charts().plot(0).rect();
        let at = Point::new(page.charts().axis().x(cpu, 3000.0), cpu.center().y);
        assert_eq!(
            page.handle(UiEvent::MouseMove(at)),
            PageOutcome::Reaction(Reaction::REPAINT)
        );
        assert_eq!(page.charts().hover_age(), Some(3000.0));
        let dl = paint_in(&mut page, WIDE, &tl, &s);
        let dots = dl
            .cmds()
            .iter()
            .filter(|c| matches!(c, DrawCmd::FillRoundRect { rect, .. } if rect.w <= 8.0))
            .count();
        assert_eq!(dots, 4, "a dot on every chart");
        assert_eq!(
            page.handle(UiEvent::MouseLeave),
            PageOutcome::Reaction(Reaction::REPAINT)
        );
        assert_eq!(page.charts().hover_age(), None);
    }

    #[test]
    fn the_top_list_keeps_the_highest_scores_in_order() {
        let mut top = Top::new();
        for (score, i) in [
            (3.0, 0),
            (0.0, 1),
            (9.0, 2),
            (1.0, 3),
            (5.0, 4),
            (7.0, 5),
            (2.0, 6),
        ] {
            top.push(score, i);
        }
        let got: Vec<(f64, usize)> = top.iter().collect();
        assert_eq!(got, [(9.0, 2), (7.0, 5), (5.0, 4), (3.0, 0), (2.0, 6)]);
        top.push(4.0, 7);
        let got: Vec<usize> = top.iter().map(|(_, i)| i).collect();
        assert_eq!(got, [2, 5, 4, 7, 0], "4.0 takes the place of 2.0");
        top.push(1.0, 8);
        assert_eq!(top.iter().count(), TOP_N, "too small to enter");
    }
}
