//! The root view: summary cards on top, process table below.

use std::cmp::Ordering;
use std::sync::Arc;

use ot_core::{Snapshot, Timeline};
use ot_model::cpu::CoreKind;
use ot_model::process::ProcessSample;
use ot_model::ProcessKey;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, Size, VAlign};

use crate::format;
use crate::sparkline::{self, SparkStyle};
use crate::table::{Column, Hit, RowId, RowSource, Table};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// Input from the shell, in DIPs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiEvent {
    Resize(Size),
    MouseMove(Point),
    MouseLeave,
    MouseDown {
        at: Point,
        button: MouseButton,
    },
    MouseUp {
        at: Point,
        button: MouseButton,
    },
    /// Positive `lines` scrolls content down (wheel toward the user).
    Wheel {
        at: Point,
        lines: f32,
    },
    Key(Key),
}

const HISTORY_POINTS: usize = 600;
const CARD_H: f32 = 96.0;

/// Process table columns, in display order. Indices are used by [`ProcessRows`].
mod col {
    pub const NAME: usize = 0;
    pub const PID: usize = 1;
    pub const CPU: usize = 2;
    pub const MEMORY: usize = 3;
    pub const WORKING_SET: usize = 4;
    pub const DISK_READ: usize = 5;
    pub const DISK_WRITE: usize = 6;
    pub const THREADS: usize = 7;
    pub const HANDLES: usize = 8;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 240.0),
        Column::number("PID", 70.0),
        Column::number("CPU %", 70.0),
        Column::number("Memory", 95.0),
        Column::number("Working set", 95.0),
        Column::number("Disk read", 95.0),
        Column::number("Disk write", 95.0),
        Column::number("Threads", 70.0),
        Column::number("Handles", 75.0),
    ]
}

/// Adapts a snapshot's process list to the table.
struct ProcessRows<'a> {
    procs: &'a [ProcessSample],
    interval_secs: f32,
    mem_total: f32,
}

fn row_id(key: ProcessKey) -> RowId {
    RowId(u64::from(key.pid) ^ key.birth.0.rotate_left(32))
}

impl RowSource for ProcessRows<'_> {
    fn len(&self) -> usize {
        self.procs.len()
    }

    fn id(&self, row: usize) -> RowId {
        row_id(self.procs[row].key())
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let p = &self.procs[row];
        match col {
            col::NAME => {
                out.clear();
                out.push_str(p.name());
            }
            col::PID => format::count(out, p.key().pid),
            col::CPU => format::percent(out, p.cpu.get()),
            col::MEMORY => format::bytes(out, p.private_bytes),
            col::WORKING_SET => format::bytes(out, p.working_set),
            col::DISK_READ => format::rate(out, p.disk_read, self.interval_secs),
            col::DISK_WRITE => format::rate(out, p.disk_write, self.interval_secs),
            col::THREADS => format::count(out, p.threads),
            col::HANDLES => format::count(out, p.handles),
            _ => out.clear(),
        }
    }

    fn heat(&self, row: usize, col: usize) -> Option<f32> {
        let p = &self.procs[row];
        match col {
            col::CPU => Some((p.cpu.get() / 100.0).min(1.0)),
            col::MEMORY if self.mem_total > 0.0 => {
                // Memory heat is relative to a tenth of RAM: one process holding 10% of
                // the machine is fully hot.
                Some((p.private_bytes.get() as f32 / (self.mem_total * 0.1)).min(1.0))
            }
            _ => None,
        }
    }

    fn visible(&self, row: usize) -> bool {
        // PID 0 is the kernel's idle accounting, not a process. Its "CPU" is the
        // machine's idle time, which would otherwise pin it to the top of every sort.
        self.procs[row].key().pid != 0
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (a, b) = (&self.procs[a], &self.procs[b]);
        match col {
            col::NAME => a
                .name()
                .to_ascii_lowercase()
                .cmp(&b.name().to_ascii_lowercase()),
            col::PID => a.key().pid.cmp(&b.key().pid),
            col::CPU => a.cpu.get().total_cmp(&b.cpu.get()),
            col::MEMORY => a.private_bytes.cmp(&b.private_bytes),
            col::WORKING_SET => a.working_set.cmp(&b.working_set),
            col::DISK_READ => a.disk_read.cmp(&b.disk_read),
            col::DISK_WRITE => a.disk_write.cmp(&b.disk_write),
            col::THREADS => a.threads.cmp(&b.threads),
            col::HANDLES => a.handles.cmp(&b.handles),
            _ => Ordering::Equal,
        }
    }
}

/// The whole application view. One per window.
#[derive(Debug)]
pub struct App {
    theme: Theme,
    backdrop: bool,
    size: Size,
    table: Table,
    snap: Arc<Snapshot>,
    timeline: Timeline,
    buf: String,
    scratch: Vec<Point>,
}

