//! The Performance page.
//!
//! A list of devices down the left, each with a live mini chart and its headline
//! number, and a detail pane for the selected one: a large chart (or, for the CPU,
//! one small chart per logical processor), then the numbers. Every chart on the
//! page, the mini ones included, shares one hover line.

use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use ot_core::{Snapshot, Timeline};
use ot_model::cpu::CoreKind;
use ot_model::Hertz;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

use crate::charts::{self, ChartGroup, ValueFmt, AXIS_BAND_H};
use crate::format;
use crate::theme::Theme;
use crate::view::{Key, MouseButton, Reaction, UiEvent};

/// Something the page can show in detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Device {
    #[default]
    Cpu,
    Memory,
}

impl Device {
    const ALL: [Self; 2] = [Self::Cpu, Self::Memory];

    fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&d| d == self).unwrap_or(0)
    }

    /// `n` steps down the list, stopping at either end.
    fn step(self, n: isize) -> Self {
        let last = Self::ALL.len() - 1;
        Self::ALL[self.index().saturating_add_signed(n).min(last)]
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Cpu => theme.cpu,
            Self::Memory => theme.memory,
        }
    }

    fn value_fmt(self) -> ValueFmt {
        match self {
            Self::Cpu => charts::percent_value,
            Self::Memory => charts::bytes_value,
        }
    }
}

/// Mini charts come first in the chart group, one per device, in list order; the
/// detail pane's charts follow.
const MINI: usize = Device::ALL.len();

const LIST_W: f32 = 212.0;
const ITEM_H: f32 = 60.0;
const ITEM_GAP: f32 = 4.0;
const MINI_W: f32 = 72.0;
const HEADER_H: f32 = 36.0;
const CAPTION_H: f32 = 22.0;
const STATS_H: f32 = 140.0;
const COMPOSITION_H: f32 = 60.0;
const STAT_W: f32 = 132.0;
const STAT_H: f32 = 44.0;
const FACT_H: f32 = 19.0;
const FACT_LABEL_W: f32 = 128.0;
/// The CPU graph switch: all processors as one line, or one chart each.
const TOGGLE: [(&str, f32); 2] = [("Overall", 70.0), ("Logical processors", 128.0)];
const CELL_GAP: f32 = 4.0;
/// Target width-to-height ratio of a per-processor chart.
const CELL_ASPECT: f32 = 2.0;

/// Where everything goes this frame.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    items: [Rect; MINI],
    minis: [Rect; MINI],
    card: Rect,
    header: Rect,
    caption: Rect,
    /// The detail chart, label band included.
    chart: Rect,
    /// Memory only: the composition bar and its legend.
    composition: Rect,
    stats: Rect,
}

fn layout(page: Rect, device: Device, theme: &Theme) -> Frame {
    let (list, right) = page.split_left(LIST_W.min(page.w));
    let (_, card) = right.split_left(theme.gap.min(right.w));
    let mut f = Frame {
        card,
        ..Frame::default()
    };
    let mut y = list.y;
    for i in 0..MINI {
        let r = Rect::new(list.x, y, list.w, ITEM_H);
        f.items[i] = r;
        f.minis[i] = Rect::new(
            r.x + theme.pad,
            r.y + theme.pad,
            MINI_W,
            r.h - 2.0 * theme.pad,
        );
        y += ITEM_H + ITEM_GAP;
    }
    let inner = card.inset(theme.pad * 2.0, theme.pad);
    let (header, body) = inner.split_top(HEADER_H);
    let (caption, body) = body.split_top(CAPTION_H);
    let (stats, body) = body.split_bottom(STATS_H.min(body.h * 0.45));
    let (composition, body) = if device == Device::Memory {
        body.split_bottom(COMPOSITION_H.min(body.h * 0.3))
    } else {
        (Rect::ZERO, body)
    };
    let (_, chart) = body.split_bottom(theme.gap.min(body.h));
    f.header = header;
    f.caption = caption;
    f.chart = chart;
    f.composition = composition;
    f.stats = stats;
    f
}

