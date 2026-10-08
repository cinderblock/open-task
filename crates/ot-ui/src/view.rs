//! The root view: summary cards on top, a toolbar, and the process table below.
//!
//! Input comes in as [`UiEvent`]s and goes out as a [`Reaction`]: whether to repaint,
//! and optionally an [`Effect`] for the shell to carry out with native APIs (a
//! context menu, a confirmation and kill, opening a folder). The view decides what
//! should happen; the shell decides how it looks on the platform.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use ot_core::{ProgramId, Retention, Snapshot, Timeline, Usage};
use ot_model::attribution::Attribution;
use ot_model::cpu::CoreKind;
use ot_model::process::Priority;
use ot_model::ProcessKey;
use ot_paint::{Color, DisplayList, HAlign, Point, Rect, Size, VAlign};

use crate::charts::{self, ChartClock, ChartGroup, Charted};
use crate::format;
use crate::nav::{NavHit, NavRail, Page};
use crate::pages::apps::AppsPage;
use crate::pages::connections::ConnectionsPage;
use crate::pages::services::ServicesPage;
use crate::pages::startup::StartupPage;
use crate::pages::summary::{self, SummaryPage};
use crate::pages::system::SystemPage;
use crate::pages::users::UsersPage;
use crate::pages::PageOutcome;
use crate::perf::PerfPage;
use crate::process_rows::{self, col, columns, process_matches, Layout, ProcessRows, ProcessTree};
use crate::replay::{RecordingView, ReplayAction, ReplayState, Transport};
use crate::search::SearchBox;
use crate::settings::{Context, HistoryCost, Settings, SettingsPage};
use crate::sparkline::TimeAxis;
use crate::steady::Steady;
use crate::table::{ColumnLayout, Hit, RowSource, Table};
use crate::task_manager::TaskManager;
use crate::theme::Theme;
use crate::update::{UpdateAction, UpdateView};
use crate::usage_chart::{ChartHit, UsageChart};
use crate::usage_map::{UsageMap, STRIP_H};

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
    /// The processes as a treemap of the cycles they have used.
    Map,
    /// The cycles each program has used over the last hour, as a chart.
    History,
}

impl ViewMode {
    /// Parse a command-line value. Unknown values fall back to `List`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "tree" => Self::Tree,
            "map" => Self::Map,
            "history" => Self::History,
            _ => Self::List,
        }
    }

    /// The other table arrangement, for Ctrl+T. The Map and the History have none
    /// of their own; the view goes back to the table (see [`Command::ToggleView`]).
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::List => Self::Tree,
            Self::Tree | Self::Map | Self::History => Self::List,
        }
    }
}

/// An item of a context menu, and the meaning of a shortcut that does the same
/// thing (Delete, Shift+Delete). The process ones act on the selected process;
/// the rest on the selected row of their page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Terminate the selected process.
    EndTask,
    /// Terminate the selected process and everything below it.
    EndTree,
    /// End the process and start its command line again, as Process Explorer's
    /// Restart does.
    Restart,
    /// Freeze every thread of the process, and let it go again.
    Suspend,
    Resume,
    /// Turn efficiency mode (power throttling and idle priority) on or off.
    EfficiencyMode(bool),
    SetPriority(Priority),
    /// Restrict the process to these logical processors (bit `i` is processor
    /// `i`). Chosen in the affinity submenu the shell fills.
    SetAffinity(u64),
    /// Bring the process's window to the front.
    SwitchTo,
    /// Show the selected process's executable in the file manager.
    OpenFileLocation,
    /// Look the process up in the browser.
    SearchOnline,
    /// The file manager's Properties sheet for the executable.
    Properties,
    /// Copy the row's visible cells to the clipboard.
    Copy,
    /// Write the process's memory to a dump file.
    CreateDump,
    /// What each thread of the process waits on, and on whom.
    WaitChain,
    /// Sample the selected process's CPU for a few seconds: which modules its
    /// threads run, and for a broker service, which clients it served.
    SampleCpu,
    /// Show or hide a column of the process table (the header's menu).
    ToggleColumn(usize),
    /// Put the process table's columns back as they were made.
    ResetColumns,
    /// Select the row's process on the Processes page (from another page).
    GoToProcess,
    ServiceStart,
    ServiceStop,
    ServiceRestart,
    /// The system's own services console.
    OpenServices,
    SessionDisconnect,
    SessionSignOut,
    /// Let a startup entry run at sign-in, or stop it.
    StartupEnable(bool),
    /// Run an installed program's uninstaller.
    Uninstall,
    /// Open an installed program's folder.
    OpenInstallLocation,
}

/// One line of a context menu, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        action: MenuAction,
        label: Cow<'static, str>,
        enabled: bool,
        /// Shown with a check mark: a setting that is on, or the chosen one of a
        /// set.
        checked: bool,
    },
    Submenu {
        label: &'static str,
        entries: Vec<MenuEntry>,
    },
    /// The "Set affinity" submenu, which the shell fills in: one check item per
    /// logical processor, from the process's affinity as it stands when the
    /// menu opens. A choice comes back as [`MenuAction::SetAffinity`] with the
    /// new mask.
    Affinity {
        target: ProcessKey,
    },
    Separator,
}

impl MenuEntry {
    #[must_use]
    pub fn item(action: MenuAction, label: impl Into<Cow<'static, str>>, enabled: bool) -> Self {
        Self::Item {
            action,
            label: label.into(),
            enabled,
            checked: false,
        }
    }

    #[must_use]
    pub fn checked(
        action: MenuAction,
        label: impl Into<Cow<'static, str>>,
        enabled: bool,
        checked: bool,
    ) -> Self {
        Self::Item {
            action,
            label: label.into(),
            enabled,
            checked,
        }
    }
}

/// An action on one process that the shell carries out through the platform's
/// process control, reporting a failure to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAction {
    SetPriority(Priority),
    SetAffinity(u64),
    Suspend,
    Resume,
    EfficiencyMode(bool),
    /// Write a dump into the user's temporary directory and reveal it.
    WriteDump,
    /// Analyze the wait chain of these threads of the process.
    WaitChain {
        threads: Vec<u32>,
    },
    /// End the process (asking first) and start `command_line` again, in
    /// `directory` when known.
    Restart {
        command_line: String,
        directory: Option<String>,
    },
}

/// What a page asks the shell to read, off the UI thread; the answer comes back
/// through [`App::set_inventory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Query {
    Startup,
    InstalledApps,
    Connections,
    System,
}

/// An answer to a [`Query`].
#[derive(Debug, Clone, PartialEq)]
pub enum Inventory {
    Startup(Vec<ot_model::startup::StartupEntry>),
    InstalledApps(Vec<ot_model::apps::InstalledApp>),
    Connections(Vec<ot_model::connection::Connection>),
    System(Box<ot_model::system::SystemFacts>),
}

/// What the shell saves at exit and restores at start: which page and
/// arrangement were showing, and the process table's sort and columns.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewLayout {
    pub page: Page,
    pub view: ViewMode,
    /// The sort column's title, and whether it sorts descending.
    pub sort: String,
    pub sort_desc: bool,
    pub columns: Vec<ColumnLayout>,
}

impl ViewLayout {
    /// One line the shell can store: `page=Processes;view=List;sort=Cycles;
    /// desc=1;cols=Name:300:1|PID:70:1|...`.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut s = format!(
            "page={};view={};sort={};desc={};cols=",
            self.page.label(),
            match self.view {
                ViewMode::List => "List",
                ViewMode::Tree => "Tree",
                ViewMode::Map => "Map",
                ViewMode::History => "History",
            },
            self.sort,
            u8::from(self.sort_desc)
        );
        for (i, c) in self.columns.iter().enumerate() {
            if i > 0 {
                s.push('|');
            }
            let _ = write!(s, "{}:{:.0}:{}", c.title, c.width, u8::from(c.visible));
        }
        s
    }

    /// Read what [`ViewLayout::encode`] wrote. Anything malformed is left at its
    /// default; a line with nothing usable is `None`.
    #[must_use]
    pub fn decode(s: &str) -> Option<Self> {
        let mut layout = Self {
            page: Page::default(),
            view: ViewMode::default(),
            sort: String::new(),
            sort_desc: true,
            columns: Vec::new(),
        };
        let mut any = false;
        for field in s.split(';') {
            let Some((k, v)) = field.split_once('=') else {
                continue;
            };
            any = true;
            match k {
                "page" => layout.page = Page::parse(v).unwrap_or_default(),
                "view" => layout.view = ViewMode::parse(v),
                "sort" => v.clone_into(&mut layout.sort),
                "desc" => layout.sort_desc = v != "0",
                "cols" => {
                    for c in v.split('|') {
                        let mut parts = c.rsplitn(3, ':');
                        let (Some(vis), Some(w), Some(title)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let Ok(width) = w.parse::<f32>() else {
                            continue;
                        };
                        layout.columns.push(ColumnLayout {
                            title: title.to_owned(),
                            width,
                            visible: vis != "0",
                        });
                    }
                }
                _ => {}
            }
        }
        any.then_some(layout)
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
    /// Open the Run dialog to start a program (Ctrl+N).
    RunTask,
    /// Start the crosshair: the next window the pointer is released over has its
    /// process selected.
    PickWindow,
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
    /// Act on one process; `name` is for the message if it fails.
    Process {
        target: ProcessKey,
        name: String,
        action: ProcessAction,
    },
    /// Bring a window to the front, by the handle the probe reported.
    SwitchTo(u64),
    /// Open a web page in the browser.
    OpenUrl(String),
    /// The file manager's Properties sheet for a file.
    Properties(String),
    /// Put text on the clipboard.
    CopyText(String),
    /// The Run dialog: a command line to start, with the choice of starting it as
    /// administrator.
    RunTask,
    /// Start a copy of this program as administrator and, if that works, close
    /// this one.
    RunAsAdministrator,
    /// Drive the replay: a press on the transport bar.
    Replay(ReplayAction),
    /// Start recording to a file the user names (`true`), or stop (`false`).
    Record(bool),
    /// Start the window crosshair; the shell reports the pick with
    /// [`App::select_pid`].
    PickWindow,
    /// Start, stop or restart a service by its key name.
    Service {
        name: String,
        display_name: String,
        action: ServiceAction,
    },
    /// The system's services console.
    OpenServices,
    /// Disconnect a session or sign it out, asking first; `user` names it.
    Session {
        id: u32,
        user: String,
        action: SessionAction,
    },
    /// Let a startup entry run at sign-in, or stop it.
    Startup {
        entry: ot_model::startup::StartupEntry,
        on: bool,
    },
    /// Open an installed program's folder: the recorded install location, else
    /// the folder its uninstaller lives in.
    OpenInstallLocation {
        location: Option<String>,
        uninstall: Option<String>,
    },
    /// Run an installed program's uninstall command, asking first.
    Uninstall { name: String, command: String },
    /// Read a list off the UI thread; see [`Query`].
    Query(Query),
    /// Sample `target`'s CPU for `seconds`, off the UI thread, and hand the result
    /// to [`App::set_attribution`] (or [`App::sampling_failed`]).
    SampleCpu { target: ProcessKey, seconds: u32 },
    /// The user changed a setting; store these so the next start has them.
    SaveSettings(Settings),
    /// The update button was pressed: take the step it names. The updater's new
    /// status comes back through [`App::set_update_status`].
    Update(UpdateAction),
    /// Make the system open this copy in its task manager's place (`true`), or
    /// stop (`false`). What it does afterwards comes back through
    /// [`App::set_task_manager`].
    ReplaceTaskManager(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Disconnect,
    SignOut,
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

    pub(crate) fn effect(effect: Effect) -> Self {
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
            cycles: &$app.cycles,
            cycles_core: $app.cycles_core,
            matched: &$app.matched,
            shown: &$app.shown,
            tree_mode: $tree_mode,
            steady: Some(&$app.steady),
            now_unix_ms: snapshot_unix_ms(&$app.snap),
            percent: $app.settings.resource_percent,
            disk_totals: $app.disk_totals,
        }
    };
}

/// The two buttons at the right of the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolButton {
    /// The Run dialog.
    RunTask,
    /// The window crosshair.
    PickWindow,
}

