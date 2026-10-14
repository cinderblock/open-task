//! Toolkit-neutral shell logic.
//!
//! A shell is a window around an [`ot_ui::App`]. Most of what a shell does has
//! nothing to do with its window system: start the feed (the sampler, or a
//! replay's player), run the updater, read lists and sample CPU on worker
//! threads, record, replay, save settings, and carry out the [`Effect`]s the view
//! returns that only need the OS (ending, suspending or restarting a process,
//! services, sessions, startup entries). [`Shell`] does all of that, once, for
//! every platform.
//!
//! What it cannot do is reported to the host as [`Out`] values, which the host
//! acts on with its own means: repaint, show a menu or a question
//! ([`Ask`]), tell the user something ([`Notice`]), or perform a [`HostEffect`]
//! such as revealing a file. Nothing here blocks on the user. An [`Ask`] carries
//! what to do with the answer, and the host hands the answer back
//! ([`Shell::menu_closed`], [`Shell::confirmed`], [`Shell::record_to`]) whenever
//! it has one, so a modal dialog (Win32, Qt) and an asynchronous one (GTK 4) are
//! the same to this crate. The host calls in only with the shell unborrowed by
//! anything re-entrant, which is what lets Win32's modal loops re-enter the
//! window procedure safely.
//!
//! Worker threads and the feed reach the host's UI thread through a [`Waker`]
//! the host supplies (`PostMessage`, `g_idle_add`, a queued Qt call); the host
//! then calls [`Shell::drain`] on its own thread.

#![forbid(unsafe_code)]

mod menu;
mod report;
pub mod settings_store;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ot_core::{Feed, Player, RecordHeader, Recorder, Recording, Sampler, SamplerConfig};
use ot_model::ProcessKey;
use ot_paint::Point;
use ot_probe::{
    ControlError, CpuSampler, Inventory as _, PlatformControl, PlatformSampler, ProcessControl,
    ServiceControl, SessionControl, StartupControl, SystemProbe,
};
use ot_ui::{
    App, Command, Effect, Inventory, MenuAction, Page, ProcessAction, Query, RecordingView,
    ReplayAction, ReplayState, ServiceAction, SessionAction, Settings, TaskManager, Theme, UiEvent,
    UpdateAction, UpdateView, ViewLayout, ViewMode,
};
use ot_update::{Installation, Updater};

pub use menu::MenuNode;

/// How often a scheduled update check runs.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How soon the Connections page reads its list again while it is showing.
pub const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// Which theme to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// Follow the system's light or dark setting, including live changes.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemePreference {
    /// Parse a command-line value. Unknown values fall back to `System`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::System,
        }
    }

    /// Whether the theme is dark, given what the system prefers.
    #[must_use]
    pub fn dark(self, system_dark: bool) -> bool {
        match self {
            Self::System => system_dark,
            Self::Dark => true,
            Self::Light => false,
        }
    }
}

/// The light or dark theme.
#[must_use]
pub fn theme_for(dark: bool) -> Theme {
    if dark {
        Theme::dark()
    } else {
        Theme::light()
    }
}

/// Startup options for a shell.
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    pub theme: ThemePreference,
    /// How the process table starts out; Ctrl+T switches at runtime.
    pub view: ViewMode,
    /// The page shown first; the rail and Ctrl+Tab switch at runtime.
    pub page: Page,
    /// Whether `view` and `page` were asked for on the command line. When not,
    /// the window opens where the last session left off.
    pub view_given: bool,
    pub page_given: bool,
    /// This build's version, as `crates/ot-app/build.rs` made it: shown on the
    /// update button, and what updates are compared with.
    pub version: &'static str,
    /// Play this recording instead of sampling the machine.
    pub replay: Option<String>,
}

/// Why a shell could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("cannot replay {path}: {source}")]
    Replay {
        path: String,
        #[source]
        source: ot_core::RecordError,
    },
}

/// Where the user's settings are kept: the registry on Windows, a file
/// elsewhere. Nothing here is fatal; a value that cannot be read is the default.
pub trait Prefs {
    fn load(&self) -> Settings;
    fn save(&self, settings: &Settings);
    fn load_layout(&self) -> Option<ViewLayout>;
    fn save_layout(&self, layout: &ViewLayout);
}

/// What only the OS can do and needs no window, called from worker threads too.
pub trait Platform: Send + Sync {
    /// Start `command_line` as a new process, in `directory` when given, with this
    /// process's rights. For Restart and uninstallers.
    ///
    /// # Errors
    /// Why it could not start, in words for the user.
    fn launch(&self, command_line: &str, directory: Option<&str>) -> Result<(), String>;
}

