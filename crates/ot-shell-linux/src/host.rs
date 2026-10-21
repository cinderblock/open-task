//! The host's side of the plugin ABI: the callbacks a plugin makes, turned into
//! view events for [`Shell`], and what the shell reports turned into calls on
//! the plugin.
//!
//! Everything runs on the plugin's UI thread. The state is borrowed only while
//! the shell works; every call into the plugin happens after the borrow ends, so
//! a plugin that calls back from inside one of its functions (a toolkit emitting
//! a signal as a window closes) finds the state free.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

use ot_core::SamplerConfig;
use ot_paint::{DisplayList, Point, Size};
use ot_probe::SystemProbe;
use ot_shell_abi::{self as abi, button, event, menu, modifier, notice, DisplayList as AbiList};
use ot_shell_core::{
    theme_for, Ask, CloseOutcome, Confirmed, HostEffect, MenuNode, NoticeKind, Out, Shell,
    ShellOptions, Start, ThemePreference,
};
use ot_ui::{
    AnimationSource, Capabilities, Cursor, MouseButton, PathRenderer, TaskManager, UiEvent,
};

use crate::charts::{self, Charts};
use crate::desktop::{self, LinuxPlatform};
use crate::frame_stats::FrameLog;
use crate::keys;
use crate::loader::{self, Embedded};
use crate::prefs::FilePrefs;
use crate::select::{self, Toolkit};
use crate::RunError;

/// Scheduled update checks: first shortly after start, then hourly to see
/// whether a day has passed.
const TIMER_UPDATE: u32 = 1;
const FIRST_CHECK_MS: u32 = 10_000;
const CHECK_TICK_MS: u32 = 3_600_000;
/// Re-reads the Connections page's list while it is showing.
const TIMER_REFRESH: u32 = 2;

/// The reverse-DNS name the desktop knows the app by: its `.desktop` file and
/// the compositor's window grouping.
pub const APP_ID: &str = "io.github.cinderblock.open-task";
const INITIAL_SIZE: (f32, f32) = (1180.0, 760.0);

/// The plugin being run, for the waker, which other threads call.
static PLUGIN: AtomicPtr<abi::Plugin> = AtomicPtr::new(std::ptr::null_mut());

struct State {
    shell: Shell,
    dl: DisplayList,
    /// The charts' images, when the CPU draws them.
    charts: Charts,
    /// The drawing area, in DIPs, as last reported.
    size: Size,
    /// The pointer as last set on the window.
    cursor: Cursor,
    next_token: u64,
    /// What a question goes on to do when answered Yes, by token.
    confirms: HashMap<u64, Confirmed>,
    theme_pref: ThemePreference,
    dark: bool,
    /// With `OT_FRAME_STATS` set: what the frames cost.
    frame_log: Option<FrameLog>,
}

struct Ctx {
    plugin: Cell<&'static abi::Plugin>,
    /// What the plugin is written for.
    toolkit: Cell<Toolkit>,
    state: RefCell<State>,
    /// The update timer is set once the loop runs.
    armed: Cell<bool>,
}

/// Open the window with the first toolkit that will, and run until it closes.
///
/// # Errors
/// No toolkit could be loaded or connect to the display; each one's reason.
pub fn run(
    embedded: &[Embedded],
    asked: Option<Toolkit>,
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    options: &ShellOptions,
) -> Result<(), RunError> {
    let desktop_env = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let order = select::order(asked, desktop_env.as_deref());
    let mut probe = Some(probe);
    let mut ctx: Option<Box<Ctx>> = None;
    let mut failures = Vec::new();
    for toolkit in order {
        let Some(source) = loader::source(toolkit, embedded) else {
            failures.push(format!("{}: not in this build", toolkit.name()));
            continue;
        };
        let loaded = match source.load(toolkit) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(toolkit = toolkit.name(), error = %e, "the toolkit plugin could not be loaded");
                failures.push(format!("{}: {e}", toolkit.name()));
                continue;
            }
        };
        PLUGIN.store(
            std::ptr::from_ref(loaded.plugin).cast_mut(),
            Ordering::Release,
        );
        // SAFETY: the toolkit string is static in the plugin.
        let name = unsafe { cstr((loaded.plugin.toolkit)()) };
        tracing::info!(toolkit = %name, "toolkit plugin loaded");

        let cx = if let Some(cx) = &ctx {
            cx.plugin.set(loaded.plugin);
            cx.toolkit.set(toolkit);
            if let Ok(mut st) = cx.state.try_borrow_mut() {
                st.shell
                    .app_mut()
                    .set_capabilities(capabilities(toolkit, 0));
            }
            cx
        } else {
            let dark = options.theme.dark(desktop::prefers_dark());
            let shell = start_shell(
                probe.take().ok_or(RunError::Spent)?,
                config,
                options,
                dark,
                capabilities(toolkit, 0),
                Box::new(FilePrefs::standard()),
            )?;
            ctx.insert(Box::new(Ctx {
                plugin: Cell::new(loaded.plugin),
                toolkit: Cell::new(toolkit),
                state: RefCell::new(State {
                    dark,
                    shell,
                    dl: DisplayList::new(),
                    charts: Charts::from_env(),
                    size: Size::new(INITIAL_SIZE.0, INITIAL_SIZE.1),
                    cursor: Cursor::Arrow,
                    next_token: 1,
                    confirms: HashMap::new(),
                    theme_pref: options.theme,
                    frame_log: FrameLog::from_env(),
                }),
                armed: Cell::new(false),
            }))
        };
        match run_plugin(cx, loaded.plugin) {
            Ok(()) => {
                if let Ok(mut st) = cx.state.try_borrow_mut() {
                    st.shell.stop_feed();
                }
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(toolkit = toolkit.name(), error = %e, "the toolkit could not start");
                failures.push(format!("{}: {e}", toolkit.name()));
            }
        }
    }
    Err(RunError::NoToolkit(failures))
}