impl App {
    #[must_use]
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            backdrop: false,
            size: Size::new(800.0, 600.0),
            table: Table::new(columns(), col::CPU),
            snap: Arc::new(Snapshot::default()),
            timeline: Timeline::new(HISTORY_POINTS),
            buf: String::with_capacity(64),
            scratch: Vec::with_capacity(HISTORY_POINTS),
        }
    }

    /// Whether the shell composites us over a system backdrop. When true, the view
    /// clears to transparent; when false, to an opaque theme color.
    pub fn set_backdrop(&mut self, on: bool) {
        self.backdrop = on;
    }

    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Offer the newest snapshot. Returns true if it was new and a repaint is due.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) -> bool {
        if snap.is_empty() || (!self.snap.is_empty() && snap.tick == self.snap.tick) {
            return false;
        }
        self.timeline.observe(&snap);
        self.snap = snap;
        self.table.invalidate_order();
        true
    }

    /// Handle input. Returns true if a repaint is needed.
    pub fn handle(&mut self, ev: UiEvent) -> bool {
        let rows = rows_of(&self.snap);
        match ev {
            UiEvent::Resize(s) => {
                self.size = s;
                true
            }
            UiEvent::MouseMove(p) => {
                let hover = match self.table.hit(p, &self.theme) {
                    Hit::Row(i) => Some(i),
                    _ => None,
                };
                if hover == self.table.hover {
                    false
                } else {
                    self.table.hover = hover;
                    true
                }
            }
            UiEvent::MouseLeave => self.table.hover.take().is_some(),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => match self.table.hit(at, &self.theme) {
                Hit::Header(c) => {
                    self.table.set_sort(c);
                    true
                }
                Hit::Row(pos) => {
                    self.table.selected = self.table.row_at(pos).map(|r| rows.id(r));
                    true
                }
                Hit::Nothing => false,
            },
            UiEvent::MouseDown { .. } | UiEvent::MouseUp { .. } => false,
            UiEvent::Wheel { lines, .. } => {
                self.table.scroll_lines(lines * 3.0);
                true
            }
            UiEvent::Key(k) => {
                let page = isize::try_from(self.table.rows_visible(&self.theme).max(1))
                    .unwrap_or(isize::MAX);
                match k {
                    Key::Up => self.table.move_selection(&rows, -1, &self.theme),
                    Key::Down => self.table.move_selection(&rows, 1, &self.theme),
                    Key::PageUp => self.table.move_selection(&rows, -page, &self.theme),
                    Key::PageDown => self.table.move_selection(&rows, page, &self.theme),
                    Key::Home => self.table.select_end(&rows, true, &self.theme),
                    Key::End => self.table.select_end(&rows, false, &self.theme),
                }
                true
            }
        }
    }

    /// Produce this frame.
    pub fn paint(&mut self, dl: &mut DisplayList) {
        dl.clear();
        let theme = &self.theme;
        dl.clear_to(if self.backdrop {
            Color::TRANSPARENT
        } else {
            theme.bg_solid
        });

        let full = Rect::from_size(self.size).inset(theme.gap, theme.gap);
        let (cards, rest) = full.split_top(CARD_H);
        let (_, table_rect) = rest.split_top(theme.gap);

        let (cpu_card, mem_card) = cards.split_left((cards.w - theme.gap) * 0.5);
        let (_, mem_card) = mem_card.split_left(theme.gap);

        let snap = Arc::clone(&self.snap);
        Self::paint_cpu_card(
            dl,
            cpu_card,
            &snap,
            &self.timeline,
            theme,
            &mut self.buf,
            &mut self.scratch,
        );
        Self::paint_mem_card(
            dl,
            mem_card,
            &snap,
            &self.timeline,
            theme,
            &mut self.buf,
            &mut self.scratch,
        );

        self.table
            .paint(dl, table_rect, &rows_of(&snap), theme, &mut self.buf);
    }

    fn card_frame(dl: &mut DisplayList, rect: Rect, theme: &Theme) -> Rect {
        dl.fill_round_rect(rect, theme.card_radius, theme.surface);
        dl.stroke_rect(rect, theme.surface_border, 1.0);
        rect.inset(theme.pad, theme.pad * 0.75)
    }

    fn paint_cpu_card(
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        tl: &Timeline,
        theme: &Theme,
        buf: &mut String,
        scratch: &mut Vec<Point>,
    ) {
        let inner = Self::card_frame(dl, rect, theme);
        let (text_col, graph) = inner.split_left(150.0);
        let (title, below) = text_col.split_top(16.0);
        let (big, sub) = below.split_top(30.0);

        dl.label("CPU", title, theme.title, theme.text_dim);

        buf.clear();
        if !snap.is_empty() {
            format::percent(buf, snap.cpu.total.get());
            buf.push('%');
        }
        dl.label(buf, big, theme.big, theme.text);

        buf.clear();
        let n = snap.cpu.cores.len();
        let p = snap
            .cpu
            .cores
            .iter()
            .filter(|c| c.kind == CoreKind::Performance)
            .count();
        let e = snap
            .cpu
            .cores
            .iter()
            .filter(|c| c.kind == CoreKind::Efficiency)
            .count();
        if p > 0 || e > 0 {
            use std::fmt::Write as _;
            let _ = write!(buf, "{p} P + {e} E cores");
        } else if n > 0 {
            use std::fmt::Write as _;
            let _ = write!(buf, "{n} logical cores");
        }
        dl.label(buf, sub, theme.small, theme.text_dim);

        let style = SparkStyle {
            line: theme.cpu,
            fill: theme.cpu.with_alpha(0.18),
            grid: theme.grid,
            width: 1.5,
        };
        sparkline::paint(dl, graph, &tl.cpu_total, 100.0, &style, scratch);
    }

    fn paint_mem_card(
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        tl: &Timeline,
        theme: &Theme,
        buf: &mut String,
        scratch: &mut Vec<Point>,
    ) {
        let inner = Self::card_frame(dl, rect, theme);
        let (text_col, graph) = inner.split_left(170.0);
        let (title, below) = text_col.split_top(16.0);
        let (big, sub) = below.split_top(30.0);

        dl.label("Memory", title, theme.title, theme.text_dim);

        let m = &snap.memory;
        buf.clear();
        if !snap.is_empty() {
            format::bytes_of(buf, m.in_use(), m.total);
        }
        dl.text(
            buf,
            big,
            theme.big,
            theme.text,
            HAlign::Left,
            VAlign::Middle,
            true,
        );

        buf.clear();
        if m.total.get() > 0 {
            use std::fmt::Write as _;
            let pct = m.in_use().get() as f64 / m.total.get() as f64 * 100.0;
            let _ = write!(buf, "{pct:.0}% in use");
        }
        dl.label(buf, sub, theme.small, theme.text_dim);

        let style = SparkStyle {
            line: theme.memory,
            fill: theme.memory.with_alpha(0.18),
            grid: theme.grid,
            width: 1.5,
        };
        sparkline::paint(
            dl,
            graph,
            &tl.mem_in_use,
            m.total.get() as f32,
            &style,
            scratch,
        );
    }
}