/// Wakes the host's UI thread from another thread; the host then calls
/// [`Shell::drain`]. Must be cheap and must not block.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Everything [`Shell::start`] needs.
pub struct Start<'a> {
    pub probe: Box<dyn SystemProbe>,
    pub config: SamplerConfig,
    pub options: &'a ShellOptions,
    /// The theme to start with.
    pub theme: Theme,
    /// Whether a system backdrop shows where the view leaves pixels transparent.
    pub backdrop: bool,
    /// Whether the system's animation effects are on.
    pub system_animations: bool,
    /// The "Replace Task Manager" card as it stands.
    pub task_manager: TaskManager,
    /// Where the installer put this copy, if it did; updates install only then.
    pub installation: Option<Installation>,
    pub prefs: Box<dyn Prefs>,
    pub platform: Arc<dyn Platform>,
    pub waker: Waker,
}

impl std::fmt::Debug for Start<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Start")
            .field("options", self.options)
            .field("backdrop", &self.backdrop)
            .finish_non_exhaustive()
    }
}

/// How serious a [`Notice`] is, for the icon on the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warning,
    Error,
}

/// Something to tell the user in a message box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
}

/// A question for the user. The host shows it however its toolkit does and hands
/// the answer back; dismissing it is answering nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// A popup menu at `at` (DIPs in the window). Answer with
    /// [`Shell::menu_closed`].
    Menu { at: Point, nodes: Vec<MenuNode> },
    /// A yes/no question, No the default. On Yes, call [`Shell::confirmed`] with
    /// `then`.
    Confirm { question: String, then: Confirmed },
    /// Where to record: a save dialog for a Flight Recorder file
    /// (`open-task.otrec`). Answer with [`Shell::record_to`].
    RecordPath,
    /// The Run dialog, filled in with `initial`. The host runs what the user
    /// types, as the platform does, then reports it with [`Shell::set_last_run`].
    Run { initial: String },
}

/// What a confirmed [`Ask::Confirm`] goes on to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    Terminate {
        targets: Vec<ProcessKey>,
        label: String,
    },
    Restart {
        target: ProcessKey,
        name: String,
        command_line: String,
        directory: Option<String>,
    },
    Session {
        id: u32,
        action: SessionAction,
        what: String,
    },
    Uninstall {
        name: String,
        command: String,
    },
}

/// An effect the host performs with its own means: they need its window, its
/// toolkit, or a platform facility this crate does not wrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEffect {
    /// Reveal this file in the file manager.
    OpenFileLocation(String),
    /// Bring the window with this handle forward.
    SwitchTo(u64),
    /// Minimize this window ("Minimize on use").
    Minimize,
    /// Open a web page in the browser.
    OpenUrl(String),
    /// The file manager's properties for a file.
    Properties(String),
    CopyText(String),
    /// Start a copy of this program with administrator (root) rights and, if that
    /// works, close this one with [`Shell::set_closing`]`(Closing::Relaunching)`.
    RunAsAdministrator,
    /// Start the window crosshair; report the pick with [`App::select_pid`].
    PickWindow,
    /// The system's services console.
    OpenServices,
    /// Open an installed program's folder.
    OpenInstallLocation {
        location: Option<String>,
        uninstall: Option<String>,
    },
    /// Make the system open this copy in its task manager's place, or stop.
    ReplaceTaskManager(bool),
}

/// What the host should do after a call into the shell, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// Draw a frame.
    Repaint,
    /// The window title changed.
    Title(String),
    Notice(Notice),
    Ask(Ask),
    Do(HostEffect),
    /// Settings the shell applies itself changed (always on top, chart drawing).
    Settings(Settings),
    /// The newest CPU and memory use, in percent, for a tray meter.
    Meters {
        cpu: f32,
        memory: Option<f32>,
    },
    /// Call [`Shell::refresh`] after this long.
    RefreshIn(Duration),
    /// Close the window.
    Close,
}

/// How far the app is along in closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    No,
    /// Closed with an update waiting and "Install updates automatically" on: the
    /// window is hidden and Setup is running.
    InstallingUpdate,
    /// Logoff or shutdown is closing the app: never install then.
    SessionEnding,
    /// A copy started as administrator takes over: close without installing.
    Relaunching,
}

/// What the host does when its window is asked to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Close as usual.
    Close,
    /// Hide the window and keep running: the update is installing and the
    /// installer ends the process.
    Hide,
}

/// What a worker thread hands back.
enum Wake {
    Sample {
        target: ProcessKey,
        name: String,
        result: Result<ot_model::attribution::Attribution, ot_probe::SampleError>,
    },
    Inventory(Inventory),
    Action {
        /// `stop the service Spooler`, for a failure's message.
        what: String,
        result: Result<Option<String>, String>,
    },
}

/// The queue from other threads to the UI thread, plus flags for the two
/// notifications that only ever mean "read the newest".
struct Inbox {
    queue: Mutex<Vec<Wake>>,
    snapshot: AtomicBool,
    update: AtomicBool,
    waker: Waker,
}