/// What the shell can do under `toolkit`, whose plugin reports `features`
/// ([`abi::feature`] bits; 0 until its window is up).
fn capabilities(toolkit: Toolkit, features: u32) -> Capabilities {
    Capabilities {
        system: "Linux",
        animations: if features & abi::feature::ANIMATIONS != 0 {
            AnimationSource::Desktop
        } else {
            AnimationSource::Unknown
        },
        chart_drawings: charts::OFFERED,
        paths: match toolkit {
            Toolkit::Gtk => PathRenderer::Cairo,
            Toolkit::Qt => PathRenderer::QPainter,
        },
        keep_above: features & abi::feature::KEEP_ABOVE != 0,
        // Not on Linux yet: the tray, Switch to, the updater, a copy as root.
        tray: false,
        switch_to: false,
        updates: false,
        run_elevated: false,
    }
}

fn start_shell(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    options: &ShellOptions,
    dark: bool,
    capabilities: Capabilities,
    prefs: Box<dyn ot_shell_core::Prefs>,
) -> Result<Shell, RunError> {
    let waker = Arc::new(|| {
        let p = PLUGIN.load(Ordering::Acquire);
        // SAFETY: a plugin table, once stored, lives for the process; `wake` is
        // the one function any thread may call.
        if let Some(p) = unsafe { p.as_ref() } {
            unsafe { (p.wake)() };
        }
    });
    Ok(Shell::start(Start {
        probe,
        config,
        options,
        theme: theme_for(dark),
        backdrop: false,
        system_animations: true,
        capabilities,
        task_manager: TaskManager {
            replacement: ot_ui::Replacement::Unavailable,
            install: None,
            pending: false,
        },
        installation: ot_update::installation(),
        prefs,
        platform: Arc::new(LinuxPlatform),
        waker,
    })?)
}

fn run_plugin(cx: &Ctx, plugin: &'static abi::Plugin) -> Result<(), String> {
    let host = abi::Host {
        ctx: std::ptr::from_ref(cx).cast_mut().cast(),
        event: on_event,
        paint: on_paint,
        animating: on_animating,
        wake: on_wake,
        timer: on_timer,
        menu_chosen: on_menu_chosen,
        confirmed: on_confirmed,
        file_chosen: on_file_chosen,
        run_entered: on_run_entered,
        log: on_log,
    };
    let (title, dark) = {
        let st = cx.state.borrow();
        (st.shell.title().to_owned(), st.dark)
    };
    let exe = CString::new(std::env::current_exe().map_or_else(
        |_| b"open-task".to_vec(),
        |p| p.as_os_str().as_encoded_bytes().to_vec(),
    ))
    .unwrap_or_default();
    let mut argv = [exe.as_ptr().cast_mut(), std::ptr::null_mut()];
    let startup = abi::Startup {
        title: title.as_ptr().cast(),
        title_len: title.len(),
        app_id: APP_ID.as_ptr().cast(),
        app_id_len: APP_ID.len(),
        width: INITIAL_SIZE.0,
        height: INITIAL_SIZE.1,
        maximized: false,
        dark,
        argc: 1,
        argv: argv.as_mut_ptr(),
    };
    let mut error = [0u8; 512];
    // SAFETY: the host table and startup outlive the call, which runs the main
    // loop to its end; the error buffer's size is given.
    let r = unsafe {
        (plugin.run)(
            &raw const host,
            &raw const startup,
            error.as_mut_ptr().cast(),
            error.len(),
        )
    };
    if r == 0 {
        Ok(())
    } else {
        let end = error.iter().position(|&b| b == 0).unwrap_or(error.len());
        let message = String::from_utf8_lossy(&error[..end]).into_owned();
        Err(if message.is_empty() {
            format!("failed with code {r}")
        } else {
            message
        })
    }
}