/// Columns and rows for `n` charts in `area`, choosing the column count whose
/// cells come closest to [`CELL_ASPECT`].
fn grid_shape(area: Rect, n: usize) -> (usize, usize) {
    if n == 0 || area.is_empty() {
        return (1, 1);
    }
    let mut best = (1, n, f32::MAX);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let w = (area.w - CELL_GAP * (cols - 1) as f32) / cols as f32;
        let h = (area.h - CELL_GAP * (rows - 1) as f32) / rows as f32;
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let miss = ((w / h) / CELL_ASPECT).ln().abs();
        if miss < best.2 {
            best = (cols, rows, miss);
        }
    }
    (best.0, best.1)
}

fn cell(area: Rect, cols: usize, rows: usize, index: usize) -> Rect {
    let width = (area.w - CELL_GAP * (cols - 1) as f32) / cols as f32;
    let height = (area.h - CELL_GAP * (rows - 1) as f32) / rows as f32;
    let (col, row) = (index % cols, index / cols);
    Rect::new(
        area.x + col as f32 * (width + CELL_GAP),
        area.y + row as f32 * (height + CELL_GAP),
        width,
        height,
    )
}

#[derive(Debug, Default)]
pub(crate) struct PerfPage {
    device: Device,
    /// The CPU detail shows one chart per logical processor.
    per_core: bool,
    charts: ChartGroup,
    list: [Rect; MINI],
    list_hover: Option<Device>,
    toggle: [Rect; 2],
    toggle_hover: Option<usize>,
}

impl PerfPage {
    #[cfg(test)]
    pub fn device(&self) -> Device {
        self.device
    }

    #[cfg(test)]
    pub fn charts(&self) -> &ChartGroup {
        &self.charts
    }

    pub fn handle(&mut self, ev: UiEvent) -> Reaction {
        match ev {
            UiEvent::MouseMove(p) => self.hover(Some(p)),
            UiEvent::MouseLeave => self.hover(None),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => self.click(at),
            UiEvent::Key(Key::Up) => self.select(self.device.step(-1)),
            UiEvent::Key(Key::Down) => self.select(self.device.step(1)),
            UiEvent::Key(Key::Home) => self.select(Device::ALL[0]),
            UiEvent::Key(Key::End) => self.select(Device::ALL[Device::ALL.len() - 1]),
            _ => Reaction::NONE,
        }
    }

    fn item_at(&self, p: Point) -> Option<Device> {
        Device::ALL
            .into_iter()
            .zip(self.list)
            .find(|(_, r)| r.contains(p))
            .map(|(d, _)| d)
    }

    fn toggle_at(&self, p: Point) -> Option<usize> {
        if self.device != Device::Cpu {
            return None;
        }
        self.toggle.iter().position(|r| r.contains(p))
    }

    fn hover(&mut self, at: Option<Point>) -> Reaction {
        let item = at.and_then(|p| self.item_at(p));
        let segment = at.and_then(|p| self.toggle_at(p));
        let crosshair = self.charts.hover(at);
        let item_moved = std::mem::replace(&mut self.list_hover, item) != item;
        let segment_moved = std::mem::replace(&mut self.toggle_hover, segment) != segment;
        Reaction::painted(item_moved || segment_moved || crosshair)
    }

    fn click(&mut self, at: Point) -> Reaction {
        if let Some(d) = self.item_at(at) {
            return self.select(d);
        }
        if let Some(i) = self.toggle_at(at) {
            let on = i == 1;
            return Reaction::painted(std::mem::replace(&mut self.per_core, on) != on);
        }
        Reaction::NONE
    }

