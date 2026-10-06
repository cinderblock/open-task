//! The Performance page.
//!
//! A list of devices down the left, each with a live mini graph and its headline
//! number, and a detail pane for the selected one. The list comes from the
//! snapshot (the CPU, memory, every physical disk, every connected network adapter)
//! so it follows devices as they come and go; a selection whose device disappears
//! falls back to the CPU. Every graph on the page, the mini ones included, shares
//! one hover line.
//!
//! Charts are planned first as [`Slot`]s (which series, where, on what scale), all
//! built, and only then painted, so the hover can snap to the chart under the
//! pointer before any chart draws its line.

use std::fmt::Write as _;
use std::time::UNIX_EPOCH;

use ot_core::{Retention, Series, Snapshot, Timeline};
use ot_model::battery::{BatterySample, BatteryState};
use ot_model::cpu::{CoreKind, ThermalSensor};
use ot_model::device::{AdapterSample, DiskSample, LinkKind};
use ot_model::gpu::GpuSample;
use ot_model::{Bytes, Hertz};
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

use crate::charts::{self, ChartGroup, Charted, ValueFmt, AXIS_BAND_H};
use crate::format;
use crate::sparkline::{self, PlotPoint, TimeAxis};
use crate::theme::Theme;
use crate::view::{Key, MouseButton, Reaction, UiEvent};

/// Something the page can show in detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Device {
    #[default]
    Cpu,
    Memory,
    /// By disk number.
    Disk(u32),
    /// By adapter id.
    Adapter(u64),
    /// By the graphics adapter's id.
    Gpu(u64),
    Battery,
}

/// A plotted line: which of the timeline's series it draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    CpuTotal,
    Core(usize),
    Memory,
    DiskActive(u32),
    DiskRead(u32),
    DiskWrite(u32),
    Rx(u64),
    Tx(u64),
    GpuUtil(u64),
    GpuMemory(u64),
    BatteryCharge,
}

impl Line {
    fn color(self, theme: &Theme) -> Color {
        match self {
            Self::CpuTotal | Self::Core(_) => theme.cpu,
            Self::Memory => theme.memory,
            Self::DiskActive(_) | Self::DiskRead(_) => theme.disk,
            Self::DiskWrite(_) => theme.disk_write,
            Self::Rx(_) => theme.network,
            Self::Tx(_) => theme.network_send,
            Self::GpuUtil(_) => theme.gpu,
            Self::GpuMemory(_) => theme.gpu_memory,
            Self::BatteryCharge => theme.battery,
        }
    }

    fn value_fmt(self) -> ValueFmt {
        match self {
            Self::CpuTotal
            | Self::Core(_)
            | Self::DiskActive(_)
            | Self::GpuUtil(_)
            | Self::BatteryCharge => charts::percent_value,
            Self::Memory | Self::GpuMemory(_) => charts::bytes_value,
            Self::DiskRead(_) | Self::DiskWrite(_) => charts::byte_rate_value,
            Self::Rx(_) | Self::Tx(_) => charts::bit_rate_value,
        }
    }
}

fn series(tl: &Timeline, line: Line) -> Option<&Series> {
    match line {
        Line::CpuTotal => Some(&tl.cpu_total),
        Line::Core(i) => tl.cores.get(i),
        Line::Memory => Some(&tl.mem_in_use),
        Line::DiskActive(n) => tl.disk(n).map(|d| &d.active),
        Line::DiskRead(n) => tl.disk(n).map(|d| &d.read),
        Line::DiskWrite(n) => tl.disk(n).map(|d| &d.write),
        Line::Rx(id) => tl.adapter(id).map(|a| &a.rx),
        Line::Tx(id) => tl.adapter(id).map(|a| &a.tx),
        Line::GpuUtil(id) => tl.gpu(id).map(|g| &g.utilization),
        Line::GpuMemory(id) => tl.gpu(id).map(|g| &g.dedicated),
        Line::BatteryCharge => Some(&tl.battery_charge),
    }
}

/// One chart of this frame: what it draws, where, and on what scale.
#[derive(Debug, Clone, Copy)]
struct Slot {
    line: Line,
    area: Rect,
    band_h: f32,
    max: f32,
}

const LIST_W: f32 = 212.0;
const ITEM_H: f32 = 56.0;
const ITEM_GAP: f32 = 4.0;
const MINI_W: f32 = 64.0;
const HEADER_H: f32 = 36.0;
const CAPTION_H: f32 = 22.0;
/// Room for the headline grid and nine fact lines beside it, which is what the
/// CPU pane lists.
const STATS_H: f32 = 180.0;
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
/// Room for one entry of a chart's legend.
const LEGEND_W: f32 = 78.0;
const SCALE_W: f32 = 110.0;
/// The smallest full scale a rate chart uses, so a quiet device's noise stays flat.
const DISK_FLOOR: f32 = 1024.0 * 1024.0;
const NET_FLOOR_BITS: f32 = 100_000.0;

/// Where the fixed parts go this frame.
#[derive(Debug, Clone, Copy, Default)]
struct Frame {
    list: Rect,
    card: Rect,
    header: Rect,
    caption: Rect,
    /// The detail chart area, label bands included.
    chart: Rect,
    /// Memory only: the composition bar and its legend.
    composition: Rect,
    stats: Rect,
}

fn layout(page: Rect, device: Device, theme: &Theme) -> Frame {
    let (list, right) = page.split_left(LIST_W.min(page.w));
    let (_, card) = right.split_left(theme.gap.min(right.w));
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
    Frame {
        list,
        card,
        header,
        caption,
        chart,
        composition,
        stats,
    }
}