impl Inbox {
    fn post(&self, wake: Wake) {
        if let Ok(mut q) = self.queue.lock() {
            q.push(wake);
        }
        (self.waker)();
    }

    fn flag(&self, flag: &AtomicBool) {
        // Only the first notification since the last drain wakes the host.
        if !flag.swap(true, Ordering::AcqRel) {
            (self.waker)();
        }
    }
}

/// The app with its feed, updater and workers. Lives on the host's UI thread.
pub struct Shell {
    app: App,
    feed: Option<Feed>,
    inbox: Arc<Inbox>,
    control: PlatformControl,
    prefs: Box<dyn Prefs>,
    platform: Arc<dyn Platform>,
    version: &'static str,
    /// The file being recorded to, by name, while one is.
    record_name: Option<String>,
    /// The recording being played, by name.
    replay_name: String,
    /// What the Run dialog showed last, shown again next time.
    last_run: String,
    /// The actions of the menu showing, by item id.
    menu: Vec<MenuAction>,
    /// `None` if this build's version does not parse, which a build from this
    /// repository never produces.
    updater: Option<Updater>,
    closing: Closing,
    elevated: bool,
    /// The window title as last reported.
    title: String,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("version", &self.version)
            .field("closing", &self.closing)
            .finish_non_exhaustive()
    }
}