/// # Safety
/// `p` is NULL or a NUL-terminated string.
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: as the caller promises.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// # Safety
/// `p` is NULL or points at `len` bytes.
unsafe fn string(p: *const c_char, len: usize) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Run a callback's body, never letting a panic unwind into the plugin.
fn guard(ctx: *mut c_void, f: impl FnOnce(&Ctx)) {
    // SAFETY: the host passed this pointer to the plugin, and it outlives `run`.
    let Some(cx) = (unsafe { ctx.cast::<Ctx>().as_ref() }) else {
        return;
    };
    if catch_unwind(AssertUnwindSafe(|| f(cx))).is_err() {
        tracing::error!("panic in a host callback");
        std::process::abort();
    }
}

unsafe extern "C" fn on_event(ctx: *mut c_void, ev: *const abi::Event) {
    // SAFETY: the plugin passes a valid event for the call.
    let Some(ev) = (unsafe { ev.as_ref() }).copied() else {
        return;
    };
    guard(ctx, |cx| cx.event(&ev));
}

unsafe extern "C" fn on_paint(ctx: *mut c_void, scale: f32, out: *mut AbiList) {
    // SAFETY: the plugin passes somewhere to write the list.
    let Some(out) = (unsafe { out.as_mut() }) else {
        return;
    };
    *out = AbiList::EMPTY;
    guard(ctx, |cx| {
        let Ok(mut st) = cx.state.try_borrow_mut() else {
            return;
        };
        let st = &mut *st;
        let painting = std::time::Instant::now();
        st.shell.app_mut().paint(&mut st.dl);
        let painted = painting.elapsed();
        let charting = std::time::Instant::now();
        let app = st.shell.app();
        let mode = st
            .charts
            .mode(&app.capabilities(), app.settings().chart_drawing);
        let rasters = st.charts.prepare(&st.dl, mode, scale, st.size);
        if let Some(log) = st.frame_log.as_mut() {
            log.add(app.last_paint(), painted, charting.elapsed());
        }
        // The list and the images live in the state, unchanged until the plugin
        // calls in again.
        *out = AbiList::of(&st.dl, rasters);
    });
}

unsafe extern "C" fn on_animating(ctx: *mut c_void) -> bool {
    let mut animating = false;
    guard(ctx, |cx| {
        animating = cx
            .state
            .try_borrow()
            .is_ok_and(|st| st.shell.app().animating());
    });
    animating
}

unsafe extern "C" fn on_wake(ctx: *mut c_void) {
    guard(ctx, Ctx::wake);
}

unsafe extern "C" fn on_timer(ctx: *mut c_void, id: u32) {
    guard(ctx, |cx| cx.timer(id));
}

unsafe extern "C" fn on_menu_chosen(ctx: *mut c_void, _token: u64, chosen: bool, id: u64) {
    guard(ctx, |cx| {
        let id = usize::try_from(id).ok().filter(|_| chosen);
        // The toolkit's motion and leave events say where the pointer went.
        cx.with_shell(|shell, outs| shell.menu_closed(id, None, outs));
    });
}

unsafe extern "C" fn on_confirmed(ctx: *mut c_void, token: u64, yes: bool) {
    guard(ctx, |cx| {
        let then = cx
            .state
            .try_borrow_mut()
            .ok()
            .and_then(|mut st| st.confirms.remove(&token));
        if let (true, Some(then)) = (yes, then) {
            cx.with_shell(|shell, outs| shell.confirmed(then, outs));
        }
    });
}

unsafe extern "C" fn on_file_chosen(
    ctx: *mut c_void,
    _token: u64,
    path: *const c_char,
    len: usize,
) {
    // SAFETY: the plugin passes NULL or `len` bytes.
    let Some(path) = (unsafe { string(path, len) }) else {
        return;
    };
    guard(ctx, |cx| {
        cx.with_shell(|shell, outs| shell.record_to(&path, outs));
    });
}

unsafe extern "C" fn on_run_entered(
    ctx: *mut c_void,
    _token: u64,
    command: *const c_char,
    len: usize,
    elevated: bool,
) {
    // SAFETY: the plugin passes NULL or `len` bytes.
    let Some(command) = (unsafe { string(command, len) }) else {
        return;
    };
    guard(ctx, |cx| {
        if let Ok(mut st) = cx.state.try_borrow_mut() {
            st.shell.set_last_run(&command);
        }
        if let Err(e) = desktop::run_command(&command, elevated) {
            cx.message(
                NoticeKind::Warning,
                &format!("Could not start {command}.\n\n{e}"),
            );
        }
    });
}

