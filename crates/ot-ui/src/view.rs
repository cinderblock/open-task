//! The root view: summary cards on top, a toolbar, and the process table below.
//!
//! Input comes in as [`UiEvent`]s and goes out as a [`Reaction`]: whether to repaint,
//! and optionally an [`Effect`] for the shell to carry out with native APIs (a
//! context menu, a confirmation and kill, opening a folder). The view decides what
//! should happen; the shell decides how it looks on the platform.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use ot_core::{Resolution, Retention, Snapshot, Timeline};
use ot_model::attribution::Attribution;
use ot_model::cpu::CoreKind;
use ot_model::ProcessKey;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, Size, VAlign};

use crate::charts::{self, ChartGroup};
use crate::format;
use crate::nav::{NavHit, NavRail, Page};
use crate::perf::PerfPage;
use crate::process_rows::{self, col, columns, process_matches, Layout, ProcessRows, ProcessTree};
use crate::settings::{Settings, SettingsPage};
use crate::steady::Steady;
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
    /// Delete the last character of the search.
    Backspace,
    /// Delete the last word of the search (Ctrl+Backspace).
    WordBackspace,
    /// Clear the search and leave it.
    Escape,
    /// Leave the search, keeping it.
    Enter,
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

/// An item of the process context menu, and the meaning of a shortcut that does
/// the same thing (Delete, Shift+Delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Terminate the selected process.
    EndTask,
    /// Terminate the selected process and everything below it.
    EndTree,
    /// Show the selected process's executable in the file manager.
    OpenFileLocation,
    /// Sample the selected process's CPU for a few seconds: which modules its
    /// threads run, and for a broker service, which clients it served.
    SampleCpu,
}

/// One line of a context menu, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        action: MenuAction,
        label: &'static str,
        enabled: bool,
    },
    Separator,
}

/// A semantic action, as a menu item or accelerator would issue it. The shell maps
/// keys to these; the meaning lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Switch between list and tree, keeping and revealing the selection.
    ToggleView,
    /// Show a particular mode. Asking for the current one re-reveals the selection.
    SetView(ViewMode),
    /// Put the keyboard in the search field (Ctrl+F).
    Find,
    /// A context-menu item was chosen, by mouse or by its shortcut.
    Menu(MenuAction),
    /// Show a page (Ctrl+1..9, or a click on the rail).
    SetPage(Page),
    /// Move along the rail, wrapping (Ctrl+Tab is 1, Ctrl+Shift+Tab is -1).
    StepPage(isize),
    /// Freeze the display, or let it follow the samples again (Space, when the
    /// search field does not have the keyboard). Sampling carries on underneath.
    TogglePause,
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
    /// Positive `lines` scrolls content down (wheel toward the user), or to the
    /// right when `horizontal`.
    Wheel {
        at: Point,
        lines: f32,
        horizontal: bool,
    },
    Key(Key),
    /// A printable character was typed. Goes to the search, focused or not.
    Char(char),
    /// The user asked for a context menu: at a point, or from the keyboard
    /// (`None`), in which case it belongs to the selected row.
    ContextMenu {
        at: Option<Point>,
    },
    Command(Command),
}

/// Something the shell must do with native means.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Show a popup menu at `at`. The chosen entry comes back as
    /// [`Command::Menu`]; dismissal sends nothing.
    Menu { at: Point, entries: Vec<MenuEntry> },
    /// After the user confirms, terminate `targets` in order. `label` names what is
    /// being ended, for the confirmation ("chrome.exe and 41 processes under it").
    Terminate {
        targets: Vec<ProcessKey>,
        label: String,
    },
    /// Reveal this file in the platform's file manager.
    OpenFileLocation(String),
    /// Sample `target`'s CPU for `seconds`, off the UI thread, and hand the result
    /// to [`App::set_attribution`] (or [`App::sampling_failed`]).
    SampleCpu { target: ProcessKey, seconds: u32 },
    /// The user changed a setting; store these so the next start has them.
    SaveSettings(Settings),
}

/// What an event led to.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use]
pub struct Reaction {
    /// The view changed; paint again.
    pub repaint: bool,
    pub effect: Option<Effect>,
}

impl Reaction {
    pub const NONE: Self = Self {
        repaint: false,
        effect: None,
    };
    pub const REPAINT: Self = Self {
        repaint: true,
        effect: None,
    };

    pub(crate) fn painted(repaint: bool) -> Self {
        Self {
            repaint,
            effect: None,
        }
    }

    fn effect(effect: Effect) -> Self {
        Self {
            repaint: true,
            effect: Some(effect),
        }
    }
}

/// Which pointer to show, for the shell to map to a platform cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    /// Over a column divider, or dragging one.
    ResizeColumn,
    /// Over the search field.
    Text,
}

/// History kept for the summary charts: every sample for ten minutes (at the
/// default 1 Hz), then 10 s buckets for two hours. Covers [`AXIS`] (one hour) with
/// room to spare.
const HISTORY: Retention = Retention {
    raw: 600,
    tiers: &[Resolution {
        bucket_ms: 10_000,
        capacity: 720,
    }],
};
const CARD_H: f32 = 96.0;

/// Build a [`ProcessRows`] from an [`App`]'s fields without borrowing the table,
/// so the table can be mutated while the rows are in use. A macro rather than a
/// method because a method would borrow all of `self`.
macro_rules! rows_of {
    ($app:expr, $tree_mode:expr) => {
        ProcessRows {
            procs: &$app.snap.processes,
            threads: &$app.snap.threads,
            tree: &$app.tree,
            layout: &$app.layout,
            attribution: $app.attribution.as_deref(),
            interval_secs: interval_secs(&$app.snap),
            mem_total: $app.snap.memory.total.get() as f32,
            matched: &$app.matched,
            shown: &$app.shown,
            tree_mode: $tree_mode,
            steady: Some(&$app.steady),
        }
    };
}

/// The search field. Type-to-filter: printable keys land here whether or not it has
/// focus, so focus only decides where Enter and Escape go and whether a caret shows.
#[derive(Debug, Default)]
struct SearchBox {
    text: String,
    /// Lower-cased `text`, what the matcher uses.
    needle: String,
    focused: bool,
    rect: Rect,
    /// The clear button at the right end, when there is text.
    clear_rect: Rect,
    hover_clear: bool,
}

impl SearchBox {
    fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.needle.clear();
        self.needle
            .extend(text.chars().map(|c| c.to_ascii_lowercase()));
    }

    fn push(&mut self, c: char) {
        self.text.push(c);
        self.needle.push(c.to_ascii_lowercase());
    }

    /// Remove the last character, or the last word. Returns false if empty.
    fn backspace(&mut self, word: bool) -> bool {
        if self.text.is_empty() {
            return false;
        }
        if word {
            let trimmed = self.text.trim_end();
            let cut = trimmed
                .rfind(|c: char| c.is_whitespace() || c == '\\' || c == '/')
                .map_or(0, |i| i + 1);
            self.text.truncate(cut);
        } else {
            self.text.pop();
        }
        let t = std::mem::take(&mut self.text);
        self.set_text(&t);
        true
    }

    fn clear(&mut self) {
        self.text.clear();
        self.needle.clear();
    }

    fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let r = Rect::new(rect.x, rect.y + 2.0, rect.w, rect.h - 4.0);
        dl.fill_round_rect(r, theme.card_radius, theme.input_bg);
        let border = if self.focused {
            theme.accent
        } else {
            theme.surface_border
        };
        dl.stroke_rect(r, border, 1.0);

        let inner = r.inset(theme.pad, 0.0);
        if self.text.is_empty() {
            self.clear_rect = Rect::ZERO;
            dl.label("Filter (Ctrl+F)", inner, theme.cell, theme.text_dim);
            if self.focused {
                dl.fill_rect(
                    Rect::new(inner.x, inner.y + 5.0, 1.0, inner.h - 10.0),
                    theme.text,
                );
            }
            return;
        }
        let (text_rect, clear) = inner.split_left((inner.w - 18.0).max(0.0));
        self.clear_rect = clear;
        dl.field(&self.text, text_rect, theme.cell, theme.text, self.focused);
        // A small ×, as geometry so it needs no glyph.
        let c = clear.center();
        let s = 3.5;
        let color = if self.hover_clear {
            theme.text
        } else {
            theme.text_dim
        };
        dl.line(
            Point::new(c.x - s, c.y - s),
            Point::new(c.x + s, c.y + s),
            color,
            1.2,
        );
        dl.line(
            Point::new(c.x - s, c.y + s),
            Point::new(c.x + s, c.y - s),
            color,
            1.2,
        );
    }
}

