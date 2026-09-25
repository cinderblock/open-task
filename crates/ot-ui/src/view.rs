//! The root view: summary cards on top, a toolbar, and the process table below.

use std::fmt::Write as _;
use std::sync::Arc;

use ot_core::{Snapshot, Timeline};
use ot_model::cpu::CoreKind;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, Size, VAlign};

use crate::format;
use crate::process_rows::{col, columns, ProcessRows, ProcessTree};
use crate::sparkline::{self, SparkStyle};
use crate::table::{Hit, RowSource, Table};
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
    /// In tree mode: collapse the selected row, or move to its parent.
    Left,
    /// In tree mode: expand the selected row, or move to its first child.
    Right,
}

/// How the process table is arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// One flat list sorted by the active column.
    #[default]
    List,
    /// Parent-child hierarchy; siblings sorted by the active column.
    Tree,
}

impl ViewMode {
    /// Parse a command-line value. Unknown values fall back to `List`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "tree" => Self::Tree,
            _ => Self::List,
        }
    }

    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::List => Self::Tree,
            Self::Tree => Self::List,
        }
    }
}

/// A semantic action, as a menu item or accelerator would issue it. The shell maps
/// keys to these; the meaning lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Switch between list and tree, keeping and revealing the selection.
    ToggleView,
    /// Show a particular mode. Asking for the current one re-reveals the selection.
    SetView(ViewMode),
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
    Command(Command),
}

const HISTORY_POINTS: usize = 600;
const CARD_H: f32 = 96.0;

/// The strip between the cards and the table: the List/Tree switch, the selected
/// process's ancestry, and the process count.
#[derive(Debug, Default)]
struct Toolbar {
    /// Segment rectangles from the last paint, in [`SEGMENTS`] order.
    segments: [Rect; 2],
    hover: Option<usize>,
    chain: Vec<u32>,
}

const SEGMENTS: [(ViewMode, &str); 2] = [(ViewMode::List, "List"), (ViewMode::Tree, "Tree")];
const SEGMENT_W: f32 = 60.0;
const COUNT_W: f32 = 120.0;

impl Toolbar {
    fn segment_at(&self, p: Point) -> Option<usize> {
        self.segments.iter().position(|r| r.contains(p))
    }

    fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        table: &Table,
        rows: &ProcessRows<'_>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let current = if table.tree() {
            ViewMode::Tree
        } else {
            ViewMode::List
        };
        let group = Rect::new(rect.x, rect.y + 2.0, SEGMENT_W * 2.0, rect.h - 4.0);
        dl.fill_round_rect(group, theme.card_radius, theme.surface);
        dl.stroke_rect(group, theme.surface_border, 1.0);
        for (i, (mode, label)) in SEGMENTS.iter().enumerate() {
            let r = Rect::new(group.x + SEGMENT_W * i as f32, group.y, SEGMENT_W, group.h);
            self.segments[i] = r;
            let active = *mode == current;
            let fill = if active {
                Some(theme.button_active)
            } else if self.hover == Some(i) {
                Some(theme.button_hover)
            } else {
                None
            };
            if let Some(c) = fill {
                dl.fill_round_rect(r.inset(2.0, 2.0), theme.card_radius - 1.0, c);
            }
            let color = if active { theme.text } else { theme.text_dim };
            dl.text(
                label,
                r,
                theme.header,
                color,
                HAlign::Center,
                VAlign::Middle,
                false,
            );
        }

        // Right edge: how many processes the table lists.
        let (_, after_group) = rect.split_left(group.w + theme.pad);
        let (crumb, count) = after_group.split_left((after_group.w - COUNT_W).max(0.0));
        buf.clear();
        let _ = write!(buf, "{} processes", rows.listed());
        dl.text(
            buf,
            count,
            theme.small,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );

        // Between: where the selected process sits in the hierarchy. The same text in
        // both modes is what ties the sorted list to the tree.
        buf.clear();
        match table.selected.and_then(|id| rows.row_of(id)) {
            Some(row) => rows.ancestry(row, buf, &mut self.chain),
            None => buf.push_str("Ctrl+T switches between list and tree"),
        }
        dl.label(buf, crumb, theme.small, theme.text_dim);
    }
}

/// The whole application view. One per window.
#[derive(Debug)]
pub struct App {
    theme: Theme,
    backdrop: bool,
    size: Size,
    table: Table,
    toolbar: Toolbar,
    snap: Arc<Snapshot>,
    tree: ProcessTree,
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
            toolbar: Toolbar::default(),
            snap: Arc::new(Snapshot::default()),
            tree: ProcessTree::default(),
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