impl Shell {
    /// Build the app as the last session left it, start the feed and the updater.
    /// The host shows the first frame after this returns.
    ///
    /// # Errors
    /// A replay that cannot be opened.
    pub fn start(start: Start<'_>) -> Result<Self, StartError> {
        let Start {
            probe,
            config,
            options,
            theme,
            backdrop,
            system_animations,
            task_manager,
            installation,
            prefs,
            platform,
            waker,
        } = start;
        let inbox = Arc::new(Inbox {
            queue: Mutex::new(Vec::new()),
            snapshot: AtomicBool::new(false),
            update: AtomicBool::new(false),
            waker,
        });

        let elevated = ot_probe::is_elevated();
        let mut app = App::new(theme);
        app.set_backdrop(backdrop);
        app.set_settings(prefs.load());
        app.set_system_animations(system_animations);
        app.set_elevated(elevated);
        // Where the last session left off, unless the command line says otherwise.
        if let Some(layout) = prefs.load_layout() {
            app.apply_view_layout(&layout);
        }
        if options.view_given {
            app.set_view(options.view);
        }
        if options.page_given {
            app.set_page(options.page);
        }
        tracing::info!(?installation, "installation");
        app.set_task_manager(task_manager);
        let updater = start_updater(options.version, &inbox, installation);
        let can_install = updater.as_ref().is_some_and(Updater::can_install);
        app.set_update(UpdateView::new(options.version, can_install));

        let notify = {
            let inbox = Arc::clone(&inbox);
            move || inbox.flag(&inbox.snapshot)
        };
        // A replay stands in for the sampler: the player publishes frames the same way.
        let replay_name = options.replay.as_deref().map(file_name).unwrap_or_default();
        let feed = match &options.replay {
            Some(path) => {
                let replay_error = |source| StartError::Replay {
                    path: path.clone(),
                    source,
                };
                let recording = Recording::open(path).map_err(replay_error)?;
                let player = Player::with_notify(recording, notify).map_err(replay_error)?;
                Feed::Replay(player)
            }
            None => Feed::Live(Sampler::start_with_notify(probe, config, notify)),
        };
        feed.set_interval(Duration::from_millis(u64::from(
            app.settings().update_interval_ms,
        )));

        let shell = Self {
            app,
            feed: Some(feed),
            inbox,
            control: PlatformControl,
            prefs,
            platform,
            version: options.version,
            record_name: None,
            replay_name,
            last_run: String::new(),
            menu: Vec::new(),
            updater,
            closing: Closing::No,
            elevated,
            title: window_title(elevated, false),
        };
        if let Some(q) = shell.app.page_query() {
            shell.spawn_query(q);
        }
        // Show what the feed already holds. A replay's player publishes its first
        // frame before the window exists and stays paused, so nothing would bring it.
        shell.inbox.flag(&shell.inbox.snapshot);
        Ok(shell)
    }

    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The view, for what the host feeds it directly: its size, the theme, a
    /// picked window, and painting.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// The window title: the name, whether this copy runs as administrator (Task
    /// Manager says so the same way), and whether the display is paused.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn elevated(&self) -> bool {
        self.elevated
    }

    #[must_use]
    pub fn closing(&self) -> Closing {
        self.closing
    }

    pub fn set_closing(&mut self, closing: Closing) {
        self.closing = closing;
    }

    /// What the Run dialog ran; it shows it again next time.
    pub fn set_last_run(&mut self, command: &str) {
        command.clone_into(&mut self.last_run);
    }

    /// Feed an event to the view and carry out what it asks for.
    pub fn handle(&mut self, ev: UiEvent, out: &mut Vec<Out>) {
        let reaction = self.app.handle(ev);
        if reaction.repaint {
            out.push(Out::Repaint);
        }
        let title = window_title(self.elevated, self.app.paused());
        if self.title != title {
            self.title.clone_from(&title);
            out.push(Out::Title(title));
        }
        if let Some(effect) = reaction.effect {
            self.perform(effect, out);
        }
    }

    /// The menu last asked for closed: `chosen` is the item picked, if one was,
    /// and `at` where the pointer is now, if the shell knows and it is over the
    /// window.
    pub fn menu_closed(&mut self, chosen: Option<usize>, at: Option<Point>, out: &mut Vec<Out>) {
        self.handle(UiEvent::MenuClosed { at }, out);
        if let Some(action) = chosen.and_then(|id| self.menu.get(id).copied()) {
            self.handle(UiEvent::Command(Command::Menu(action)), out);
        }
    }

    /// The user said Yes to an [`Ask::Confirm`].
    pub fn confirmed(&mut self, then: Confirmed, out: &mut Vec<Out>) {
        match then {
            Confirmed::Terminate { targets, label } => self.terminate(&targets, &label, out),
            Confirmed::Restart {
                target,
                name,
                command_line,
                directory,
            } => match self.control.terminate(target) {
                Ok(()) | Err(ControlError::Gone) => {
                    if let Err(e) = self.platform.launch(&command_line, directory.as_deref()) {
                        out.push(warning(format!(
                            "{name} was ended but could not be started again.\n\n{e}"
                        )));
                    }
                }
                Err(e) => out.push(warning(format!("Could not restart {name}.\n\n{e}"))),
            },
            Confirmed::Session { id, action, what } => {
                self.spawn_action(what, move || {
                    let control = PlatformControl;
                    match action {
                        SessionAction::Disconnect => control.disconnect_session(id),
                        SessionAction::SignOut => control.logoff_session(id),
                    }
                    .map(|()| None)
                    .map_err(|e| e.to_string())
                });
            }
            Confirmed::Uninstall { name, command } => {
                if let Err(e) = self.platform.launch(&command, None) {
                    out.push(warning(format!(
                        "Could not start the uninstaller for {name}.\n\n{e}"
                    )));
                }
            }
        }
    }

    /// Take in what other threads sent since the last call. Call it on the UI
    /// thread whenever the [`Waker`] fires.
    pub fn drain(&mut self, out: &mut Vec<Out>) {
        if self.inbox.snapshot.swap(false, Ordering::AcqRel) {
            self.on_snapshot(out);
        }
        if self.inbox.update.swap(false, Ordering::AcqRel) {
            self.on_update(out);
        }
        let wakes = self
            .inbox
            .queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for wake in wakes {
            self.on_wake(wake, out);
        }
    }

    fn on_snapshot(&mut self, out: &mut Vec<Out>) {
        let Some(feed) = &self.feed else {
            return;
        };
        let snap = feed.latest();
        let memory = (snap.memory.total.get() > 0)
            .then(|| snap.memory.in_use().get() as f32 / snap.memory.total.get() as f32 * 100.0);
        out.push(Out::Meters {
            cpu: snap.cpu.total.get(),
            memory,
        });
        let mut changed = self.app.set_snapshot(snap);
        changed |= match feed {
            Feed::Replay(player) => self
                .app
                .set_replay(Some(replay_state(player, &self.replay_name))),
            Feed::Live(sampler) => self
                .app
                .set_recording(recording_view(sampler, self.record_name.as_deref())),
        };
        if changed {
            out.push(Out::Repaint);
        }
    }

    fn on_update(&mut self, out: &mut Vec<Out>) {
        let Some(u) = &self.updater else {
            return;
        };
        let status = u.status();
        // Installing as it closed, and Setup ended without closing it (cancelled,
        // or failed): finish closing.
        let installing = matches!(status, ot_update::Status::Installing { .. });
        if self.closing == Closing::InstallingUpdate && !installing {
            tracing::info!(?status, "setup did not close the app; closing");
            out.push(Out::Close);
        }
        if self.app.set_update_status(status) {
            out.push(Out::Repaint);
        }
    }

    fn on_wake(&mut self, wake: Wake, out: &mut Vec<Out>) {
        match wake {
            Wake::Sample {
                target,
                name,
                result,
            } => match result {
                Ok(a) => {
                    tracing::info!(pid = target.pid, samples = a.samples, "CPU sample done");
                    self.app.set_attribution(Arc::new(a));
                    out.push(Out::Repaint);
                }
                Err(e) => {
                    tracing::warn!(pid = target.pid, error = %e, "CPU sample failed");
                    self.app.sampling_failed(target);
                    out.push(Out::Repaint);
                    out.push(warning(format!("Could not sample {name}.\n\n{e}")));
                }
            },
            Wake::Inventory(inventory) => {
                let connections = matches!(inventory, Inventory::Connections(_));
                if self.app.set_inventory(inventory) {
                    out.push(Out::Repaint);
                }
                // The Connections page follows the system: read it again shortly,
                // while it is showing.
                if connections && self.app.page() == Page::Connections {
                    out.push(Out::RefreshIn(REFRESH_EVERY));
                }
            }
            Wake::Action { what, result } => {
                out.push(Out::Repaint);
                match result {
                    Ok(None) => {}
                    Ok(Some(message)) => out.push(Out::Notice(Notice {
                        kind: NoticeKind::Info,
                        text: message,
                    })),
                    Err(e) => out.push(warning(format!("Could not {what}.\n\n{e}"))),
                }
            }
        }
    }

    /// The [`Out::RefreshIn`] time came: read the Connections list again if it
    /// is still showing.
    pub fn refresh(&self) {
        if self.app.page() == Page::Connections && self.closing == Closing::No {
            self.spawn_query(Query::Connections);
        }
    }

    /// The host's hourly tick: check for an update when a day has passed since the
    /// last check and the settings allow it.
    pub fn update_tick(&self) {
        let settings = self.app.settings();
        if let Some(u) = &self.updater {
            if settings.check_updates && u.due(CHECK_EVERY) {
                let download = settings.download_updates || settings.install_updates;
                u.check(false, download && u.can_install());
            }
        }
    }

    /// The window is closing. Saves where things stand for the next start. With
    /// "Install updates automatically" on and a verified update waiting, starts
    /// the installer and says to hide rather than close: the next start is the
    /// new version.
    pub fn close(&mut self) -> CloseOutcome {
        self.prefs.save_layout(&self.app.view_layout());
        if self.closing != Closing::No {
            return CloseOutcome::Close;
        }
        let settings = self.app.settings();
        let Some(u) = self.updater.clone() else {
            return CloseOutcome::Close;
        };
        let ready = matches!(u.status(), ot_update::Status::Ready { .. });
        if !(settings.install_updates && ready && u.can_install()) {
            return CloseOutcome::Close;
        }
        tracing::info!("installing the update as open-task closes");
        self.closing = Closing::InstallingUpdate;
        self.stop_feed();
        u.install(false);
        CloseOutcome::Hide
    }

    /// Stop whatever feeds the window, so nothing posts to it once it is gone.
    pub fn stop_feed(&mut self) {
        match self.feed.take() {
            Some(Feed::Live(mut sampler)) => sampler.stop(),
            Some(Feed::Replay(mut player)) => player.stop(),
            None => {}
        }
    }

    /// Toggle "Always on top" from outside the view (the tray menu).
    pub fn toggle_always_on_top(&mut self, out: &mut Vec<Out>) {
        let mut settings = self.app.settings();
        settings.always_on_top = !settings.always_on_top;
        self.app.set_settings(settings);
        self.save_settings(settings, out);
        out.push(Out::Repaint);
    }

    /// Start recording to `path`, which the user chose for [`Ask::RecordPath`].
    pub fn record_to(&mut self, path: &str, out: &mut Vec<Out>) {
        let Some(sampler) = self.feed.as_ref().and_then(Feed::sampler) else {
            return;
        };
        let snap = sampler.latest();
        let header = RecordHeader {
            app_version: self.version.to_owned(),
            hardware: (*snap.hardware).clone(),
            capabilities: snap.capabilities,
            started: std::time::SystemTime::now(),
            interval: sampler.interval(),
        };
        let recorder = match Recorder::create(path, &header) {
            Ok(r) => r,
            Err(e) => {
                out.push(warning(format!("Could not record to {path}.\n\n{e}")));
                return;
            }
        };
        let _ = sampler.record_to(recorder);
        let name = file_name(path);
        let view = recording_view(sampler, Some(&name));
        self.record_name = Some(name);
        self.app.set_recording(view);
        out.push(Out::Repaint);
    }

    fn stop_recording(&mut self, out: &mut Vec<Out>) {
        let Some(sampler) = self.feed.as_ref().and_then(Feed::sampler) else {
            return;
        };
        let stats = sampler.stop_recording();
        let name = self.record_name.take().unwrap_or_default();
        self.app.set_recording(None);
        out.push(Out::Repaint);
        match stats {
            Some(Ok(s)) => {
                let mut size = String::new();
                ot_ui::format::bytes(&mut size, ot_model::Bytes(s.written.bytes));
                let mut text = format!("Recorded {} frames to {name} ({size}).", s.written.frames);
                if s.dropped > 0 {
                    let _ = write!(
                        text,
                        "\n\n{} frames were dropped: the disk could not keep up.",
                        s.dropped
                    );
                }
                out.push(warning(text));
            }
            Some(Err(e)) => out.push(warning(format!(
                "The recording {name} could not be finished.\n\n{e}"
            ))),
            None => {}
        }
    }

    fn save_settings(&mut self, settings: Settings, out: &mut Vec<Out>) {
        self.prefs.save(&settings);
        if let Some(feed) = &self.feed {
            feed.set_interval(Duration::from_millis(u64::from(
                settings.update_interval_ms,
            )));
        }
        out.push(Out::Settings(settings));
        // Downloading was just turned on with a release waiting: start now.
        if let Some(u) = &self.updater {
            let waiting = matches!(u.status(), ot_update::Status::Available { .. });
            let download = settings.download_updates || settings.install_updates;
            if download && waiting && u.can_install() {
                u.download();
            }
        }
    }

    // One arm per effect; each is short, and the match is the map of who does what.
    #[allow(clippy::too_many_lines)]
    fn perform(&mut self, effect: Effect, out: &mut Vec<Out>) {
        match effect {
            Effect::Menu { at, entries } => {
                self.menu.clear();
                let nodes = menu::build(&entries, &mut self.menu, self.control);
                out.push(Out::Ask(Ask::Menu { at, nodes }));
            }
            Effect::Terminate { targets, label } => out.push(Out::Ask(Ask::Confirm {
                question: format!("End {label}?\n\nAny unsaved data in it will be lost."),
                then: Confirmed::Terminate { targets, label },
            })),
            Effect::OpenFileLocation(path) => out.push(Out::Do(HostEffect::OpenFileLocation(path))),
            Effect::SampleCpu { target, seconds } => self.sample_cpu(target, seconds, out),
            Effect::Process {
                target,
                name,
                action,
            } => self.process_action(target, &name, action, out),
            Effect::SwitchTo(handle) => {
                out.push(Out::Do(HostEffect::SwitchTo(handle)));
                // Minimize on use, as Task Manager's: get out of the way of the
                // window switched to.
                if self.app.settings().minimize_on_use {
                    out.push(Out::Do(HostEffect::Minimize));
                }
            }
            Effect::OpenUrl(url) => out.push(Out::Do(HostEffect::OpenUrl(url))),
            Effect::Properties(path) => out.push(Out::Do(HostEffect::Properties(path))),
            Effect::CopyText(text) => out.push(Out::Do(HostEffect::CopyText(text))),
            Effect::RunTask => out.push(Out::Ask(Ask::Run {
                initial: self.last_run.clone(),
            })),
            Effect::Replay(action) => {
                if let Some(player) = self.feed.as_ref().and_then(Feed::player) {
                    match action {
                        ReplayAction::Toggle => player.toggle(),
                        ReplayAction::Seek(i) => player.seek(i),
                        ReplayAction::Step(by) => player.step(by),
                        ReplayAction::Speed(speed) => player.set_speed(speed),
                    }
                    // A seek or a step publishes a frame, which repaints; a speed
                    // change and a pause do not, so paint the bar's new state now.
                    let state = replay_state(player, &self.replay_name);
                    self.app.set_replay(Some(state));
                }
                out.push(Out::Repaint);
            }
            Effect::Record(true) => out.push(Out::Ask(Ask::RecordPath)),
            Effect::Record(false) => self.stop_recording(out),
            Effect::RunAsAdministrator => out.push(Out::Do(HostEffect::RunAsAdministrator)),
            Effect::PickWindow => out.push(Out::Do(HostEffect::PickWindow)),
            Effect::Service {
                name,
                display_name,
                action,
            } => {
                let what = match action {
                    ServiceAction::Start => format!("start {display_name}"),
                    ServiceAction::Stop => format!("stop {display_name}"),
                    ServiceAction::Restart => format!("restart {display_name}"),
                };
                self.spawn_action(what, move || {
                    let control = PlatformControl;
                    match action {
                        ServiceAction::Start => control.start_service(&name),
                        ServiceAction::Stop => control.stop_service(&name),
                        ServiceAction::Restart => control
                            .stop_service(&name)
                            .and_then(|()| control.start_service(&name)),
                    }
                    .map(|()| None)
                    .map_err(|e| e.to_string())
                });
            }
            Effect::OpenServices => out.push(Out::Do(HostEffect::OpenServices)),
            Effect::Session { id, user, action } => {
                let (question, what) = match action {
                    SessionAction::Disconnect => (
                        format!("Disconnect {user}?\n\nTheir programs keep running."),
                        format!("disconnect {user}"),
                    ),
                    SessionAction::SignOut => (
                        format!(
                            "Sign out {user}?\n\nAny unsaved data in their programs will be lost."
                        ),
                        format!("sign out {user}"),
                    ),
                };
                out.push(Out::Ask(Ask::Confirm {
                    question,
                    then: Confirmed::Session { id, action, what },
                }));
            }
            Effect::Startup { entry, on } => {
                if let Err(e) = self.control.set_startup_enabled(&entry, on) {
                    let verb = if on { "enable" } else { "disable" };
                    out.push(warning(format!("Could not {verb} {}.\n\n{e}", entry.name)));
                } else {
                    // Show the change: the list is read again.
                    self.spawn_query(Query::Startup);
                }
            }
            Effect::OpenInstallLocation {
                location,
                uninstall,
            } => out.push(Out::Do(HostEffect::OpenInstallLocation {
                location,
                uninstall,
            })),
            Effect::Uninstall { name, command } => out.push(Out::Ask(Ask::Confirm {
                question: format!("Uninstall {name}?\n\nIts uninstaller will start."),
                then: Confirmed::Uninstall { name, command },
            })),
            Effect::Query(query) => self.spawn_query(query),
            Effect::SaveSettings(settings) => self.save_settings(settings, out),
            Effect::Update(action) => self.update(action, out),
            Effect::ReplaceTaskManager(on) => {
                out.push(Out::Do(HostEffect::ReplaceTaskManager(on)));
            }
        }
    }

    /// One action on a process. The quick ones run here and report a failure at
    /// once; a dump and a wait chain run on a worker thread; a restart asks first.
    fn process_action(
        &self,
        target: ProcessKey,
        name: &str,
        action: ProcessAction,
        out: &mut Vec<Out>,
    ) {
        let what = report::describe(&action, name);
        let control = self.control;
        let r = match action {
            ProcessAction::WriteDump => {
                self.spawn_action(what, move || {
                    control
                        .write_dump(target, &std::env::temp_dir())
                        .map(|p| Some(format!("Dump written to\n{}", p.display())))
                        .map_err(|e| e.to_string())
                });
                return;
            }
            ProcessAction::WaitChain { threads } => {
                let name = name.to_owned();
                self.spawn_action(what, move || {
                    let chain = control
                        .wait_chain(target, &threads)
                        .map_err(|e| e.to_string())?;
                    Ok(Some(report::wait_chain(&name, target.pid, &chain)))
                });
                return;
            }
            ProcessAction::Restart {
                command_line,
                directory,
            } => {
                out.push(Out::Ask(Ask::Confirm {
                    question: format!("Restart {name}?\n\nAny unsaved data in it will be lost."),
                    then: Confirmed::Restart {
                        target,
                        name: name.to_owned(),
                        command_line,
                        directory,
                    },
                }));
                return;
            }
            ProcessAction::SetPriority(p) => control.set_priority(target, p),
            ProcessAction::SetAffinity(mask) => control.set_affinity(target, mask),
            ProcessAction::Suspend => control.suspend(target),
            ProcessAction::Resume => control.resume(target),
            ProcessAction::EfficiencyMode(on) => control.set_efficiency_mode(target, on),
        };
        match r {
            Ok(()) => tracing::info!(pid = target.pid, %what, "done"),
            Err(ControlError::Gone) => {}
            Err(e) => out.push(warning(format!("Could not {what}.\n\n{e}"))),
        }
        out.push(Out::Repaint);
    }

    /// End every target. Processes that are already gone are not failures;
    /// anything else that fails is reported together at the end.
    fn terminate(&self, targets: &[ProcessKey], label: &str, out: &mut Vec<Out>) {
        let mut failures = String::new();
        for key in targets {
            match self.control.terminate(*key) {
                Ok(()) => tracing::info!(pid = key.pid, "terminated"),
                Err(ControlError::Gone) => {}
                Err(e) => {
                    tracing::warn!(pid = key.pid, error = %e, "terminate failed");
                    let _ = writeln!(failures, "PID {}: {e}", key.pid);
                }
            }
        }
        if !failures.is_empty() {
            out.push(Out::Notice(Notice {
                kind: NoticeKind::Error,
                text: format!("Could not end {label}.\n\n{failures}"),
            }));
        }
    }

    /// The update button was pressed. The updater works on its own threads and
    /// reports back through the waker.
    fn update(&self, action: UpdateAction, out: &mut Vec<Out>) {
        let Some(u) = &self.updater else {
            return;
        };
        match action {
            // Asked for: download straight away if this copy can install it.
            UpdateAction::Check => u.check(true, u.can_install()),
            UpdateAction::Download => u.download(),
            UpdateAction::Install => u.install(true),
            UpdateAction::OpenReleasePage => {
                if let Some(url) = u.release_page() {
                    out.push(Out::Do(HostEffect::OpenUrl(url)));
                }
            }
        }
    }

    /// Sample a process's CPU on a worker thread. The sampler blocks for the whole
    /// window, so it must not run on the UI thread; the table keeps updating.
    fn sample_cpu(&mut self, target: ProcessKey, seconds: u32, out: &mut Vec<Out>) {
        let snap = self.feed.as_ref().map(Feed::latest);
        let p = snap
            .as_ref()
            .and_then(|s| s.processes.iter().find(|p| p.key() == target));
        let (name, services) = match p {
            Some(p) => (
                p.name().to_owned(),
                p.services
                    .iter()
                    .map(|s| s.name.to_string())
                    .collect::<Vec<_>>(),
            ),
            None => (format!("PID {}", target.pid), Vec::new()),
        };
        let inbox = Arc::clone(&self.inbox);
        let spawned = std::thread::Builder::new()
            .name("ot-cpu-sample".into())
            .spawn(move || {
                let result = PlatformSampler.sample(
                    target,
                    &services,
                    Duration::from_secs(u64::from(seconds)),
                );
                inbox.post(Wake::Sample {
                    target,
                    name,
                    result,
                });
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the sampling thread");
            self.app.sampling_failed(target);
            out.push(Out::Repaint);
        }
    }

    /// Run `work` on a worker thread; its outcome comes back through the waker.
    /// `what` names the attempt for the message if it fails.
    fn spawn_action(
        &self,
        what: String,
        work: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
    ) {
        let inbox = Arc::clone(&self.inbox);
        let spawned = std::thread::Builder::new()
            .name("ot-action".into())
            .spawn(move || {
                let result = work();
                inbox.post(Wake::Action { what, result });
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start a worker thread");
        }
    }

    /// Read a list on a worker thread.
    fn spawn_query(&self, query: Query) {
        let inbox = Arc::clone(&self.inbox);
        let spawned = std::thread::Builder::new()
            .name("ot-inventory".into())
            .spawn(move || {
                let control = PlatformControl;
                let inventory = match query {
                    Query::Startup => Inventory::Startup(control.startup_entries()),
                    Query::InstalledApps => Inventory::InstalledApps(control.installed_apps()),
                    Query::Connections => Inventory::Connections(control.connections()),
                    Query::System => Inventory::System(Box::new(control.system_facts())),
                };
                inbox.post(Wake::Inventory(inventory));
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the inventory thread");
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.stop_feed();
    }
}

fn warning(text: String) -> Out {
    Out::Notice(Notice {
        kind: NoticeKind::Warning,
        text,
    })
}

/// The window title: the name, whether this copy runs as administrator, and
/// whether the display is paused.
fn window_title(elevated: bool, paused: bool) -> String {
    let mut t = String::from("open-task");
    if elevated {
        t.push_str(" (Administrator)");
    }
    if paused {
        t.push_str(" (paused)");
    }
    t
}

/// The updater, waking the host after every change. It installs updates only if
/// the installer put this copy where it runs (`installation`).
fn start_updater(
    version: &str,
    inbox: &Arc<Inbox>,
    installation: Option<Installation>,
) -> Option<Updater> {
    let build = match ot_update::Build::parse(version) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "no updates for a build whose version does not parse");
            return None;
        }
    };
    let config = ot_update::Config {
        build,
        feed: ot_update::Feed::official(),
        download_dir: ot_update::default_download_dir(),
        installation,
    };
    let platform = ot_update::native(&format!("open-task/{version}"));
    let inbox = Arc::clone(inbox);
    Some(Updater::new(config, platform, move || {
        inbox.flag(&inbox.update);
    }))
}