    fn select(&mut self, d: Device) -> Reaction {
        Reaction::painted(std::mem::replace(&mut self.device, d) != d)
    }

    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        tl: &Timeline,
        theme: &Theme,
        buf: &mut String,
    ) {
        let f = layout(rect, self.device, theme);
        self.list = f.items;
        if self.device != Device::Cpu {
            self.toggle = [Rect::ZERO; 2];
        }

        // Build every chart before painting any, so the hover can snap to the one
        // under the pointer and every other chart marks the same moment.
        let cores = if self.device == Device::Cpu && self.per_core {
            tl.cores.len()
        } else {
            0
        };
        let mem_total = snap.memory.total.get() as f32;
        self.charts.begin(MINI + cores.max(1));
        self.charts.build(0, f.minis[0], 0.0, &tl.cpu_total, 100.0);
        self.charts
            .build(1, f.minis[1], 0.0, &tl.mem_in_use, mem_total);
        let (band, grid) = if cores > 0 {
            f.chart.split_bottom(AXIS_BAND_H)
        } else {
            (Rect::ZERO, f.chart)
        };
        let (cols, rows) = grid_shape(grid, cores);
        if cores > 0 {
            for (i, series) in tl.cores.iter().enumerate() {
                let r = cell(grid, cols, rows, i);
                self.charts.build(MINI + i, r, 0.0, series, 100.0);
            }
        } else {
            let (series, max) = match self.device {
                Device::Cpu => (&tl.cpu_total, 100.0),
                Device::Memory => (&tl.mem_in_use, mem_total),
            };
            self.charts.build(MINI, f.chart, AXIS_BAND_H, series, max);
        }
        self.charts.snap();

        self.paint_list(dl, &f, snap, theme, buf);
        dl.fill_round_rect(f.card, theme.card_radius, theme.surface);
        dl.stroke_rect(f.card, theme.surface_border, 1.0);
        match self.device {
            Device::Cpu => {
                self.paint_cpu_head(dl, &f, snap, theme);
                if cores > 0 {
                    self.paint_core_grid(dl, snap, band, theme, buf);
                } else {
                    let style = charts::style(theme.cpu, theme);
                    self.charts
                        .paint(MINI, dl, &style, charts::percent_value, theme, buf);
                }
                paint_cpu_stats(dl, f.stats, snap, theme, buf);
            }
            Device::Memory => {
                paint_memory_head(dl, &f, snap, theme, buf);
                let style = charts::style(theme.memory, theme);
                self.charts
                    .paint(MINI, dl, &style, charts::bytes_value, theme, buf);
                paint_composition(dl, f.composition, snap, theme, buf);
                paint_memory_stats(dl, f.stats, snap, theme, buf);
            }
        }
    }

    fn paint_list(
        &mut self,
        dl: &mut DisplayList,
        f: &Frame,
        snap: &Snapshot,
        theme: &Theme,
        buf: &mut String,
    ) {
        for (i, d) in Device::ALL.into_iter().enumerate() {
            let r = f.items[i];
            let fill = if d == self.device {
                Some(theme.button_active)
            } else if self.list_hover == Some(d) {
                Some(theme.button_hover)
            } else {
                None
            };
            if let Some(c) = fill {
                dl.fill_round_rect(r, theme.card_radius, c);
            }
            let color = d.color(theme);
            let style = charts::style(color, theme);
            self.charts.paint(i, dl, &style, d.value_fmt(), theme, buf);
            dl.stroke_rect(f.minis[i], color.with_alpha(0.45), 1.0);

            let left = f.minis[i].right() + theme.pad;
            let text = Rect::new(
                left,
                r.y + 10.0,
                (r.right() - left - theme.pad).max(0.0),
                r.h - 20.0,
            );
            let (title, value) = text.split_top(text.h * 0.5);
            dl.label(d.title(), title, theme.cell, theme.text);
            summary(d, snap, buf);
            dl.label(buf, value, theme.small, theme.text_dim);
        }
    }

    fn paint_cpu_head(&mut self, dl: &mut DisplayList, f: &Frame, snap: &Snapshot, theme: &Theme) {
        let (title, name) = f.header.split_left(120.0_f32.min(f.header.w));
        dl.label("CPU", title, theme.big, theme.text);
        if let Some(n) = &snap.hardware.cpu_name {
            dl.text(
                n,
                name,
                theme.cell,
                theme.text_dim,
                HAlign::Right,
                VAlign::Middle,
                true,
            );
        }

        // Caption on the left, the graph switch on the right.
        let group_w: f32 = TOGGLE.iter().map(|t| t.1).sum();
        let (caption, group) = f.caption.split_left((f.caption.w - group_w).max(0.0));
        let label = if self.per_core {
            "Utilization of each logical processor"
        } else {
            "Utilization"
        };
        dl.label(label, caption, theme.small, theme.text_dim);
        let group = Rect::new(group.x, group.y + 1.0, group.w, group.h - 2.0);
        dl.fill_round_rect(group, theme.card_radius, theme.surface);
        dl.stroke_rect(group, theme.surface_border, 1.0);
        let mut x = group.x;
        for (i, (text, w)) in TOGGLE.iter().enumerate() {
            let r = Rect::new(x, group.y, *w, group.h);
            self.toggle[i] = r;
            x += w;
            let active = self.per_core == (i == 1);
            let fill = if active {
                Some(theme.button_active)
            } else if self.toggle_hover == Some(i) {
                Some(theme.button_hover)
            } else {
                None
            };
            if let Some(c) = fill {
                dl.fill_round_rect(r.inset(2.0, 2.0), theme.card_radius - 1.0, c);
            }
            let color = if active { theme.text } else { theme.text_dim };
            dl.text(
                text,
                r,
                theme.small,
                color,
                HAlign::Center,
                VAlign::Middle,
                false,
            );
        }
    }

    /// One chart per logical processor, colored by core kind, each with its value in
    /// the corner: the hovered moment's, or the latest.
    fn paint_core_grid(
        &mut self,
        dl: &mut DisplayList,
        snap: &Snapshot,
        band: Rect,
        theme: &Theme,
        buf: &mut String,
    ) {
        let hovering = self.charts.hover_age();
        for i in 0..snap.cpu.cores.len().min(self.charts_len() - MINI) {
            let core = &snap.cpu.cores[i];
            let color = if core.kind == CoreKind::Efficiency {
                theme.cpu_efficiency
            } else {
                theme.cpu
            };
            let style = charts::style(color, theme);
            let point = self
                .charts
                .paint(MINI + i, dl, &style, charts::percent_value, theme, buf);
            let r = self.charts.plot(MINI + i).rect();
            dl.stroke_rect(r, theme.grid, 1.0);
            if r.h >= 28.0 {
                let value = if hovering.is_some() {
                    point.map(|p| p.mean)
                } else {
                    Some(core.usage.get())
                };
                if let Some(v) = value {
                    charts::percent_value(buf, v);
                    let corner = Rect::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, 14.0);
                    dl.text(
                        buf,
                        corner,
                        theme.small,
                        theme.text_dim,
                        HAlign::Right,
                        VAlign::Top,
                        false,
                    );
                }
            }
        }
        match hovering {
            Some(age) => {
                format::ago(buf, age);
                dl.text(
                    buf,
                    band,
                    theme.small,
                    theme.text,
                    HAlign::Right,
                    VAlign::Middle,
                    true,
                );
            }
            None => dl.label(
                "The last hour on a log scale, newest at the right",
                band,
                theme.small,
                theme.text_dim,
            ),
        }
    }

    fn charts_len(&self) -> usize {
        self.charts.len()
    }
}