unsafe extern "C" fn on_log(_ctx: *mut c_void, level: u32, message: *const c_char, len: usize) {
    // SAFETY: the plugin passes `len` bytes.
    let Some(message) = (unsafe { string(message, len) }) else {
        return;
    };
    match level {
        abi::log::ERROR => tracing::error!(target: "plugin", "{message}"),
        abi::log::WARN => tracing::warn!(target: "plugin", "{message}"),
        abi::log::INFO => tracing::info!(target: "plugin", "{message}"),
        _ => tracing::debug!(target: "plugin", "{message}"),
    }
}

impl Ctx {
    fn plugin(&self) -> &'static abi::Plugin {
        self.plugin.get()
    }

    /// Let the shell work with the state borrowed, then carry out what it
    /// reported with the state free.
    fn with_shell(&self, f: impl FnOnce(&mut Shell, &mut Vec<Out>)) {
        let mut outs = Vec::new();
        {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                tracing::warn!("host state busy; a callback was dropped");
                return;
            };
            f(&mut st.shell, &mut outs);
        }
        self.carry_out(outs);
    }

    fn event(&self, ev: &abi::Event) {
        let at = Point::new(ev.x, ev.y);
        let ui = match ev.kind {
            event::RESIZE => {
                if let Ok(mut st) = self.state.try_borrow_mut() {
                    st.size = Size::new(ev.x, ev.y);
                }
                Some(UiEvent::Resize(Size::new(ev.x, ev.y)))
            }
            event::MOUSE_MOVE => Some(UiEvent::MouseMove(at)),
            event::MOUSE_LEAVE => Some(UiEvent::MouseLeave),
            event::MOUSE_DOWN | event::MOUSE_UP => {
                let button = match ev.button {
                    button::LEFT => MouseButton::Left,
                    button::RIGHT => MouseButton::Right,
                    _ => return,
                };
                Some(if ev.kind == event::MOUSE_DOWN {
                    UiEvent::MouseDown { at, button }
                } else {
                    UiEvent::MouseUp { at, button }
                })
            }
            event::WHEEL => {
                // Shift turns a vertical wheel sideways, as on Windows.
                let shift = ev.modifiers & modifier::SHIFT != 0;
                if ev.dy != 0.0 {
                    self.dispatch(UiEvent::Wheel {
                        at,
                        lines: ev.dy,
                        horizontal: shift,
                    });
                }
                (ev.dx != 0.0).then_some(UiEvent::Wheel {
                    at,
                    lines: ev.dx,
                    horizontal: true,
                })
            }
            event::KEY => keys::map(ev),
            event::TEXT => char::from_u32(ev.codepoint)
                .filter(|c| !c.is_control())
                .map(UiEvent::Char),
            event::CONTEXT_MENU => Some(UiEvent::ContextMenu {
                at: (ev.flag == 1).then_some(at),
            }),
            event::CLOSE_REQUEST => {
                self.close_window();
                None
            }
            event::THEME => {
                self.follow_theme(ev.flag == 1);
                None
            }
            event::ANIMATIONS => {
                self.with_shell(|shell, outs| {
                    shell.app_mut().set_system_animations(ev.flag == 1);
                    outs.push(Out::Repaint);
                });
                None
            }
            // Scale comes with each frame (the plugin draws in device pixels);
            // focus and visibility are not used yet.
            _ => None,
        };
        if let Some(ui) = ui {
            self.dispatch(ui);
        }
    }

    fn dispatch(&self, ev: UiEvent) {
        self.with_shell(|shell, outs| shell.handle(ev, outs));
        // The pointer follows what is under it.
        let cursor = {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            let c = st.shell.app().cursor();
            (c != st.cursor).then(|| {
                st.cursor = c;
                c
            })
        };
        if let Some(c) = cursor {
            let id = match c {
                Cursor::Arrow => abi::cursor::ARROW,
                Cursor::ResizeColumn => abi::cursor::RESIZE_COLUMN,
                Cursor::Text => abi::cursor::TEXT,
            };
            // SAFETY: a plugin function, called on the UI thread.
            unsafe { (self.plugin().set_cursor)(id) };
        }
    }

    fn follow_theme(&self, dark: bool) {
        let changed = {
            let Ok(mut st) = self.state.try_borrow_mut() else {
                return;
            };
            if st.theme_pref != ThemePreference::System || st.dark == dark {
                return;
            }
            st.dark = dark;
            st.shell.app_mut().set_theme(theme_for(dark));
            true
        };
        if changed {
            // SAFETY: plugin functions, called on the UI thread.
            unsafe {
                (self.plugin().set_dark)(dark);
                (self.plugin().invalidate)();
            }
        }
    }

    fn wake(&self) {
        if !self.armed.replace(true) {
            // SAFETY: a plugin function, called on the UI thread, the window up.
            let features = unsafe { (self.plugin().features)() };
            let caps = capabilities(self.toolkit.get(), features);
            if let Ok(mut st) = self.state.try_borrow_mut() {
                st.shell.app_mut().set_capabilities(caps);
            }
            // Scheduled checks only where the updater works; where it does not,
            // its switches are not on the page to turn them off either.
            if caps.updates {
                // SAFETY: a plugin function, called on the UI thread.
                unsafe { (self.plugin().set_timer)(TIMER_UPDATE, FIRST_CHECK_MS) };
            }
            let settings = self.state.try_borrow().map(|st| st.shell.app().settings());
            if let Ok(settings) = settings {
                self.carry_out(vec![Out::Settings(settings)]);
            }
        }
        self.with_shell(Shell::drain);
    }

    fn timer(&self, id: u32) {
        match id {
            TIMER_UPDATE => {
                // SAFETY: a plugin function, called on the UI thread.
                unsafe { (self.plugin().set_timer)(TIMER_UPDATE, CHECK_TICK_MS) };
                if let Ok(st) = self.state.try_borrow() {
                    st.shell.update_tick();
                }
            }
            TIMER_REFRESH => {
                if let Ok(st) = self.state.try_borrow() {
                    st.shell.refresh();
                }
            }
            _ => {}
        }
    }

    /// The window's close button, or the shell finishing a close: save where
    /// things stand and close, or hide while an update installs.
    fn close_window(&self) {
        let outcome = match self.state.try_borrow_mut() {
            Ok(mut st) => st.shell.close(),
            Err(_) => CloseOutcome::Close,
        };
        // SAFETY: plugin functions, called on the UI thread.
        unsafe {
            match outcome {
                CloseOutcome::Close => (self.plugin().close)(),
                CloseOutcome::Hide => (self.plugin().hide)(),
            }
        }
    }

    fn message(&self, kind: NoticeKind, text: &str) {
        let kind = match kind {
            NoticeKind::Info => notice::INFO,
            NoticeKind::Warning => notice::WARNING,
            NoticeKind::Error => notice::ERROR,
        };
        // SAFETY: a plugin function on the UI thread; the text outlives the call.
        unsafe { (self.plugin().message)(kind, text.as_ptr().cast(), text.len()) };
    }

    fn token(&self) -> u64 {
        self.state.try_borrow_mut().map_or(0, |mut st| {
            let t = st.next_token;
            st.next_token += 1;
            t
        })
    }

    /// Carry out what the shell reported, in order. An answer that comes back
    /// at once (none do with the ABI's rules, but a plugin may) is carried out
    /// by its own callback.
    fn carry_out(&self, outs: Vec<Out>) {
        let p = self.plugin();
        let mut queue = VecDeque::from(outs);
        while let Some(out) = queue.pop_front() {
            // SAFETY (every block below): plugin functions, called on the UI
            // thread with arguments that outlive the call.
            match out {
                Out::Repaint => unsafe { (p.invalidate)() },
                Out::Title(title) => unsafe { (p.set_title)(title.as_ptr().cast(), title.len()) },
                Out::Notice(n) => self.message(n.kind, &n.text),
                Out::Ask(ask) => self.ask(ask),
                Out::Do(effect) => self.host_effect(effect),
                Out::Settings(s) => unsafe { (p.set_keep_above)(s.always_on_top) },
                // The tray is not there yet.
                Out::Meters { .. } => {}
                Out::RefreshIn(after) => {
                    let ms = u32::try_from(after.as_millis()).unwrap_or(u32::MAX);
                    unsafe { (p.set_timer)(TIMER_REFRESH, ms) };
                }
                Out::Close => self.close_window(),
            }
        }
    }

    fn ask(&self, ask: Ask) {
        let p = self.plugin();
        let token = self.token();
        match ask {
            Ask::Menu { at, nodes } => {
                let mut labels = Vec::new();
                let mut flat = Vec::new();
                flatten(&nodes, &mut flat, &mut labels);
                let items: Vec<abi::MenuItem> = flat
                    .iter()
                    .map(|f| abi::MenuItem {
                        kind: f.kind,
                        enabled: f.enabled,
                        checked: f.checked,
                        id: f.id,
                        label: labels[f.label].as_ptr().cast(),
                        label_len: labels[f.label].len(),
                        icon: f.icon,
                    })
                    .collect();
                // SAFETY: the items and their labels outlive the call.
                unsafe { (p.show_menu)(token, at.x, at.y, items.as_ptr(), items.len()) };
            }
            Ask::Confirm { question, then } => {
                if let Ok(mut st) = self.state.try_borrow_mut() {
                    st.confirms.insert(token, then);
                }
                // SAFETY: the question outlives the call.
                unsafe { (p.confirm)(token, question.as_ptr().cast(), question.len()) };
            }
            Ask::RecordPath => {
                let (title, name, filter, pattern) = (
                    "Record to",
                    "open-task.otrec",
                    "Flight Recorder (*.otrec)",
                    "*.otrec",
                );
                // SAFETY: the strings are static.
                unsafe {
                    (p.save_file)(
                        token,
                        title.as_ptr().cast(),
                        title.len(),
                        name.as_ptr().cast(),
                        name.len(),
                        filter.as_ptr().cast(),
                        filter.len(),
                        pattern.as_ptr().cast(),
                        pattern.len(),
                    );
                }
            }
            Ask::Run { initial } => {
                // SAFETY: the text outlives the call.
                unsafe { (p.run_dialog)(token, initial.as_ptr().cast(), initial.len()) };
            }
        }
    }

    fn host_effect(&self, effect: HostEffect) {
        let failed = |what: &str, e: String| {
            self.message(NoticeKind::Warning, &format!("Could not {what}.\n\n{e}"));
        };
        match effect {
            HostEffect::OpenFileLocation(path) => {
                if let Err(e) = desktop::reveal(&path) {
                    failed("open the file's location", e);
                }
            }
            HostEffect::OpenUrl(url) => {
                if let Err(e) = desktop::open(&url) {
                    failed("open the browser", e);
                }
            }
            HostEffect::Properties(path) => {
                if let Err(e) = desktop::properties(&path) {
                    failed("show the file's properties", e);
                }
            }
            HostEffect::OpenInstallLocation { location, .. } => match location {
                Some(dir) => {
                    if let Err(e) = desktop::open(&dir) {
                        failed("open the folder", e);
                    }
                }
                None => self.message(
                    NoticeKind::Warning,
                    "The program's folder could not be worked out.",
                ),
            },
            // SAFETY: plugin functions on the UI thread; the text outlives the call.
            HostEffect::Minimize => unsafe { (self.plugin().minimize)() },
            HostEffect::CopyText(text) => unsafe {
                (self.plugin().copy_text)(text.as_ptr().cast(), text.len());
            },
            HostEffect::SwitchTo(_) => {
                tracing::info!("switching to another app's window is not available on Linux yet");
            }
            HostEffect::RunAsAdministrator
            | HostEffect::PickWindow
            | HostEffect::OpenServices
            | HostEffect::ReplaceTaskManager(_) => {
                self.message(NoticeKind::Info, "This is not available on Linux yet.");
            }
        }
    }
}

