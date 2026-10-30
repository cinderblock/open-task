//! The function table and the state behind it.
//!
//! The plugin has one window, so its state is a thread-local on the UI thread
//! ([`UI`]). The host is copied out of it before every call into the host, so no
//! borrow is held while the host runs, and the host may call straight back in.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use ot_shell_abi::{self as abi, event};

use crate::canvas::Canvas;
use crate::dialogs;

pub struct Ui {
    pub host: abi::Host,
    pub window: gtk::Window,
    pub canvas: Canvas,
    main_loop: glib::MainLoop,
    timers: HashMap<u32, glib::SourceId>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// The loop is running: a wake can be delivered.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// A wake came before the loop ran; deliver it once it does.
static PENDING_WAKE: AtomicBool = AtomicBool::new(false);
/// A wake is on its way to the UI thread; more are folded into it.
static WAKE_QUEUED: AtomicBool = AtomicBool::new(false);

/// The host, copied out so no borrow is held while it runs.
pub fn host() -> Option<abi::Host> {
    UI.with(|ui| ui.borrow().as_ref().map(|u| u.host))
}

/// Something from the plugin's state, if the window is up.
pub fn with_ui<R>(f: impl FnOnce(&Ui) -> R) -> Option<R> {
    UI.with(|ui| ui.borrow().as_ref().map(f))
}

/// Report an input event to the host.
pub fn send(ev: &abi::Event) {
    if let Some(h) = host() {
        // SAFETY: the host's callback, on the UI thread, with an event that
        // outlives the call.
        unsafe { (h.event)(h.ctx, ev) };
    }
}

pub fn log(level: u32, message: &str) {
    if let Some(h) = host() {
        // SAFETY: the host's callback; the message outlives the call.
        unsafe { (h.log)(h.ctx, level, message.as_ptr().cast(), message.len()) };
    }
}

/// # Safety
/// `p` is NULL or points at `len` bytes.
pub unsafe fn text(p: *const c_char, len: usize) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: as the caller promises.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn write_error(error: *mut c_char, cap: usize, message: &str) {
    if error.is_null() || cap == 0 {
        return;
    }
    let n = message.len().min(cap - 1);
    // SAFETY: the host gave `cap` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(message.as_ptr(), error.cast::<u8>(), n);
        *error.add(n) = 0;
    }
}

unsafe extern "C" fn toolkit() -> *const c_char {
    // The version this process runs, built once and kept.
    static NAME: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::ffi::CString::new(format!(
            "GTK {}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        ))
        .unwrap_or_default()
    })
    .as_ptr()
}

unsafe extern "C" fn run(
    host: *const abi::Host,
    startup: *const abi::Startup,
    error: *mut c_char,
    error_cap: usize,
) -> i32 {
    // SAFETY: the host passes both for the call.
    let (Some(host), Some(startup)) = (unsafe { host.as_ref() }, unsafe { startup.as_ref() })
    else {
        write_error(error, error_cap, "no host table");
        return 2;
    };
    // SAFETY: strings the host passes with their lengths.
    let app_id = unsafe { text(startup.app_id, startup.app_id_len) };
    let title = unsafe { text(startup.title, startup.title_len) };
    // Wayland names the window's app by the program name, so set it first.
    glib::set_prgname(Some(app_id.as_str()));
    glib::set_application_name("open-task");
    if let Err(e) = gtk::init() {
        write_error(error, error_cap, &format!("GTK could not start: {e}"));
        return 1;
    }
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(startup.dark);
        // The desktop's animation setting, which GTK reads from it (GNOME's
        // enable-animations, through the settings portal or XSETTINGS).
        settings.connect_gtk_enable_animations_notify(|s| {
            send_animations(s.is_gtk_enable_animations());
        });
    }

    let window = gtk::Window::new();
    window.set_title(Some(&title));
    window.set_icon_name(Some(&app_id));
    let (w, h) = (startup.width, startup.height);
    if w > 0.0 && h > 0.0 {
        window.set_default_size(w.round() as i32, h.round() as i32);
    }
    if startup.maximized {
        window.maximize();
    }
    let canvas = Canvas::new();
    window.set_child(Some(&canvas));
    canvas.attach_input(&window);
    window.connect_close_request(|_| {
        send(&abi::Event {
            kind: event::CLOSE_REQUEST,
            ..abi::Event::default()
        });
        glib::Propagation::Stop
    });

    let main_loop = glib::MainLoop::new(None, false);
    UI.with(|ui| {
        *ui.borrow_mut() = Some(Ui {
            host: *host,
            window: window.clone(),
            canvas: canvas.clone(),
            main_loop: main_loop.clone(),
            timers: HashMap::new(),
        });
    });
    // SAFETY: the toolkit string is static.
    let name = unsafe { std::ffi::CStr::from_ptr(toolkit()) }
        .to_string_lossy()
        .into_owned();
    log(abi::log::INFO, &format!("{name} window opening"));
    if let Some(settings) = gtk::Settings::default() {
        send_animations(settings.is_gtk_enable_animations());
    }
    RUNNING.store(true, Ordering::Release);
    if PENDING_WAKE.swap(false, Ordering::AcqRel) {
        deliver_wake();
    }
    window.present();
    canvas.grab_focus();
    main_loop.run();
    RUNNING.store(false, Ordering::Release);
    UI.with(|ui| ui.borrow_mut().take());
    0
}

fn send_animations(on: bool) {
    send(&abi::Event {
        kind: event::ANIMATIONS,
        flag: u32::from(on),
        ..abi::Event::default()
    });
}