/// The strip between the cards and the table: the List/Tree switch, the selected
/// process's ancestry, the search field, and the process count.
#[derive(Debug, Default)]
struct Toolbar {
    /// Why the table is standing still, if it is: shown before the search field.
    status: Option<(&'static str, Color)>,
    /// Segment rectangles from the last paint, in [`SEGMENTS`] order.
    segments: [Rect; 2],
    hover: Option<usize>,
    chain: Vec<u32>,
    search: SearchBox,
}

const SEGMENTS: [(ViewMode, &str); 2] = [(ViewMode::List, "List"), (ViewMode::Tree, "Tree")];
const SEGMENT_W: f32 = 60.0;
const COUNT_W: f32 = 130.0;
/// The toolbar's "Paused" / "Order held" note.
const STATUS_W: f32 = 150.0;

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

        // Right to left: the count, then the search field; the breadcrumb gets what
        // is left in the middle.
        let (_, after_group) = rect.split_left(group.w + theme.pad);
        let (middle, count) = after_group.split_left((after_group.w - COUNT_W).max(0.0));
        let search_w = theme.search_w.min(middle.w);
        let (crumb, search) = middle.split_left((middle.w - search_w - theme.pad).max(0.0));
        let (_, search) = search.split_left(theme.pad.min(search.w));
        // Why the table is not moving, when it is not: at the end of the crumb's
        // space.
        let crumb = match self.status {
            Some((text, color)) => {
                let (crumb, slot) = crumb.split_left((crumb.w - STATUS_W).max(0.0));
                dl.text(
                    text,
                    slot,
                    theme.small,
                    color,
                    HAlign::Right,
                    VAlign::Middle,
                    true,
                );
                crumb
            }
            None => crumb,
        };

        buf.clear();
        if rows.filtered() {
            let _ = write!(buf, "{} of {} processes", rows.listed(), rows.population());
        } else {
            let _ = write!(buf, "{} processes", rows.listed());
        }
        dl.text(
            buf,
            count,
            theme.small,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );

        self.search.paint(dl, search, theme);

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

/// The display, frozen. Samples keep arriving and are recorded; the screen shows
/// the moment of the pause until it ends.
#[derive(Debug)]
struct Paused {
    /// History as of the pause, for the charts. The live timeline keeps
    /// recording, so resuming loses nothing.
    timeline: Timeline,
    /// The newest snapshot that arrived during the pause, shown on resume.
    latest: Option<Arc<Snapshot>>,
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
    layout: Layout,
    /// The last finished CPU sample, shown under its process while it lives.
    attribution: Option<Arc<Attribution>>,
    /// A sample in progress, shown as a marker row under its process.
    sampling: Option<ProcessKey>,
    timeline: Timeline,
    /// The summary charts above the process table.
    charts: ChartGroup,
    page: Page,
    nav: NavRail,
    perf: PerfPage,
    /// Sort keys with hysteresis, so noise does not reorder the table. Held while
    /// the pointer is over the table.
    steady: Steady,
    /// Set while the display is paused.
    paused: Option<Paused>,
    settings: Settings,
    settings_page: SettingsPage,
    /// Whether the platform has animation effects on; row slides need both this
    /// and the setting.
    system_animations: bool,
    /// Search results per process; empty when there is no search.
    matched: Vec<bool>,
    /// What the table lists: matches, plus their ancestors in tree mode.
    shown: Vec<bool>,
    /// Last known pointer position, for the cursor shape.
    mouse: Option<Point>,
    buf: String,
    pid_buf: String,
    subtree: Vec<usize>,
}

impl App {
    #[must_use]
    pub fn new(theme: Theme) -> Self {
        let mut app = Self {
            theme,
            backdrop: false,
            size: Size::new(800.0, 600.0),
            table: Table::new(columns(), col::CPU),
            toolbar: Toolbar::default(),
            snap: Arc::new(Snapshot::default()),
            tree: ProcessTree::default(),
            layout: Layout::default(),
            attribution: None,
            sampling: None,
            timeline: Timeline::new(HISTORY),
            charts: ChartGroup::default(),
            page: Page::default(),
            nav: NavRail::default(),
            perf: PerfPage::default(),
            steady: Steady::default(),
            paused: None,
            settings: Settings::default(),
            settings_page: SettingsPage::default(),
            system_animations: true,
            matched: Vec::new(),
            shown: Vec::new(),
            mouse: None,
            buf: String::with_capacity(64),
            pid_buf: String::with_capacity(12),
            subtree: Vec::new(),
        };
        app.apply_animation();
        app
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
        // The filter's ancestor rule depends on the mode, and the reveal that
        // follows needs the order the new mode will actually show.
        self.refilter(mode == ViewMode::Tree);
        let rows = rows_of!(self, mode == ViewMode::Tree);
        apply_view(&mut self.table, mode, &rows, &self.theme);
    }

    /// The row source for the current snapshot and mode, borrowing all of `self`.
    /// Where the table is mutated while the rows are alive, use [`rows_of!`], which
    /// borrows only the fields it needs.
    fn rows(&self) -> ProcessRows<'_> {
        rows_of!(self, self.table.tree())
    }

    fn relayout(&mut self) {
        self.layout
            .rebuild(&self.snap, self.attribution.as_deref(), self.sampling);
        self.update_steady();
        self.table.refresh();
    }

    /// Fold the rows' current values into the sticky sort keys.
    fn update_steady(&mut self) {
        let tree = self.table.tree();
        let column = self.table.sort_col;
        // The rows borrow the app; the keys are taken out while they are updated.
        let mut steady = std::mem::take(&mut self.steady);
        {
            let rows = ProcessRows {
                steady: None,
                ..rows_of!(self, tree)
            };
            steady.update(
                column,
                tree,
                process_rows::band(column),
                (0..rows.len()).filter_map(|r| rows.raw_key(r, column).map(|v| (rows.id(r), v))),
            );
        }
        self.steady = steady;
    }

    /// Hold the order while the pointer is over the table; let it go, re-sorting
    /// at once, when it leaves. Returns whether anything is to be repainted.
    fn hold_order(&mut self, on: bool) -> bool {
        if !self.steady.set_held(on) {
            return false;
        }
        if !on && self.paused.is_none() {
            self.update_steady();
            self.table.refresh();
        }
        true
    }