fn rows_of(snap: &Snapshot) -> ProcessRows<'_> {
    ProcessRows {
        procs: &snap.processes,
        interval_secs: interval_secs(snap),
        mem_total: snap.memory.total.get() as f32,
    }
}

fn interval_secs(snap: &Snapshot) -> f32 {
    let s = snap.interval.as_secs_f32();
    if s > 0.0 {
        s
    } else {
        1.0
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new(Theme::dark())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::process::{Integrity, ProcessStatic};
    use ot_model::{Bytes, Percent, Tick};
    use std::time::{Duration, SystemTime};

    fn proc(pid: u32, cpu: f32) -> ProcessSample {
        ProcessSample {
            statics: Arc::new(ProcessStatic {
                key: ProcessKey::new(pid, 1),
                parent: None,
                name: format!("p{pid}.exe"),
                image_path: None,
                command_line: None,
                user: None,
                integrity: Integrity::Unknown,
                started_unix_ms: None,
            }),
            cpu: Percent(cpu),
            working_set: Bytes(1),
            private_bytes: Bytes(1),
            disk_read: Bytes(0),
            disk_write: Bytes(0),
            net_rx: Bytes(0),
            net_tx: Bytes(0),
            threads: 1,
            handles: 1,
            power: None,
            gpu: None,
            suspended: false,
        }
    }

    fn snapshot(tick: u64, procs: Vec<ProcessSample>) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            tick: Tick(tick),
            taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(tick)),
            interval: Duration::from_secs(1),
            processes: procs,
            ..Default::default()
        })
    }

    #[test]
    fn paints_without_data_and_balances_clips() {
        let mut app = App::default();
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        assert!(!dl.is_empty());
        assert_eq!(dl.clip_depth(), 0);
    }

    #[test]
    fn new_snapshot_requests_repaint_once() {
        let mut app = App::default();
        let s = snapshot(1, vec![proc(1, 10.0), proc(2, 90.0)]);
        assert!(app.set_snapshot(Arc::clone(&s)));
        assert!(!app.set_snapshot(s));
        assert!(app.set_snapshot(snapshot(2, vec![])));
    }

    #[test]
    fn keyboard_selects_top_cpu_process_first() {
        let mut app = App::default();
        app.set_snapshot(snapshot(
            1,
            vec![proc(1, 10.0), proc(2, 90.0), proc(3, 50.0)],
        ));
        app.handle(UiEvent::Resize(Size::new(800.0, 600.0)));
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        assert!(app.handle(UiEvent::Key(Key::Down)));
        assert_eq!(app.table.selected, Some(row_id(ProcessKey::new(2, 1))));
        app.handle(UiEvent::Key(Key::Down));
        assert_eq!(app.table.selected, Some(row_id(ProcessKey::new(3, 1))));
    }

    #[test]
    fn row_ids_distinguish_pid_reuse() {
        assert_ne!(
            row_id(ProcessKey::new(100, 1)),
            row_id(ProcessKey::new(100, 2))
        );
    }
}