/// The headline under a device's name in the list.
fn summary(d: Device, snap: &Snapshot, buf: &mut String) {
    buf.clear();
    if snap.is_empty() {
        return;
    }
    match d {
        Device::Cpu => {
            format::percent(buf, snap.cpu.total.get());
            buf.push('%');
            if let Some(hz) = current_clock(snap) {
                let mut clock = String::new();
                format::clock(&mut clock, hz);
                let _ = write!(buf, "  {clock}");
            }
        }
        Device::Memory => {
            let m = &snap.memory;
            format::bytes_of(buf, m.in_use(), m.total);
            if m.total.get() > 0 {
                let pct = m.in_use().get() as f64 / m.total.get() as f64 * 100.0;
                let _ = write!(buf, " ({:.0}%)", pct.round());
            }
        }
    }
}

/// The average clock across logical processors, when the probe reports clocks.
fn current_clock(snap: &Snapshot) -> Option<Hertz> {
    let (n, sum) = snap
        .cpu
        .cores
        .iter()
        .filter_map(|c| c.frequency)
        .fold((0u64, 0u64), |(n, s), f| (n + 1, s + f.0));
    (n > 0).then(|| Hertz(sum / n))
}

/// A headline number with its label above it.
fn stat(dl: &mut DisplayList, r: Rect, label: &str, value: &str, theme: &Theme) {
    let (l, v) = r.split_top(16.0);
    dl.label(label, l, theme.small, theme.text_dim);
    dl.label(value, v.split_top(24.0).0, theme.stat, theme.text);
}