/// One menu line, flattened for the ABI; `label` indexes the labels.
#[derive(Debug, Clone, Copy)]
struct Flat {
    kind: u32,
    enabled: bool,
    checked: bool,
    id: u64,
    label: usize,
    icon: u32,
}

impl Flat {
    /// A line of `kind`, enabled, with no id, label or icon yet.
    fn of(kind: u32) -> Self {
        Self {
            kind,
            enabled: true,
            checked: false,
            id: 0,
            label: 0,
            icon: abi::ICON_NONE,
        }
    }
}

/// `f`, labelled `label`.
fn push(f: Flat, label: &str, out: &mut Vec<Flat>, labels: &mut Vec<String>) {
    labels.push(label.to_owned());
    out.push(Flat {
        label: labels.len() - 1,
        ..f
    });
}

/// An icon as the ABI numbers it.
fn icon_index(icon: Option<ot_paint::Icon>) -> u32 {
    icon.map_or(abi::ICON_NONE, |i| i as u32)
}

fn flatten(nodes: &[MenuNode], out: &mut Vec<Flat>, labels: &mut Vec<String>) {
    for node in nodes {
        match node {
            MenuNode::Item {
                id,
                label,
                enabled,
                checked,
                icon,
            } => {
                let item = Flat {
                    enabled: *enabled,
                    checked: *checked,
                    id: *id as u64,
                    // The toolkits show a check where the icon would be, or
                    // frame the icon instead: a checked item shows its check.
                    icon: icon_index(icon.filter(|_| !checked)),
                    ..Flat::of(menu::ITEM)
                };
                push(item, label, out, labels);
            }
            MenuNode::Submenu {
                label,
                enabled,
                icon,
                children,
            } => {
                let sub = Flat {
                    enabled: *enabled,
                    icon: icon_index(*icon),
                    ..Flat::of(menu::SUBMENU)
                };
                push(sub, label, out, labels);
                flatten(children, out, labels);
                push(Flat::of(menu::SUBMENU_END), "", out, labels);
            }
            MenuNode::Separator => push(Flat::of(menu::SEPARATOR), "", out, labels),
            // The plugins draw no header: its lines go in as items that cannot
            // be chosen, with an id no action has.
            MenuNode::Header(h) => {
                let lines = std::iter::once(&h.title)
                    .chain(&h.lines)
                    .chain(h.path.as_ref());
                let line = Flat {
                    enabled: false,
                    id: u64::MAX,
                    ..Flat::of(menu::ITEM)
                };
                for text in lines {
                    push(line, text, out, labels);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_flatten_with_submenus_bracketed() {
        let nodes = vec![
            MenuNode::Item {
                id: 0,
                label: "End task".into(),
                enabled: true,
                checked: false,
                icon: Some(ot_paint::Icon::EndTask),
            },
            MenuNode::Separator,
            MenuNode::Submenu {
                label: "Set priority".into(),
                enabled: true,
                icon: None,
                children: vec![MenuNode::Item {
                    id: 1,
                    label: "High".into(),
                    enabled: true,
                    checked: true,
                    icon: None,
                }],
            },
        ];
        let (mut flat, mut labels) = (Vec::new(), Vec::new());
        flatten(&nodes, &mut flat, &mut labels);
        let kinds: Vec<u32> = flat.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                menu::ITEM,
                menu::SEPARATOR,
                menu::SUBMENU,
                menu::ITEM,
                menu::SUBMENU_END
            ]
        );
        assert_eq!(flat[0].icon, ot_paint::Icon::EndTask as u32);
        assert_eq!(flat[1].icon, abi::ICON_NONE);
        assert_eq!(labels[flat[3].label], "High");
        assert!(flat[3].checked);
        assert_eq!(flat[3].id, 1);
    }

    /// A plugin with no toolkit: its `run` plays a short session against the
    /// host through the real callbacks and records what the host asks of it.
    mod fake {
        use super::super::*;
        use std::cell::RefCell;

        thread_local! {
            pub static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
            pub static PAINTED: RefCell<usize> = const { RefCell::new(0) };
            /// Each raster of the frame painted: its commands, rect and size.
            pub static RASTERS: RefCell<Vec<abi::Raster>> = const { RefCell::new(Vec::new()) };
        }

        fn note(s: impl Into<String>) {
            CALLS.with(|c| c.borrow_mut().push(s.into()));
        }

        unsafe extern "C" fn toolkit() -> *const c_char {
            c"fake".as_ptr()
        }

        unsafe extern "C" fn run(
            host: *const abi::Host,
            _startup: *const abi::Startup,
            _error: *mut c_char,
            _cap: usize,
        ) -> i32 {
            // SAFETY: the host passes its table for the call.
            let h = unsafe { *host };
            let send = |ev: abi::Event| unsafe { (h.event)(h.ctx, &raw const ev) };
            send(abi::Event {
                kind: event::RESIZE,
                x: 900.0,
                y: 600.0,
                ..abi::Event::default()
            });
            // The desktop has animations off.
            send(abi::Event {
                kind: event::ANIMATIONS,
                flag: 0,
                ..abi::Event::default()
            });
            // The shell flagged its first snapshot as it started; the waker would
            // have brought the host here.
            unsafe { (h.wake)(h.ctx) };
            let mut list = AbiList::EMPTY;
            unsafe { (h.paint)(h.ctx, 2.0, &raw mut list) };
            PAINTED.with(|p| *p.borrow_mut() = list.cmd_count);
            let rasters = if list.rasters.is_null() {
                &[][..]
            } else {
                // SAFETY: as the host left them, valid until the next call in.
                unsafe { std::slice::from_raw_parts(list.rasters, list.raster_count) }
            };
            RASTERS.with(|r| *r.borrow_mut() = rasters.to_vec());
            // Ctrl+N: the Run dialog.
            send(abi::Event {
                kind: event::KEY,
                key: abi::key::CHARACTER,
                codepoint: u32::from('n'),
                modifiers: modifier::CTRL,
                ..abi::Event::default()
            });
            send(abi::Event {
                kind: event::CLOSE_REQUEST,
                ..abi::Event::default()
            });
            0
        }

        unsafe extern "C" fn nothing() {}
        unsafe extern "C" fn invalidate() {
            note("invalidate");
        }
        unsafe extern "C" fn close() {
            note("close");
        }
        unsafe extern "C" fn set_title(_: *const c_char, _: usize) {}
        unsafe extern "C" fn set_cursor(_: u32) {}
        unsafe extern "C" fn flag(_: bool) {}
        unsafe extern "C" fn set_timer(id: u32, _: u32) {
            note(format!("timer {id}"));
        }
        unsafe extern "C" fn show_menu(_: u64, _: f32, _: f32, _: *const abi::MenuItem, _: usize) {
            note("menu");
        }
        unsafe extern "C" fn message(_: u32, _: *const c_char, _: usize) {
            note("message");
        }
        unsafe extern "C" fn confirm(_: u64, _: *const c_char, _: usize) {
            note("confirm");
        }
        #[allow(clippy::too_many_arguments)]
        unsafe extern "C" fn save_file(
            _: u64,
            _: *const c_char,
            _: usize,
            _: *const c_char,
            _: usize,
            _: *const c_char,
            _: usize,
            _: *const c_char,
            _: usize,
        ) {
        }
        unsafe extern "C" fn run_dialog(_: u64, _: *const c_char, _: usize) {
            note("run dialog");
        }
        unsafe extern "C" fn copy_text(_: *const c_char, _: usize) {}
        unsafe extern "C" fn features() -> u32 {
            abi::feature::KEEP_ABOVE | abi::feature::ANIMATIONS
        }

        pub static PLUGIN: abi::Plugin = abi::Plugin {
            abi_version: abi::ABI_VERSION,
            struct_size: size_of::<abi::Plugin>() as u32,
            toolkit,
            run,
            wake: nothing,
            invalidate,
            set_title,
            set_cursor,
            set_keep_above: flag,
            set_dark: flag,
            present: nothing,
            minimize: nothing,
            hide: nothing,
            close,
            set_timer,
            show_menu,
            message,
            confirm,
            save_file,
            run_dialog,
            copy_text,
            features,
        };
    }

    #[test]
    fn a_session_runs_through_the_abi() {
        let probe = match ot_probe::PlatformProbe::new() {
            Ok(p) => Box::new(p),
            Err(e) => {
                eprintln!("no probe here ({e}); skipping");
                return;
            }
        };
        let dir = std::env::temp_dir().join(format!("ot-host-{}", std::process::id()));
        let options = ShellOptions {
            version: "0.0.0-test",
            ..ShellOptions::default()
        };
        let shell = start_shell(
            probe,
            SamplerConfig {
                interval: std::time::Duration::from_secs(1),
            },
            &options,
            true,
            capabilities(Toolkit::Gtk, 0),
            Box::new(FilePrefs::at(dir.join("settings"))),
        )
        .expect("shell");
        let cx = Ctx {
            plugin: Cell::new(&fake::PLUGIN),
            toolkit: Cell::new(Toolkit::Gtk),
            state: RefCell::new(State {
                shell,
                dl: DisplayList::new(),
                charts: Charts::default(),
                size: Size::new(INITIAL_SIZE.0, INITIAL_SIZE.1),
                cursor: Cursor::Arrow,
                next_token: 1,
                confirms: HashMap::new(),
                theme_pref: ThemePreference::System,
                dark: true,
                frame_log: None,
            }),
            armed: Cell::new(false),
        };
        run_plugin(&cx, &fake::PLUGIN).expect("ran");
        cx.state.borrow_mut().shell.stop_feed();
        // What the plugin said it can do, once its window was up.
        {
            let st = cx.state.borrow();
            let caps = st.shell.app().capabilities();
            assert!(caps.keep_above);
            assert_eq!(caps.animations, AnimationSource::Desktop);
            assert_eq!(caps.paths, PathRenderer::Cairo);
            assert!(!st.shell.app().system_animations());
        }
        // The GPU, the default, is the CPU here: any charts came as images, in
        // order, in whole device pixels at the frame's scale. (The first frame
        // comes before any sample, so it may have none; `charts` tests the
        // images themselves.)
        let rasters = fake::RASTERS.with(|r| r.borrow().clone());
        let painted = fake::PAINTED.with(|p| *p.borrow()) as u32;
        let mut after = 0;
        for r in &rasters {
            assert!(
                after <= r.first && r.first < r.end && r.end <= painted,
                "{rasters:?}"
            );
            after = r.end;
            assert!((r.rect.w * 2.0 - r.width as f32).abs() < 1e-3, "{r:?}");
            assert!((r.rect.h * 2.0 - r.height as f32).abs() < 1e-3, "{r:?}");
        }
        let calls = fake::CALLS.with(|c| c.borrow().clone());
        assert!(
            fake::PAINTED.with(|p| *p.borrow()) > 0,
            "a frame was painted"
        );
        // No updater on Linux yet: no scheduled checks.
        assert!(
            !calls.contains(&format!("timer {TIMER_UPDATE}")),
            "{calls:?}"
        );
        assert!(calls.contains(&"run dialog".to_owned()), "{calls:?}");
        assert_eq!(calls.last().map(String::as_str), Some("close"), "{calls:?}");
        assert!(dir.join("settings").is_file(), "closing saved the layout");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