/// Ask the UI thread to call the host's wake, from any thread.
fn deliver_wake() {
    if WAKE_QUEUED.swap(true, Ordering::AcqRel) {
        return;
    }
    glib::MainContext::default().invoke(|| {
        WAKE_QUEUED.store(false, Ordering::Release);
        if let Some(h) = host() {
            // SAFETY: the host's callback, on the UI thread.
            unsafe { (h.wake)(h.ctx) };
        }
    });
}

unsafe extern "C" fn wake() {
    if RUNNING.load(Ordering::Acquire) {
        deliver_wake();
    } else {
        PENDING_WAKE.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn invalidate() {
    with_ui(|ui| ui.canvas.queue_draw());
}

unsafe extern "C" fn set_title(title: *const c_char, len: usize) {
    // SAFETY: the host passes `len` bytes.
    let title = unsafe { text(title, len) };
    with_ui(|ui| ui.window.set_title(Some(&title)));
}

unsafe extern "C" fn set_cursor(cursor: u32) {
    let name = match cursor {
        abi::cursor::RESIZE_COLUMN => "col-resize",
        abi::cursor::TEXT => "text",
        abi::cursor::CROSSHAIR => "crosshair",
        _ => "default",
    };
    with_ui(|ui| ui.canvas.set_cursor_from_name(Some(name)));
}

unsafe extern "C" fn set_keep_above(on: bool) {
    // GTK 4 dropped "keep above"; window managers offer it on the title bar's
    // menu instead.
    if on {
        log(
            abi::log::INFO,
            "GTK 4 cannot keep a window on top; use the window manager's menu",
        );
    }
}

unsafe extern "C" fn set_dark(dark: bool) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(dark);
    }
}

unsafe extern "C" fn present() {
    with_ui(|ui| {
        ui.window.set_visible(true);
        ui.window.unminimize();
        ui.window.present();
    });
}

unsafe extern "C" fn minimize() {
    with_ui(|ui| ui.window.minimize());
}

unsafe extern "C" fn hide() {
    with_ui(|ui| ui.window.set_visible(false));
}

unsafe extern "C" fn close() {
    let parts = with_ui(|ui| (ui.window.clone(), ui.main_loop.clone()));
    if let Some((window, main_loop)) = parts {
        window.destroy();
        main_loop.quit();
    }
}

unsafe extern "C" fn set_timer(id: u32, ms: u32) {
    let source =
        glib::timeout_add_local_once(std::time::Duration::from_millis(u64::from(ms)), move || {
            UI.with(|ui| {
                if let Some(u) = ui.borrow_mut().as_mut() {
                    u.timers.remove(&id);
                }
            });
            if let Some(h) = host() {
                // SAFETY: the host's callback, on the UI thread.
                unsafe { (h.timer)(h.ctx, id) };
            }
        });
    UI.with(|ui| {
        if let Some(u) = ui.borrow_mut().as_mut() {
            if let Some(old) = u.timers.insert(id, source) {
                old.remove();
            }
        }
    });
}

unsafe extern "C" fn show_menu(
    token: u64,
    x: f32,
    y: f32,
    items: *const abi::MenuItem,
    count: usize,
) {
    let items = if items.is_null() {
        &[][..]
    } else {
        // SAFETY: the host passes `count` items for the call.
        unsafe { std::slice::from_raw_parts(items, count) }
    };
    dialogs::menu(token, x, y, items);
}

unsafe extern "C" fn message(kind: u32, text_ptr: *const c_char, len: usize) {
    // SAFETY: the host passes `len` bytes.
    let t = unsafe { text(text_ptr, len) };
    dialogs::message(kind, &t);
}

unsafe extern "C" fn confirm(token: u64, question: *const c_char, len: usize) {
    // SAFETY: the host passes `len` bytes.
    let q = unsafe { text(question, len) };
    dialogs::confirm(token, &q);
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn save_file(
    token: u64,
    title: *const c_char,
    title_len: usize,
    default_name: *const c_char,
    default_name_len: usize,
    filter_name: *const c_char,
    filter_name_len: usize,
    pattern: *const c_char,
    pattern_len: usize,
) {
    // SAFETY: the host passes each string with its length.
    let (title, name, filter, pattern) = unsafe {
        (
            text(title, title_len),
            text(default_name, default_name_len),
            text(filter_name, filter_name_len),
            text(pattern, pattern_len),
        )
    };
    dialogs::save_file(token, &title, &name, &filter, &pattern);
}

unsafe extern "C" fn run_dialog(token: u64, initial: *const c_char, len: usize) {
    // SAFETY: the host passes `len` bytes.
    let initial = unsafe { text(initial, len) };
    dialogs::run(token, &initial);
}

unsafe extern "C" fn copy_text(t: *const c_char, len: usize) {
    // SAFETY: the host passes `len` bytes.
    let t = unsafe { text(t, len) };
    with_ui(|ui| WidgetExt::clipboard(&ui.canvas).set_text(&t));
}

// No offscreen context: GPU chart drawing is Qt's for now.
unsafe extern "C" fn gl_make_current() -> bool {
    false
}

unsafe extern "C" fn gl_proc(_: *const std::ffi::c_char) -> *const std::ffi::c_void {
    std::ptr::null()
}

unsafe extern "C" fn features() -> u32 {
    // No keep-above: GTK 4 has no way to ask for it.
    abi::feature::ANIMATIONS
}

pub static PLUGIN: abi::Plugin = abi::Plugin {
    abi_version: abi::ABI_VERSION,
    struct_size: size_of::<abi::Plugin>() as u32,
    toolkit,
    run,
    wake,
    invalidate,
    set_title,
    set_cursor,
    set_keep_above,
    set_dark,
    present,
    minimize,
    hide,
    close,
    set_timer,
    show_menu,
    message,
    confirm,
    save_file,
    run_dialog,
    copy_text,
    features,
    gl_make_current,
    gl_proc,
};