/// A chart area split in two stacked charts with a caption over the lower one:
/// `(upper, lower caption, lower)`.
fn split_pair(chart: Rect, theme: &Theme) -> (Rect, Rect, Rect) {
    let (lower, upper) = chart.split_bottom((chart.h * 0.5).floor());
    let (_, lower) = lower.split_top(theme.gap.min(lower.h));
    let (caption, lower) = lower.split_top(CAPTION_H.min(lower.h));
    (upper, caption, lower)
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

/// The largest value `series` reaches within the last `span_ms`, the time axis's
/// span.
fn peak(series: Option<&Series>, span_ms: f32) -> f32 {
    let Some(s) = series else {
        return 0.0;
    };
    let Some(newest) = s.latest() else {
        return 0.0;
    };
    let mut top = 0.0f32;
    for b in s.history() {
        if (newest.at_unix_ms - b.last_ms) as f32 > span_ms {
            break;
        }
        top = top.max(b.max);
    }
    top
}

/// A full scale for a byte rate: the next 1, 2 or 5 times a power of ten times a
/// power of 1024 at or above `v` (so it prints as `500 KB/s`, `2.00 MB/s`), and at
/// least `floor`.
fn nice_bytes(v: f32, floor: f32) -> f32 {
    let want = v.max(floor);
    let mut unit = 1.0f32;
    for _ in 0..6 {
        for m in [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0] {
            if m * unit >= want {
                return m * unit;
            }
        }
        unit *= 1024.0;
    }
    want
}

/// A full scale for a bit rate: the next 1, 2 or 5 times a power of ten.
fn nice_bits(v: f32, floor: f32) -> f32 {
    let want = v.max(floor);
    let mut unit = 1.0f32;
    for _ in 0..14 {
        for m in [1.0, 2.0, 5.0] {
            if m * unit >= want {
                return m * unit;
            }
        }
        unit *= 10.0;
    }
    want
}

/// What the detail pane needs besides its charts' slots.
#[derive(Debug, Clone, Copy, Default)]
struct Detail {
    /// Index of the detail pane's first chart.
    first: usize,
    /// Per-processor mode: how many charts, and the band under the grid.
    cores: usize,
    grid_band: Rect,
    /// Disk: the caption over the transfer chart.
    second_caption: Rect,
    /// The band under a two-line chart, which this page reads out itself.
    pair_band: Rect,
    /// Full scale of the two-line chart, in its series' units (bytes per second).
    pair_max: f32,
}

#[derive(Debug)]
pub(crate) struct PerfPage {
    device: Device,
    /// The CPU detail shows one chart per logical processor.
    per_core: bool,
    charts: ChartGroup,
    slots: Vec<Slot>,
    /// This frame's devices in list order, where each was painted, and its first
    /// chart and chart count.
    devices: Vec<Device>,
    items: Vec<Rect>,
    minis: Vec<(usize, usize)>,
    detail: Detail,
    /// The list's viewport, for hit-testing and wheel scrolling.
    list_view: Rect,
    /// How far the list is scrolled, in DIPs.
    list_scroll: f32,
    list_hover: Option<Device>,
    /// Scroll the selection into view at the next paint (after a key moved it).
    reveal: bool,
    toggle: [Rect; 2],
    toggle_hover: Option<usize>,
    /// Stands in for a device's series before the timeline has one.
    empty: Series,
    /// The time axis of the frame being painted.
    axis: TimeAxis,
}

impl Default for PerfPage {
    fn default() -> Self {
        Self {
            device: Device::default(),
            per_core: false,
            charts: ChartGroup::default(),
            axis: TimeAxis::DEFAULT,
            slots: Vec::new(),
            devices: Vec::new(),
            items: Vec::new(),
            minis: Vec::new(),
            detail: Detail::default(),
            list_view: Rect::ZERO,
            list_scroll: 0.0,
            list_hover: None,
            reveal: false,
            toggle: [Rect::ZERO; 2],
            toggle_hover: None,
            empty: Series::new(Retention::raw(1)),
        }
    }
}

impl PerfPage {
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
        if i == PAIR_AXIS_LAYER as usize {
            let plot = self.charts.plot(self.detail.first).rect();
            let band = self.detail.pair_band;
            sparkline::paint_axis(dl, plot, band, &self.axis, theme.small, theme.text_dim);
            return;
        }
        let Some(slot) = self.slots.get(i) else {
            return;
        };
        let series = series(tl, slot.line).unwrap_or(&self.empty);
        self.charts.repaint(i, dl, series, theme);
    }

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
            UiEvent::Wheel {
                at,
                lines,
                horizontal: false,
            } if self.list_view.contains(at) => {
                self.list_scroll = (self.list_scroll + lines * (ITEM_H + ITEM_GAP)).max(0.0);
                Reaction::REPAINT
            }
            UiEvent::Key(Key::Up) => self.step(-1),
            UiEvent::Key(Key::Down) => self.step(1),
            UiEvent::Key(Key::Home) => self.step(isize::MIN / 2),
            UiEvent::Key(Key::End) => self.step(isize::MAX / 2),
            _ => Reaction::NONE,
        }
    }

    fn step(&mut self, n: isize) -> Reaction {
        let Some(i) = self.devices.iter().position(|&d| d == self.device) else {
            return Reaction::NONE;
        };
        let j = i.saturating_add_signed(n).min(self.devices.len() - 1);
        self.reveal = true;
        self.select(self.devices[j])
    }

    fn item_at(&self, p: Point) -> Option<Device> {
        if !self.list_view.contains(p) {
            return None;
        }
        self.devices
            .iter()
            .zip(&self.items)
            .find(|(_, r)| r.contains(p))
            .map(|(&d, _)| d)
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

    /// Place the list's entries, scrolled, keeping the scroll in range and, after a
    /// key moved the selection, the selection in view.
    fn lay_out_list(&mut self, view: Rect) {
        self.list_view = view;
        let step = ITEM_H + ITEM_GAP;
        let content = self.devices.len() as f32 * step - ITEM_GAP;
        let max_scroll = (content - view.h).max(0.0);
        if std::mem::take(&mut self.reveal) {
            if let Some(i) = self.devices.iter().position(|&d| d == self.device) {
                let top = i as f32 * step;
                if top < self.list_scroll {
                    self.list_scroll = top;
                } else if top + ITEM_H > self.list_scroll + view.h {
                    self.list_scroll = top + ITEM_H - view.h;
                }
            }
        }
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
        self.items.clear();
        let y0 = view.y - self.list_scroll;
        self.items.extend(
            (0..self.devices.len())
                .map(|i| Rect::new(view.x, y0 + i as f32 * step, view.w, ITEM_H)),
        );
    }

    fn plan(&mut self, line: Line, area: Rect, band_h: f32, max: f32) {
        self.slots.push(Slot {
            line,
            area,
            band_h,
            max,
        });
    }

    /// The mini chart(s) of one list entry.
    fn plan_mini(&mut self, d: Device, area: Rect, snap: &Snapshot, tl: &Timeline) {
        match d {
            Device::Cpu => self.plan(Line::CpuTotal, area, 0.0, 100.0),
            Device::Memory => {
                let total = snap.memory.total.get() as f32;
                self.plan(Line::Memory, area, 0.0, total);
            }
            Device::Disk(n) => self.plan(Line::DiskActive(n), area, 0.0, 100.0),
            Device::Adapter(id) => {
                let max = net_scale(tl, id, self.axis.span_ms);
                self.plan(Line::Rx(id), area, 0.0, max);
                self.plan(Line::Tx(id), area, 0.0, max);
            }
            Device::Gpu(id) => self.plan(Line::GpuUtil(id), area, 0.0, 100.0),
            Device::Battery => self.plan(Line::BatteryCharge, area, 0.0, 100.0),
        }
    }

    /// The detail pane's charts.
    fn plan_detail(&mut self, f: &Frame, snap: &Snapshot, tl: &Timeline, theme: &Theme) {
        let mut detail = Detail {
            first: self.slots.len(),
            ..Detail::default()
        };
        match self.device {
            Device::Cpu if self.per_core && !tl.cores.is_empty() => {
                let (band, grid) = f.chart.split_bottom(AXIS_BAND_H);
                let n = tl.cores.len();
                let (cols, rows) = grid_shape(grid, n);
                for i in 0..n {
                    self.plan(Line::Core(i), cell(grid, cols, rows, i), 0.0, 100.0);
                }
                detail.cores = n;
                detail.grid_band = band;
            }
            Device::Cpu => self.plan(Line::CpuTotal, f.chart, AXIS_BAND_H, 100.0),
            Device::Memory => {
                let total = snap.memory.total.get() as f32;
                self.plan(Line::Memory, f.chart, AXIS_BAND_H, total);
            }
            Device::Disk(n) => {
                let (upper, caption, lower) = split_pair(f.chart, theme);
                self.plan(Line::DiskActive(n), upper, AXIS_BAND_H, 100.0);
                let (band, plot) = lower.split_bottom(AXIS_BAND_H);
                let d = tl.disk(n);
                let span = self.axis.span_ms;
                let top = peak(d.map(|d| &d.read), span).max(peak(d.map(|d| &d.write), span));
                let max = nice_bytes(top, DISK_FLOOR);
                self.plan(Line::DiskRead(n), plot, 0.0, max);
                self.plan(Line::DiskWrite(n), plot, 0.0, max);
                detail.second_caption = caption;
                detail.pair_band = band;
                detail.pair_max = max;
            }
            Device::Adapter(id) => {
                let (band, plot) = f.chart.split_bottom(AXIS_BAND_H);
                let max = net_scale(tl, id, self.axis.span_ms);
                self.plan(Line::Rx(id), plot, 0.0, max);
                self.plan(Line::Tx(id), plot, 0.0, max);
                detail.pair_band = band;
                detail.pair_max = max;
            }
            Device::Gpu(id) => {
                let (upper, caption, lower) = split_pair(f.chart, theme);
                self.plan(Line::GpuUtil(id), upper, AXIS_BAND_H, 100.0);
                // The memory chart's full scale is the adapter's memory, so a half
                // full chart means half the memory; without a known size, the peak.
                let total = snap
                    .gpus
                    .iter()
                    .find(|g| g.info.id == id)
                    .and_then(|g| g.info.dedicated_total)
                    .map_or(0.0, |b| b.get() as f32);
                let max = if total > 0.0 {
                    total
                } else {
                    nice_bytes(
                        peak(tl.gpu(id).map(|g| &g.dedicated), self.axis.span_ms),
                        DISK_FLOOR,
                    )
                };
                self.plan(Line::GpuMemory(id), lower, AXIS_BAND_H, max);
                detail.second_caption = caption;
            }
            Device::Battery => self.plan(Line::BatteryCharge, f.chart, AXIS_BAND_H, 100.0),
        }
        self.detail = detail;
    }

    #[allow(clippy::too_many_lines)]
    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        charted: Charted<'_>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let Charted { timeline: tl, axis } = charted;
        self.axis = axis;
        self.devices.clear();
        self.devices.extend([Device::Cpu, Device::Memory]);
        self.devices
            .extend(snap.disks.iter().map(|d| Device::Disk(d.info.number)));
        self.devices
            .extend(snap.adapters.iter().map(|a| Device::Adapter(a.info.id)));
        // Software adapters (the basic render driver) are not GPUs anyone watches.
        self.devices.extend(
            snap.gpus
                .iter()
                .filter(|g| !g.info.software)
                .map(|g| Device::Gpu(g.info.id)),
        );
        if snap.battery.is_some() {
            self.devices.push(Device::Battery);
        }
        if !self.devices.contains(&self.device) {
            self.device = Device::Cpu;
        }

        let f = layout(rect, self.device, theme);
        self.lay_out_list(f.list);
        if self.device != Device::Cpu {
            self.toggle = [Rect::ZERO; 2];
        }

        // Plan every chart and build them all before painting any, so the hover can
        // snap to the one under the pointer and every other chart marks that moment.
        self.slots.clear();
        self.minis.clear();
        for i in 0..self.devices.len() {
            let r = self.items[i];
            let mini = Rect::new(
                r.x + theme.pad,
                r.y + theme.pad,
                MINI_W,
                r.h - 2.0 * theme.pad,
            );
            let first = self.slots.len();
            self.plan_mini(self.devices[i], mini, snap, tl);
            self.minis.push((first, self.slots.len() - first));
        }
        self.plan_detail(&f, snap, tl, theme);
        self.charts.begin(self.slots.len(), self.axis);
        for (i, s) in self.slots.iter().enumerate() {
            let series = series(tl, s.line).unwrap_or(&self.empty);
            self.charts.build(i, s.area, s.band_h, series, s.max);
        }
        self.charts.snap();

        self.paint_list(dl, snap, theme, buf);
        dl.fill_round_rect(f.card, theme.card_radius, theme.surface);
        dl.stroke_rect(f.card, theme.surface_border, 1.0);
        let first = self.detail.first;
        match self.device {
            Device::Cpu => {
                self.paint_cpu_head(dl, &f, snap, theme);
                if self.detail.cores > 0 {
                    self.paint_core_grid(dl, snap, theme, buf);
                } else {
                    self.paint_slot(first, dl, theme, buf);
                }
                paint_cpu_stats(dl, f.stats, snap, theme, buf);
            }
            Device::Memory => {
                paint_memory_head(dl, &f, snap, theme, buf);
                self.paint_slot(first, dl, theme, buf);
                paint_composition(dl, f.composition, snap, theme, buf);
                paint_memory_stats(dl, f.stats, snap, theme, buf);
            }
            Device::Disk(n) => {
                let Some(d) = snap.disks.iter().find(|d| d.info.number == n) else {
                    return;
                };
                head(dl, &f, &d.info.name, d.info.model.as_deref(), theme);
                dl.label("Active time", f.caption, theme.small, theme.text_dim);
                self.paint_slot(first, dl, theme, buf);
                let caption = self.detail.second_caption;
                self.paint_pair(dl, first + 1, caption, ["Read", "Write"], theme, buf);
                paint_disk_stats(dl, f.stats, d, snap, theme, buf);
            }
            Device::Adapter(id) => {
                let Some(a) = snap.adapters.iter().find(|a| a.info.id == id) else {
                    return;
                };
                head(dl, &f, &a.info.name, Some(&a.info.adapter), theme);
                self.paint_pair(dl, first, f.caption, ["Receive", "Send"], theme, buf);
                paint_adapter_stats(dl, f.stats, a, theme, buf);
            }
            Device::Gpu(id) => {
                let Some(g) = snap.gpus.iter().find(|g| g.info.id == id) else {
                    return;
                };
                head(dl, &f, &g.info.name, Some(&g.info.adapter), theme);
                dl.label("Utilization", f.caption, theme.small, theme.text_dim);
                self.paint_slot(first, dl, theme, buf);
                let caption = self.detail.second_caption;
                dl.label("Dedicated GPU memory", caption, theme.small, theme.text_dim);
                self.paint_slot(first + 1, dl, theme, buf);
                paint_gpu_stats(dl, f.stats, g, theme, buf);
            }
            Device::Battery => {
                let Some(b) = &snap.battery else {
                    return;
                };
                head(dl, &f, "Battery", b.manufacturer.as_deref(), theme);
                dl.label("Charge", f.caption, theme.small, theme.text_dim);
                self.paint_slot(first, dl, theme, buf);
                paint_battery_stats(dl, f.stats, b, theme, buf);
            }
        }
    }

    /// Paint one planned chart in its line's color. Returns the point under the
    /// crosshair.
    fn paint_slot(
        &mut self,
        i: usize,
        dl: &mut DisplayList,
        theme: &Theme,
        buf: &mut String,
    ) -> Option<PlotPoint> {
        let line = self.slots[i].line;
        let style = charts::style(line.color(theme), theme);
        self.charts
            .paint(i, dl, &style, line.value_fmt(), theme, buf)
    }

    fn paint_list(
        &mut self,
        dl: &mut DisplayList,
        snap: &Snapshot,
        theme: &Theme,
        buf: &mut String,
    ) {
        dl.push_clip(self.list_view);
        for i in 0..self.devices.len() {
            let (d, r) = (self.devices[i], self.items[i]);
            if r.bottom() < self.list_view.y || r.y > self.list_view.bottom() {
                continue;
            }
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
            let (first, count) = self.minis[i];
            for k in first..first + count {
                self.paint_slot(k, dl, theme, buf);
            }
            let mini = self.slots[first].area;
            let edge = self.slots[first].line.color(theme).with_alpha(0.45);
            dl.stroke_rect(mini, edge, 1.0);

            let left = mini.right() + theme.pad;
            let text = Rect::new(
                left,
                r.y + 8.0,
                (r.right() - left - theme.pad).max(0.0),
                r.h - 16.0,
            );
            let (title, value) = text.split_top(text.h * 0.5);
            dl.label(title_of(d, snap), title, theme.cell, theme.text);
            summary(d, snap, buf);
            dl.label(buf, value, theme.small, theme.text_dim);
        }
        dl.pop_clip();
    }

    fn paint_cpu_head(&mut self, dl: &mut DisplayList, f: &Frame, snap: &Snapshot, theme: &Theme) {
        head(dl, f, "CPU", snap.hardware.cpu_name.as_deref(), theme);

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
        theme: &Theme,
        buf: &mut String,
    ) {
        let hovering = self.charts.crosshair();
        let first = self.detail.first;
        for i in 0..self.detail.cores {
            let core = snap.cpu.cores.get(i);
            let color = if core.is_some_and(|c| c.kind == CoreKind::Efficiency) {
                theme.cpu_efficiency
            } else {
                theme.cpu
            };
            let style = charts::style(color, theme);
            let point = self
                .charts
                .paint(first + i, dl, &style, charts::percent_value, theme, buf);
            let r = self.charts.plot(first + i).rect();
            dl.stroke_rect(r, theme.grid, 1.0);
            if r.h >= 28.0 {
                let value = if hovering.is_some() {
                    point.map(|p| p.mean)
                } else {
                    core.map(|c| c.usage.get())
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
        let band = self.detail.grid_band;
        match hovering {
            Some(c) => {
                c.ago(buf);
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

    /// Two lines on one chart (read and write, receive and send): a caption with the
    /// full scale and a legend above, both lines, and one readout for both below.
    fn paint_pair(
        &mut self,
        dl: &mut DisplayList,
        first: usize,
        caption: Rect,
        names: [&str; 2],
        theme: &Theme,
        buf: &mut String,
    ) {
        let lines = [self.slots[first].line, self.slots[first + 1].line];
        let fmt = lines[0].value_fmt();

        // Caption, then from the right: the legend, the full scale.
        let label = if matches!(lines[0], Line::Rx(_)) {
            "Throughput"
        } else {
            "Transfer rate"
        };
        let (left, legend) = caption.split_left((caption.w - LEGEND_W * 2.0).max(0.0));
        let (text, scale) = left.split_left((left.w - SCALE_W).max(0.0));
        dl.label(label, text, theme.small, theme.text_dim);
        fmt(buf, self.detail.pair_max);
        dl.text(
            buf,
            scale,
            theme.small,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );
        for (k, (name, line)) in names.iter().zip(lines).enumerate() {
            let x = legend.x + theme.pad + k as f32 * LEGEND_W;
            let mark = Rect::new(x, legend.center().y - 1.0, 12.0, 2.0);
            dl.fill_rect(mark, line.color(theme));
            let r = Rect::new(x + 16.0, legend.y, LEGEND_W - 16.0, legend.h);
            dl.label(name, r, theme.small, theme.text_dim);
        }

        let points = [
            self.paint_slot(first, dl, theme, buf),
            self.paint_slot(first + 1, dl, theme, buf),
        ];
        let plot = self.charts.plot(first).rect();
        let band = self.detail.pair_band;
        let Some(c) = self.charts.crosshair() else {
            // The labels under a pair move with the clock while the span grows.
            dl.begin_layer(PAIR_AXIS_LAYER);
            sparkline::paint_axis(dl, plot, band, &self.axis, theme.small, theme.text_dim);
            dl.end_layer();
            return;
        };
        let mut values = String::with_capacity(48);
        for (name, p) in names.iter().zip(points) {
            if let Some(p) = p {
                fmt(buf, p.mean);
                if !values.is_empty() {
                    values.push_str(" \u{b7} ");
                }
                let _ = write!(values, "{name} {buf}");
            }
        }
        c.ago(buf);
        let mut text = String::with_capacity(64);
        charts::readout(&mut text, &values, buf, c.side);
        let x = self.axis.x(plot, c.age_ms);
        sparkline::paint_readout(dl, band, x, c.side, &text, theme.small, theme.text);
    }
}

/// The layer of the time labels under a pair of charts; the charts' own are their
/// indexes.
const PAIR_AXIS_LAYER: u32 = u32::MAX;

/// The full scale of an adapter's chart, in bytes per second, from its busiest
/// moment in the last `span_ms`.
pub(crate) fn net_scale(tl: &Timeline, id: u64, span_ms: f32) -> f32 {
    let a = tl.adapter(id);
    let top = peak(a.map(|a| &a.rx), span_ms).max(peak(a.map(|a| &a.tx), span_ms));
    nice_bits(top * 8.0, NET_FLOOR_BITS) / 8.0
}

/// The detail pane's title, with a subtitle on the right (a model, an adapter).
fn head(dl: &mut DisplayList, f: &Frame, title: &str, subtitle: Option<&str>, theme: &Theme) {
    let (left, right) = f.header.split_left((f.header.w * 0.5).max(0.0));
    dl.label(title, left, theme.big, theme.text);
    if let Some(s) = subtitle {
        dl.text(
            s,
            right,
            theme.cell,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );
    }
}

fn title_of(d: Device, snap: &Snapshot) -> &str {
    match d {
        Device::Cpu => "CPU",
        Device::Memory => "Memory",
        Device::Disk(n) => snap
            .disks
            .iter()
            .find(|d| d.info.number == n)
            .map_or("Disk", |d| d.info.name.as_str()),
        Device::Adapter(id) => snap
            .adapters
            .iter()
            .find(|a| a.info.id == id)
            .map_or("Network", |a| a.info.name.as_str()),
        Device::Gpu(id) => snap
            .gpus
            .iter()
            .find(|g| g.info.id == id)
            .map_or("GPU", |g| g.info.name.as_str()),
        Device::Battery => "Battery",
    }
}

/// The headline under a device's name in the list.
fn summary(d: Device, snap: &Snapshot, buf: &mut String) {
    buf.clear();
    if snap.is_empty() {
        return;
    }
    let mut tmp = String::new();
    match d {
        Device::Cpu => {
            format::percent(buf, snap.cpu.total.get());
            buf.push('%');
            if let Some(hz) = current_clock(snap) {
                format::clock(&mut tmp, hz);
                let _ = write!(buf, "  {tmp}");
            }
            // The temperature fits the line; the power is on the pane.
            if let Some(c) = snap.cpu.hotspot_celsius {
                format::celsius(&mut tmp, c);
                let _ = write!(buf, "  {tmp}");
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
        Device::Disk(n) => {
            if let Some(disk) = snap.disks.iter().find(|x| x.info.number == n) {
                match disk.info.ssd {
                    Some(true) => buf.push_str("SSD  "),
                    Some(false) => buf.push_str("HDD  "),
                    None => {}
                }
                format::percent(&mut tmp, disk.active.get());
                let _ = write!(buf, "{tmp}%");
            }
        }
        Device::Adapter(id) => {
            if let Some(a) = snap.adapters.iter().find(|x| x.info.id == id) {
                charts::bit_rate_value(&mut tmp, a.rx_per_sec.get() as f32);
                let _ = write!(buf, "\u{2193} {tmp}  ");
                charts::bit_rate_value(&mut tmp, a.tx_per_sec.get() as f32);
                let _ = write!(buf, "\u{2191} {tmp}");
            }
        }
        Device::Gpu(id) => {
            if let Some(g) = snap.gpus.iter().find(|x| x.info.id == id) {
                format::percent(buf, g.utilization.get());
                buf.push('%');
                if let Some(total) = g.info.dedicated_total.filter(|t| t.get() > 0) {
                    format::bytes_of(&mut tmp, g.dedicated_used, total);
                    let _ = write!(buf, "  {tmp}");
                }
            }
        }
        Device::Battery => {
            if let Some(b) = &snap.battery {
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
            }
        }
    }
}

/// The average clock across logical processors, when the probe reports clocks.
pub(crate) fn current_clock(snap: &Snapshot) -> Option<Hertz> {
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

/// The slot at `col`, `row` of the headline grid.
fn slot(stats: Rect, col: usize, row: usize) -> Rect {
    Rect::new(
        stats.x + col as f32 * STAT_W,
        stats.y + row as f32 * STAT_H,
        STAT_W - 8.0,
        STAT_H,
    )
}

/// Label-and-value lines to the right of the headline grid, where there is room.
/// The value is written into `text` before each [`Facts::line`]; the lines are
/// laid out by [`Facts::paint`] once they are all known, in one column when they
/// fit, in two when they do not and the pane is wide enough, and the rest are left
/// out.
struct Facts {
    rect: Rect,
    /// The value being written, for the next line.
    text: String,
    /// Every line's label and value, as ranges into `store`.
    lines: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
    store: String,
}

/// The least a facts column needs: its label and a short value.
const FACT_COL_MIN_W: f32 = FACT_LABEL_W + 60.0;

impl Facts {
    /// Right of `cols` headline columns; `None` if the pane is too narrow for them.
    fn beside(stats: Rect, cols: usize, theme: &Theme) -> Option<Self> {
        let (_, rect) = stats.split_left(cols as f32 * STAT_W + theme.gap);
        (rect.w >= FACT_COL_MIN_W).then(|| Self {
            rect,
            text: String::with_capacity(32),
            lines: Vec::with_capacity(12),
            store: String::with_capacity(256),
        })
    }

    /// Add a line: `label`, then what `text` holds.
    fn line(&mut self, label: &str) {
        let l = self.store.len()..self.store.len() + label.len();
        self.store.push_str(label);
        let v = self.store.len()..self.store.len() + self.text.len();
        self.store.push_str(&self.text);
        self.lines.push((l, v));
    }

    fn word(&mut self, label: &str, value: &str) {
        self.text.clear();
        self.text.push_str(value);
        self.line(label);
    }

    fn paint(self, dl: &mut DisplayList, theme: &Theme) {
        let rows = ((self.rect.h / FACT_H).floor().max(1.0)) as usize;
        let two = self.lines.len() > rows && self.rect.w >= 2.0 * FACT_COL_MIN_W;
        let col_w = if two { self.rect.w * 0.5 } else { self.rect.w };
        let shown = if two { rows * 2 } else { rows };
        for (i, (l, v)) in self.lines.iter().take(shown).enumerate() {
            let (col, row) = (i / rows, i % rows);
            let r = Rect::new(
                self.rect.x + col as f32 * col_w,
                self.rect.y + row as f32 * FACT_H,
                col_w,
                FACT_H,
            );
            let (lr, vr) = r.split_left(FACT_LABEL_W.min(r.w));
            dl.label(&self.store[l.clone()], lr, theme.small, theme.text_dim);
            dl.label(&self.store[v.clone()], vr, theme.small, theme.text);
        }
    }
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
    if let Some(w) = snap.cpu.package_power {
        format::watts(buf, w.0);
        stat(dl, slot(stats, 1, 2), "Power", buf, theme);
    }
    if let Some(c) = snap.cpu.hotspot_celsius {
        format::celsius(buf, c);
        let label = hw
            .thermal_sensor
            .map_or("Temperature", ThermalSensor::label);
        stat(dl, slot(stats, 2, 2), label, buf, theme);
    }

    let Some(mut facts) = Facts::beside(stats, 3, theme) else {
        return;
    };
    if let Some(hz) = hw.base_frequency {
        format::clock(&mut facts.text, hz);
        facts.line("Base speed");
    }
    if hw.sockets > 0 {
        format::count(&mut facts.text, hw.sockets);
        facts.line("Sockets");
    }
    if hw.physical_cores > 0 {
        format::count(&mut facts.text, hw.physical_cores);
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
            let _ = write!(facts.text, " ({p} P + {e} E threads)");
        }
        facts.line("Cores");
    }
    let logical = if hw.logical_processors > 0 {
        hw.logical_processors
    } else {
        snap.cpu.cores.len() as u32
    };
    format::count(&mut facts.text, logical);
    facts.line("Logical processors");
    if let Some(on) = hw.virtualization {
        let value = if on { "Enabled" } else { "Disabled" };
        facts.word("Virtualization", value);
    }
    if let Some(running) = hw.hypervisor {
        let value = if running { "Running" } else { "No" };
        facts.word("Hypervisor", value);
    }
    for (name, size) in [
        ("L1 cache", hw.cache_l1),
        ("L2 cache", hw.cache_l2),
        ("L3 cache", hw.cache_l3),
    ] {
        if let Some(b) = size {
            format::bytes(&mut facts.text, b);
            facts.line(name);
        }
    }
    facts.paint(dl, theme);
}

fn paint_memory_head(
    dl: &mut DisplayList,
    f: &Frame,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    let total = (snap.memory.total.get() > 0).then(|| {
        format::bytes(buf, snap.memory.total);
        buf.as_str()
    });
    head(dl, f, "Memory", total, theme);
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
    let (parts, n) = composition(m, theme);
    let parts = &parts[..n];
    let (caption, below) = rect.split_top(18.0);
    let (bar, legend) = below.split_top(18.0);
    dl.label("Memory composition", caption, theme.small, theme.text_dim);
    // The lists are read a moment apart from the total, so they can add up to a
    // little more than it; scale by whichever is larger so the bar never overflows.
    let sum: u64 = parts.iter().map(|p| p.1.get()).sum();
    let total = m.total.get().max(sum) as f32;
    let mut x = bar.x;
    for &(_, bytes, color) in parts {
        let w = bar.w * (bytes.get() as f32 / total).clamp(0.0, 1.0);
        dl.fill_rect(Rect::new(x, bar.y, w, bar.h), color);
        x += w;
    }
    dl.stroke_rect(bar, theme.memory.with_alpha(0.6), 1.0);

    let (_, legend) = legend.split_top(4.0);
    let item_w = legend.w / parts.len() as f32;
    let mut x = legend.x;
    let mut text = String::with_capacity(24);
    for &(label, bytes, color) in parts {
        let swatch = Rect::new(x, legend.y + (legend.h - 8.0) * 0.5, 8.0, 8.0);
        dl.fill_rect(swatch, color);
        dl.stroke_rect(swatch, theme.memory.with_alpha(0.6), 1.0);
        format::bytes(buf, bytes);
        text.clear();
        let _ = write!(text, "{label} {buf}");
        let r = Rect::new(x + 12.0, legend.y, item_w - 16.0, legend.h);
        dl.label(&text, r, theme.small, theme.text_dim);
        x += item_w;
    }
}

/// The segments of the composition bar, left to right, and how many there are.
/// With the memory lists: in use (less modified), modified, standby, free, the
/// division Task Manager draws. Without them: in use and available.
fn composition(
    m: &ot_model::memory::MemorySample,
    theme: &Theme,
) -> ([(&'static str, Bytes, Color); 4], usize) {
    let empty = ("", Bytes::ZERO, Color::TRANSPARENT);
    match (m.modified, m.standby, m.free) {
        (Some(modified), Some(standby), Some(free)) => (
            [
                ("In use", m.in_use().saturating_sub(modified), theme.memory),
                ("Modified", modified, theme.memory_modified),
                ("Standby", standby, theme.memory.with_alpha(0.4)),
                ("Free", free, theme.memory.with_alpha(0.1)),
            ],
            4,
        ),
        _ => (
            [
                ("In use", m.in_use(), theme.memory),
                ("Available", m.available, theme.memory.with_alpha(0.22)),
                empty,
                empty,
            ],
            2,
        ),
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

    let Some(mut facts) = Facts::beside(stats, 3, theme) else {
        return;
    };
    let hw = &snap.hardware;
    if let Some(mts) = hw.memory_speed_mts {
        facts.text.clear();
        let _ = write!(facts.text, "{mts} MT/s");
        facts.line("Speed");
    }
    match (hw.memory_slots_used, hw.memory_slots) {
        (Some(used), Some(total)) => {
            facts.text.clear();
            let _ = write!(facts.text, "{used} of {total}");
            facts.line("Slots used");
        }
        (Some(used), None) => {
            format::count(&mut facts.text, used);
            facts.line("Slots used");
        }
        _ => {}
    }
    if let Some(ff) = &hw.memory_form_factor {
        facts.word("Form factor", ff);
    }
    // What the firmware has that Windows does not get: memory-mapped devices,
    // the integrated GPU's share, and the like.
    if let Some(installed) = hw.installed_memory.filter(|i| i.get() > m.total.get()) {
        format::bytes(&mut facts.text, Bytes(installed.get() - m.total.get()));
        facts.line("Hardware reserved");
    }
    if let Some(c) = m.compressed {
        format::bytes(&mut facts.text, c);
        facts.line("Compressed");
    }
    facts.paint(dl, theme);
}

fn paint_disk_stats(
    dl: &mut DisplayList,
    stats: Rect,
    d: &DiskSample,
    snap: &Snapshot,
    theme: &Theme,
    buf: &mut String,
) {
    charts::percent_value(buf, d.active.get());
    stat(dl, slot(stats, 0, 0), "Active time", buf, theme);
    if let Some(ms) = d.response_ms {
        format::ms(buf, ms);
        stat(dl, slot(stats, 1, 0), "Average response time", buf, theme);
    }
    format::bytes_per_sec(buf, d.read_per_sec);
    stat(dl, slot(stats, 0, 1), "Read speed", buf, theme);
    format::bytes_per_sec(buf, d.write_per_sec);
    stat(dl, slot(stats, 1, 1), "Write speed", buf, theme);

    let Some(mut facts) = Facts::beside(stats, 2, theme) else {
        return;
    };
    if let Some(c) = d.info.capacity {
        format::bytes(&mut facts.text, c);
        facts.line("Capacity");
    }
    if let Some(ssd) = d.info.ssd {
        facts.word("Type", if ssd { "SSD" } else { "HDD" });
    }
    if d.info.removable {
        facts.word("Removable", "Yes");
    }
    if let Some(bus) = &d.info.bus {
        facts.word("Bus", bus);
    }
    // The volumes on this disk, two lines each: `C: Windows   250 GB free of
    // 500 GB`, then what it is and what it holds, `NTFS - System, Page file`.
    let mut name = String::new();
    let mut tmp = String::new();
    for v in snap
        .volumes
        .iter()
        .filter(|v| v.disk == Some(d.info.number))
    {
        name.clear();
        name.push_str(v.mount.trim_end_matches(['\\', '/']));
        if let Some(label) = v.label.as_deref().filter(|l| !l.is_empty()) {
            let _ = write!(name, " {label}");
        }
        format::bytes(&mut facts.text, v.free);
        format::bytes(&mut tmp, v.total);
        let _ = write!(facts.text, " free of {tmp}");
        facts.line(&name);

        facts.text.clear();
        if let Some(fs) = &v.filesystem {
            facts.text.push_str(fs);
        }
        let roles = match (v.system, v.page_file) {
            (true, true) => "System, Page file",
            (true, false) => "System",
            (false, true) => "Page file",
            (false, false) => "",
        };
        if !roles.is_empty() {
            if !facts.text.is_empty() {
                facts.text.push_str(" \u{b7} ");
            }
            facts.text.push_str(roles);
        }
        if !facts.text.is_empty() {
            facts.line("");
        }
    }
    facts.paint(dl, theme);
}

fn paint_adapter_stats(
    dl: &mut DisplayList,
    stats: Rect,
    a: &AdapterSample,
    theme: &Theme,
    buf: &mut String,
) {
    charts::bit_rate_value(buf, a.rx_per_sec.get() as f32);
    stat(dl, slot(stats, 0, 0), "Receive", buf, theme);
    charts::bit_rate_value(buf, a.tx_per_sec.get() as f32);
    stat(dl, slot(stats, 1, 0), "Send", buf, theme);

    let Some(mut facts) = Facts::beside(stats, 2, theme) else {
        return;
    };
    let kind = match a.info.kind {
        LinkKind::Ethernet => "Ethernet",
        LinkKind::WiFi => "Wi-Fi",
        LinkKind::Cellular => "Cellular",
        LinkKind::Virtual => "Tunnel",
        LinkKind::Other => "Other",
    };
    facts.word("Connection type", kind);
    let hardware = if a.info.hardware {
        "Physical"
    } else {
        "Virtual"
    };
    facts.word("Adapter", hardware);
    if let Some(bps) = a.link_bps {
        format::bits(&mut facts.text, bps as f64);
        facts.line("Link speed");
    }
    // Two of each family: a machine with many addresses is a server, and its
    // administrator has other tools.
    for ip in a.info.addresses.iter().filter(|ip| ip.is_ipv4()).take(2) {
        facts.text.clear();
        let _ = write!(facts.text, "{ip}");
        facts.line("IPv4 address");
    }
    for ip in a.info.addresses.iter().filter(|ip| ip.is_ipv6()).take(2) {
        facts.text.clear();
        let _ = write!(facts.text, "{ip}");
        facts.line("IPv6 address");
    }
    if let Some(dns) = &a.info.dns_suffix {
        facts.word("DNS suffix", dns);
    }
    if let Some(mac) = &a.info.mac {
        facts.word("MAC address", mac);
    }
    facts.paint(dl, theme);
}

fn paint_gpu_stats(
    dl: &mut DisplayList,
    stats: Rect,
    g: &GpuSample,
    theme: &Theme,
    buf: &mut String,
) {
    charts::percent_value(buf, g.utilization.get());
    stat(dl, slot(stats, 0, 0), "Utilization", buf, theme);
    match g.info.dedicated_total.filter(|t| t.get() > 0) {
        Some(total) => format::bytes_of(buf, g.dedicated_used, total),
        None => format::bytes(buf, g.dedicated_used),
    }
    stat(dl, slot(stats, 1, 0), "Dedicated memory", buf, theme);
    match g.info.shared_total.filter(|t| t.get() > 0) {
        Some(total) => format::bytes_of(buf, g.shared_used, total),
        None => format::bytes(buf, g.shared_used),
    }
    stat(dl, slot(stats, 2, 0), "Shared memory", buf, theme);
    // The busiest engines, so "3D 40%, Video Decode 12%" reads off the pane.
    for (i, e) in g.engines.iter().take(3).enumerate() {
        charts::percent_value(buf, e.usage.get());
        stat(dl, slot(stats, i, 1), &e.name, buf, theme);
    }

    let Some(mut facts) = Facts::beside(stats, 3, theme) else {
        return;
    };
    if let Some(v) = &g.info.driver_version {
        facts.word("Driver version", v);
    }
    if let Some(d) = &g.info.driver_date {
        facts.word("Driver date", d);
    }
    if let Some(l) = &g.info.location {
        facts.word("Location", l);
    }
    facts.paint(dl, theme);
}

fn paint_battery_stats(
    dl: &mut DisplayList,
    stats: Rect,
    b: &BatterySample,
    theme: &Theme,
    buf: &mut String,
) {
    let charging = b.state == BatteryState::Charging;
    if let Some(c) = b.charge {
        charts::percent_value(buf, c);
        stat(dl, slot(stats, 0, 0), "Charge", buf, theme);
    }
    let doing = b.state.label();
    if !doing.is_empty() {
        buf.clear();
        buf.push_str(doing);
        stat(dl, slot(stats, 1, 0), "State", buf, theme);
    }
    if let Some(w) = b.rate {
        format::watts(buf, w.0);
        let label = if charging {
            "Charging at"
        } else {
            "Power draw"
        };
        stat(dl, slot(stats, 2, 0), label, buf, theme);
    }
    if let Some(t) = b.time_left {
        format::hms(buf, t.as_secs());
        let label = if charging {
            "Time to full"
        } else {
            "Time left"
        };
        stat(dl, slot(stats, 0, 1), label, buf, theme);
    }
    if let Some(ac) = b.ac_power {
        buf.clear();
        buf.push_str(if ac { "Plugged in" } else { "On battery" });
        stat(dl, slot(stats, 1, 1), "Power source", buf, theme);
    }

    let Some(mut facts) = Facts::beside(stats, 3, theme) else {
        return;
    };
    if let Some(mwh) = b.full_capacity_mwh {
        facts.text.clear();
        let _ = write!(facts.text, "{:.1} Wh", f64::from(mwh) / 1000.0);
        facts.line("Full capacity");
    }
    if let Some(mwh) = b.design_capacity_mwh {
        facts.text.clear();
        let _ = write!(facts.text, "{:.1} Wh", f64::from(mwh) / 1000.0);
        facts.line("Design capacity");
    }
    if let (Some(full), Some(design)) = (b.full_capacity_mwh, b.design_capacity_mwh) {
        if design > 0 {
            facts.text.clear();
            let health = f64::from(full) / f64::from(design) * 100.0;
            let _ = write!(facts.text, "{health:.0}%");
            facts.line("Health");
        }
    }
    if let Some(n) = b.cycle_count {
        format::count(&mut facts.text, n);
        facts.line("Cycle count");
    }
    if let Some(c) = &b.chemistry {
        facts.word("Chemistry", c);
    }
    if let Some(m) = &b.manufacturer {
        facts.word("Manufacturer", m);
    }
    facts.paint(dl, theme);
}

/// The synthetic machine the Performance tests paint, shared with the Summary
/// page's tests: a snapshot and twenty seconds of its history, with whichever
/// devices a test asks for.
#[cfg(test)]
#[allow(unused_must_use)] // tests drive the page and ignore most reactions
pub(crate) mod tests {
    use super::*;
    use ot_model::cpu::{CpuSample, LogicalCore};
    use ot_model::device::{AdapterInfo, DiskInfo, VolumeSample};
    use ot_model::gpu::{EngineSample, GpuInfo};
    use ot_model::hardware::Hardware;
    use ot_model::memory::MemorySample;
    use ot_model::{Percent, Tick, Watts};
    use ot_paint::DrawCmd;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    pub(crate) const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;

    pub(crate) fn disk(n: u32) -> DiskSample {
        DiskSample {
            info: Arc::new(DiskInfo {
                number: n,
                name: format!("Disk {n} (C:)"),
                model: Some("Test SSD 1TB".to_owned()),
                ssd: Some(true),
                capacity: Some(Bytes(1000 * GB)),
                removable: false,
                bus: None,
            }),
            active: Percent(12.0),
            read_per_sec: Bytes(3 << 20),
            write_per_sec: Bytes(1 << 20),
            response_ms: Some(0.4),
        }
    }

    pub(crate) fn adapter(id: u64, name: &str) -> AdapterSample {
        AdapterSample {
            info: Arc::new(AdapterInfo {
                id,
                name: name.to_owned(),
                adapter: "Test Ethernet Controller".to_owned(),
                kind: LinkKind::Ethernet,
                hardware: true,
                addresses: vec!["192.168.1.10".parse().unwrap(), "fe80::1".parse().unwrap()],
                dns_suffix: Some("lan".to_owned()),
                ..AdapterInfo::default()
            }),
            rx_per_sec: Bytes(700_000),
            tx_per_sec: Bytes(12_500),
            link_bps: Some(1_000_000_000),
        }
    }

    pub(crate) fn gpu(id: u64) -> GpuSample {
        GpuSample {
            info: Arc::new(GpuInfo {
                id,
                name: format!("GPU {id}"),
                adapter: "Test Graphics 4000".to_owned(),
                dedicated_total: Some(Bytes(8 * GB)),
                shared_total: Some(Bytes(16 * GB)),
                driver_version: Some("31.0.15".to_owned()),
                ..GpuInfo::default()
            }),
            utilization: Percent(42.0),
            engines: vec![
                EngineSample {
                    name: "3D".into(),
                    usage: Percent(42.0),
                },
                EngineSample {
                    name: "Copy".into(),
                    usage: Percent(3.0),
                },
            ],
            dedicated_used: Bytes(2 * GB),
            shared_used: Bytes(GB),
        }
    }

    pub(crate) fn battery() -> BatterySample {
        BatterySample {
            charge: Some(80.0),
            state: BatteryState::Discharging,
            rate: Some(Watts(12.5)),
            time_left: Some(Duration::from_secs(3 * 3600 + 600)),
            ac_power: Some(false),
            full_capacity_mwh: Some(50_000),
            design_capacity_mwh: Some(60_000),
            cycle_count: Some(120),
            manufacturer: Some("Test Cells".to_owned()),
            chemistry: Some("LiP".to_owned()),
        }
    }

    fn volume(disk: u32) -> VolumeSample {
        VolumeSample {
            mount: "C:\\".to_owned(),
            label: Some("Windows".to_owned()),
            filesystem: Some("NTFS".to_owned()),
            total: Bytes(500 * GB),
            free: Bytes(250 * GB),
            disk: Some(disk),
            system: true,
            page_file: true,
        }
    }

    pub(crate) fn snap(tick: u64, cores: usize) -> Snapshot {
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
                virtualization: Some(true),
                hypervisor: Some(false),
                installed_memory: Some(Bytes(16 * GB + 512 * MB)),
                memory_speed_mts: Some(3200),
                memory_slots: Some(4),
                memory_slots_used: Some(2),
                memory_form_factor: Some("SODIMM".to_owned()),
                ..Hardware::default()
            }),
            ..Snapshot::default()
        }
    }

    /// Everything a snapshot can list beyond the CPU and memory.
    #[derive(Debug, Default)]
    pub(crate) struct Devices {
        pub disks: Vec<DiskSample>,
        pub adapters: Vec<AdapterSample>,
        pub gpus: Vec<GpuSample>,
        pub battery: Option<BatterySample>,
        pub volumes: Vec<VolumeSample>,
    }

    /// Twenty seconds of the same snapshot, with the given devices.
    pub(crate) fn timeline_of(cores: usize, devices: &Devices) -> (Timeline, Snapshot) {
        let mut tl = Timeline::new(Retention::raw(100));
        let mut last = snap(1, cores);
        for t in 1..=20 {
            last = snap(t, cores);
            last.disks.clone_from(&devices.disks);
            last.adapters.clone_from(&devices.adapters);
            last.gpus.clone_from(&devices.gpus);
            last.battery.clone_from(&devices.battery);
            last.volumes.clone_from(&devices.volumes);
            tl.observe(&last);
        }
        (tl, last)
    }

    /// Twenty seconds of the same snapshot, with the given disks and adapters.
    fn timeline_with(
        cores: usize,
        disks: &[DiskSample],
        adapters: &[AdapterSample],
    ) -> (Timeline, Snapshot) {
        timeline_of(
            cores,
            &Devices {
                disks: disks.to_vec(),
                adapters: adapters.to_vec(),
                volumes: disks.iter().map(|d| volume(d.info.number)).collect(),
                ..Devices::default()
            },
        )
    }

    fn timeline(cores: usize) -> (Timeline, Snapshot) {
        timeline_with(cores, &[], &[])
    }

    fn paint_with(page: &mut PerfPage, tl: &Timeline, s: &Snapshot) -> DisplayList {
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 1000.0, 640.0),
            s,
            Charted::still(tl),
            &Theme::dark(),
            &mut buf,
        );
        assert_eq!(dl.clip_depth(), 0);
        dl
    }

    fn paint(page: &mut PerfPage, cores: usize) -> DisplayList {
        let (tl, s) = timeline(cores);
        paint_with(page, &tl, &s)
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
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        assert!(!has(&t, "Speed"), "no clock reported, no speed");
        assert!(!has(&t, "L1 cache"), "unknown facts are left out");
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
        assert_eq!(page.charts().len(), 2 + 4);
        let theme = Theme::dark();
        // Two mini charts, then two P-cores and two E-cores.
        assert_eq!(
            line_colors(&dl),
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
        assert!(has(&t, "30%"), "the last core's latest value: {t:?}");
        let overall = page.toggle[0].center();
        page.handle(UiEvent::MouseDown {
            at: overall,
            button: MouseButton::Left,
        });
        let _ = paint(&mut page, 4);
        assert_eq!(page.charts().len(), 2 + 1);
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
        let mem = page.items[1].center();
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
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        // The graph switch belongs to the CPU.
        assert_eq!(page.toggle[1], Rect::ZERO);
    }

    #[test]
    fn with_the_memory_lists_the_bar_splits_four_ways_and_the_clock_shows() {
        let (tl, mut s) = timeline(2);
        s.memory.modified = Some(Bytes(GB));
        s.memory.standby = Some(Bytes(4 * GB));
        s.memory.free = Some(Bytes(2 * GB));
        for c in &mut s.cpu.cores {
            c.frequency = Some(Hertz::from_mhz(3000));
        }
        let mut page = PerfPage::default();
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(has(&t, "25%  3.00 GHz"), "list headline: {t:?}");
        assert!(has(&t, "Speed") && has(&t, "3.00 GHz"));

        page.handle(UiEvent::Key(Key::Down));
        let t = texts(&paint_with(&mut page, &tl, &s));
        for want in [
            "In use 9.00 GB",
            "Modified 1.00 GB",
            "Standby 4.00 GB",
            "Free 2.00 GB",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
    }

    #[test]
    fn hovering_the_detail_chart_marks_the_mini_charts_too() {
        let mut page = PerfPage::default();
        let _ = paint(&mut page, 2);
        let big = page.charts().plot(page.detail.first).rect();
        let at = Point::new(page.charts().axis().x(big, 3000.0), big.center().y);
        assert!(page.handle(UiEvent::MouseMove(at)).repaint);
        assert_eq!(page.charts().hover_age(), Some(3000.0));
        let dl = paint(&mut page, 2);
        let dots = dl
            .cmds()
            .iter()
            .filter(|c| matches!(c, DrawCmd::FillRoundRect { rect, .. } if rect.w <= 8.0))
            .count();
        assert_eq!(dots, 3, "a dot on the big chart and on both mini charts");
        assert!(has(&texts(&dl), "25% · \u{2007}3s ago"));
        assert!(page.handle(UiEvent::MouseLeave).repaint);
    }

    #[test]
    fn a_two_line_chart_reads_out_both_lines_with_the_time_next_to_the_line() {
        let (tl, s) = timeline_with(2, &[], &[adapter(7, "Ethernet")]);
        let mut page = PerfPage::default();
        let _ = paint_with(&mut page, &tl, &s);
        page.handle(UiEvent::Key(Key::End));
        let _ = paint_with(&mut page, &tl, &s);
        let rx = page.charts().plot(page.detail.first).rect();
        let at = Point::new(page.charts().axis().x(rx, 3000.0), rx.center().y);
        assert!(page.handle(UiEvent::MouseMove(at)).repaint);
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(
            has(&t, "Receive 5.60 Mbps · Send 100 Kbps · \u{2007}3s ago"),
            "{t:?}"
        );
    }

    #[test]
    fn disks_and_adapters_are_listed_with_their_own_panes() {
        let (tl, s) = timeline_with(2, &[disk(0)], &[adapter(7, "Ethernet")]);
        let mut page = PerfPage::default();
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(has(&t, "Disk 0 (C:)") && has(&t, "SSD  12%"), "{t:?}");
        assert!(
            has(&t, "Ethernet") && has(&t, "\u{2193} 5.60 Mbps  \u{2191} 100 Kbps"),
            "{t:?}"
        );

        page.handle(UiEvent::Key(Key::End));
        assert_eq!(page.device(), Device::Adapter(7));
        let dl = paint_with(&mut page, &tl, &s);
        let t = texts(&dl);
        for want in [
            "Test Ethernet Controller",
            "Throughput",
            "Receive",
            "Send",
            "5.60 Mbps",
            "Link speed",
            "1.00 Gbps",
            "Physical",
            "IPv4 address",
            "192.168.1.10",
            "IPv6 address",
            "fe80::1",
            "DNS suffix",
            "lan",
            // The scale rounds 5.6 Mbps up to the next 1-2-5 step.
            "10.0 Mbps",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        // Two lines on one chart, received then sent.
        let theme = Theme::dark();
        let colors = line_colors(&dl);
        assert_eq!(
            &colors[colors.len() - 2..],
            [theme.network, theme.network_send]
        );

        page.handle(UiEvent::Key(Key::Up));
        assert_eq!(page.device(), Device::Disk(0));
        let t = texts(&paint_with(&mut page, &tl, &s));
        for want in [
            "Test SSD 1TB",
            "Active time",
            "Transfer rate",
            "Read",
            "Write",
            "Average response time",
            "0.4 ms",
            "Read speed",
            "3.00 MB/s",
            "Capacity",
            "Type",
            "SSD",
            // The volume on it, with its space and roles.
            "C: Windows",
            "250 GB free of 500 GB",
            "NTFS \u{b7} System, Page file",
            // Read peaks at 3 MB/s: the scale is 5 MB/s.
            "5.00 MB/s",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
    }

    #[test]
    fn gpus_and_the_battery_are_listed_with_their_own_panes() {
        let (tl, s) = timeline_of(
            2,
            &Devices {
                gpus: vec![gpu(3)],
                battery: Some(battery()),
                ..Devices::default()
            },
        );
        let mut page = PerfPage::default();
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(has(&t, "GPU 3") && has(&t, "42%  2.0 / 8.0 GB"), "{t:?}");
        assert!(has(&t, "Battery") && has(&t, "80%  Discharging"), "{t:?}");

        page.handle(UiEvent::Key(Key::End));
        assert_eq!(page.device(), Device::Battery);
        let dl = paint_with(&mut page, &tl, &s);
        let t = texts(&dl);
        for want in [
            "Test Cells",
            "Charge",
            "80%",
            "State",
            "Discharging",
            "Power draw",
            "12.5 W",
            "Time left",
            "3:10:00",
            "Power source",
            "On battery",
            "Full capacity",
            "50.0 Wh",
            "Health",
            "83%",
            "Cycle count",
            "120",
            "Chemistry",
            "LiP",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        let theme = Theme::dark();
        assert!(line_colors(&dl).contains(&theme.battery));

        page.handle(UiEvent::Key(Key::Up));
        assert_eq!(page.device(), Device::Gpu(3));
        let dl = paint_with(&mut page, &tl, &s);
        let t = texts(&dl);
        for want in [
            "Test Graphics 4000",
            "Utilization",
            "Dedicated GPU memory",
            "42%",
            "Dedicated memory",
            "2.0 / 8.0 GB",
            "Shared memory",
            "1.0 / 16.0 GB",
            "3D",
            "Copy",
            "3.0%",
            "Driver version",
            "31.0.15",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        let colors = line_colors(&dl);
        assert!(colors.contains(&theme.gpu) && colors.contains(&theme.gpu_memory));
    }

    #[test]
    fn the_cpu_pane_shows_package_power_and_temperature_when_the_probe_has_them() {
        let (tl, mut s) = timeline(2);
        s.cpu.package_power = Some(Watts(23.4));
        s.cpu.hotspot_celsius = Some(61.2);
        Arc::make_mut(&mut s.hardware).thermal_sensor = Some(ThermalSensor::Package);
        let mut page = PerfPage::default();
        let t = texts(&paint_with(&mut page, &tl, &s));
        for want in ["Power", "23.4 W", "Temperature", "61 °C"] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
        // The list's CPU line carries the temperature too.
        assert!(has(&t, "25%  61 °C"), "{t:?}");

        // An AMD reading is Tctl and says so.
        Arc::make_mut(&mut s.hardware).thermal_sensor = Some(ThermalSensor::Tctl);
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(has(&t, "Tctl") && !has(&t, "Temperature"), "{t:?}");

        // Without the readings the slots stay empty.
        let (tl, s) = timeline(2);
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(!has(&t, "Power") && !has(&t, "Temperature"), "{t:?}");
        assert!(has(&t, "25%"), "{t:?}");
    }

    #[test]
    fn the_memory_and_cpu_panes_show_the_firmware_facts() {
        let (tl, s) = timeline(2);
        let mut page = PerfPage::default();
        let t = texts(&paint_with(&mut page, &tl, &s));
        assert!(has(&t, "Virtualization") && has(&t, "Enabled"), "{t:?}");
        assert!(has(&t, "Hypervisor") && has(&t, "No"), "{t:?}");

        page.handle(UiEvent::Key(Key::Down));
        assert_eq!(page.device(), Device::Memory);
        let t = texts(&paint_with(&mut page, &tl, &s));
        for want in [
            "Speed",
            "3200 MT/s",
            "Slots used",
            "2 of 4",
            "Form factor",
            "SODIMM",
            "Hardware reserved",
            "512 MB",
        ] {
            assert!(has(&t, want), "{want} missing from {t:?}");
        }
    }

    #[test]
    fn a_device_that_goes_away_hands_the_selection_back_to_the_cpu() {
        let (tl, s) = timeline_with(2, &[disk(0)], &[]);
        let mut page = PerfPage::default();
        let _ = paint_with(&mut page, &tl, &s);
        page.handle(UiEvent::Key(Key::End));
        assert_eq!(page.device(), Device::Disk(0));
        let (tl, s) = timeline(2);
        let _ = paint_with(&mut page, &tl, &s);
        assert_eq!(page.device(), Device::Cpu);
    }

    #[test]
    fn a_long_list_scrolls_and_follows_the_keys() {
        let adapters: Vec<AdapterSample> = (0..14).map(|i| adapter(i, "vEthernet")).collect();
        let (tl, s) = timeline_with(2, &[], &adapters);
        let mut page = PerfPage::default();
        let _ = paint_with(&mut page, &tl, &s);
        let view = page.list_view;
        let before = page.items[0];
        assert!(page.items.last().unwrap().y > view.bottom(), "overflows");
        let r = page.handle(UiEvent::Wheel {
            at: view.center(),
            lines: 2.0,
            horizontal: false,
        });
        assert!(r.repaint);
        let _ = paint_with(&mut page, &tl, &s);
        assert!((before.y - page.items[0].y - 2.0 * (ITEM_H + ITEM_GAP)).abs() < 1e-3);
        // End jumps to the last entry and scrolls it into view.
        page.handle(UiEvent::Key(Key::End));
        let _ = paint_with(&mut page, &tl, &s);
        let last = *page.items.last().unwrap();
        assert!(
            last.bottom() <= view.bottom() + 1e-3 && last.y >= view.y,
            "{last:?} in {view:?}"
        );
        // Scrolling never runs past either end.
        page.handle(UiEvent::Wheel {
            at: view.center(),
            lines: -100.0,
            horizontal: false,
        });
        let _ = paint_with(&mut page, &tl, &s);
        assert!((page.items[0].y - view.y).abs() < 1e-3);
    }

    #[test]
    fn rate_scales_round_up_to_readable_steps() {
        let mb = 1024.0 * 1024.0;
        assert!((nice_bytes(3.0 * mb, mb) - 5.0 * mb).abs() < 1.0);
        assert!((nice_bytes(0.0, mb) - mb).abs() < 1.0, "the floor");
        assert!((nice_bytes(120.0 * mb, mb) - 200.0 * mb).abs() < 1.0);
        assert!((nice_bits(5.6e6, 1e5) - 1e7).abs() < 1.0);
        assert!((nice_bits(1.5e5, 1e5) - 2e5).abs() < 1.0);
        assert!((nice_bits(0.0, 1e5) - 1e5).abs() < 1.0);
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