    #[must_use]
    pub fn view(&self) -> ViewMode {
        if self.table.tree() {
            ViewMode::Tree
        } else {
            ViewMode::List
        }
    }

    /// Switch the process table's arrangement, keeping the selection in view.
    pub fn set_view(&mut self, mode: ViewMode) {
        let rows = rows_of(&self.snap, &self.tree);
        apply_view(&mut self.table, mode, &rows, &self.theme);
    }

    /// Offer the newest snapshot. Returns true if it was new and a repaint is due.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) -> bool {
        if snap.is_empty() || (!self.snap.is_empty() && snap.tick == self.snap.tick) {
            return false;
        }
        self.timeline.observe(&snap);
        self.tree.rebuild(&snap.processes);
        self.snap = snap;
        self.table.invalidate_order();
        true
    }

    /// Handle input. Returns true if a repaint is needed.
    pub fn handle(&mut self, ev: UiEvent) -> bool {
        let rows = rows_of(&self.snap, &self.tree);
        let theme = &self.theme;
        match ev {
            UiEvent::Resize(s) => {
                self.size = s;
                true
            }
            UiEvent::MouseMove(p) => {
                let hover = match self.table.hit(p, theme) {
                    Hit::Row(i) | Hit::Expander(i) => Some(i),
                    _ => None,
                };
                let segment = self.toolbar.segment_at(p);
                let changed = hover != self.table.hover || segment != self.toolbar.hover;
                self.table.hover = hover;
                self.toolbar.hover = segment;
                changed
            }
            UiEvent::MouseLeave => {
                let row = self.table.hover.take().is_some();
                let segment = self.toolbar.hover.take().is_some();
                row || segment
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => {
                if let Some(i) = self.toolbar.segment_at(at) {
                    apply_view(&mut self.table, SEGMENTS[i].0, &rows, theme);
                    return true;
                }
                match self.table.hit(at, theme) {
                    Hit::Header(c) => {
                        self.table.set_sort(c);
                        true
                    }
                    Hit::Expander(pos) => self.table.toggle_expanded(pos, &rows),
                    Hit::Row(pos) => {
                        self.table.selected = self.table.row_at(pos).map(|r| rows.id(r));
                        true
                    }
                    Hit::Nothing => false,
                }
            }
            UiEvent::MouseDown { .. } | UiEvent::MouseUp { .. } => false,
            UiEvent::Wheel { lines, .. } => {
                self.table.scroll_lines(lines * 3.0);
                true
            }
            UiEvent::Key(k) => {
                let page =
                    isize::try_from(self.table.rows_visible(theme).max(1)).unwrap_or(isize::MAX);
                match k {
                    Key::Up => self.table.move_selection(&rows, -1, theme),
                    Key::Down => self.table.move_selection(&rows, 1, theme),
                    Key::PageUp => self.table.move_selection(&rows, -page, theme),
                    Key::PageDown => self.table.move_selection(&rows, page, theme),
                    Key::Home => self.table.select_end(&rows, true, theme),
                    Key::End => self.table.select_end(&rows, false, theme),
                    Key::Left => return self.table.collapse_or_parent(&rows, theme),
                    Key::Right => return self.table.expand_or_child(&rows, theme),
                }
                true
            }
            UiEvent::Command(c) => {
                let mode = match c {
                    Command::ToggleView => self.view().other(),
                    Command::SetView(m) => m,
                };
                apply_view(&mut self.table, mode, &rows, theme);
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
        let (_, rest) = rest.split_top(theme.gap);
        let (toolbar, rest) = rest.split_top(theme.toolbar_h);
        let (_, table_rect) = rest.split_top(theme.gap * 0.5);

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

        let rows = rows_of(&snap, &self.tree);
        // The table first: the toolbar reads its state, and the order must be
        // current before the ancestry lookup.
        self.table
            .paint(dl, table_rect, &rows, theme, &mut self.buf);
        self.toolbar
            .paint(dl, toolbar, &self.table, &rows, theme, &mut self.buf);
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
            let _ = write!(buf, "{p} P + {e} E cores");
        } else if n > 0 {
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

/// Put the table in `mode`. Asking for the mode it is already in re-reveals the
/// selection, which doubles as a "where is it?" jump.
fn apply_view(table: &mut Table, mode: ViewMode, rows: &ProcessRows<'_>, theme: &Theme) {
    let tree = mode == ViewMode::Tree;
    if table.tree() == tree {
        table.reveal_selected(rows, theme);
    } else {
        table.set_tree(tree, rows, theme);
    }
}

fn rows_of<'a>(snap: &'a Snapshot, tree: &'a ProcessTree) -> ProcessRows<'a> {
    ProcessRows {
        procs: &snap.processes,
        tree,
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
    use crate::process_rows::row_id;
    use crate::process_rows::tests::proc;
    use ot_model::process::ProcessSample;
    use ot_model::{ProcessKey, Tick};
    use ot_paint::DrawCmd;
    use std::time::{Duration, SystemTime};

    fn snapshot(tick: u64, procs: Vec<ProcessSample>) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            tick: Tick(tick),
            taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(tick)),
            interval: Duration::from_secs(1),
            processes: procs,
            ..Default::default()
        })
    }

    fn id(pid: u32) -> crate::table::RowId {
        row_id(ProcessKey::new(pid, 1))
    }

    /// Idle(0) ─ 4 ─ 10 ─ 11
    ///              └─ 12 ─ 13
    /// 20 (orphan)
    fn family() -> Arc<Snapshot> {
        snapshot(
            1,
            vec![
                proc(0, None, 900.0),
                proc(4, Some(0), 1.0),
                proc(10, Some(4), 2.0),
                proc(11, Some(10), 30.0),
                proc(12, Some(4), 5.0),
                proc(13, Some(12), 3.0),
                proc(20, Some(99), 10.0),
            ],
        )
    }

    fn painted_strings(app: &mut App) -> Vec<String> {
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        assert_eq!(dl.clip_depth(), 0);
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    /// Names in the order the table paints them. Only the table body is clipped, so
    /// text inside a clip is a table cell and not, say, the toolbar's breadcrumb.
    fn painted_names(app: &mut App) -> Vec<String> {
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        let mut depth = 0;
        let mut names = Vec::new();
        for c in dl.cmds() {
            match c {
                DrawCmd::PushClip(_) => depth += 1,
                DrawCmd::PopClip => depth -= 1,
                DrawCmd::Text(t) if depth > 0 => {
                    let s = dl.str(t.text);
                    if s.starts_with('p') && s.contains(".exe") {
                        names.push(s.to_owned());
                    }
                }
                _ => {}
            }
        }
        names
    }

    fn ready(app: &mut App) {
        app.handle(UiEvent::Resize(Size::new(900.0, 700.0)));
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
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
        let s = snapshot(1, vec![proc(1, None, 10.0), proc(2, None, 90.0)]);
        assert!(app.set_snapshot(Arc::clone(&s)));
        assert!(!app.set_snapshot(s));
        assert!(app.set_snapshot(snapshot(2, vec![])));
    }

    #[test]
    fn keyboard_selects_top_cpu_process_first() {
        let mut app = App::default();
        app.set_snapshot(snapshot(
            1,
            vec![
                proc(1, None, 10.0),
                proc(2, None, 90.0),
                proc(3, None, 50.0),
            ],
        ));
        ready(&mut app);
        assert!(app.handle(UiEvent::Key(Key::Down)));
        assert_eq!(app.table.selected, Some(id(2)));
        app.handle(UiEvent::Key(Key::Down));
        assert_eq!(app.table.selected, Some(id(3)));
    }

    #[test]
    fn row_ids_distinguish_pid_reuse() {
        assert_ne!(
            row_id(ProcessKey::new(100, 1)),
            row_id(ProcessKey::new(100, 2))
        );
    }

    #[test]
    fn list_mode_sorts_by_own_cpu_and_hides_idle() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        assert_eq!(app.view(), ViewMode::List);
        assert_eq!(
            painted_names(&mut app),
            ["p11.exe", "p20.exe", "p12.exe", "p13.exe", "p10.exe", "p4.exe"]
        );
    }

    #[test]
    fn tree_mode_nests_children_and_orders_siblings_by_subtree_cpu() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        app.handle(UiEvent::Command(Command::SetView(ViewMode::Tree)));
        assert_eq!(app.view(), ViewMode::Tree);
        // Roots: 4 (subtree 41) above the orphan 20 (10). Under 4: branch 10 (32)
        // above branch 12 (8), even though 12's own CPU is higher than 10's.
        assert_eq!(
            painted_names(&mut app),
            ["p4.exe", "p10.exe", "p11.exe", "p12.exe", "p13.exe", "p20.exe"]
        );
    }

    #[test]
    fn toggling_keeps_the_selection_and_reveals_it() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        // Pick the hottest process in the list.
        app.handle(UiEvent::Key(Key::Down));
        assert_eq!(app.table.selected, Some(id(11)));

        // Collapse everything above it in the tree first, to prove reveal expands.
        app.handle(UiEvent::Command(Command::ToggleView));
        app.handle(UiEvent::Key(Key::Left)); // 11 is a leaf: moves to 10
        app.handle(UiEvent::Key(Key::Left)); // collapses 10
        app.handle(UiEvent::Key(Key::Left)); // moves to 4
        app.handle(UiEvent::Key(Key::Left)); // collapses 4
        assert_eq!(painted_names(&mut app), ["p4.exe (4)", "p20.exe"]);
        app.table.selected = Some(id(11));

        app.handle(UiEvent::Command(Command::ToggleView));
        assert_eq!(app.view(), ViewMode::List);
        assert_eq!(app.table.selected, Some(id(11)));
        app.handle(UiEvent::Command(Command::ToggleView));
        assert_eq!(app.view(), ViewMode::Tree);
        assert_eq!(app.table.selected, Some(id(11)));
        let names = painted_names(&mut app);
        assert!(names.iter().any(|n| n == "p11.exe"), "revealed: {names:?}");
        assert_eq!(names[0], "p4.exe", "ancestors expanded: {names:?}");
    }

    #[test]
    fn collapsed_row_paints_subtree_totals() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        app.handle(UiEvent::Command(Command::SetView(ViewMode::Tree)));
        app.handle(UiEvent::Key(Key::Down)); // selects 4
        app.handle(UiEvent::Key(Key::Left)); // collapses it
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "p4.exe (4)"), "{strings:?}");
        // 1 + 2 + 30 + 5 + 3 = 41, shown in the CPU column.
        assert!(strings.iter().any(|s| s == "41"), "{strings:?}");
    }

    #[test]
    fn toolbar_shows_ancestry_of_the_selection_in_both_modes() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let strings = painted_strings(&mut app);
        assert!(
            strings.iter().any(|s| s.starts_with("Ctrl+T")),
            "{strings:?}"
        );
        assert!(strings.iter().any(|s| s == "6 processes"), "{strings:?}");

        app.table.selected = Some(id(13));
        let crumb = "p4.exe › p12.exe › p13.exe";
        assert!(painted_strings(&mut app).iter().any(|s| s == crumb));
        app.handle(UiEvent::Command(Command::ToggleView));
        assert!(painted_strings(&mut app).iter().any(|s| s == crumb));
    }

    #[test]
    fn clicking_the_segments_switches_modes() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let tree_seg = app.toolbar.segments[1].center();
        assert!(app.handle(UiEvent::MouseMove(tree_seg)));
        assert_eq!(app.toolbar.hover, Some(1));
        assert!(app.handle(UiEvent::MouseDown {
            at: tree_seg,
            button: MouseButton::Left,
        }));
        assert_eq!(app.view(), ViewMode::Tree);
        let list_seg = app.toolbar.segments[0].center();
        app.handle(UiEvent::MouseDown {
            at: list_seg,
            button: MouseButton::Left,
        });
        assert_eq!(app.view(), ViewMode::List);
        assert!(app.handle(UiEvent::MouseLeave));
        assert_eq!(app.toolbar.hover, None);
    }

    #[test]
    fn clicking_an_expander_collapses_without_selecting() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        app.handle(UiEvent::Command(Command::SetView(ViewMode::Tree)));
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        let theme = app.theme.clone();
        // First row is p4.exe at depth 0; its expander box starts at the padding.
        let table_top = theme.gap + CARD_H + theme.gap + theme.toolbar_h + theme.gap * 0.5;
        let at = Point::new(
            theme.gap + theme.pad + theme.expander_w * 0.5,
            table_top + theme.header_h + theme.row_h * 0.5,
        );
        assert_eq!(app.table.hit(at, &theme), Hit::Expander(0));
        assert!(app.handle(UiEvent::MouseDown {
            at,
            button: MouseButton::Left,
        }));
        assert_eq!(app.table.selected, None);
        assert_eq!(painted_names(&mut app), ["p4.exe (4)", "p20.exe"]);
    }
}