/// A label and a value on one line, for the static facts.
fn fact(dl: &mut DisplayList, r: Rect, label: &str, value: &str, theme: &Theme) {
    let (l, v) = r.split_left(FACT_LABEL_W.min(r.w));
    dl.label(label, l, theme.small, theme.text_dim);
    dl.label(value, v, theme.small, theme.text);
}

/// The slot at `col`, `row` of the headline grid.
fn slot(stats: Rect, col: usize, row: usize) -> Rect {
    Rect::new(
        stats.x + col as f32 * STAT_W,
        stats.y + row as f32 * STAT_H,
        STAT_W - 8.0,
        STAT_H,
    )
}

fn paint_cpu_stats(
    dl: &mut DisplayList,
    stats: Rect,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    if snap.is_empty() {
        return;
    }
    let (mut procs, mut threads, mut handles) = (0u32, 0u32, 0u32);
    for p in snap.processes.iter().filter(|p| p.key().pid != 0) {
        procs += 1;
        threads = threads.saturating_add(p.threads);
        handles = handles.saturating_add(p.handles);
    }

    charts::percent_value(buf, snap.cpu.total.get());
    stat(dl, slot(stats, 0, 0), "Utilization", buf, theme);
    if let Some(hz) = current_clock(snap) {
        format::clock(buf, hz);
        stat(dl, slot(stats, 1, 0), "Speed", buf, theme);
    }
    format::count(buf, procs);
    stat(dl, slot(stats, 0, 1), "Processes", buf, theme);
    format::count(buf, threads);
    stat(dl, slot(stats, 1, 1), "Threads", buf, theme);
    format::count(buf, handles);
    stat(dl, slot(stats, 2, 1), "Handles", buf, theme);
    let hw = &snap.hardware;
    let now_ms = snap
        .taken_at
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    if let (Some(now), Some(boot)) = (now_ms, hw.boot_unix_ms) {
        format::uptime(buf, u64::try_from((now - boot) / 1000).unwrap_or(0));
        stat(dl, slot(stats, 0, 2), "Up time", buf, theme);
    }

    // The static facts to the right of the headline grid, where there is room.
    let (_, facts) = stats.split_left(3.0 * STAT_W + theme.gap);
    if facts.w < FACT_LABEL_W + 60.0 {
        return;
    }
    let mut row = 0.0;
    let mut line = |dl: &mut DisplayList, label: &str, value: &str| {
        let r = Rect::new(facts.x, facts.y + row * FACT_H, facts.w, FACT_H);
        fact(dl, r, label, value, theme);
        row += 1.0;
    };
    let mut text = String::new();
    if let Some(hz) = hw.base_frequency {
        format::clock(&mut text, hz);
        line(dl, "Base speed", &text);
    }
    if hw.sockets > 0 {
        format::count(&mut text, hw.sockets);
        line(dl, "Sockets", &text);
    }
    if hw.physical_cores > 0 {
        format::count(&mut text, hw.physical_cores);
        let (p, e) = snap
            .cpu
            .cores
            .iter()
            .fold((0, 0), |(p, e), c| match c.kind {
                CoreKind::Performance => (p + 1, e),
                CoreKind::Efficiency => (p, e + 1),
                CoreKind::Unknown => (p, e),
            });
        if p > 0 && e > 0 {
            let _ = write!(text, " ({p} P + {e} E threads)");
        }
        line(dl, "Cores", &text);
    }
    let logical = if hw.logical_processors > 0 {
        hw.logical_processors
    } else {
        snap.cpu.cores.len() as u32
    };
    format::count(&mut text, logical);
    line(dl, "Logical processors", &text);
    for (name, size) in [
        ("L1 cache", hw.cache_l1),
        ("L2 cache", hw.cache_l2),
        ("L3 cache", hw.cache_l3),
    ] {
        if let Some(b) = size {
            format::bytes(&mut text, b);
            line(dl, name, &text);
        }
    }
}