/// The strip between the cards and the table: the List/Tree switch, the selected
/// process's ancestry, the search field, the Run and crosshair buttons, and the
/// process count.
#[derive(Debug, Default)]
struct Toolbar {
    /// Why the table is standing still, if it is: shown before the search field.
    status: Option<(&'static str, Color)>,
    /// The arrangement shown, for the segment control.
    mode: ViewMode,
    /// Shown instead of the selection's ancestry while not empty: what the pointer
    /// is over in the usage strip above.
    note: String,
    /// Segment rectangles from the last paint, in [`SEGMENTS`] order.
    segments: [Rect; 4],
    hover: Option<usize>,
    chain: Vec<u32>,
    search: SearchBox,
    /// The Run and crosshair buttons, from the last paint.
    buttons: [Rect; 2],
    hover_button: Option<ToolButton>,
}

const SEGMENTS: [(ViewMode, &str); 4] = [
    (ViewMode::List, "List"),
    (ViewMode::Tree, "Tree"),
    (ViewMode::Map, "Map"),
    (ViewMode::History, "History"),
];
const SEGMENT_W: f32 = 60.0;
const COUNT_W: f32 = 130.0;
/// The Run button, with its label, and the crosshair button, icon only.
const RUN_W: f32 = 104.0;
const PICK_W: f32 = 28.0;
/// The toolbar's "Paused" / "Order held" note.
const STATUS_W: f32 = 150.0;

impl Toolbar {
    fn segment_at(&self, p: Point) -> Option<usize> {
        self.segments.iter().position(|r| r.contains(p))
    }

    fn button_at(&self, p: Point) -> Option<ToolButton> {
        if self.buttons[0].contains(p) {
            Some(ToolButton::RunTask)
        } else if self.buttons[1].contains(p) {
            Some(ToolButton::PickWindow)
        } else {
            None
        }
    }

    /// The Run button and the crosshair, at the right end of `rect`; returns
    /// what is left of it.
    fn paint_buttons(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme) -> Rect {
        let (left, pick) = rect.split_left((rect.w - PICK_W).max(0.0));
        let (left, run) = left.split_left((left.w - RUN_W - theme.pad).max(0.0));
        let (_, run) = run.split_left(theme.pad.min(run.w));
        self.buttons = [run, pick];
        for (i, r) in [run, pick].into_iter().enumerate() {
            let r = Rect::new(r.x, r.y + 2.0, r.w, r.h - 4.0);
            let hovered = matches!(
                (i, self.hover_button),
                (0, Some(ToolButton::RunTask)) | (1, Some(ToolButton::PickWindow))
            );
            let fill = if hovered {
                theme.button_hover
            } else {
                theme.surface
            };
            dl.fill_round_rect(r, theme.card_radius, fill);
            dl.stroke_rect(r, theme.surface_border, 1.0);
            if i == 0 {
                dl.text(
                    "Run new task",
                    r,
                    theme.header,
                    theme.text,
                    HAlign::Center,
                    VAlign::Middle,
                    true,
                );
            } else {
                // A crosshair, as geometry: a small ring with four ticks.
                let c = r.center();
                let s = 3.0;
                dl.stroke_rect(
                    Rect::new(c.x - s, c.y - s, 2.0 * s, 2.0 * s),
                    theme.text,
                    1.2,
                );
                for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                    dl.line(
                        Point::new(c.x + dx * s, c.y + dy * s),
                        Point::new(c.x + dx * s * 2.6, c.y + dy * s * 2.6),
                        theme.text,
                        1.2,
                    );
                }
            }
        }
        left
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
        let current = self.mode;
        let group = Rect::new(
            rect.x,
            rect.y + 2.0,
            SEGMENT_W * SEGMENTS.len() as f32,
            rect.h - 4.0,
        );
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

        // Right to left: the count, the two buttons, then the search field; the
        // breadcrumb gets what is left in the middle.
        let (_, after_group) = rect.split_left(group.w + theme.pad);
        let (middle, count) = after_group.split_left((after_group.w - COUNT_W).max(0.0));
        let middle = self.paint_buttons(dl, middle, theme);
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
        if self.note.is_empty() {
            match table.selected.and_then(|id| rows.row_of(id)) {
                Some(row) => rows.ancestry(row, buf, &mut self.chain),
                None => buf.push_str("Ctrl+T switches between list and tree"),
            }
        } else {
            buf.push_str(&self.note);
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
    /// Cycles used as of the pause, for the Map, the strip and the History.
    usage: Usage,
    /// The newest snapshot that arrived during the pause, shown on resume.
    latest: Option<Arc<Snapshot>>,
}

/// What the area under the toolbar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InPlace {
    Table,
    Map,
    History,
}

/// How [`App::paint_at`] made a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintKind {
    /// Only the chart clock moved: the charts spliced into the last full paint.
    Tick,
    /// A full paint while the table's rows slide.
    Slide,
    /// Any other full paint.
    Full,
}