/// A path's last component, for the transport bar and the Recording card.
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Where the player stands, for the transport bar.
fn replay_state(player: &Player, name: &str) -> ReplayState {
    let interval_ms = u64::try_from(player.interval().as_millis()).unwrap_or(1000);
    ReplayState {
        name: name.to_owned(),
        position: player.position(),
        len: player.len(),
        playing: player.is_playing(),
        speed: player.speed(),
        elapsed_ms: player.position() as u64 * interval_ms,
        duration_ms: u64::try_from(player.recording().duration().as_millis()).unwrap_or(0),
    }
}

/// What the sampler is recording, for the Settings page; `None` when nothing.
fn recording_view(sampler: &Sampler, name: Option<&str>) -> Option<RecordingView> {
    sampler.recording().map(|p| RecordingView {
        name: name.unwrap_or_default().to_owned(),
        frames: p.frames,
        bytes: p.bytes,
        dropped: p.dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_says_elevated_and_paused() {
        assert_eq!(window_title(false, false), "open-task");
        assert_eq!(
            window_title(true, true),
            "open-task (Administrator) (paused)"
        );
    }

    #[test]
    fn theme_preference_follows_the_system_only_when_asked() {
        assert!(ThemePreference::System.dark(true));
        assert!(!ThemePreference::System.dark(false));
        assert!(ThemePreference::Dark.dark(false));
        assert!(!ThemePreference::Light.dark(true));
    }
}