    /// Whether the display is paused.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.paused.is_some()
    }

    fn toggle_pause(&mut self) -> Reaction {
        match self.paused.take() {
            None => {
                self.paused = Some(Paused {
                    timeline: self.timeline.clone(),
                    latest: None,
                });
            }
            Some(p) => {
                if let Some(snap) = p.latest {
                    self.show_snapshot(snap);
                }
            }
        }
        Reaction::REPAINT
    }

    /// The settings as they stand.
    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Apply settings, as loaded at start or changed on the Settings page.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.apply_animation();
    }

    /// Whether the platform has animation effects on (Windows: "Animation
    /// effects" in Accessibility > Visual effects). Row slides need it.
    pub fn set_system_animations(&mut self, on: bool) {
        self.system_animations = on;
        self.apply_animation();
    }

    /// The platform's animation effects, as last told.
    #[must_use]
    pub fn system_animations(&self) -> bool {
        self.system_animations
    }

    fn apply_animation(&mut self) {
        self.table
            .set_animate(self.settings.animates_rows(self.system_animations));
    }

    /// Whether something is moving, so the shell should paint another frame soon.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.page == Page::Processes && self.table.animating()
    }

    /// A CPU sample finished: show it under its process. Returns true if the
    /// process is still in the table.
    pub fn set_attribution(&mut self, a: Arc<Attribution>) -> bool {
        if self.sampling == Some(a.target) {
            self.sampling = None;
        }
        let live = self.snap.processes.iter().any(|p| p.key() == a.target);
        self.attribution = Some(a);
        self.relayout();
        live
    }

    /// A CPU sample could not run or finish. Clears the marker row; the shell
    /// tells the user why.
    pub fn sampling_failed(&mut self, target: ProcessKey) {
        if self.sampling == Some(target) {
            self.sampling = None;
            self.relayout();
        }
    }

    /// Whether a sample is in progress.
    #[must_use]
    pub fn sampling(&self) -> Option<ProcessKey> {
        self.sampling
    }

    /// The current search text.
    #[must_use]
    pub fn search(&self) -> &str {
        &self.toolbar.search.text
    }

    /// Which pointer to show at the last known mouse position.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        if self.page != Page::Processes {
            return Cursor::Arrow;
        }
        if self.table.resizing() {
            return Cursor::ResizeColumn;
        }
        let Some(p) = self.mouse else {
            return Cursor::Arrow;
        };
        let search = &self.toolbar.search;
        if self.table.divider_at(p).is_some() {
            Cursor::ResizeColumn
        } else if search.rect.contains(p) && !search.clear_rect.contains(p) {
            Cursor::Text
        } else {
            Cursor::Arrow
        }
    }

    /// Offer the newest snapshot. Returns true if it was new and a repaint is due.
    /// While paused it is recorded but not shown, and no repaint is due.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) -> bool {
        if snap.is_empty() || (!self.snap.is_empty() && snap.tick == self.snap.tick) {
            return false;
        }
        self.timeline.observe(&snap);
        if let Some(p) = &mut self.paused {
            p.latest = Some(snap);
            return false;
        }
        self.show_snapshot(snap);
        true
    }

    /// Make `snap` the one on screen. Its history is already in the timeline.
    fn show_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.tree.rebuild(&snap.processes);
        self.snap = snap;
        // A finished sample outlives its process only until the next snapshot.
        if let Some(a) = &self.attribution {
            if !self.snap.processes.iter().any(|p| p.key() == a.target) {
                self.attribution = None;
            }
        }
        self.relayout();
        self.refilter(self.table.tree());
    }

    /// Recompute the search results against the current snapshot.
    fn refilter(&mut self, tree: bool) {
        self.matched.clear();
        self.shown.clear();
        self.table.invalidate_order();
        let needle = &self.toolbar.search.needle;
        if needle.is_empty() {
            return;
        }
        let pid_buf = &mut self.pid_buf;
        self.matched.extend(
            self.snap
                .processes
                .iter()
                .map(|p| process_matches(p, needle, pid_buf)),
        );
        self.shown.extend_from_slice(&self.matched);
        if tree {
            self.tree.propagate_up(&mut self.shown);
        }
    }

    /// The search text changed: apply it. Repaints.
    fn search_changed(&mut self) -> Reaction {
        self.refilter(self.table.tree());
        Reaction::REPAINT
    }

    /// Show `page`, dropping hover state the page being left would otherwise keep.
    pub fn set_page(&mut self, page: Page) {
        if page != self.page {
            let _ = self.charts.hover(None);
            self.table.hover = None;
            let _ = self.hold_order(false);
            let _ = self.perf.handle(UiEvent::MouseLeave);
            let _ = self.settings_page.handle(
                UiEvent::MouseLeave,
                &mut self.settings,
                self.system_animations,
            );
            self.page = page;
        }
    }

    #[must_use]
    pub fn page(&self) -> Page {
        self.page
    }

    /// Handle input. Says whether to repaint and what else to do.
    pub fn handle(&mut self, ev: UiEvent) -> Reaction {
        let mut rail_moved = false;
        match ev {
            UiEvent::Resize(s) => {
                self.size = s;
                return Reaction::REPAINT;
            }
            UiEvent::MouseMove(p) => {
                self.mouse = Some(p);
                rail_moved = self.nav.set_hover(Some(p));
            }
            UiEvent::MouseLeave => {
                self.mouse = None;
                rail_moved = self.nav.set_hover(None);
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => match self.nav.hit(at) {
                Some(NavHit::Toggle) => {
                    self.nav.toggle(self.size.w);
                    return Reaction::REPAINT;
                }
                Some(NavHit::Page(page)) => {
                    self.set_page(page);
                    return Reaction::REPAINT;
                }
                None => {}
            },
            UiEvent::Command(Command::SetPage(page)) => {
                self.set_page(page);
                return Reaction::REPAINT;
            }
            UiEvent::Command(Command::StepPage(n)) => {
                self.set_page(self.page.step(n));
                return Reaction::REPAINT;
            }
            // Space pauses, as in Process Explorer, on any page; typed into the
            // search field it is just a space.
            UiEvent::Command(Command::TogglePause) => return self.toggle_pause(),
            UiEvent::Char(' ') if !self.toolbar.search.focused => return self.toggle_pause(),
            // Search belongs to the process table: typing, or Ctrl+F, on another page
            // goes there, the way Task Manager's search box does.
            UiEvent::Char(_) | UiEvent::Command(Command::Find) => self.set_page(Page::Processes),
            _ => {}
        }
        let r = match self.page {
            Page::Processes => self.handle_processes(ev),
            Page::Performance => self.perf.handle(ev),
            Page::Settings => {
                let r = self
                    .settings_page
                    .handle(ev, &mut self.settings, self.system_animations);
                if matches!(r.effect, Some(Effect::SaveSettings(_))) {
                    self.apply_animation();
                }
                r
            }
        };
        Reaction {
            repaint: r.repaint || rail_moved,
            ..r
        }
    }

    fn handle_processes(&mut self, ev: UiEvent) -> Reaction {
        match ev {
            UiEvent::Resize(s) => {
                self.size = s;
                Reaction::REPAINT
            }
            UiEvent::MouseMove(p) => {
                self.mouse = Some(p);
                if self.table.resizing() {
                    return Reaction::painted(self.table.resize_to(p));
                }
                let hover = match self.table.hit(p, &self.theme) {
                    Hit::Row(i) | Hit::Expander(i) => Some(i),
                    _ => None,
                };
                let segment = self.toolbar.segment_at(p);
                let clear = self.toolbar.search.clear_rect.contains(p);
                let crosshair = self.charts.hover(Some(p));
                let held = self.hold_order(self.table.rect().contains(p));
                let changed = hover != self.table.hover
                    || segment != self.toolbar.hover
                    || clear != self.toolbar.search.hover_clear
                    || crosshair
                    || held;
                self.table.hover = hover;
                self.toolbar.hover = segment;
                self.toolbar.search.hover_clear = clear;
                Reaction::painted(changed)
            }
            UiEvent::MouseLeave => {
                self.mouse = None;
                let row = self.table.hover.take().is_some();
                let segment = self.toolbar.hover.take().is_some();
                let clear = std::mem::take(&mut self.toolbar.search.hover_clear);
                let crosshair = self.charts.hover(None);
                let held = self.hold_order(false);
                Reaction::painted(row || segment || clear || crosshair || held)
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => self.left_down(at),
            UiEvent::MouseUp {
                at,
                button: MouseButton::Left,
            } => {
                if self.table.resizing() {
                    self.table.resize_to(at);
                    self.table.end_resize();
                    return Reaction::REPAINT;
                }
                Reaction::NONE
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Right,
            } => {
                // Right-click selects like a left click, so the menu that follows
                // (via `ContextMenu`) applies to the row under the pointer.
                Reaction::painted(self.select_at(at))
            }
            UiEvent::MouseUp { .. } => Reaction::NONE,
            UiEvent::Wheel {
                lines, horizontal, ..
            } => {
                if horizontal {
                    self.table.scroll_x_by(lines * 40.0);
                } else {
                    self.table.scroll_lines(lines * 3.0);
                }
                Reaction::REPAINT
            }
            UiEvent::Char(c) => {
                self.toolbar.search.focused = true;
                self.toolbar.search.push(c);
                self.search_changed()
            }
            UiEvent::Key(k) => self.key(k),
            UiEvent::ContextMenu { at } => self.context_menu(at),
            UiEvent::Command(c) => self.command(c),
        }
    }

    fn left_down(&mut self, at: Point) -> Reaction {
        let theme = &self.theme;
        if let Some(i) = self.toolbar.segment_at(at) {
            self.toolbar.search.focused = false;
            self.set_view(SEGMENTS[i].0);
            return Reaction::REPAINT;
        }
        let search = &mut self.toolbar.search;
        if search.clear_rect.contains(at) {
            search.clear();
            search.focused = false;
            return self.search_changed();
        }
        if search.rect.contains(at) {
            search.focused = true;
            return Reaction::REPAINT;
        }
        search.focused = false;
        let rows = rows_of!(self, self.table.tree());
        match self.table.hit(at, theme) {
            Hit::Divider(c) => {
                self.table.begin_resize(c, at);
                Reaction::NONE
            }
            Hit::Header(c) => {
                self.table.set_sort(c);
                Reaction::REPAINT
            }
            Hit::Expander(pos) => Reaction::painted(self.table.toggle_expanded(pos, &rows)),
            Hit::Row(pos) => {
                self.table.selected = self.table.row_at(pos).map(|r| rows.id(r));
                Reaction::REPAINT
            }
            Hit::Nothing => Reaction::REPAINT,
        }
    }

    /// Select the row under `at`, if there is one. Returns whether anything changed.
    fn select_at(&mut self, at: Point) -> bool {
        let rows = rows_of!(self, self.table.tree());
        match self.table.hit(at, &self.theme) {
            Hit::Row(pos) | Hit::Expander(pos) => {
                let id = self.table.row_at(pos).map(|r| rows.id(r));
                let changed = id != self.table.selected;
                self.table.selected = id;
                changed
            }
            _ => false,
        }
    }

    fn key(&mut self, k: Key) -> Reaction {
        let rows = rows_of!(self, self.table.tree());
        let theme = &self.theme;
        let page = isize::try_from(self.table.rows_visible(theme).max(1)).unwrap_or(isize::MAX);
        match k {
            Key::Up => self.table.move_selection(&rows, -1, theme),
            Key::Down => self.table.move_selection(&rows, 1, theme),
            Key::PageUp => self.table.move_selection(&rows, -page, theme),
            Key::PageDown => self.table.move_selection(&rows, page, theme),
            Key::Home => self.table.select_end(&rows, true, theme),
            Key::End => self.table.select_end(&rows, false, theme),
            Key::Left => return Reaction::painted(self.table.collapse_or_parent(&rows, theme)),
            Key::Right => return Reaction::painted(self.table.expand_or_child(&rows, theme)),
            Key::Backspace | Key::WordBackspace => {
                let search = &mut self.toolbar.search;
                if !search.backspace(k == Key::WordBackspace) {
                    return Reaction::NONE;
                }
                search.focused = true;
                return self.search_changed();
            }
            Key::Escape => {
                let search = &mut self.toolbar.search;
                if search.text.is_empty() && !search.focused {
                    return Reaction::NONE;
                }
                search.clear();
                search.focused = false;
                return self.search_changed();
            }
            Key::Enter => {
                let search = &mut self.toolbar.search;
                return Reaction::painted(std::mem::take(&mut search.focused));
            }
        }
        Reaction::REPAINT
    }

    fn command(&mut self, c: Command) -> Reaction {
        match c {
            Command::ToggleView => {
                self.set_view(self.view().other());
                Reaction::REPAINT
            }
            Command::SetView(m) => {
                self.set_view(m);
                Reaction::REPAINT
            }
            Command::Find => {
                self.toolbar.search.focused = true;
                Reaction::REPAINT
            }
            Command::Menu(action) => self.menu_action(action),
            // Handled in `handle` before a page sees them.
            Command::SetPage(_) | Command::StepPage(_) | Command::TogglePause => Reaction::NONE,
        }
    }

    /// The process the selected row belongs to, as an index into the snapshot. A
    /// service, thread or sample row acts for its process.
    fn selected_process(&self) -> Option<usize> {
        let rows = self.rows();
        self.table
            .selected
            .and_then(|id| rows.row_of(id))
            .map(|row| rows.process_of(row))
    }

    fn context_menu(&mut self, at: Option<Point>) -> Reaction {
        let anchor = if let Some(p) = at {
            self.select_at(p);
            match self.table.hit(p, &self.theme) {
                Hit::Row(_) | Hit::Expander(_) => p,
                _ => return Reaction::NONE,
            }
        } else {
            // From the keyboard: just under the selected row's name.
            let rows = self.rows();
            let Some(r) = self.table.selected_rect(&rows, &self.theme) else {
                return Reaction::NONE;
            };
            Point::new(r.x + self.theme.pad + self.theme.expander_w, r.bottom())
        };
        let Some(row) = self.selected_process() else {
            return Reaction::NONE;
        };
        let p = &self.snap.processes[row];
        let can_sample =
            self.snap.capabilities.cpu_sampling && p.key().pid > 4 && self.sampling.is_none();
        let entries = vec![
            MenuEntry::Item {
                action: MenuAction::EndTask,
                label: "End task",
                enabled: true,
            },
            MenuEntry::Item {
                action: MenuAction::EndTree,
                label: "End process tree",
                enabled: self.tree.rollup(row).descendants > 0,
            },
            MenuEntry::Separator,
            MenuEntry::Item {
                action: MenuAction::OpenFileLocation,
                label: "Open file location",
                enabled: p.statics.image_path.is_some(),
            },
            MenuEntry::Separator,
            MenuEntry::Item {
                action: MenuAction::SampleCpu,
                label: "Sample CPU for 5 s",
                enabled: can_sample,
            },
        ];
        Reaction::effect(Effect::Menu {
            at: anchor,
            entries,
        })
    }

    fn menu_action(&mut self, action: MenuAction) -> Reaction {
        let Some(row) = self.selected_process() else {
            return Reaction::NONE;
        };
        let p = &self.snap.processes[row];
        match action {
            MenuAction::SampleCpu => {
                if !self.snap.capabilities.cpu_sampling || self.sampling.is_some() {
                    return Reaction::NONE;
                }
                let target = p.key();
                self.sampling = Some(target);
                self.relayout();
                Reaction::effect(Effect::SampleCpu { target, seconds: 5 })
            }
            MenuAction::EndTask => Reaction::effect(Effect::Terminate {
                targets: vec![p.key()],
                label: p.name().to_owned(),
            }),
            MenuAction::EndTree => {
                self.tree.subtree(row, &mut self.subtree);
                let targets: Vec<ProcessKey> = self
                    .subtree
                    .iter()
                    .map(|&i| self.snap.processes[i].key())
                    .collect();
                let below = targets.len() - 1;
                let label = if below == 0 {
                    p.name().to_owned()
                } else {
                    format!(
                        "{} and {below} process{} under it",
                        p.name(),
                        if below == 1 { "" } else { "es" }
                    )
                };
                Reaction::effect(Effect::Terminate { targets, label })
            }
            MenuAction::OpenFileLocation => match &p.statics.image_path {
                Some(path) => Reaction::effect(Effect::OpenFileLocation(path.clone())),
                None => Reaction::NONE,
            },
        }
    }

    /// Produce this frame.
    pub fn paint(&mut self, dl: &mut DisplayList) {
        self.paint_at(dl, Instant::now());
    }

    /// Produce the frame for time `now`, which paces the row slides.
    pub fn paint_at(&mut self, dl: &mut DisplayList, now: Instant) {
        self.table.tick(now);
        dl.clear();
        let theme = &self.theme;
        dl.clear_to(if self.backdrop {
            Color::TRANSPARENT
        } else {
            theme.bg_solid
        });

        let window = Rect::from_size(self.size);
        let expanded = self.nav.is_expanded(self.size.w);
        let (rail, content) = window.split_left(self.nav.width(self.size.w));
        self.nav.paint(dl, rail, self.page, expanded, theme);
        let full = content.inset(theme.gap, theme.gap);
        // While paused, the charts show history as of the pause.
        let timeline = match &self.paused {
            Some(p) => &p.timeline,
            None => &self.timeline,
        };
        match self.page {
            Page::Processes => {}
            Page::Performance => {
                let snap = Arc::clone(&self.snap);
                self.perf
                    .paint(dl, full, &snap, timeline, theme, &mut self.buf);
                return;
            }
            Page::Settings => {
                self.settings_page
                    .paint(dl, full, self.settings, self.system_animations, theme);
                return;
            }
        }
        let (cards, rest) = full.split_top(CARD_H);
        let (_, rest) = rest.split_top(theme.gap);
        let (toolbar, rest) = rest.split_top(theme.toolbar_h);
        let (_, table_rect) = rest.split_top(theme.gap * 0.5);

        let (cpu_card, mem_card) = cards.split_left((cards.w - theme.gap) * 0.5);
        let (_, mem_card) = mem_card.split_left(theme.gap);

        let snap = Arc::clone(&self.snap);
        let graphs = [
            Self::paint_cpu_card(dl, cpu_card, &snap, theme, &mut self.buf),
            Self::paint_mem_card(dl, mem_card, &snap, theme, &mut self.buf),
        ];
        // Both summary charts share one hover.
        let series = [&timeline.cpu_total, &timeline.mem_in_use];
        let max = [100.0, snap.memory.total.get() as f32];
        let colors = [theme.cpu, theme.memory];
        let values: [charts::ValueFmt; 2] = [charts::percent_value, charts::bytes_value];
        self.charts.begin(2);
        for i in 0..2 {
            self.charts
                .build(i, graphs[i], charts::AXIS_BAND_H, series[i], max[i]);
        }
        self.charts.snap();
        for i in 0..2 {
            let style = charts::style(colors[i], theme);
            let _ = self
                .charts
                .paint(i, dl, &style, values[i], theme, &mut self.buf);
        }

        let rows = ProcessRows {
            procs: &snap.processes,
            threads: &snap.threads,
            tree: &self.tree,
            layout: &self.layout,
            attribution: self.attribution.as_deref(),
            interval_secs: interval_secs(&snap),
            mem_total: snap.memory.total.get() as f32,
            matched: &self.matched,
            shown: &self.shown,
            tree_mode: self.table.tree(),
            steady: Some(&self.steady),
        };
        // The table first: the toolbar reads its state, and the order must be
        // current before the ancestry lookup.
        self.table
            .paint(dl, table_rect, &rows, theme, &mut self.buf);
        self.toolbar.status = if self.paused.is_some() {
            Some(("Paused \u{b7} Space resumes", theme.accent))
        } else if self.steady.held() {
            Some(("Order held", theme.text_dim))
        } else {
            None
        };
        self.toolbar
            .paint(dl, toolbar, &self.table, &rows, theme, &mut self.buf);
    }

    fn card_frame(dl: &mut DisplayList, rect: Rect, theme: &Theme) -> Rect {
        dl.fill_round_rect(rect, theme.card_radius, theme.surface);
        dl.stroke_rect(rect, theme.surface_border, 1.0);
        rect.inset(theme.pad, theme.pad * 0.75)
    }

    /// The card's frame and text; returns the area left for its chart.
    fn paint_cpu_card(
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        theme: &Theme,
        buf: &mut String,
    ) -> Rect {
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
        graph
    }

    /// The card's frame and text; returns the area left for its chart.
    fn paint_mem_card(
        dl: &mut DisplayList,
        rect: Rect,
        snap: &Snapshot,
        theme: &Theme,
        buf: &mut String,
    ) -> Rect {
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
        graph
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
#[allow(unused_must_use)] // tests drive the view and ignore most reactions
mod tests {
    use super::*;
    use crate::process_rows::row_id;
    use crate::process_rows::tests::proc;
    use ot_model::memory::MemorySample;
    use ot_model::process::{ProcessSample, ProcessStatic};
    use ot_model::{Bytes, Percent, ProcessKey, Tick};
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

    /// Names in the order the table paints them, with the color they got. Only the
    /// table body and header are clipped, so text inside a clip is a table cell
    /// and not, say, the toolbar's breadcrumb.
    fn painted_rows(app: &mut App) -> Vec<(String, Color)> {
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
                        names.push((s.to_owned(), t.color));
                    }
                }
                _ => {}
            }
        }
        names
    }

    fn painted_names(app: &mut App) -> Vec<String> {
        painted_rows(app).into_iter().map(|(n, _)| n).collect()
    }

    /// Sized and painted once. Rows jump rather than slide, so a frame shows the
    /// order as it is; the slide has tests of its own.
    fn ready(app: &mut App) {
        app.set_system_animations(false);
        app.handle(UiEvent::Resize(Size::new(900.0, 700.0)));
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
    }

    fn cmd(app: &mut App, c: Command) -> Reaction {
        app.handle(UiEvent::Command(c))
    }

    fn key(app: &mut App, k: Key) -> Reaction {
        app.handle(UiEvent::Key(k))
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle(UiEvent::Char(c));
        }
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
        assert!(key(&mut app, Key::Down).repaint);
        assert_eq!(app.table.selected, Some(id(2)));
        key(&mut app, Key::Down);
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
        cmd(&mut app, Command::SetView(ViewMode::Tree));
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
        key(&mut app, Key::Down);
        assert_eq!(app.table.selected, Some(id(11)));

        // Collapse everything above it in the tree first, to prove reveal expands.
        cmd(&mut app, Command::ToggleView);
        key(&mut app, Key::Left); // 11 is a leaf: moves to 10
        key(&mut app, Key::Left); // collapses 10
        key(&mut app, Key::Left); // moves to 4
        key(&mut app, Key::Left); // collapses 4
        assert_eq!(painted_names(&mut app), ["p4.exe (4)", "p20.exe"]);
        app.table.selected = Some(id(11));

        cmd(&mut app, Command::ToggleView);
        assert_eq!(app.view(), ViewMode::List);
        assert_eq!(app.table.selected, Some(id(11)));
        cmd(&mut app, Command::ToggleView);
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
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        key(&mut app, Key::Down); // selects 4
        key(&mut app, Key::Left); // collapses it
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
        cmd(&mut app, Command::ToggleView);
        assert!(painted_strings(&mut app).iter().any(|s| s == crumb));
    }

    #[test]
    fn clicking_the_segments_switches_modes() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let tree_seg = app.toolbar.segments[1].center();
        assert!(app.handle(UiEvent::MouseMove(tree_seg)).repaint);
        assert_eq!(app.toolbar.hover, Some(1));
        assert!(
            app.handle(UiEvent::MouseDown {
                at: tree_seg,
                button: MouseButton::Left,
            })
            .repaint
        );
        assert_eq!(app.view(), ViewMode::Tree);
        let list_seg = app.toolbar.segments[0].center();
        app.handle(UiEvent::MouseDown {
            at: list_seg,
            button: MouseButton::Left,
        });
        assert_eq!(app.view(), ViewMode::List);
        assert!(app.handle(UiEvent::MouseLeave).repaint);
        assert_eq!(app.toolbar.hover, None);
    }

    /// The table's left edge: right of the rail, which is compact in the 900-wide
    /// test window.
    fn table_left(theme: &Theme) -> f32 {
        crate::nav::COMPACT_W + theme.gap
    }

    fn table_top(theme: &Theme) -> f32 {
        theme.gap + CARD_H + theme.gap + theme.toolbar_h + theme.gap * 0.5
    }

    #[test]
    fn clicking_an_expander_collapses_without_selecting() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        let theme = app.theme.clone();
        // First row is p4.exe at depth 0; its expander box starts at the padding.
        let at = Point::new(
            table_left(&theme) + theme.pad + theme.expander_w * 0.5,
            table_top(&theme) + theme.header_h + theme.row_h * 0.5,
        );
        assert_eq!(app.table.hit(at, &theme), Hit::Expander(0));
        assert!(
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            })
            .repaint
        );
        assert_eq!(app.table.selected, None);
        assert_eq!(painted_names(&mut app), ["p4.exe (4)", "p20.exe"]);
    }

    #[test]
    fn typing_filters_the_list_and_escape_clears() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        type_str(&mut app, "P1");
        assert_eq!(app.search(), "P1");
        assert!(app.toolbar.search.focused);
        assert_eq!(
            painted_names(&mut app),
            ["p11.exe", "p12.exe", "p13.exe", "p10.exe"]
        );
        let strings = painted_strings(&mut app);
        assert!(
            strings.iter().any(|s| s == "4 of 6 processes"),
            "{strings:?}"
        );
        assert!(strings.iter().any(|s| s == "P1"), "field text painted");

        // Backspace narrows back out; Ctrl+Backspace removes the word.
        key(&mut app, Key::Backspace);
        assert_eq!(app.search(), "P");
        type_str(&mut app, "20 tail");
        key(&mut app, Key::WordBackspace);
        assert_eq!(app.search(), "P20 ");
        key(&mut app, Key::WordBackspace);
        assert_eq!(app.search(), "");
        assert_eq!(key(&mut app, Key::Backspace), Reaction::NONE);

        type_str(&mut app, "zzz");
        assert_eq!(painted_names(&mut app), Vec::<String>::new());
        assert!(key(&mut app, Key::Escape).repaint);
        assert_eq!(app.search(), "");
        assert!(!app.toolbar.search.focused);
        assert_eq!(painted_names(&mut app).len(), 6);
        assert_eq!(key(&mut app, Key::Escape), Reaction::NONE);

        // Enter just leaves the field.
        assert!(cmd(&mut app, Command::Find).repaint);
        assert!(key(&mut app, Key::Enter).repaint);
        assert_eq!(key(&mut app, Key::Enter), Reaction::NONE);
    }

    #[test]
    fn a_filtered_tree_keeps_ancestors_muted_and_arrows_still_move() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        type_str(&mut app, "p13");
        let rows = painted_rows(&mut app);
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["p4.exe", "p12.exe", "p13.exe"]);
        let dim = app.theme.text_dim;
        assert_eq!(rows[0].1, dim, "context ancestor is muted");
        assert_eq!(rows[1].1, dim);
        assert_eq!(rows[2].1, app.theme.text, "the match is not");

        // Switching to the list drops the context rows; back to the tree brings
        // them back, because the filter is recomputed for the mode.
        cmd(&mut app, Command::ToggleView);
        assert_eq!(painted_names(&mut app), ["p13.exe"]);
        cmd(&mut app, Command::ToggleView);
        assert_eq!(painted_names(&mut app).len(), 3);

        // The field has focus, and Down still walks the table.
        assert!(app.toolbar.search.focused);
        key(&mut app, Key::Down);
        assert_eq!(app.table.selected, Some(id(4)));
    }

    #[test]
    fn clicking_the_field_and_its_clear_button() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let field = app.toolbar.search.rect;
        assert!(field.w > 0.0, "the field was laid out");
        let at = Point::new(field.x + 5.0, field.center().y);
        app.handle(UiEvent::MouseMove(at));
        assert_eq!(app.cursor(), Cursor::Text);
        assert!(
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left
            })
            .repaint
        );
        assert!(app.toolbar.search.focused);
        type_str(&mut app, "p2");
        painted_strings(&mut app); // lays out the clear button
        let clear = app.toolbar.search.clear_rect.center();
        assert!(app.handle(UiEvent::MouseMove(clear)).repaint);
        assert!(app.toolbar.search.hover_clear);
        assert_eq!(app.cursor(), Cursor::Arrow);
        app.handle(UiEvent::MouseDown {
            at: clear,
            button: MouseButton::Left,
        });
        assert_eq!(app.search(), "");
        assert!(!app.toolbar.search.focused);
        // Clicking empty table space drops focus too.
        cmd(&mut app, Command::Find);
        app.handle(UiEvent::MouseDown {
            at: Point::new(400.0, 690.0),
            button: MouseButton::Left,
        });
        assert!(!app.toolbar.search.focused);
    }

    #[test]
    fn right_click_selects_and_the_menu_reflects_the_row() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let theme = app.theme.clone();
        let row_y =
            |pos: usize| table_top(&theme) + theme.header_h + theme.row_h * (pos as f32 + 0.5);
        // List order: p11 first. Right-click the second row (p20, a leaf).
        let at = Point::new(100.0, row_y(1));
        assert!(
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Right
            })
            .repaint
        );
        assert_eq!(app.table.selected, Some(id(20)));
        let r = app.handle(UiEvent::ContextMenu { at: Some(at) });
        let Some(Effect::Menu {
            at: anchor,
            entries,
        }) = r.effect
        else {
            panic!("{r:?}");
        };
        assert_eq!(anchor, at);
        assert_eq!(
            entries,
            vec![
                MenuEntry::Item {
                    action: MenuAction::EndTask,
                    label: "End task",
                    enabled: true
                },
                MenuEntry::Item {
                    action: MenuAction::EndTree,
                    label: "End process tree",
                    enabled: false
                },
                MenuEntry::Separator,
                MenuEntry::Item {
                    action: MenuAction::OpenFileLocation,
                    label: "Open file location",
                    enabled: false
                },
                MenuEntry::Separator,
                MenuEntry::Item {
                    action: MenuAction::SampleCpu,
                    label: "Sample CPU for 5 s",
                    enabled: false
                },
            ]
        );
        // Off the rows: nothing.
        assert_eq!(
            app.handle(UiEvent::ContextMenu {
                at: Some(Point::new(100.0, 5.0))
            }),
            Reaction::NONE
        );
        // From the keyboard: anchored to the selected row.
        let r = app.handle(UiEvent::ContextMenu { at: None });
        let Some(Effect::Menu { at: anchor, .. }) = r.effect else {
            panic!("{r:?}");
        };
        assert!((anchor.y - (row_y(1) + theme.row_h * 0.5)).abs() < 0.01);
        app.table.selected = None;
        assert_eq!(
            app.handle(UiEvent::ContextMenu { at: None }),
            Reaction::NONE
        );
    }

    #[test]
    fn end_task_and_end_tree_name_their_targets_parent_first() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        assert_eq!(
            cmd(&mut app, Command::Menu(MenuAction::EndTask)),
            Reaction::NONE
        );
        app.table.selected = Some(id(4));
        let r = cmd(&mut app, Command::Menu(MenuAction::EndTask));
        assert_eq!(
            r.effect,
            Some(Effect::Terminate {
                targets: vec![ProcessKey::new(4, 1)],
                label: "p4.exe".to_owned()
            })
        );
        let r = cmd(&mut app, Command::Menu(MenuAction::EndTree));
        let Some(Effect::Terminate { targets, label }) = r.effect else {
            panic!("{r:?}");
        };
        assert_eq!(label, "p4.exe and 4 processes under it");
        assert_eq!(targets[0], ProcessKey::new(4, 1));
        assert_eq!(targets.len(), 5);
        let pos = |pid: u32| targets.iter().position(|k| k.pid == pid).unwrap();
        assert!(pos(10) < pos(11) && pos(12) < pos(13));
        // A leaf's tree is just itself, named plainly.
        app.table.selected = Some(id(20));
        let r = cmd(&mut app, Command::Menu(MenuAction::EndTree));
        assert!(
            matches!(r.effect, Some(Effect::Terminate { ref label, ref targets }) if label == "p20.exe" && targets.len() == 1)
        );
        // No path known: no effect.
        assert_eq!(
            cmd(&mut app, Command::Menu(MenuAction::OpenFileLocation)),
            Reaction::NONE
        );
    }

    #[test]
    fn open_file_location_uses_the_image_path() {
        let mut p = proc(7, None, 1.0);
        p.statics = Arc::new(ProcessStatic {
            image_path: Some("C:\\x\\p7.exe".to_owned()),
            ..(*p.statics).clone()
        });
        let mut app = App::default();
        app.set_snapshot(snapshot(1, vec![p]));
        ready(&mut app);
        app.table.selected = Some(id(7));
        let r = cmd(&mut app, Command::Menu(MenuAction::OpenFileLocation));
        assert_eq!(
            r.effect,
            Some(Effect::OpenFileLocation("C:\\x\\p7.exe".to_owned()))
        );
        let r = app.handle(UiEvent::ContextMenu { at: None });
        let Some(Effect::Menu { entries, .. }) = r.effect else {
            panic!("{r:?}");
        };
        assert!(matches!(
            entries[3],
            MenuEntry::Item {
                action: MenuAction::OpenFileLocation,
                enabled: true,
                ..
            }
        ));
    }

    #[test]
    fn dragging_a_header_divider_resizes_the_column() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let theme = app.theme.clone();
        let name_w = app.table.columns[0].width;
        let edge = Point::new(
            table_left(&theme) + name_w,
            table_top(&theme) + theme.header_h * 0.5,
        );
        app.handle(UiEvent::MouseMove(edge));
        assert_eq!(app.cursor(), Cursor::ResizeColumn);
        assert_eq!(
            app.handle(UiEvent::MouseDown {
                at: edge,
                button: MouseButton::Left
            }),
            Reaction::NONE
        );
        assert!(app.table.resizing());
        let moved = Point::new(edge.x + 35.0, 300.0);
        assert!(app.handle(UiEvent::MouseMove(moved)).repaint);
        assert_eq!(app.cursor(), Cursor::ResizeColumn);
        assert!(
            app.handle(UiEvent::MouseUp {
                at: moved,
                button: MouseButton::Left
            })
            .repaint
        );
        assert!(!app.table.resizing());
        assert!((app.table.columns[0].width - (name_w + 35.0)).abs() < 0.01);
        // The sort did not change: a divider click is not a header click.
        assert_eq!(app.table.sort_col, col::CPU);
        app.handle(UiEvent::MouseMove(Point::new(400.0, 400.0)));
        assert_eq!(app.cursor(), Cursor::Arrow);
    }

    /// Twenty seconds of history ending at t = 20 s: CPU climbing 5 % a second to
    /// 95 %, memory steady at 60 of 100 bytes.
    fn with_history(app: &mut App) {
        for t in 1..=20u64 {
            let mut s = (*snapshot(t, vec![proc(1, None, 1.0)])).clone();
            s.cpu.total = Percent(t as f32 * 5.0 - 5.0);
            s.memory = MemorySample {
                total: Bytes(100),
                available: Bytes(40),
                ..Default::default()
            };
            app.set_snapshot(Arc::new(s));
        }
    }

    #[test]
    fn charts_label_their_log_axis_and_share_one_crosshair() {
        let mut app = App::default();
        with_history(&mut app);
        ready(&mut app);
        let strings = painted_strings(&mut app);
        for label in ["now", "10s", "1m", "10m", "1h"] {
            let n = strings.iter().filter(|s| *s == label).count();
            assert_eq!(n, 2, "{label} under both charts: {strings:?}");
        }

        // Point at the CPU chart, a little off the sample 3 s old: it snaps.
        let cpu = app.charts.plot(0).rect();
        let at = Point::new(charts::AXIS.x(cpu, 3000.0) + 1.5, cpu.center().y);
        assert!(app.handle(UiEvent::MouseMove(at)).repaint);
        assert_eq!(app.charts.hover_age(), Some(3000.0));
        let nudge = Point::new(at.x + 0.5, at.y);
        assert!(
            !app.handle(UiEvent::MouseMove(nudge)).repaint,
            "same sample, nothing to redraw"
        );
        // Both charts read out the same moment; the tick labels make way.
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "80% · 3 s ago"), "{strings:?}");
        assert!(strings.iter().any(|s| s == "60 B · 3 s ago"), "{strings:?}");
        assert!(!strings.iter().any(|s| s == "10s"), "{strings:?}");

        // Hovering the memory chart drives the CPU chart's line too.
        let mem = app.charts.plot(1).rect();
        app.handle(UiEvent::MouseMove(Point::new(
            mem.right() - 1.0,
            mem.center().y,
        )));
        assert_eq!(app.charts.hover_age(), Some(0.0));
        assert!(painted_strings(&mut app).iter().any(|s| s == "95% · now"));

        assert!(app.handle(UiEvent::MouseLeave).repaint);
        assert_eq!(app.charts.hover_age(), None);
        let n = painted_strings(&mut app)
            .iter()
            .filter(|s| *s == "1m")
            .count();
        assert_eq!(n, 2, "labels are back");
    }

    #[test]
    fn a_horizontal_wheel_scrolls_the_columns() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        let total = app.table.total_width();
        assert!(total > 900.0, "columns overflow a 900-wide window: {total}");
        assert!(
            app.handle(UiEvent::Wheel {
                at: Point::new(400.0, 400.0),
                lines: 1.0,
                horizontal: true
            })
            .repaint
        );
        painted_strings(&mut app);
        let theme = app.theme.clone();
        // The first header cell moved left by the wheel step.
        assert_eq!(
            app.table.hit(
                Point::new(table_left(&theme) + 2.0, table_top(&theme) + 5.0),
                &theme
            ),
            Hit::Header(0)
        );
        assert!(matches!(
            app.table.hit(
                Point::new(
                    table_left(&theme) + app.table.columns[0].width - 30.0,
                    table_top(&theme) + 5.0
                ),
                &theme
            ),
            Hit::Header(1) | Hit::Divider(0)
        ));
    }

    #[test]
    fn the_rail_switches_pages_and_the_keys_cycle_them() {
        let mut app = App::default();
        with_history(&mut app);
        ready(&mut app);
        assert_eq!(app.page(), Page::Processes);
        let perf = app.nav.item_rect(Page::Performance).center();
        assert!(
            app.handle(UiEvent::MouseMove(perf)).repaint,
            "hover highlight"
        );
        assert!(
            app.handle(UiEvent::MouseDown {
                at: perf,
                button: MouseButton::Left
            })
            .repaint
        );
        assert_eq!(app.page(), Page::Performance);
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "Utilization"), "{strings:?}");
        assert!(!strings.iter().any(|s| s == "Name"), "no process table");

        cmd(&mut app, Command::StepPage(1));
        assert_eq!(app.page(), Page::Settings);
        cmd(&mut app, Command::StepPage(1));
        assert_eq!(app.page(), Page::Processes, "wraps around");
        cmd(&mut app, Command::StepPage(-1));
        assert_eq!(app.page(), Page::Settings);
        cmd(&mut app, Command::StepPage(-1));
        assert_eq!(app.page(), Page::Performance);
        cmd(&mut app, Command::SetPage(Page::Processes));
        assert_eq!(app.page(), Page::Processes);
    }

    #[test]
    fn typing_on_another_page_searches_the_process_table() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        app.set_page(Page::Performance);
        type_str(&mut app, "p13");
        assert_eq!(app.page(), Page::Processes);
        assert_eq!(app.search(), "p13");
        assert_eq!(painted_names(&mut app), ["p13.exe"]);
        app.set_page(Page::Performance);
        cmd(&mut app, Command::Find);
        assert_eq!(app.page(), Page::Processes);
        assert!(app.toolbar.search.focused);
    }

    #[test]
    fn process_actions_do_nothing_off_the_processes_page() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        app.table.selected = Some(id(4));
        app.set_page(Page::Performance);
        // Delete, Shift+Delete and the menu key must not reach a process the user
        // cannot see.
        assert_eq!(
            cmd(&mut app, Command::Menu(MenuAction::EndTask)),
            Reaction::NONE
        );
        assert_eq!(
            cmd(&mut app, Command::Menu(MenuAction::EndTree)),
            Reaction::NONE
        );
        assert_eq!(
            app.handle(UiEvent::ContextMenu { at: None }),
            Reaction::NONE
        );
        assert_eq!(app.cursor(), Cursor::Arrow);
    }

    #[test]
    fn the_hamburger_shows_and_hides_labels() {
        let mut app = App::default();
        ready(&mut app);
        assert!(!painted_strings(&mut app).iter().any(|s| s == "Performance"));
        let theme = app.theme.clone();
        let toggle = Point::new(crate::nav::COMPACT_W * 0.5, theme.gap + 20.0);
        assert!(
            app.handle(UiEvent::MouseDown {
                at: toggle,
                button: MouseButton::Left
            })
            .repaint
        );
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "Performance"), "{strings:?}");
        assert!(strings.iter().any(|s| s == "Processes"), "{strings:?}");
        assert_eq!(app.page(), Page::Processes, "the toggle is not a page");
    }

    fn two(tick: u64, cpu1: f32, cpu2: f32) -> Arc<Snapshot> {
        snapshot(tick, vec![proc(1, None, cpu1), proc(2, None, cpu2)])
    }

    #[test]
    fn pointing_at_the_table_holds_its_order() {
        let mut app = App::default();
        app.set_snapshot(two(1, 10.0, 50.0));
        ready(&mut app);
        assert_eq!(painted_names(&mut app), ["p2.exe", "p1.exe"]);
        let inside = app.table.rect().center();
        assert!(app.handle(UiEvent::MouseMove(inside)).repaint);
        assert!(painted_strings(&mut app).iter().any(|s| s == "Order held"));

        // The numbers swap places; the rows do not, but show the new numbers.
        app.set_snapshot(two(2, 90.0, 5.0));
        assert_eq!(painted_names(&mut app), ["p2.exe", "p1.exe"]);
        assert!(painted_strings(&mut app).iter().any(|s| s == "90"));
        // A process that appears still takes its place by value.
        app.set_snapshot(snapshot(
            3,
            vec![proc(1, None, 90.0), proc(2, None, 5.0), proc(3, None, 30.0)],
        ));
        assert_eq!(painted_names(&mut app), ["p2.exe", "p3.exe", "p1.exe"]);

        // Leaving lets go at once.
        assert!(app.handle(UiEvent::MouseLeave).repaint);
        assert_eq!(painted_names(&mut app), ["p1.exe", "p3.exe", "p2.exe"]);
        assert!(!painted_strings(&mut app).iter().any(|s| s == "Order held"));
    }

    #[test]
    fn noise_does_not_reorder_rows() {
        let mut app = App::default();
        app.set_snapshot(two(1, 5.0, 5.5));
        ready(&mut app);
        assert_eq!(painted_names(&mut app), ["p2.exe", "p1.exe"]);
        // Within a point of each other: they trade numbers, not places.
        app.set_snapshot(two(2, 5.9, 5.1));
        assert_eq!(painted_names(&mut app), ["p2.exe", "p1.exe"]);
        // A clear lead moves the row.
        app.set_snapshot(two(3, 12.0, 5.1));
        assert_eq!(painted_names(&mut app), ["p1.exe", "p2.exe"]);
    }

    #[test]
    fn space_pauses_the_display_and_resuming_catches_up() {
        let mut app = App::default();
        with_history(&mut app);
        ready(&mut app);
        assert!(app.handle(UiEvent::Char(' ')).repaint);
        assert!(app.paused());
        assert!(painted_strings(&mut app)
            .iter()
            .any(|s| s == "Paused \u{b7} Space resumes"));

        let newer = snapshot(21, vec![proc(1, None, 1.0), proc(3, None, 50.0)]);
        assert!(!app.set_snapshot(newer), "nothing to repaint while paused");
        assert!(!painted_names(&mut app).contains(&"p3.exe".to_owned()));
        // Recorded underneath, but the charts show the moment of the pause.
        assert_eq!(app.timeline.cpu_total.len(), 21);
        assert_eq!(
            app.paused.as_ref().map(|p| p.timeline.cpu_total.len()),
            Some(20)
        );

        // Space on another page pauses (and resumes) there too.
        cmd(&mut app, Command::SetPage(Page::Performance));
        assert!(app.handle(UiEvent::Char(' ')).repaint);
        assert_eq!(app.page(), Page::Performance, "Space is not a search");
        assert!(!app.paused());
        cmd(&mut app, Command::SetPage(Page::Processes));
        assert!(painted_names(&mut app).contains(&"p3.exe".to_owned()));
        assert!(!painted_strings(&mut app)
            .iter()
            .any(|s| s.starts_with("Paused")));
    }

    #[test]
    fn space_in_the_search_field_is_a_space() {
        let mut app = App::default();
        app.set_snapshot(family());
        ready(&mut app);
        type_str(&mut app, "p1 ");
        assert_eq!(app.search(), "p1 ");
        assert!(!app.paused());
        key(&mut app, Key::Enter); // leaves the field, keeping the text
        app.handle(UiEvent::Char(' '));
        assert!(app.paused());
        assert_eq!(app.search(), "p1 ");
    }

    #[test]
    fn a_reorder_slides_unless_animation_is_off() {
        let mut app = App::default();
        app.set_snapshot(two(1, 10.0, 50.0));
        ready(&mut app);
        app.set_system_animations(true);
        let t0 = Instant::now();
        let mut dl = DisplayList::new();
        app.paint_at(&mut dl, t0);
        app.set_snapshot(two(2, 90.0, 5.0));
        app.paint_at(&mut dl, t0);
        assert!(app.animating());
        app.paint_at(&mut dl, t0 + Duration::from_millis(500));
        assert!(!app.animating());

        // Windows' animation effects off: rows jump.
        app.set_system_animations(false);
        app.set_snapshot(two(3, 5.0, 90.0));
        app.paint_at(&mut dl, t0 + Duration::from_millis(600));
        assert!(!app.animating());
        app.set_system_animations(true);

        // Turned off in Settings: rows jump, and the shell is asked to store it.
        cmd(&mut app, Command::SetPage(Page::Settings));
        let strings = painted_strings(&mut app);
        assert!(strings
            .iter()
            .any(|s| s == "Animate rows as the order changes"));
        let card = app.settings_page.card(0).center();
        let r = app.handle(UiEvent::MouseDown {
            at: card,
            button: MouseButton::Left,
        });
        assert_eq!(
            r.effect,
            Some(Effect::SaveSettings(Settings {
                animate_rows: Some(false)
            }))
        );
        assert_eq!(app.settings().animate_rows, Some(false));
        cmd(&mut app, Command::SetPage(Page::Processes));
        app.paint_at(&mut dl, t0 + Duration::from_millis(700));
        app.set_snapshot(two(4, 90.0, 5.0));
        app.paint_at(&mut dl, t0 + Duration::from_millis(700));
        assert!(!app.animating());
    }
}