/// The whole application view. One per window.
#[derive(Debug)]
// The flags are independent facts about the view; an enum would only obscure them.
#[allow(clippy::struct_excessive_bools)]
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
    /// Cycles used per process, as totals that fade, and the Map drawn from them.
    usage: Usage,
    map: UsageMap,
    /// The History chart.
    history: UsageChart,
    /// What is shown where the table goes. The table keeps its own List or Tree
    /// mode, which Ctrl+T goes back to from the Map and the History.
    shown_in_place: InPlace,
    /// Each process's fading cycle total, parallel to the snapshot's process list,
    /// for the table's Cycles column; and the total one busy core settles at.
    cycles: Vec<f64>,
    cycles_core: f64,
    /// Every process's disk reads and writes in the snapshot shown, for the Disk
    /// columns as shares.
    disk_totals: (u64, u64),
    /// The summary charts above the process table.
    charts: ChartGroup,
    page: Page,
    nav: NavRail,
    summary: SummaryPage,
    perf: PerfPage,
    users: UsersPage,
    services: ServicesPage,
    startup: StartupPage,
    connections: ConnectionsPage,
    apps: AppsPage,
    system: SystemPage,
    /// Whether this copy runs as administrator: what the Services page may do,
    /// and whether Settings offers to restart elevated.
    elevated: bool,
    /// Sort keys with hysteresis, so noise does not reorder the table. Held while
    /// the pointer is over the table.
    steady: Steady,
    /// Set while the display is paused.
    paused: Option<Paused>,
    settings: Settings,
    settings_page: SettingsPage,
    /// The replay transport, while a recording plays instead of the live sampler.
    transport: Option<Transport>,
    /// What is being recorded, for the Settings page.
    recording: Option<RecordingView>,
    /// The version and the update button, on the rail and the Settings page.
    update: UpdateView,
    /// Whether the system opens this copy in Task Manager's place, for the
    /// Settings page.
    task_manager: TaskManager,
    /// Whether the platform has animation effects on; row slides need both this
    /// and the setting.
    system_animations: bool,
    /// The moment the charts' right edge shows, run smoothly between samples.
    clock: ChartClock,
    /// The last full paint, whose charts a frame where only the clock moved paints
    /// again in place ([`DisplayList::splice`]).
    kept: DisplayList,
    /// The page `kept` shows.
    kept_page: Page,
    /// Something changed since `kept` was painted, beyond the clock: the next frame
    /// is painted whole. Every public method that changes the app sets it.
    full_due: bool,
    /// The program the History highlighted in the last full paint.
    history_selected: Option<ProgramId>,
    /// What the last frame painted, for frame statistics.
    last_paint: PaintKind,
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
            table: Table::new(columns(), col::CYCLES),
            toolbar: Toolbar::default(),
            snap: Arc::new(Snapshot::default()),
            tree: ProcessTree::default(),
            layout: Layout::default(),
            attribution: None,
            sampling: None,
            timeline: Timeline::new(Retention::covering(Settings::default().history_ms())),
            usage: Usage::new(Settings::default().usage_decay()),
            map: UsageMap::default(),
            history: UsageChart::default(),
            shown_in_place: InPlace::Table,
            cycles: Vec::new(),
            cycles_core: 0.0,
            disk_totals: (0, 0),
            charts: ChartGroup::default(),
            page: Page::default(),
            nav: NavRail::default(),
            summary: SummaryPage::default(),
            perf: PerfPage::default(),
            users: UsersPage::default(),
            services: ServicesPage::default(),
            startup: StartupPage::default(),
            connections: ConnectionsPage::default(),
            apps: AppsPage::default(),
            system: SystemPage::default(),
            elevated: false,
            steady: Steady::default(),
            paused: None,
            settings: Settings::default(),
            settings_page: SettingsPage::default(),
            transport: None,
            recording: None,
            update: UpdateView::default(),
            task_manager: TaskManager::default(),
            system_animations: true,
            clock: ChartClock::default(),
            kept: DisplayList::new(),
            kept_page: Page::default(),
            full_due: true,
            history_selected: None,
            last_paint: PaintKind::Full,
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
        self.full_due = true;
        self.backdrop = on;
    }

    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.full_due = true;
        self.theme = theme;
    }

    #[must_use]
    pub fn view(&self) -> ViewMode {
        if self.map_on() {
            ViewMode::Map
        } else if self.history_on() {
            ViewMode::History
        } else if self.table.tree() {
            ViewMode::Tree
        } else {
            ViewMode::List
        }
    }

    fn map_on(&self) -> bool {
        self.shown_in_place == InPlace::Map
    }

    fn history_on(&self) -> bool {
        self.shown_in_place == InPlace::History
    }

    /// Whether the Map or the History is shown where the table would be. The
    /// table's rectangles are stale then, and nothing on it can be pointed at.
    fn table_off(&self) -> bool {
        self.shown_in_place != InPlace::Table
    }

    /// Switch the process table's arrangement, keeping the selection in view.
    pub fn set_view(&mut self, mode: ViewMode) {
        self.full_due = true;
        self.shown_in_place = match mode {
            ViewMode::Map => InPlace::Map,
            ViewMode::History => InPlace::History,
            ViewMode::List | ViewMode::Tree => InPlace::Table,
        };
        if self.table_off() {
            let _ = self.hold_order(false);
            self.table.hover = None;
            return;
        }
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
                    usage: self.usage.clone(),
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
        self.full_due = true;
        self.settings = settings;
        self.apply_settings();
    }

    /// Make the settings take effect: the row animation, how fast the cycle totals
    /// fade, and how far back the history reaches.
    fn apply_settings(&mut self) {
        self.apply_animation();
        let decay = self.settings.usage_decay();
        let history_ms = self.settings.history_ms();
        self.usage.set_decay(decay);
        self.usage.set_history_span(history_ms);
        self.timeline.set_retention(Retention::covering(history_ms));
        if let Some(p) = &mut self.paused {
            p.usage.set_decay(decay);
            p.usage.set_history_span(history_ms);
        }
        self.cycles_core = cycles_core(&self.snap, &self.usage);
    }

    /// What a history length costs here, for the Settings page to say.
    fn history_cost(&self) -> HistoryCost {
        HistoryCost {
            series: self.timeline.series_count(),
            usage_frame_bytes: self.usage.frame_bytes(),
        }
    }

    /// The running build and whether it can install updates, for the update
    /// button. Set once at start.
    pub fn set_update(&mut self, update: UpdateView) {
        self.full_due = true;
        self.update = update;
    }

    /// The updater's latest status. Returns whether the button changed, so a
    /// repaint is due.
    pub fn set_update_status(&mut self, status: ot_update::Status) -> bool {
        self.full_due = true;
        self.update.set_status(status)
    }

    /// The update button as it stands.
    #[must_use]
    pub fn update(&self) -> &UpdateView {
        &self.update
    }

    /// What the system opens in Task Manager's place, as the shell last read it,
    /// and whether a change is under way. Returns whether anything changed, so a
    /// repaint is due.
    pub fn set_task_manager(&mut self, task_manager: TaskManager) -> bool {
        self.full_due = true;
        std::mem::replace(&mut self.task_manager, task_manager) != self.task_manager
    }

    /// The Task Manager card's state as it stands.
    #[must_use]
    pub fn task_manager(&self) -> &TaskManager {
        &self.task_manager
    }

    /// Whether the platform has animation effects on (Windows: "Animation
    /// effects" in Accessibility > Visual effects). Row slides need it.
    pub fn set_system_animations(&mut self, on: bool) {
        self.full_due = true;
        self.system_animations = on;
        self.apply_animation();
    }

    /// The platform's animation effects, as last told.
    #[must_use]
    pub fn system_animations(&self) -> bool {
        self.system_animations
    }

    /// Whether this copy runs as administrator. Set once at start.
    /// Show the replay transport with this state, or hide it (`None`). The shell
    /// calls this after every frame its player publishes. True when something
    /// changed.
    pub fn set_replay(&mut self, state: Option<ReplayState>) -> bool {
        self.full_due = true;
        match (state, self.transport.as_mut()) {
            (Some(s), Some(t)) => {
                if t.state == s {
                    return false;
                }
                t.set(s);
                true
            }
            (Some(s), None) => {
                self.transport = Some(Transport::new(s));
                true
            }
            (None, Some(_)) => {
                self.transport = None;
                true
            }
            (None, None) => false,
        }
    }

    /// Whether a recording is playing instead of the live sampler.
    #[must_use]
    pub fn replaying(&self) -> bool {
        self.transport.is_some()
    }

    /// What is being recorded (`None` when nothing is), for the Settings page.
    /// True when something changed.
    pub fn set_recording(&mut self, recording: Option<RecordingView>) -> bool {
        self.full_due = true;
        if self.recording == recording {
            return false;
        }
        self.recording = recording;
        true
    }

    pub fn set_elevated(&mut self, on: bool) {
        self.full_due = true;
        self.elevated = on;
    }

    /// A list a page asked for ([`Effect::Query`]) has arrived. Returns whether
    /// a repaint is due.
    pub fn set_inventory(&mut self, inventory: Inventory) -> bool {
        self.full_due = true;
        match inventory {
            Inventory::Startup(e) => self.startup.set_entries(e),
            Inventory::InstalledApps(a) => self.apps.set_apps(a),
            Inventory::Connections(c) => self.connections.set_connections(c),
            Inventory::System(f) => self.system.set_facts(*f),
        }
        true
    }

    /// What the page showing needs read now, if anything: the shell runs it
    /// off the UI thread and answers with [`App::set_inventory`]. The Summary
    /// wants the System facts too, for its Windows edition.
    #[must_use]
    pub fn page_query(&self) -> Option<Query> {
        match self.page {
            Page::Startup => self.startup.query(),
            Page::Apps => self.apps.query(),
            Page::Connections => self.connections.query(),
            Page::Summary | Page::System => self.system.query(),
            _ => None,
        }
    }

    /// Turn a page's answer into the app's: a jump to a process selects it on
    /// the Processes page.
    fn page_outcome(&mut self, outcome: PageOutcome) -> Reaction {
        match outcome {
            PageOutcome::Reaction(r) => r,
            PageOutcome::SelectPid(pid) => {
                self.select_pid(pid);
                Reaction::REPAINT
            }
        }
    }

    fn apply_animation(&mut self) {
        self.table
            .set_animate(self.settings.animates_rows(self.system_animations));
    }

    /// Whether something is moving, so the shell should paint another frame soon:
    /// rows sliding, or charts on screen scrolling.
    #[must_use]
    pub fn animating(&self) -> bool {
        (self.page == Page::Processes && self.table.animating())
            || (self.page.has_charts() && self.clock.moving())
    }

    /// Whether the charts scroll between samples: the setting, not paused, and a
    /// page with charts on screen.
    fn charts_scroll(&self) -> bool {
        self.settings.smooth_charts && self.paused.is_none() && self.page.has_charts()
    }

    /// A CPU sample finished: show it under its process. Returns true if the
    /// process is still in the table.
    pub fn set_attribution(&mut self, a: Arc<Attribution>) -> bool {
        self.full_due = true;
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
        self.full_due = true;
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
        match self.page {
            Page::Processes => {}
            Page::Users => return self.users.cursor(),
            Page::Services => return self.services.cursor(),
            Page::Startup => return self.startup.cursor(),
            Page::Connections => return self.connections.cursor(),
            Page::Apps => return self.apps.cursor(),
            _ => return Cursor::Arrow,
        }
        if self.table.resizing() {
            return Cursor::ResizeColumn;
        }
        let Some(p) = self.mouse else {
            return Cursor::Arrow;
        };
        let search = &self.toolbar.search;
        if !self.table_off() && self.table.divider_at(p).is_some() {
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
        self.full_due = true;
        if snap.is_empty() || (!self.snap.is_empty() && snap.tick == self.snap.tick) {
            return false;
        }
        self.timeline.observe(&snap);
        self.usage.observe(&snap);
        if let Some(p) = &mut self.paused {
            p.latest = Some(snap);
            return false;
        }
        self.show_snapshot(snap);
        true
    }

    /// Make `snap` the one on screen. Its history is already in the timeline.
    fn show_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.disk_totals = snap.processes.iter().fold((0, 0), |(r, w), p| {
            (r + p.disk_read.get(), w + p.disk_write.get())
        });
        self.tree.rebuild(&snap.processes);
        self.cycles.clear();
        self.cycles.extend(
            snap.processes
                .iter()
                .map(|p| self.usage.used(p.key()).unwrap_or(0.0)),
        );
        self.tree.roll_cycles(&self.cycles);
        self.cycles_core = cycles_core(&snap, &self.usage);
        self.snap = snap;
        // A finished sample outlives its process only until the next snapshot.
        if let Some(a) = &self.attribution {
            if !self.snap.processes.iter().any(|p| p.key() == a.target) {
                self.attribution = None;
            }
        }
        self.relayout();
        self.refilter(self.table.tree());
        self.users.set_snapshot(Arc::clone(&self.snap));
        self.services.set_snapshot(&self.snap);
        self.connections.set_snapshot(&self.snap);
        self.system.set_snapshot(Arc::clone(&self.snap));
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
        self.full_due = true;
        if page != self.page {
            let _ = self.charts.hover(None);
            let _ = self.charts.mark(None);
            let _ = self.history.set_pointer(None);
            self.table.hover = None;
            let _ = self.hold_order(false);
            let _ = self.perf.handle(UiEvent::MouseLeave);
            let _ = self.summary.handle(UiEvent::MouseLeave);
            let cx = Context {
                system_animations: self.system_animations,
                update: &self.update,
                task_manager: &self.task_manager,
                elevated: self.elevated,
                history: self.history_cost(),
                recording: self.recording.as_ref(),
                replaying: self.transport.is_some(),
            };
            let _ = self
                .settings_page
                .handle(UiEvent::MouseLeave, &mut self.settings, cx);
            self.page = page;
        }
    }

    /// Show `page` and ask for what it needs, in one reaction.
    fn go_to(&mut self, page: Page) -> Reaction {
        self.set_page(page);
        Reaction {
            repaint: true,
            effect: self.page_query().map(Effect::Query),
        }
    }

    #[must_use]
    pub fn page(&self) -> Page {
        self.page
    }

    /// Handle input. Says whether to repaint and what else to do.
    #[allow(clippy::too_many_lines)]
    pub fn handle(&mut self, ev: UiEvent) -> Reaction {
        self.full_due = true;
        if let Some(t) = self.transport.as_mut() {
            if let Some(r) = t.handle(ev) {
                return r;
            }
        }
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
                Some(NavHit::Page(page)) => return self.go_to(page),
                Some(NavHit::Update) => {
                    return self
                        .update
                        .action()
                        .map_or(Reaction::NONE, |a| Reaction::effect(Effect::Update(a)));
                }
                None => {}
            },
            UiEvent::Command(Command::SetPage(page)) => return self.go_to(page),
            UiEvent::Command(Command::StepPage(n)) => return self.go_to(self.page.step(n)),
            // In a replay Space is the transport's play/pause.
            UiEvent::Char(' ') if self.transport.is_some() && !self.search_focused() => {
                return Reaction::effect(Effect::Replay(ReplayAction::Toggle));
            }
            // Space pauses, as in Process Explorer, on any page; typed into a
            // search field it is just a space.
            UiEvent::Command(Command::TogglePause) => return self.toggle_pause(),
            UiEvent::Char(' ') if !self.search_focused() => return self.toggle_pause(),
            // Typing, or Ctrl+F, goes to the page's own search when it has one,
            // else to the process table's, the way Task Manager's search box does.
            UiEvent::Char(_) | UiEvent::Command(Command::Find) if !self.page.has_search() => {
                self.set_page(Page::Processes);
            }
            // The Run dialog and the crosshair work from any page.
            UiEvent::Command(Command::RunTask) => return Reaction::effect(Effect::RunTask),
            UiEvent::Command(Command::PickWindow) => return Reaction::effect(Effect::PickWindow),
            _ => {}
        }
        let theme = self.theme.clone();
        let r = match self.page {
            Page::Summary => {
                let o = self.summary.handle(ev);
                self.page_outcome(o)
            }
            Page::Processes => self.handle_processes(ev),
            Page::Performance => self.perf.handle(ev),
            Page::Users => {
                let o = match ev {
                    UiEvent::Command(Command::Menu(a)) => self.users.menu_action(a),
                    ev => self.users.handle(ev, &theme),
                };
                self.page_outcome(o)
            }
            Page::Services => {
                let o = match ev {
                    UiEvent::Command(Command::Menu(a)) => self.services.menu_action(a),
                    ev => self.services.handle(ev, &theme, self.elevated),
                };
                self.page_outcome(o)
            }
            Page::Startup => {
                let o = match ev {
                    UiEvent::Command(Command::Menu(a)) => self.startup.menu_action(a),
                    ev => self.startup.handle(ev, &theme),
                };
                self.page_outcome(o)
            }
            Page::Connections => {
                let o = match ev {
                    UiEvent::Command(Command::Menu(a)) => {
                        self.connections.menu_action(a, &self.snap)
                    }
                    ev => self.connections.handle(ev, &theme),
                };
                self.page_outcome(o)
            }
            Page::Apps => {
                let o = match ev {
                    UiEvent::Command(Command::Menu(a)) => self.apps.menu_action(a),
                    ev => self.apps.handle(ev, &theme),
                };
                self.page_outcome(o)
            }
            Page::System => self.system.handle(ev),
            Page::Settings => {
                let cx = Context {
                    system_animations: self.system_animations,
                    update: &self.update,
                    task_manager: &self.task_manager,
                    elevated: self.elevated,
                    history: self.history_cost(),
                    recording: self.recording.as_ref(),
                    replaying: self.transport.is_some(),
                };
                let r = self.settings_page.handle(ev, &mut self.settings, cx);
                if matches!(r.effect, Some(Effect::SaveSettings(_))) {
                    self.apply_settings();
                }
                r
            }
        };
        Reaction {
            repaint: r.repaint || rail_moved,
            ..r
        }
    }

    /// Whether a search field has the keyboard on the page showing, so Space
    /// types rather than pauses.
    fn search_focused(&self) -> bool {
        match self.page {
            Page::Processes => self.toolbar.search.focused,
            Page::Users => self.users.search_focused(),
            Page::Services => self.services.search_focused(),
            Page::Startup => self.startup.search_focused(),
            Page::Connections => self.connections.search_focused(),
            Page::Apps => self.apps.search_focused(),
            _ => false,
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
                if self.table.header_drag_to(p) {
                    return Reaction::REPAINT;
                }
                // The table's rectangles are stale while the Map or the History
                // is shown.
                let hover = match self.table.hit(p, &self.theme) {
                    Hit::Row(i) | Hit::Expander(i) if !self.table_off() => Some(i),
                    _ => None,
                };
                let segment = self.toolbar.segment_at(p);
                let button = self.toolbar.button_at(p);
                let clear = self.toolbar.search.clear_rect.contains(p);
                let crosshair = self.charts.hover(Some(p));
                let held = self.hold_order(!self.table_off() && self.table.rect().contains(p));
                // The Map's tiles, or in the other arrangements the usage strip's.
                let tile = self.map.set_hover(Some(p));
                let history = self.history_on() && self.history.set_pointer(Some(p));
                let changed = hover != self.table.hover
                    || segment != self.toolbar.hover
                    || button != self.toolbar.hover_button
                    || clear != self.toolbar.search.hover_clear
                    || crosshair
                    || held
                    || tile
                    || history;
                self.table.hover = hover;
                self.toolbar.hover = segment;
                self.toolbar.hover_button = button;
                self.toolbar.search.hover_clear = clear;
                Reaction::painted(changed)
            }
            UiEvent::MouseLeave => {
                self.mouse = None;
                let row = self.table.hover.take().is_some();
                let segment = self.toolbar.hover.take().is_some()
                    | self.toolbar.hover_button.take().is_some();
                let clear = std::mem::take(&mut self.toolbar.search.hover_clear);
                let crosshair = self.charts.hover(None);
                let held = self.hold_order(false);
                let tile = self.map.set_hover(None);
                let history = self.history.set_pointer(None);
                Reaction::painted(row || segment || clear || crosshair || held || tile || history)
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
                // A header press released in place sorts; moved, it reordered.
                if let Some(col) = self.table.end_header_press(at) {
                    self.table.set_sort(col);
                }
                Reaction::REPAINT
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
            UiEvent::Wheel { .. } if self.table_off() => Reaction::NONE,
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
        match self.toolbar.button_at(at) {
            Some(ToolButton::RunTask) => return Reaction::effect(Effect::RunTask),
            Some(ToolButton::PickWindow) => return Reaction::effect(Effect::PickWindow),
            None => {}
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
        // In the Map, the strip and the History, a click on what is already
        // selected lets it go; there is no empty row to click instead.
        if self.map_on() || self.map.key_at(at).is_some() {
            let selected = self.table.selected;
            if selected.is_some_and(|id| self.map.stands_for(at, id)) {
                self.table.selected = None;
                return Reaction::REPAINT;
            }
            return Reaction::painted(self.select_at(at));
        }
        if self.history_on() {
            return match self.history.click(at) {
                Some(ChartHit::Mode) => Reaction::REPAINT,
                Some(ChartHit::Program(program)) if self.selected_program() == Some(program) => {
                    self.table.selected = None;
                    Reaction::REPAINT
                }
                Some(ChartHit::Program(program)) => Reaction::painted(self.select_program(program)),
                None => Reaction::NONE,
            };
        }
        let rows = rows_of!(self, self.table.tree());
        match self.table.hit(at, theme) {
            Hit::Divider(c) => {
                self.table.begin_resize(c, at);
                Reaction::NONE
            }
            Hit::Header(c) => {
                self.table.begin_header_press(c, at);
                Reaction::NONE
            }
            Hit::Expander(pos) => Reaction::painted(self.table.toggle_expanded(pos, &rows)),
            Hit::Row(pos) => {
                self.table.selected = self.table.row_at(pos).map(|r| rows.id(r));
                Reaction::REPAINT
            }
            Hit::Nothing => Reaction::REPAINT,
        }
    }

    /// Select the process of `program` that has used the most cycles, among those
    /// still running. Returns whether the selection changed.
    fn select_program(&mut self, program: ProgramId) -> bool {
        let usage = match &self.paused {
            Some(p) => &p.usage,
            None => &self.usage,
        };
        let busiest = usage
            .iter()
            .filter(|(_, u)| u.program == program && u.alive)
            .max_by(|a, b| a.1.used.total_cmp(&b.1.used))
            .map(|(key, _)| process_rows::row_id(key));
        match busiest {
            Some(id) => self.table.selected.replace(id) != Some(id),
            None => false,
        }
    }

    /// The program the selected process is charted under.
    fn selected_program(&self) -> Option<ProgramId> {
        let name = self.snap.processes.get(self.selected_process()?)?.name();
        self.usage.program(name)
    }

    /// Select the row under `at`, if there is one. Returns whether anything changed.
    fn select_at(&mut self, at: Point) -> bool {
        // A tile of the Map, or a segment of the usage strip above the table.
        if let Some((key, alive)) = self.map.key_at(at) {
            // Only a running process can be selected: an exited one can be neither
            // ended nor opened.
            if !alive {
                return false;
            }
            let id = Some(process_rows::row_id(key));
            let changed = std::mem::replace(&mut self.table.selected, id) != id;
            if !self.map_on() {
                let rows = rows_of!(self, self.table.tree());
                self.table.reveal_selected(&rows, &self.theme);
            }
            return changed;
        }
        if self.map_on() {
            return false;
        }
        if self.history_on() {
            return self
                .history
                .program_at(at)
                .is_some_and(|program| self.select_program(program));
        }
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
                // From the Map or the History, back to whichever table
                // arrangement it replaced.
                let next = match self.view() {
                    ViewMode::Map | ViewMode::History if self.table.tree() => ViewMode::Tree,
                    v => v.other(),
                };
                self.set_view(next);
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
            Command::RunTask => Reaction::effect(Effect::RunTask),
            Command::PickWindow => Reaction::effect(Effect::PickWindow),
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

    /// The header's menu: one check item per column, and a reset.
    fn column_chooser(&self, at: Point) -> Reaction {
        let mut entries: Vec<MenuEntry> = self
            .table
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                MenuEntry::checked(
                    MenuAction::ToggleColumn(i),
                    c.title,
                    i != col::NAME,
                    c.visible,
                )
            })
            .collect();
        entries.push(MenuEntry::Separator);
        entries.push(MenuEntry::item(
            MenuAction::ResetColumns,
            "Reset columns",
            true,
        ));
        Reaction::effect(Effect::Menu { at, entries })
    }

    fn context_menu(&mut self, at: Option<Point>) -> Reaction {
        if let Some(p) = at {
            if !self.table_off() && self.table.header_rect().contains(p) {
                return self.column_chooser(p);
            }
        }
        let anchor = if let Some(p) = at {
            self.select_at(p);
            let over_process = self.map.key_at(p).is_some_and(|(_, alive)| alive)
                || (self.history_on() && self.history.program_at(p).is_some())
                || (!self.table_off()
                    && matches!(
                        self.table.hit(p, &self.theme),
                        Hit::Row(_) | Hit::Expander(_)
                    ));
            if !over_process {
                return Reaction::NONE;
            }
            p
        } else if self.map_on() {
            // From the keyboard: inside the selected process's tile.
            let Some(r) = self.table.selected.and_then(|id| self.map.tile_rect(id)) else {
                return Reaction::NONE;
            };
            Point::new(r.x + self.theme.pad, r.y + self.theme.pad)
        } else if self.history_on() {
            // The History has no place of its own for one process.
            return Reaction::NONE;
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
        // The kernel and the idle accounting cannot be acted on.
        let real = p.key().pid > 4;
        let has_path = p.statics.image_path.is_some();
        let priority = Priority::ALL
            .iter()
            .map(|&pr| {
                MenuEntry::checked(
                    MenuAction::SetPriority(pr),
                    pr.label(),
                    real,
                    pr == p.priority,
                )
            })
            .collect();
        let entries = vec![
            MenuEntry::item(MenuAction::EndTask, "End task", real),
            MenuEntry::item(
                MenuAction::EndTree,
                "End process tree",
                real && self.tree.rollup(row).descendants > 0,
            ),
            MenuEntry::item(MenuAction::Restart, "Restart", real && has_path),
            MenuEntry::Separator,
            if p.suspended {
                MenuEntry::item(MenuAction::Resume, "Resume", real)
            } else {
                MenuEntry::item(MenuAction::Suspend, "Suspend", real)
            },
            MenuEntry::checked(
                MenuAction::EfficiencyMode(p.efficiency_mode != Some(true)),
                "Efficiency mode",
                real,
                p.efficiency_mode == Some(true),
            ),
            MenuEntry::Submenu {
                label: "Set priority",
                entries: priority,
            },
            MenuEntry::Affinity { target: p.key() },
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::SwitchTo, "Switch to", p.window.is_some()),
            MenuEntry::item(MenuAction::OpenFileLocation, "Open file location", has_path),
            MenuEntry::item(MenuAction::SearchOnline, "Search online", true),
            MenuEntry::item(MenuAction::Properties, "Properties", has_path),
            MenuEntry::item(MenuAction::Copy, "Copy", true),
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::CreateDump, "Create dump file", real),
            MenuEntry::item(MenuAction::WaitChain, "Analyze wait chain", real),
            MenuEntry::item(MenuAction::SampleCpu, "Sample CPU for 5 s", can_sample),
        ];
        Reaction::effect(Effect::Menu {
            at: anchor,
            entries,
        })
    }

    /// The selected row's visible cells, tab-separated, for the clipboard.
    fn copy_row(&self) -> Option<String> {
        let rows = self.rows();
        let row = self.table.selected.and_then(|id| rows.row_of(id))?;
        let mut text = String::new();
        let mut cell = String::new();
        for (i, (ci, _)) in self.table.shown().enumerate() {
            rows.cell(row, ci, &mut cell);
            if i > 0 {
                text.push('\t');
            }
            text.push_str(&cell);
        }
        Some(text)
    }

    /// Act on the Processes page's selected process, or on the table's columns.
    fn process_action(target: ProcessKey, name: &str, action: ProcessAction) -> Reaction {
        Reaction::effect(Effect::Process {
            target,
            name: name.to_owned(),
            action,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn menu_action(&mut self, action: MenuAction) -> Reaction {
        match action {
            MenuAction::ToggleColumn(c) => {
                let on = !self.table.columns.get(c).is_some_and(|c| c.visible);
                return Reaction::painted(self.table.set_visible(c, on));
            }
            MenuAction::ResetColumns => {
                self.table.reset_columns(columns());
                return Reaction::REPAINT;
            }
            _ => {}
        }
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
            MenuAction::Restart => {
                let Some(path) = &p.statics.image_path else {
                    return Reaction::NONE;
                };
                // The command line as the process was started, or failing that
                // its image; started from the image's own folder.
                let command_line = p
                    .statics
                    .command_line
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .unwrap_or_else(|| format!("\"{path}\""));
                let directory = path.rfind(['\\', '/']).map(|i| path[..i.max(1)].to_owned());
                Self::process_action(
                    p.key(),
                    p.name(),
                    ProcessAction::Restart {
                        command_line,
                        directory,
                    },
                )
            }
            MenuAction::Suspend => Self::process_action(p.key(), p.name(), ProcessAction::Suspend),
            MenuAction::Resume => Self::process_action(p.key(), p.name(), ProcessAction::Resume),
            MenuAction::EfficiencyMode(on) => {
                Self::process_action(p.key(), p.name(), ProcessAction::EfficiencyMode(on))
            }
            MenuAction::SetPriority(pr) => {
                Self::process_action(p.key(), p.name(), ProcessAction::SetPriority(pr))
            }
            MenuAction::SetAffinity(mask) => {
                Self::process_action(p.key(), p.name(), ProcessAction::SetAffinity(mask))
            }
            MenuAction::CreateDump => {
                Self::process_action(p.key(), p.name(), ProcessAction::WriteDump)
            }
            MenuAction::WaitChain => {
                let (first, rows) = (p.thread_first as usize, p.thread_rows as usize);
                let threads = self
                    .snap
                    .threads
                    .get(first..first + rows)
                    .map(|ts| ts.iter().map(|t| t.tid).collect())
                    .unwrap_or_default();
                Self::process_action(p.key(), p.name(), ProcessAction::WaitChain { threads })
            }
            MenuAction::SwitchTo => match &p.window {
                Some(w) => Reaction::effect(Effect::SwitchTo(w.handle)),
                None => Reaction::NONE,
            },
            MenuAction::SearchOnline => {
                let mut query = p.name().to_owned();
                if let Some(d) = &p.statics.description {
                    query.push(' ');
                    query.push_str(d);
                }
                Reaction::effect(Effect::OpenUrl(search_url(&query)))
            }
            MenuAction::Properties => match &p.statics.image_path {
                Some(path) => Reaction::effect(Effect::Properties(path.clone())),
                None => Reaction::NONE,
            },
            MenuAction::Copy => self
                .copy_row()
                .map_or(Reaction::NONE, |t| Reaction::effect(Effect::CopyText(t))),
            MenuAction::ToggleColumn(_)
            | MenuAction::ResetColumns
            | MenuAction::GoToProcess
            | MenuAction::ServiceStart
            | MenuAction::ServiceStop
            | MenuAction::ServiceRestart
            | MenuAction::OpenServices
            | MenuAction::SessionDisconnect
            | MenuAction::SessionSignOut
            | MenuAction::StartupEnable(_)
            | MenuAction::Uninstall
            | MenuAction::OpenInstallLocation => Reaction::NONE,
        }
    }

    /// Select the process with `pid` on the Processes page and reveal it, as the
    /// window crosshair does. Returns whether it was found.
    pub fn select_pid(&mut self, pid: u32) -> bool {
        self.full_due = true;
        let Some(p) = self.snap.processes.iter().find(|p| p.key().pid == pid) else {
            return false;
        };
        let id = process_rows::row_id(p.key());
        self.set_page(Page::Processes);
        self.table.selected = Some(id);
        if !self.table_off() {
            let rows = rows_of!(self, self.table.tree());
            self.table.reveal_selected(&rows, &self.theme);
        }
        true
    }

    /// The page and arrangement showing and the process table's sort and
    /// columns, for the shell to save.
    #[must_use]
    pub fn view_layout(&self) -> ViewLayout {
        ViewLayout {
            page: self.page,
            view: self.view(),
            sort: self
                .table
                .columns
                .get(self.table.sort_col)
                .map_or_else(String::new, |c| c.title.to_owned()),
            sort_desc: self.table.sort_desc,
            columns: self.table.layout(),
        }
    }

    /// Restore what [`App::view_layout`] saved. Call before the first snapshot.
    pub fn apply_view_layout(&mut self, layout: &ViewLayout) {
        self.full_due = true;
        self.table.apply_layout(&layout.columns);
        if let Some(c) = self
            .table
            .columns
            .iter()
            .position(|c| c.title == layout.sort)
        {
            self.table.sort_col = c;
            self.table.sort_desc = layout.sort_desc;
            self.table.invalidate_order();
        }
        self.set_view(layout.view);
        self.set_page(layout.page);
    }

    /// Produce this frame.
    pub fn paint(&mut self, dl: &mut DisplayList) {
        self.paint_at(dl, Instant::now());
    }

    /// Produce the frame for time `now`, which paces the row slides and runs the
    /// chart clock. A frame where only the clock moved since the last full paint
    /// repaints just the charts' lines, in a copy of that paint.
    pub fn paint_at(&mut self, dl: &mut DisplayList, now: Instant) {
        self.table.tick(now);
        // The chart clock learns the pace from the live history, paused or not.
        if let Some((newest, gap)) = charts::newest_gap(&self.timeline.cpu_total) {
            self.clock.arrive(newest, gap, now);
        }
        let now_ms = if self.charts_scroll() {
            self.clock.read(now)
        } else {
            self.clock.stop();
            None
        };
        // While paused, the charts show history as of the pause.
        let timeline = match &self.paused {
            Some(p) => &p.timeline,
            None => &self.timeline,
        };
        let axis = charts::axis_of(timeline, now_ms, self.settings.history_ms());
        if self.only_the_clock_moved() {
            self.last_paint = PaintKind::Tick;
            self.paint_tick(dl, axis);
            return;
        }
        self.last_paint = if self.page == Page::Processes && self.table.animating() {
            PaintKind::Slide
        } else {
            PaintKind::Full
        };
        self.paint_full(dl, axis);
        self.kept.copy_from(dl);
        self.kept_page = self.page;
        self.full_due = false;
    }

    /// What the last frame painted.
    #[must_use]
    pub fn last_paint(&self) -> PaintKind {
        self.last_paint
    }

    /// Whether this frame differs from the last full paint only by the clock: no
    /// change to the app since, charts scrolling on the same page, no rows sliding,
    /// and no chart marking a moment (its readouts follow the clock too).
    fn only_the_clock_moved(&self) -> bool {
        let marking = match self.page {
            Page::Processes => {
                self.charts.marking() || (self.history_on() && self.history.marking())
            }
            Page::Summary => self.summary.marking(),
            Page::Performance => self.perf.marking(),
            _ => true,
        };
        !self.full_due
            && self.charts_scroll()
            && self.kept_page == self.page
            && !self.table.animating()
            && !marking
    }

    /// The last full paint with each chart's line painted again on `axis`.
    fn paint_tick(&mut self, dl: &mut DisplayList, axis: TimeAxis) {
        match self.page {
            Page::Summary => self.summary.tick(axis),
            Page::Performance => self.perf.tick(axis),
            _ => self.charts.set_axis(axis),
        }
        let kept = std::mem::take(&mut self.kept);
        dl.splice(&kept, |id, dl| self.repaint_layer(id, dl, axis));
        self.kept = kept;
    }

    /// Paint layer `id` of the page again: a chart's line, or the whole History.
    fn repaint_layer(&mut self, id: u32, dl: &mut DisplayList, axis: TimeAxis) {
        let i = id as usize;
        match self.page {
            Page::Summary => self
                .summary
                .repaint_chart(i, dl, &self.timeline, &self.theme),
            Page::Performance => self.perf.repaint_chart(i, dl, &self.timeline, &self.theme),
            Page::Processes if id == HISTORY_LAYER => {
                self.history
                    .build(self.history.area(), &self.usage, None, axis);
                self.history.paint(
                    dl,
                    &self.usage,
                    &self.toolbar.search.needle,
                    self.history_selected,
                    &self.theme,
                    &mut self.buf,
                );
            }
            Page::Processes => {
                let series = [&self.timeline.cpu_total, &self.timeline.mem_in_use];
                if let Some(s) = series.get(i) {
                    self.charts.repaint(i, dl, s, &self.theme);
                }
            }
            _ => {}
        }
    }

    /// Paint the whole frame on `axis`.
    #[allow(clippy::too_many_lines)]
    fn paint_full(&mut self, dl: &mut DisplayList, axis: TimeAxis) {
        dl.clear();
        let theme = &self.theme;
        dl.clear_to(if self.backdrop {
            Color::TRANSPARENT
        } else {
            theme.bg_solid
        });

        let window = Rect::from_size(self.size);
        // A replay's transport takes the bottom of the window, under every page.
        let window = match self.transport.as_mut() {
            Some(t) => {
                let (bar, rest) = window.split_bottom(Transport::H);
                t.paint(dl, bar, theme, &mut self.buf);
                rest
            }
            None => window,
        };
        let expanded = self.nav.is_expanded(self.size.w);
        let (rail, content) = window.split_left(self.nav.width(self.size.w));
        self.nav.paint(
            dl,
            rail,
            self.page,
            expanded,
            &self.update,
            theme,
            &mut self.buf,
        );
        let full = content.inset(theme.gap, theme.gap);
        let timeline = match &self.paused {
            Some(p) => &p.timeline,
            None => &self.timeline,
        };
        match self.page {
            Page::Processes => {}
            Page::Summary => {
                let snap = Arc::clone(&self.snap);
                let inputs = summary::Inputs {
                    snap: &snap,
                    timeline,
                    axis,
                    cycles: &self.cycles,
                    facts: self.system.facts(),
                };
                self.summary.paint(dl, full, inputs, theme, &mut self.buf);
                return;
            }
            Page::Performance => {
                let snap = Arc::clone(&self.snap);
                self.perf.paint(
                    dl,
                    full,
                    &snap,
                    Charted { timeline, axis },
                    theme,
                    &mut self.buf,
                );
                return;
            }
            Page::Users => {
                self.users.paint(dl, full, theme, &mut self.buf);
                return;
            }
            Page::Services => {
                self.services.paint(dl, full, theme, &mut self.buf);
                return;
            }
            Page::Startup => {
                self.startup.paint(dl, full, theme, &mut self.buf);
                return;
            }
            Page::Connections => {
                self.connections.paint(dl, full, theme, &mut self.buf);
                return;
            }
            Page::Apps => {
                self.apps.paint(dl, full, theme, &mut self.buf);
                return;
            }
            Page::System => {
                self.system.paint(dl, full, theme);
                return;
            }
            Page::Settings => {
                let cx = Context {
                    system_animations: self.system_animations,
                    update: &self.update,
                    task_manager: &self.task_manager,
                    elevated: self.elevated,
                    history: self.history_cost(),
                    recording: self.recording.as_ref(),
                    replaying: self.transport.is_some(),
                };
                self.settings_page
                    .paint(dl, full, self.settings, cx, theme, &mut self.buf);
                return;
            }
        }
        let (cards, rest) = full.split_top(CARD_H);
        let (_, rest) = rest.split_top(theme.gap);
        // The usage strip, except over the Map, which shows the same thing larger.
        let (strip, rest) = if self.map_on() {
            (None, rest)
        } else {
            let (strip, rest) = rest.split_top(STRIP_H);
            (Some(strip), rest.split_top(theme.gap * 0.5).1)
        };
        let (toolbar, rest) = rest.split_top(theme.toolbar_h);
        let (_, table_rect) = rest.split_top(theme.gap * 0.5);

        let (cpu_card, mem_card) = cards.split_left((cards.w - theme.gap) * 0.5);
        let (_, mem_card) = mem_card.split_left(theme.gap);

        // The History shares the summary charts' time axis, so a moment marked in
        // one is marked in all of them. It is placed first: its own pointer decides
        // what the summary charts mark.
        if self.history_on() {
            let usage = match &self.paused {
                Some(p) => &p.usage,
                None => &self.usage,
            };
            let outside = self
                .charts
                .crosshair()
                .filter(|_| self.charts.pointed())
                .map(|c| c.age_ms);
            self.history.build(table_rect, usage, outside, axis);
            let _ = self.charts.mark(self.history.hover_age());
        } else {
            let _ = self.charts.mark(None);
        }

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
        self.charts.begin(2, axis);
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

        self.paint_processes(dl, strip, table_rect, toolbar, &snap);
    }

    /// The process table, or the Map in its place, and the toolbar above them.
    fn paint_processes(
        &mut self,
        dl: &mut DisplayList,
        strip: Option<Rect>,
        table_rect: Rect,
        toolbar: Rect,
        snap: &Arc<Snapshot>,
    ) {
        let theme = &self.theme;
        let rows = ProcessRows {
            procs: &snap.processes,
            threads: &snap.threads,
            tree: &self.tree,
            layout: &self.layout,
            attribution: self.attribution.as_deref(),
            interval_secs: interval_secs(snap),
            mem_total: snap.memory.total.get() as f32,
            cycles: &self.cycles,
            cycles_core: self.cycles_core,
            matched: &self.matched,
            shown: &self.shown,
            tree_mode: self.table.tree(),
            steady: Some(&self.steady),
            now_unix_ms: snapshot_unix_ms(snap),
            percent: self.settings.resource_percent,
            disk_totals: self.disk_totals,
        };
        let usage = match &self.paused {
            Some(p) => &p.usage,
            None => &self.usage,
        };
        if let Some(strip) = strip {
            self.map.paint_strip(
                dl,
                strip,
                usage,
                snap,
                &self.toolbar.search.needle,
                self.table.selected,
                theme,
                &mut self.buf,
            );
        }
        // What the pointer is over in the strip reads out where the ancestry goes.
        if self.map_on() || !self.map.describe_hover(&mut self.toolbar.note) {
            self.toolbar.note.clear();
        }
        // The table first: the toolbar reads its state, and the order must be
        // current before the ancestry lookup. In its place, the Map or the History.
        if self.history_on() {
            let selected = self
                .table
                .selected
                .and_then(|id| rows.row_of(id))
                .and_then(|row| usage.program(snap.processes[rows.process_of(row)].name()));
            // All of the History follows the clock: a layer of its own.
            dl.begin_layer(HISTORY_LAYER);
            self.history.paint(
                dl,
                usage,
                &self.toolbar.search.needle,
                selected,
                theme,
                &mut self.buf,
            );
            dl.end_layer();
            self.history_selected = selected;
        } else if self.map_on() {
            self.map.paint(
                dl,
                table_rect,
                usage,
                snap,
                &self.toolbar.search.needle,
                self.table.selected,
                theme,
                &mut self.buf,
            );
        } else {
            self.table
                .paint(dl, table_rect, &rows, theme, &mut self.buf);
        }
        self.toolbar.mode = if self.map_on() {
            ViewMode::Map
        } else if self.history_on() {
            ViewMode::History
        } else if self.table.tree() {
            ViewMode::Tree
        } else {
            ViewMode::List
        };
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

/// The cycle total a process settles at with one core busy throughout: the rate
/// cycles are counted at (the processor's base clock) times how long a total takes
/// to fade. Zero when the clock is unknown.
fn cycles_core(snap: &Snapshot, usage: &Usage) -> f64 {
    snap.hardware
        .base_frequency
        .map_or(0.0, |f| f.0 as f64 * usage.time_constant())
}

/// A web search for `query`, with the characters a URL cannot carry escaped.
pub(crate) fn search_url(query: &str) -> String {
    let mut url = String::from("https://www.bing.com/search?q=");
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                url.push(b as char);
            }
            b' ' => url.push('+'),
            _ => {
                let _ = write!(url, "%{b:02X}");
            }
        }
    }
    url
}