fn paint_memory_head(
    dl: &mut DisplayList,
    f: &Frame,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    let (title, total) = f.header.split_left(160.0_f32.min(f.header.w));
    dl.label("Memory", title, theme.big, theme.text);
    if snap.memory.total.get() > 0 {
        format::bytes(buf, snap.memory.total);
        dl.text(
            buf,
            total,
            theme.cell,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );
    }
    dl.label("Memory in use", f.caption, theme.small, theme.text_dim);
}

/// A bar of how physical memory divides up, with a legend under it.
fn paint_composition(
    dl: &mut DisplayList,
    rect: Rect,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    let m = &snap.memory;
    if rect.is_empty() || m.total.get() == 0 {
        return;
    }
    let parts = [
        ("In use", m.in_use(), theme.memory),
        ("Available", m.available, theme.memory.with_alpha(0.22)),
    ];
    let (caption, below) = rect.split_top(18.0);
    let (bar, legend) = below.split_top(18.0);
    dl.label("Memory composition", caption, theme.small, theme.text_dim);
    let total = m.total.get() as f32;
    let mut x = bar.x;
    for (_, bytes, color) in parts {
        let w = bar.w * (bytes.get() as f32 / total).clamp(0.0, 1.0);
        dl.fill_rect(Rect::new(x, bar.y, w, bar.h), color);
        x += w;
    }
    dl.stroke_rect(bar, theme.memory.with_alpha(0.6), 1.0);

    let (_, legend) = legend.split_top(4.0);
    let mut x = legend.x;
    for (label, bytes, color) in parts {
        let swatch = Rect::new(x, legend.y + (legend.h - 8.0) * 0.5, 8.0, 8.0);
        dl.fill_rect(swatch, color);
        dl.stroke_rect(swatch, theme.memory.with_alpha(0.6), 1.0);
        format::bytes(buf, bytes);
        let mut text = String::with_capacity(24);
        let _ = write!(text, "{label} {buf}");
        dl.label(
            &text,
            Rect::new(x + 12.0, legend.y, 150.0, legend.h),
            theme.small,
            theme.text_dim,
        );
        x += 160.0;
    }
}

fn paint_memory_stats(
    dl: &mut DisplayList,
    stats: Rect,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    if snap.is_empty() {
        return;
    }
    let m = &snap.memory;
    format::bytes(buf, m.in_use());
    stat(dl, slot(stats, 0, 0), "In use", buf, theme);
    format::bytes(buf, m.available);
    stat(dl, slot(stats, 1, 0), "Available", buf, theme);
    format::bytes_of(buf, m.committed, m.commit_limit);
    stat(dl, slot(stats, 2, 0), "Committed", buf, theme);
    format::bytes(buf, m.cached);
    stat(dl, slot(stats, 0, 1), "Cached", buf, theme);
    if let Some(b) = m.paged_pool {
        format::bytes(buf, b);
        stat(dl, slot(stats, 1, 1), "Paged pool", buf, theme);
    }
    if let Some(b) = m.nonpaged_pool {
        format::bytes(buf, b);
        stat(dl, slot(stats, 2, 1), "Non-paged pool", buf, theme);
    }
}