/// The History's layer in the Processes page's display list; the summary charts'
/// are their indexes.
const HISTORY_LAYER: u32 = u32::MAX;

/// When the snapshot was taken, milliseconds since the Unix epoch; zero before
/// the first.
fn snapshot_unix_ms(snap: &Snapshot) -> i64 {
    snap.taken_at
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
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

    /// An app whose table is sorted by the CPU column, which these snapshots give
    /// values for; the default, Cycles, has tests of its own.
    fn by_cpu() -> App {
        let mut app = App::new(Theme::dark());
        app.table.set_sort(col::CPU);
        app
    }

    /// A billion cycles.
    const G: u64 = 1_000_000_000;

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
        // Still charts: every frame the same, whenever it is painted.
        app.set_settings(Settings {
            smooth_charts: false,
            ..app.settings()
        });
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
        let mut app = by_cpu();
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        assert!(!dl.is_empty());
        assert_eq!(dl.clip_depth(), 0);
    }

    #[test]
    fn new_snapshot_requests_repaint_once() {
        let mut app = by_cpu();
        let s = snapshot(1, vec![proc(1, None, 10.0), proc(2, None, 90.0)]);
        assert!(app.set_snapshot(Arc::clone(&s)));
        assert!(!app.set_snapshot(s));
        assert!(app.set_snapshot(snapshot(2, vec![])));
    }

    #[test]
    fn keyboard_selects_top_cpu_process_first() {
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        theme.gap
            + CARD_H
            + theme.gap
            + STRIP_H
            + theme.gap * 0.5
            + theme.toolbar_h
            + theme.gap * 0.5
    }

    #[test]
    fn clicking_an_expander_collapses_without_selecting() {
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        // A leaf with no known path, not sampleable: the entries that need
        // those are disabled, the rest enabled.
        let labels: Vec<(String, bool)> = entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { label, enabled, .. } => Some((label.to_string(), *enabled)),
                MenuEntry::Submenu { label, .. } => Some(((*label).to_owned(), true)),
                MenuEntry::Affinity { .. } => Some(("Set affinity".to_owned(), true)),
                MenuEntry::Separator => None,
            })
            .collect();
        let expect = |label: &str, enabled: bool| {
            assert!(
                labels.iter().any(|(l, e)| l == label && *e == enabled),
                "{label} enabled={enabled}: {labels:?}"
            );
        };
        expect("End task", true);
        expect("End process tree", false);
        expect("Restart", false);
        expect("Suspend", true);
        expect("Efficiency mode", true);
        expect("Set priority", true);
        expect("Set affinity", true);
        expect("Switch to", false);
        expect("Open file location", false);
        expect("Search online", true);
        expect("Properties", false);
        expect("Copy", true);
        expect("Create dump file", true);
        expect("Sample CPU for 5 s", false);
        assert!(matches!(
            entries[0],
            MenuEntry::Item {
                action: MenuAction::EndTask,
                ..
            }
        ));
        // The priority submenu checks the current class.
        let Some(MenuEntry::Submenu { entries: prio, .. }) = entries
            .iter()
            .find(|e| matches!(e, MenuEntry::Submenu { .. }))
        else {
            panic!("no priority submenu");
        };
        assert!(prio.iter().any(|e| matches!(
            e,
            MenuEntry::Item {
                action: MenuAction::SetPriority(Priority::Normal),
                checked: true,
                ..
            }
        )));
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        assert!(entries.iter().any(|e| matches!(
            e,
            MenuEntry::Item {
                action: MenuAction::OpenFileLocation,
                enabled: true,
                ..
            }
        )));
        // With a path, Properties and Restart are offered; Restart carries the
        // image as the command line and its folder.
        let r = cmd(&mut app, Command::Menu(MenuAction::Properties));
        assert_eq!(
            r.effect,
            Some(Effect::Properties("C:\\x\\p7.exe".to_owned()))
        );
        let r = cmd(&mut app, Command::Menu(MenuAction::Restart));
        assert_eq!(
            r.effect,
            Some(Effect::Process {
                target: ProcessKey::new(7, 1),
                name: "p7.exe".to_owned(),
                action: ProcessAction::Restart {
                    command_line: "\"C:\\x\\p7.exe\"".to_owned(),
                    directory: Some("C:\\x".to_owned()),
                },
            })
        );
        let r = cmd(&mut app, Command::Menu(MenuAction::SearchOnline));
        assert_eq!(
            r.effect,
            Some(Effect::OpenUrl(
                "https://www.bing.com/search?q=p7.exe".to_owned()
            ))
        );
        let r = cmd(&mut app, Command::Menu(MenuAction::Copy));
        let Some(Effect::CopyText(text)) = r.effect else {
            panic!("{r:?}");
        };
        assert!(text.starts_with("p7.exe\t7\t"), "{text:?}");
        let r = cmd(
            &mut app,
            Command::Menu(MenuAction::SetPriority(Priority::High)),
        );
        assert!(matches!(
            r.effect,
            Some(Effect::Process {
                action: ProcessAction::SetPriority(Priority::High),
                ..
            })
        ));
    }

    #[test]
    fn the_header_menu_toggles_columns_and_a_drag_reorders_them() {
        let mut app = by_cpu();
        app.set_snapshot(family());
        ready(&mut app);
        let theme = app.theme.clone();
        let header_y = table_top(&theme) + theme.header_h * 0.5;
        let at = Point::new(table_left(&theme) + 20.0, header_y);
        let r = app.handle(UiEvent::ContextMenu { at: Some(at) });
        let Some(Effect::Menu { entries, .. }) = r.effect else {
            panic!("{r:?}");
        };
        assert!(matches!(
            entries[col::NAME],
            MenuEntry::Item {
                action: MenuAction::ToggleColumn(col::NAME),
                enabled: false,
                checked: true,
                ..
            }
        ));
        assert!(matches!(
            entries[col::KIND],
            MenuEntry::Item {
                action: MenuAction::ToggleColumn(col::KIND),
                enabled: true,
                checked: false,
                ..
            }
        ));
        assert!(matches!(
            entries.last(),
            Some(MenuEntry::Item {
                action: MenuAction::ResetColumns,
                ..
            })
        ));
        let shown_before = app.table.shown().count();
        assert!(cmd(&mut app, Command::Menu(MenuAction::ToggleColumn(col::KIND))).repaint);
        assert_eq!(app.table.shown().count(), shown_before + 1);
        assert!(cmd(&mut app, Command::Menu(MenuAction::ToggleColumn(col::PID))).repaint);
        assert!(!app.table.columns[col::PID].visible);
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "Type"), "{strings:?}");
        assert!(!strings.iter().any(|s| s == "PID"), "{strings:?}");
        cmd(&mut app, Command::Menu(MenuAction::ResetColumns));
        assert_eq!(app.table.shown().count(), shown_before);
        assert_eq!(app.table.sort_col, col::CPU, "the sort survives a reset");

        // A click on a header sorts; a drag moves the column instead.
        let pid_x = table_left(&theme) + app.table.columns[col::NAME].width + 10.0;
        let down = Point::new(pid_x, header_y);
        app.handle(UiEvent::MouseDown {
            at: down,
            button: MouseButton::Left,
        });
        app.handle(UiEvent::MouseUp {
            at: down,
            button: MouseButton::Left,
        });
        assert_eq!(app.table.sort_col, col::PID);
        app.handle(UiEvent::MouseDown {
            at: down,
            button: MouseButton::Left,
        });
        let far = Point::new(table_left(&theme) + 5.0, header_y);
        assert!(app.handle(UiEvent::MouseMove(far)).repaint);
        assert!(app.table.header_dragging());
        app.handle(UiEvent::MouseUp {
            at: far,
            button: MouseButton::Left,
        });
        assert_eq!(app.table.column_at(0), Some(col::PID));
        assert_eq!(app.table.sort_col, col::PID, "a drag does not sort");

        // The layout round-trips through what the shell saves.
        let layout = app.view_layout();
        assert_eq!(layout.sort, "PID");
        assert_eq!(layout.columns[0].title, "PID");
        let mut fresh = App::new(Theme::dark());
        fresh.apply_view_layout(&layout);
        assert_eq!(fresh.table.column_at(0), Some(col::PID));
        assert_eq!(fresh.table.sort_col, col::PID);
    }

    #[test]
    fn the_toolbar_buttons_ask_for_the_run_dialog_and_the_crosshair() {
        let mut app = by_cpu();
        app.set_snapshot(family());
        ready(&mut app);
        let run = app.toolbar.buttons[0].center();
        let pick = app.toolbar.buttons[1].center();
        assert!(app.toolbar.buttons[0].w > 0.0);
        assert!(app.handle(UiEvent::MouseMove(run)).repaint);
        assert_eq!(app.toolbar.hover_button, Some(ToolButton::RunTask));
        let r = app.handle(UiEvent::MouseDown {
            at: run,
            button: MouseButton::Left,
        });
        assert_eq!(r.effect, Some(Effect::RunTask));
        let r = app.handle(UiEvent::MouseDown {
            at: pick,
            button: MouseButton::Left,
        });
        assert_eq!(r.effect, Some(Effect::PickWindow));
        assert_eq!(
            cmd(&mut app, Command::RunTask).effect,
            Some(Effect::RunTask)
        );
        // The crosshair's pick selects the process and reveals it.
        app.set_page(Page::Performance);
        assert!(app.select_pid(13));
        assert_eq!(app.page(), Page::Processes);
        assert_eq!(app.table.selected, Some(id(13)));
        assert!(!app.select_pid(999));
    }

    #[test]
    fn a_view_layout_round_trips_through_its_text_form() {
        let mut app = by_cpu();
        app.set_snapshot(family());
        ready(&mut app);
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        cmd(&mut app, Command::Menu(MenuAction::ToggleColumn(col::KIND)));
        cmd(&mut app, Command::SetPage(Page::Services));
        let layout = app.view_layout();
        let text = layout.encode();
        assert!(text.starts_with("page=Services;view=Tree;sort=CPU %;desc=1;cols=Name:300:1|"));
        assert!(text.contains("|Type:90:1"), "{text}");
        assert_eq!(ViewLayout::decode(&text), Some(layout.clone()));
        assert_eq!(ViewLayout::decode("garbage"), None);
        let mut fresh = App::new(Theme::dark());
        fresh.apply_view_layout(&layout);
        assert_eq!(fresh.page(), Page::Services);
        assert_eq!(fresh.view(), ViewMode::Tree);
        assert!(fresh.table.columns[col::KIND].visible);
    }

    #[test]
    fn pages_ask_for_their_lists_and_take_typing() {
        let mut app = by_cpu();
        app.set_snapshot(family());
        ready(&mut app);
        let r = cmd(&mut app, Command::SetPage(Page::Startup));
        assert_eq!(r.effect, Some(Effect::Query(Query::Startup)));
        assert!(app.set_inventory(Inventory::Startup(Vec::new())));
        assert_eq!(cmd(&mut app, Command::SetPage(Page::Startup)).effect, None);
        // Connections always wants a fresh list; the others once.
        assert_eq!(
            cmd(&mut app, Command::SetPage(Page::Connections)).effect,
            Some(Effect::Query(Query::Connections))
        );
        app.set_inventory(Inventory::Connections(Vec::new()));
        assert_eq!(app.page_query(), Some(Query::Connections));
        assert_eq!(
            cmd(&mut app, Command::SetPage(Page::System)).effect,
            Some(Effect::Query(Query::System))
        );
        // The Summary wants the same facts, until they have been read.
        assert_eq!(
            cmd(&mut app, Command::SetPage(Page::Summary)).effect,
            Some(Effect::Query(Query::System))
        );
        app.set_inventory(Inventory::System(Box::default()));
        assert_eq!(cmd(&mut app, Command::SetPage(Page::Summary)).effect, None);
        // Typing on a list page filters that page, not the process table.
        cmd(&mut app, Command::SetPage(Page::Services));
        type_str(&mut app, "spool");
        assert_eq!(app.page(), Page::Services);
        assert_eq!(app.search(), "");
        // On the System page, which has no search, typing goes to Processes.
        cmd(&mut app, Command::SetPage(Page::System));
        type_str(&mut app, "p1");
        assert_eq!(app.page(), Page::Processes);
        assert_eq!(app.search(), "p1");
        // Space pauses from the System page.
        key(&mut app, Key::Escape);
        cmd(&mut app, Command::SetPage(Page::System));
        app.handle(UiEvent::Char(' '));
        assert!(app.paused());
        painted_strings(&mut app);
        for page in Page::ALL {
            cmd(&mut app, Command::SetPage(page));
            let mut dl = DisplayList::new();
            app.paint(&mut dl);
            assert_eq!(dl.clip_depth(), 0, "{page:?}");
        }
    }

    #[test]
    fn search_urls_escape_what_they_must() {
        assert_eq!(
            search_url("a b.exe & c"),
            "https://www.bing.com/search?q=a+b.exe+%26+c"
        );
    }

    #[test]
    fn dragging_a_header_divider_resizes_the_column() {
        let mut app = by_cpu();
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

    #[test]
    fn process_rows_draw_their_program_icons() {
        let mut app = by_cpu();
        let mut with_path = proc(1, None, 1.0);
        let mut statics = (*with_path.statics).clone();
        statics.image_path = Some(r"C:\x\a.exe".to_owned());
        with_path.statics = Arc::new(statics);
        app.set_snapshot(snapshot(1, vec![with_path, proc(2, None, 0.5)]));
        ready(&mut app);
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        let images: Vec<String> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Image { path, .. } => Some(dl.str(*path).to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(images, vec![r"C:\x\a.exe".to_owned()], "one row has a path");
    }

    #[test]
    fn a_replay_puts_its_transport_along_the_bottom_and_space_drives_it() {
        let mut app = by_cpu();
        ready(&mut app);
        assert!(!app.replaying());
        assert!(app.set_replay(Some(ReplayState {
            name: "run.otrec".to_owned(),
            position: 3,
            len: 10,
            playing: false,
            speed: 1.0,
            elapsed_ms: 3000,
            duration_ms: 9000,
        })));
        assert!(app.replaying());
        let mut dl = DisplayList::new();
        app.paint(&mut dl);
        let label = dl
            .cmds()
            .iter()
            .find_map(|c| match c {
                DrawCmd::Text(t) if dl.str(t.text) == "Replaying run.otrec" => Some(t.rect),
                _ => None,
            })
            .expect("the transport is painted");
        assert!(
            label.y >= app.size.h - Transport::H,
            "the bar sits at the bottom: {label:?} in {:?}",
            app.size
        );
        // The pages above it still paint: the process table's header is there.
        assert!(painted_strings(&mut app).iter().any(|t| t == "Name"));
        assert_eq!(
            app.handle(UiEvent::Char(' ')).effect,
            Some(Effect::Replay(ReplayAction::Toggle))
        );
        assert!(app.set_replay(None));
        assert!(!app.replaying());
    }

    /// Twenty seconds of history ending at t = 20 s: CPU climbing 5 % a second to
    /// 95 %, memory steady at 60 of 100 bytes.
    fn with_history(app: &mut App) {
        for t in 1..=20u64 {
            app.set_snapshot(history_sample(t));
        }
    }

    /// The sample of [`with_history`] at `t` seconds.
    fn history_sample(t: u64) -> Arc<Snapshot> {
        let mut s = (*snapshot(t, vec![proc(1, None, 1.0)])).clone();
        s.cpu.total = Percent(t as f32 * 5.0 - 5.0);
        s.memory = MemorySample {
            total: Bytes(100),
            available: Bytes(40),
            ..Default::default()
        };
        Arc::new(s)
    }

    #[test]
    fn a_frame_where_only_the_clock_moved_repaints_just_the_charts() {
        let mut app = by_cpu();
        with_history(&mut app);
        ready(&mut app);
        app.set_settings(Settings {
            smooth_charts: true,
            ..app.settings()
        });
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        // A new sample, then frames a while after, so the clock is moving.
        app.paint_at(&mut DisplayList::new(), t0);
        app.set_snapshot(history_sample(21));
        for (page, view) in [
            (Page::Processes, ViewMode::List),
            (Page::Processes, ViewMode::History),
            (Page::Summary, ViewMode::List),
            (Page::Performance, ViewMode::List),
        ] {
            cmd(&mut app, Command::SetPage(page));
            app.set_view(view);
            let mut full = DisplayList::new();
            app.paint_at(&mut full, t0 + ms(1000));
            assert!(!app.full_due, "{page:?}: a full paint is kept");
            // Only the clock moves: the charts' layers are spliced into the kept
            // paint, and the result is what a full paint draws at that moment.
            let at = t0 + ms(1100);
            let mut tick = DisplayList::new();
            app.paint_at(&mut tick, at);
            assert!(app.only_the_clock_moved(), "{page:?}");
            assert!(!tick.layers().is_empty(), "{page:?}: charts are layers");
            app.full_due = true;
            let mut again = DisplayList::new();
            app.paint_at(&mut again, at);
            let mut damage = Vec::new();
            assert!(
                again.damage_since(&tick, &mut damage) && damage.is_empty(),
                "{page:?} {view:?}: the splice differs from a full paint at {damage:?}"
            );
            // The lines did move since the full paint.
            assert!(tick.damage_since(&full, &mut damage), "{page:?}");
            assert!(!damage.is_empty(), "{page:?}: the charts moved");
        }

        // Anything else that changes, a key or the pointer, paints whole again.
        cmd(&mut app, Command::SetPage(Page::Processes));
        app.set_view(ViewMode::List);
        app.paint_at(&mut DisplayList::new(), t0 + ms(1200));
        app.handle(UiEvent::MouseLeave);
        assert!(!app.only_the_clock_moved());
    }

    #[test]
    fn charts_scroll_between_samples_where_they_are_shown() {
        let mut app = by_cpu();
        with_history(&mut app);
        ready(&mut app);
        // Windows' animation effects are off (`ready`); the charts scroll anyway.
        app.set_settings(Settings {
            smooth_charts: true,
            ..app.settings()
        });
        let t0 = Instant::now();
        let mut dl = DisplayList::new();
        app.paint_at(&mut dl, t0);
        let newest = app.timeline.cpu_total.latest().unwrap().at_unix_ms;
        // The newest sample at the right edge at once; nothing moves until the
        // next one arrives, so the shell has no frames to paint.
        assert_eq!(app.charts.axis().now_ms, Some(newest));
        assert!(!app.animating());
        // The next sample: the charts scroll on, frame by frame, easing back to
        // trail the newest so the samples after it enter past the edge.
        assert!(app.set_snapshot(history_sample(21)));
        let ms = Duration::from_millis;
        app.paint_at(&mut dl, t0 + ms(1000));
        let a = app.charts.axis().now_ms.unwrap();
        assert!((newest..newest + 1000).contains(&a), "{a}");
        assert!(app.animating(), "the shell keeps the frames coming");
        app.paint_at(&mut dl, t0 + ms(1016));
        assert!(app.charts.axis().now_ms.unwrap() > a);

        // Paused: the charts hold still at the newest sample they had.
        app.handle(UiEvent::Char(' '));
        app.paint_at(&mut dl, t0 + ms(1032));
        assert_eq!(app.charts.axis().now_ms, None);
        assert!(!app.animating());
        app.handle(UiEvent::Char(' '));
        app.paint_at(&mut dl, t0 + ms(1048));
        assert!(app.animating());

        // A page without charts, or the setting off: no frames for nothing.
        cmd(&mut app, Command::SetPage(Page::Services));
        app.paint_at(&mut dl, t0 + ms(1064));
        assert!(!app.animating());
        cmd(&mut app, Command::SetPage(Page::Processes));
        app.set_settings(Settings {
            smooth_charts: false,
            ..app.settings()
        });
        app.paint_at(&mut dl, t0 + ms(1080));
        assert_eq!(app.charts.axis().now_ms, None);
        assert!(!app.animating());
    }

    #[test]
    fn the_axis_spans_the_history_held_and_its_reach_is_a_setting() {
        let mut app = by_cpu();
        with_history(&mut app);
        ready(&mut app);
        let _ = painted_strings(&mut app);
        let tl = &app.timeline;
        let held =
            (tl.cpu_total.latest().unwrap().at_unix_ms - tl.cpu_total.oldest_ms().unwrap()) as f32;
        assert!(held > crate::sparkline::TimeAxis::MIN_SPAN_MS, "{held}");
        assert_eq!(app.charts.axis().span_ms, held, "the chart fills its width");
        assert_eq!(app.timeline.retention(), &Retention::covering(300_000));

        let mut s = app.settings();
        s.history_minutes = 1440;
        app.set_settings(s);
        assert_eq!(
            app.timeline.retention(),
            &Retention::covering(24 * 3_600_000)
        );
        assert_eq!(app.usage.history_span(), 24 * 3_600_000);
        assert_eq!(app.timeline.cpu_total.len(), 21, "nothing held was lost");
    }

    #[test]
    fn charts_label_their_log_axis_and_share_one_crosshair() {
        let mut app = by_cpu();
        with_history(&mut app);
        ready(&mut app);
        let strings = painted_strings(&mut app);
        // Twenty seconds of history: the axis spans that, so the labels past it
        // are not drawn.
        for label in ["now", "10s"] {
            let n = strings.iter().filter(|s| *s == label).count();
            assert_eq!(n, 2, "{label} under both charts: {strings:?}");
        }
        assert!(!strings.iter().any(|s| s == "1m"), "{strings:?}");

        // Point at the CPU chart, a little off the sample 3 s old: it snaps.
        let cpu = app.charts.plot(0).rect();
        let axis = app.charts.axis();
        let at = Point::new(axis.x(cpu, 3000.0) + 1.5, cpu.center().y);
        assert!(app.handle(UiEvent::MouseMove(at)).repaint);
        assert_eq!(app.charts.hover_age(), Some(3000.0));
        let nudge = Point::new(at.x + 0.5, at.y);
        assert!(
            !app.handle(UiEvent::MouseMove(nudge)).repaint,
            "same sample, nothing to redraw"
        );
        // Both charts read out the same moment; the tick labels make way.
        let strings = painted_strings(&mut app);
        assert!(
            strings.iter().any(|s| s == "80% · \u{2007}3s ago"),
            "{strings:?}"
        );
        assert!(
            strings.iter().any(|s| s == "60 B · \u{2007}3s ago"),
            "{strings:?}"
        );
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
            .filter(|s| *s == "10s")
            .count();
        assert_eq!(n, 2, "labels are back");
    }

    #[test]
    fn a_horizontal_wheel_scrolls_the_columns() {
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        assert_eq!(app.page(), Page::Users);
        cmd(&mut app, Command::SetPage(Page::Settings));
        cmd(&mut app, Command::StepPage(1));
        assert_eq!(app.page(), Page::Summary, "wraps around to the first page");
        cmd(&mut app, Command::StepPage(-1));
        assert_eq!(app.page(), Page::Settings);
        cmd(&mut app, Command::StepPage(-1));
        assert_eq!(app.page(), Page::System);
        cmd(&mut app, Command::SetPage(Page::Processes));
        assert_eq!(app.page(), Page::Processes);
    }

    #[test]
    fn the_summary_page_sums_the_others_up_and_jumps_to_a_process() {
        let mut app = by_cpu();
        with_history(&mut app);
        ready(&mut app);
        cmd(&mut app, Command::SetPage(Page::Summary));
        let strings = painted_strings(&mut app);
        for want in [
            "Summary",
            "CPU",
            "Memory",
            "Top processes",
            "By CPU",
            "System",
        ] {
            assert!(strings.iter().any(|s| s == want), "{want} in {strings:?}");
        }
        assert!(!strings.iter().any(|s| s == "Name"), "no process table");
        // The busiest process leads the list; a click on it selects it on the
        // Processes page, as the window crosshair's pick does.
        let busiest = app
            .snap
            .processes
            .iter()
            .filter(|p| p.key().pid != 0)
            .max_by(|a, b| a.cpu.get().total_cmp(&b.cpu.get()))
            .map(|p| p.key().pid)
            .expect("the family has processes");
        let row = app
            .summary
            .row_rect(busiest)
            .expect("the busiest process is listed");
        let r = app.handle(UiEvent::MouseDown {
            at: row.center(),
            button: MouseButton::Left,
        });
        assert!(r.repaint);
        assert_eq!(app.page(), Page::Processes);
        assert_eq!(app.table.selected, Some(id(busiest)));
    }

    #[test]
    fn typing_on_another_page_searches_the_process_table() {
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
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
        // Recorded underneath, but the charts show the moment of the pause. (The
        // first sample is held twice: at the start of its interval and its end.)
        assert_eq!(app.timeline.cpu_total.len(), 22);
        assert_eq!(
            app.paused.as_ref().map(|p| p.timeline.cpu_total.len()),
            Some(21)
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
        let mut app = by_cpu();
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
        let mut app = by_cpu();
        // The charts above the table would keep the frames coming on their own.
        app.set_settings(Settings {
            smooth_charts: false,
            ..app.settings()
        });
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
                animate_rows: Some(false),
                smooth_charts: false,
                ..Settings::default()
            }))
        );
        assert_eq!(app.settings().animate_rows, Some(false));
        cmd(&mut app, Command::SetPage(Page::Processes));
        app.paint_at(&mut dl, t0 + Duration::from_millis(700));
        app.set_snapshot(two(4, 90.0, 5.0));
        app.paint_at(&mut dl, t0 + Duration::from_millis(700));
        assert!(!app.animating());
    }

    #[test]
    fn the_version_on_the_rail_is_the_update_button() {
        let mut app = App::new(Theme::dark());
        let _ = app.handle(UiEvent::Resize(Size::new(1200.0, 700.0)));
        app.set_update(UpdateView::new("0.2.1", true));
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "v0.2.1"));
        assert!(strings.iter().any(|s| s == "Check for updates"));

        let at = app.nav.update_rect().center();
        let click = |app: &mut App| {
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            })
        };
        assert_eq!(
            click(&mut app).effect,
            Some(Effect::Update(UpdateAction::Check))
        );
        assert_eq!(app.page(), Page::Processes, "no page change");

        assert!(app.set_update_status(ot_update::Status::Checking));
        assert_eq!(click(&mut app), Reaction::NONE, "busy: nothing to do");
        assert!(painted_strings(&mut app)
            .iter()
            .any(|s| s == "Checking\u{2026}"));

        let v = ot_update::Version::parse("0.3.0").unwrap();
        app.set_update_status(ot_update::Status::Ready { version: v });
        assert_eq!(
            click(&mut app).effect,
            Some(Effect::Update(UpdateAction::Install))
        );
    }

    /// The family, twice a second apart, having used this many cycles in between:
    /// p11 30 G, p12 6 G, p20 12 G. A second's fading leaves 97.479 % of them.
    fn busy_minute(app: &mut App) {
        for (tick, busy) in [(1u64, false), (2, true)] {
            let mut procs = family().processes.clone();
            for p in &mut procs {
                let used = match p.key().pid {
                    11 => 30,
                    12 => 6,
                    20 => 12,
                    _ => 0,
                };
                p.cycles = G * if busy { 100 + used } else { 100 };
            }
            app.set_snapshot(snapshot(tick, procs));
        }
    }

    #[test]
    fn the_map_shows_cycles_used_as_a_treemap_and_shares_the_selection() {
        let mut app = by_cpu();
        busy_minute(&mut app);
        ready(&mut app);
        assert_eq!(ViewMode::parse("MAP"), ViewMode::Map);
        let map = app.toolbar.segments[2].center();
        app.handle(UiEvent::MouseDown {
            at: map,
            button: MouseButton::Left,
        });
        assert_eq!(app.view(), ViewMode::Map);
        let strings = painted_strings(&mut app);
        assert!(
            strings
                .iter()
                .any(|s| s.starts_with("Cycles used: 46.8 G, fading 5% a second.")),
            "{strings:?}"
        );
        assert!(!strings.iter().any(|s| s == "PID"), "no table: {strings:?}");

        // A click on p11's tile selects it, and the ancestry strip follows.
        let tile = app.map.tile_rect(id(11)).expect("p11 has a tile");
        assert!(
            app.handle(UiEvent::MouseDown {
                at: tile.center(),
                button: MouseButton::Left,
            })
            .repaint
        );
        assert_eq!(app.table.selected, Some(id(11)));
        let crumb = "p4.exe \u{203a} p10.exe \u{203a} p11.exe";
        assert!(painted_strings(&mut app).iter().any(|s| s == crumb));

        // A second click on the tile lets the selection go, and a third takes it
        // back.
        for want in [None, Some(id(11))] {
            painted_strings(&mut app);
            app.handle(UiEvent::MouseDown {
                at: tile.center(),
                button: MouseButton::Left,
            });
            assert_eq!(app.table.selected, want);
        }

        // Right-click gives the process menu, and so does the keyboard.
        let r = app.handle(UiEvent::ContextMenu {
            at: Some(tile.center()),
        });
        assert!(matches!(r.effect, Some(Effect::Menu { .. })), "{r:?}");
        let r = app.handle(UiEvent::ContextMenu { at: None });
        assert!(
            matches!(r.effect, Some(Effect::Menu { .. })),
            "keyboard: {r:?}"
        );

        // Ctrl+T goes back to the table the Map replaced, selection kept.
        cmd(&mut app, Command::ToggleView);
        assert_eq!(app.view(), ViewMode::List);
        assert_eq!(app.table.selected, Some(id(11)));
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        cmd(&mut app, Command::SetView(ViewMode::Map));
        cmd(&mut app, Command::ToggleView);
        assert_eq!(app.view(), ViewMode::Tree, "back to the tree it came from");
    }

    #[test]
    fn a_paused_map_stays_as_it_was() {
        let mut app = by_cpu();
        busy_minute(&mut app);
        ready(&mut app);
        cmd(&mut app, Command::SetView(ViewMode::Map));
        painted_strings(&mut app);
        let before = app.map.tile_rect(id(11)).expect("p11 has a tile");
        app.handle(UiEvent::Char(' '));
        assert!(app.paused());
        // p20 goes on to use a great deal more; the paused Map does not move.
        let mut procs = family().processes.clone();
        for p in &mut procs {
            let used = match p.key().pid {
                11 => 30,
                12 => 6,
                20 => 90,
                _ => 0,
            };
            p.cycles = G * (100 + used);
        }
        app.set_snapshot(snapshot(3, procs));
        painted_strings(&mut app);
        assert_eq!(app.map.tile_rect(id(11)), Some(before));
        app.handle(UiEvent::Char(' '));
        painted_strings(&mut app);
        assert_ne!(
            app.map.tile_rect(id(11)),
            Some(before),
            "and moves on resume"
        );
    }

    #[test]
    fn the_usage_strip_sits_above_the_table_and_selects_into_it() {
        let mut app = by_cpu();
        busy_minute(&mut app);
        ready(&mut app);
        let strings = painted_strings(&mut app);
        // p10 only launched p11, so the two fold into one segment.
        let chain = "p10.exe \u{203a} p11.exe";
        assert!(strings.iter().any(|s| s == chain), "{strings:?}");
        let list_toolbar = app.toolbar.segments[0].y;

        // Pointing at a segment reads it out where the ancestry goes.
        let seg = app.map.tile_rect(id(20)).expect("p20 has a segment");
        app.handle(UiEvent::MouseMove(seg.center()));
        let strings = painted_strings(&mut app);
        assert!(
            strings
                .iter()
                .any(|s| s == "p20.exe \u{b7} PID 20 \u{b7} 11.7 G cycles"),
            "{strings:?}"
        );

        // A click selects it in the table.
        app.handle(UiEvent::MouseDown {
            at: seg.center(),
            button: MouseButton::Left,
        });
        assert_eq!(app.table.selected, Some(id(20)));
        // A second click on it lets the selection go; a right click does not.
        painted_strings(&mut app);
        let click = |app: &mut App, button| {
            app.handle(UiEvent::MouseDown {
                at: seg.center(),
                button,
            })
        };
        assert!(click(&mut app, MouseButton::Left).repaint);
        assert_eq!(app.table.selected, None);
        click(&mut app, MouseButton::Right);
        click(&mut app, MouseButton::Right);
        assert_eq!(app.table.selected, Some(id(20)));
        app.handle(UiEvent::MouseLeave);

        // In the Map the strip gives way, and the toolbar moves up.
        cmd(&mut app, Command::SetView(ViewMode::Map));
        painted_strings(&mut app);
        assert!(app.toolbar.segments[0].y < list_toolbar);
    }

    #[test]
    fn the_table_starts_sorted_by_cycles_used() {
        let mut app = App::default();
        busy_minute(&mut app);
        ready(&mut app);
        assert_eq!(app.table.sort_col, col::CYCLES);
        // By what each has used lately, not by the CPU column (p20 shows 10 %,
        // p12 5 %): p11 30 G, p20 12 G, p12 6 G, then those that used nothing.
        // (The usage strip's labels come first; the table's six rows are last.)
        let rows = |app: &mut App| {
            let names = painted_names(app);
            names[names.len() - 6..].to_vec()
        };
        assert_eq!(rows(&mut app)[..3], ["p11.exe", "p20.exe", "p12.exe"]);
        let strings = painted_strings(&mut app);
        for want in ["Cycles", "29.2 G", "11.7 G", "5.85 G"] {
            assert!(strings.iter().any(|s| s == want), "{want}: {strings:?}");
        }
        // In the tree a branch carries what is under it: p4 has all of p11's and
        // p12's cycles, and p10 has p11's.
        cmd(&mut app, Command::SetView(ViewMode::Tree));
        assert_eq!(
            rows(&mut app),
            ["p4.exe", "p10.exe", "p11.exe", "p12.exe", "p13.exe", "p20.exe"]
        );
        // Idle from here on, the totals fade: 5 % a second.
        let mut procs = family().processes.clone();
        for p in &mut procs {
            p.cycles = G * match p.key().pid {
                11 => 130,
                12 => 106,
                20 => 112,
                _ => 100,
            };
        }
        app.set_snapshot(snapshot(3, procs));
        assert!(painted_strings(&mut app).iter().any(|s| s == "27.8 G"));
    }

    #[test]
    fn the_fade_rate_is_a_setting() {
        let mut app = App::default();
        busy_minute(&mut app);
        ready(&mut app);
        app.set_settings(Settings::default().with_usage_decay(50));
        assert_eq!(app.settings().usage_decay_percent, 50);
        let mut procs = family().processes.clone();
        for p in &mut procs {
            p.cycles = G * match p.key().pid {
                11 => 130,
                12 => 106,
                20 => 112,
                _ => 100,
            };
        }
        // Half of p11's 29.2 G is gone a second later.
        app.set_snapshot(snapshot(3, procs));
        assert!(painted_strings(&mut app).iter().any(|s| s == "14.6 G"));
    }

    #[test]
    fn the_history_charts_programs_and_shares_the_selection_and_the_hover() {
        let mut app = App::default();
        busy_minute(&mut app);
        // A few more seconds, so there is a history to draw.
        for tick in 3..=12u64 {
            let mut procs = family().processes.clone();
            for p in &mut procs {
                let rate = match p.key().pid {
                    11 => 30,
                    12 => 6,
                    20 => 12,
                    _ => 0,
                };
                p.cycles = G * (100 + rate * (tick - 1));
            }
            app.set_snapshot(snapshot(tick, procs));
        }
        ready(&mut app);
        assert_eq!(ViewMode::parse("History"), ViewMode::History);
        let segment = app.toolbar.segments[3].center();
        app.handle(UiEvent::MouseDown {
            at: segment,
            button: MouseButton::Left,
        });
        assert_eq!(app.view(), ViewMode::History);
        let strings = painted_strings(&mut app);
        assert!(!strings.iter().any(|s| s == "PID"), "no table: {strings:?}");
        for want in [
            "Fading total",
            "Rate",
            "p11.exe",
            "p20.exe",
            "p12.exe",
            "now",
        ] {
            assert!(strings.iter().any(|s| s == want), "{want}: {strings:?}");
        }

        // The other reading: cycles a second.
        let p11 = app.usage.program("p11.exe").unwrap();
        app.handle(UiEvent::MouseDown {
            at: Point::new(segment.x, 0.0),
            button: MouseButton::Left,
        });
        let strings = painted_strings(&mut app);
        assert!(strings.iter().any(|s| s == "now"));
        let rate = strings
            .iter()
            .position(|s| s == "Rate")
            .expect("the switch");
        assert!(rate > 0);

        // A click on a band selects the program's busiest process, and the
        // ancestry line follows; the menu is that process's.
        let mut at = None;
        'find: for y in (0..700).step_by(4) {
            for x in (150..500).step_by(4) {
                let p = Point::new(x as f32, y as f32);
                if app.history.program_at(p) == Some(p11) {
                    at = Some(p);
                    break 'find;
                }
            }
        }
        let at = at.expect("p11 has a band");
        assert!(
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            })
            .repaint
        );
        assert_eq!(app.table.selected, Some(id(11)));
        let crumb = "p4.exe \u{203a} p10.exe \u{203a} p11.exe";
        assert!(painted_strings(&mut app).iter().any(|s| s == crumb));
        let r = app.handle(UiEvent::ContextMenu { at: Some(at) });
        assert!(matches!(r.effect, Some(Effect::Menu { .. })), "{r:?}");

        // A second click on the band lets the selection go, and a third takes it
        // back.
        for want in [None, Some(id(11))] {
            painted_strings(&mut app);
            app.handle(UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            });
            assert_eq!(app.table.selected, want);
        }

        // Pointing at the chart marks the same moment in the summary charts.
        assert!(app.handle(UiEvent::MouseMove(at)).repaint);
        painted_strings(&mut app);
        let age = app.history.hover_age().expect("over the plot");
        assert_eq!(app.charts.hover_age(), Some(age));
        app.handle(UiEvent::MouseLeave);
        painted_strings(&mut app);
        assert_eq!(app.charts.hover_age(), None);

        // Ctrl+T goes back to the table, selection kept.
        cmd(&mut app, Command::ToggleView);
        assert_eq!(app.view(), ViewMode::List);
        assert_eq!(app.table.selected, Some(id(11)));
        assert!(painted_strings(&mut app).iter().any(|s| s == "PID"));
    }
}