#[cfg(test)]
#[allow(unused_must_use)] // tests drive the page and ignore most reactions
mod tests {
    use super::*;
    use ot_core::Retention;
    use ot_model::cpu::{CpuSample, LogicalCore};
    use ot_model::hardware::Hardware;
    use ot_model::memory::MemorySample;
    use ot_model::{Bytes, Percent, Tick};
    use ot_paint::DrawCmd;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    const GB: u64 = 1 << 30;

    fn snap(tick: u64, cores: usize) -> Snapshot {
        Snapshot {
            tick: Tick(tick),
            taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + tick)),
            interval: Duration::from_secs(1),
            cpu: CpuSample {
                total: Percent(25.0),
                cores: (0..cores)
                    .map(|i| LogicalCore {
                        index: i as u32,
                        physical: i as u32 / 2,
                        kind: if i < 2 {
                            CoreKind::Performance
                        } else {
                            CoreKind::Efficiency
                        },
                        usage: Percent(10.0 * i as f32),
                        frequency: None,
                    })
                    .collect(),
                ..CpuSample::default()
            },
            memory: MemorySample {
                total: Bytes(16 * GB),
                available: Bytes(6 * GB),
                committed: Bytes(12 * GB),
                commit_limit: Bytes(20 * GB),
                cached: Bytes(4 * GB),
                paged_pool: Some(Bytes(GB / 2)),
                nonpaged_pool: Some(Bytes(GB / 4)),
                ..MemorySample::default()
            },
            processes: vec![
                crate::process_rows::tests::proc(0, None, 0.0),
                crate::process_rows::tests::proc(4, None, 1.0),
                crate::process_rows::tests::proc(8, Some(4), 2.0),
            ],
            hardware: Arc::new(Hardware {
                cpu_name: Some("Test CPU 9000".to_owned()),
                base_frequency: Some(Hertz::from_mhz(2400)),
                sockets: 1,
                physical_cores: 2,
                logical_processors: cores as u32,
                cache_l2: Some(Bytes(2 << 20)),
                boot_unix_ms: Some(0),
                ..Hardware::default()
            }),
            ..Snapshot::default()
        }
    }

    fn timeline(cores: usize) -> (Timeline, Snapshot) {
        let mut tl = Timeline::new(Retention::raw(100));
        let mut last = snap(1, cores);
        for t in 1..=20 {
            last = snap(t, cores);
            tl.observe(&last);
        }
        (tl, last)
    }

    fn paint(page: &mut PerfPage, cores: usize) -> DisplayList {
        let (tl, s) = timeline(cores);
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 1000.0, 640.0),
            &s,
            &tl,
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

    #[test]
    fn the_cpu_detail_shows_name_counts_and_facts() {
        let mut page = PerfPage::default();
        let t = texts(&paint(&mut page, 4));
        for want in [
            "CPU",
            "Memory",
            "25%",
            "Test CPU 9000",
            "Processes",
            "2",
            "Up time",
            // 1020 s after a boot at the epoch.
            "0:00:17:00",
            "Base speed",
            "2.40 GHz",
            "Logical processors",
            "4",
            "L2 cache",
            "2.00 MB",
        ] {
            assert!(t.iter().any(|s| s == want), "{want} missing from {t:?}");
        }
        assert!(
            !t.iter().any(|s| s == "Speed"),
            "no clock reported, no speed"
        );
        assert!(
            !t.iter().any(|s| s == "L1 cache"),
            "unknown facts are left out"
        );
    }

    #[test]
    fn logical_processors_get_a_chart_each_colored_by_kind() {
        let mut page = PerfPage::default();
        let _ = paint(&mut page, 4);
        let toggle = page.toggle[1].center();
        assert!(
            page.handle(UiEvent::MouseDown {
                at: toggle,
                button: MouseButton::Left
            })
            .repaint
        );
        let dl = paint(&mut page, 4);
        assert_eq!(page.charts().len(), MINI + 4);
        let theme = Theme::dark();
        let lines: Vec<Color> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Polyline { color, .. } => Some(*color),
                _ => None,
            })
            .collect();
        // Two mini charts, then two P-cores and two E-cores.
        assert_eq!(
            lines,
            [
                theme.cpu,
                theme.memory,
                theme.cpu,
                theme.cpu,
                theme.cpu_efficiency,
                theme.cpu_efficiency
            ]
        );
        let t = texts(&dl);
        assert!(
            t.iter().any(|s| s == "30%"),
            "the last core's latest value: {t:?}"
        );
        // Back to one chart.
        let overall = page.toggle[0].center();
        page.handle(UiEvent::MouseDown {
            at: overall,
            button: MouseButton::Left,
        });
        let _ = paint(&mut page, 4);
        assert_eq!(page.charts().len(), MINI + 1);
    }

    #[test]
    fn the_list_and_the_keys_pick_a_device() {
        let mut page = PerfPage::default();
        let _ = paint(&mut page, 2);
        assert_eq!(page.device(), Device::Cpu);
        assert!(page.handle(UiEvent::Key(Key::Down)).repaint);
        assert_eq!(page.device(), Device::Memory);
        assert!(
            !page.handle(UiEvent::Key(Key::Down)).repaint,
            "stops at the end"
        );
        assert!(page.handle(UiEvent::Key(Key::Home)).repaint);
        assert_eq!(page.device(), Device::Cpu);
        let mem = page.list[1].center();
        assert!(
            page.handle(UiEvent::MouseMove(mem)).repaint,
            "hover highlight"
        );
        page.handle(UiEvent::MouseDown {
            at: mem,
            button: MouseButton::Left,
        });
        assert_eq!(page.device(), Device::Memory);

        let t = texts(&paint(&mut page, 2));
        for want in [
            "Memory in use",
            "16.0 GB",
            "Memory composition",
            "In use 10.0 GB",
            "Available 6.00 GB",
            "Committed",
            "12.0 / 20.0 GB",
            "Paged pool",
            "512 MB",
            "Non-paged pool",
            "256 MB",
            "10.0 / 16.0 GB (63%)",
        ] {
            assert!(t.iter().any(|s| s == want), "{want} missing from {t:?}");
        }
        // The graph switch belongs to the CPU; it does nothing here.
        let stale = page.toggle[1];
        assert_eq!(stale, Rect::ZERO);
    }

    #[test]
    fn hovering_the_detail_chart_marks_the_mini_charts_too() {
        let mut page = PerfPage::default();
        let _ = paint(&mut page, 2);
        let big = page.charts().plot(MINI).rect();
        let at = Point::new(charts::AXIS.x(big, 3000.0), big.center().y);
        assert!(page.handle(UiEvent::MouseMove(at)).repaint);
        assert_eq!(page.charts().hover_age(), Some(3000.0));
        let dl = paint(&mut page, 2);
        let dots = dl
            .cmds()
            .iter()
            .filter(|c| matches!(c, DrawCmd::FillRoundRect { rect, .. } if rect.w <= 8.0))
            .count();
        assert_eq!(dots, 3, "a dot on the big chart and on both mini charts");
        assert!(texts(&dl).iter().any(|s| s == "25% · 3 s ago"));
        assert!(page.handle(UiEvent::MouseLeave).repaint);
    }

    #[test]
    fn grids_come_out_wider_than_tall() {
        let area = Rect::new(0.0, 0.0, 800.0, 300.0);
        assert_eq!(grid_shape(area, 1), (1, 1));
        let (c, r) = grid_shape(area, 12);
        assert!(c * r >= 12);
        let first = cell(area, c, r, 0);
        assert!(first.w > first.h, "{first:?} from {c}x{r}");
        let last = cell(area, c, r, c * r - 1);
        assert!((last.right() - area.right()).abs() < 1e-3);
        assert!((last.bottom() - area.bottom()).abs() < 1e-3);
    }
}
