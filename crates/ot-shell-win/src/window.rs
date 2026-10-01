//! Win32 window, message loop, and translation of messages into UI events.
//!
//! Every input message becomes a [`UiEvent`] and goes through [`dispatch`], which
//! hands it to the view and carries out whatever [`Effect`] comes back with native
//! APIs: a popup menu, a confirmation box and the kill, a shell verb, replacing Task
//! Manager. Effects run with the window state unborrowed, because menus and message
//! boxes pump messages and those messages re-enter the window procedure.

use std::cell::RefCell;
use std::ffi::c_void;
use std::fmt::Write as _;

use std::sync::Arc;
use std::time::Duration;

use ot_core::{Sampler, SamplerConfig};
use ot_model::ProcessKey;
use ot_paint::{DisplayList, Point, Size};
use ot_probe::{ControlError, PlatformControl, ProcessControl, SystemProbe};
use ot_probe::{CpuSampler, PlatformSampler};
use ot_ui::{
    App, Command, Cursor, Effect, Key, MenuAction, MenuEntry, MouseButton, Page, TaskManager,
    Theme, UiEvent, UpdateAction, UpdateView, ViewMode,
};
use ot_update::{Installation, Updater};
use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, ScreenToClient, ValidateRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DispatchMessageW, GetClientRect, GetMessageW, GetWindowLongPtrW, IsIconic, LoadCursorW,
    LoadImageW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    ShowWindow, TrackPopupMenuEx, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    GWLP_USERDATA, HCURSOR, HICON, HTCLIENT, ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_IBEAM,
    IDC_SIZEWE, IDYES, IMAGE_ICON, LR_DEFAULTCOLOR, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING,
    MB_OK, MB_YESNO, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SIZE_MINIMIZED, SM_CXICON,
    SM_CXSMICON, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOWDEFAULT, SW_SHOWNORMAL,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, WHEEL_DELTA, WM_ACTIVATEAPP,
    WM_APP, WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ENDSESSION,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};

use crate::gfx::Gfx;
use crate::task_manager::{self, Elevated};
use crate::{instance, prefs};
use crate::{ShellError, ShellOptions, ThemePreference};

/// Posted by the sampler thread after each publish.
const WM_APP_SNAPSHOT: u32 = WM_APP + 1;
/// A CPU sample finished on its worker thread; `lparam` is a `Box<SampleOutcome>`.
const WM_APP_SAMPLE: u32 = WM_APP + 2;
/// The updater's status changed; read it with `Updater::status`.
const WM_APP_UPDATE: u32 = WM_APP + 3;
/// The elevated helper that replaces or restores Task Manager finished; `lparam` is
/// a `Box<TaskManagerOutcome>`.
const WM_APP_TASK_MANAGER: u32 = WM_APP + 4;

/// The timer for scheduled update checks: first shortly after start, so it does
/// not compete with the first frames, then hourly to see whether a day has passed.
const TIMER_UPDATE: usize = 1;
const FIRST_CHECK_MS: u32 = 10_000;
const CHECK_TICK_MS: u32 = 3_600_000;
/// How often a scheduled check runs.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 3600);

/// What a sampling thread hands back to the window.
struct SampleOutcome {
    target: ProcessKey,
    name: String,
    result: Result<ot_model::attribution::Attribution, ot_probe::SampleError>,
}

/// What the Task Manager helper's thread hands back to the window.
struct TaskManagerOutcome {
    /// Replacing (`true`) or restoring.
    on: bool,
    result: Elevated,
}

pub(crate) const CLASS_NAME: PCWSTR = w!("OpenTaskMainWindow");
/// The app icon's resource id, as `ot-app`'s build script writes it.
const APP_ICON: u16 = 1;
const TITLE: &str = "open-task";
const TITLE_PAUSED: &str = "open-task (paused)";
const INITIAL_SIZE: (i32, i32) = (1180, 760);

/// Virtual-key codes for letters are their upper-case ASCII values.
const VK_F: u16 = b'F' as u16;
const VK_H: u16 = b'H' as u16;
const VK_M: u16 = b'M' as u16;
const VK_T: u16 = b'T' as u16;
/// The digit keys above the letters, `1` to `9`.
const VK_1: u16 = b'1' as u16;
const VK_9: u16 = b'9' as u16;

#[derive(Clone, Copy)]
struct Cursors {
    arrow: HCURSOR,
    size_we: HCURSOR,
    ibeam: HCURSOR,
}

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    app: App,
    dl: DisplayList,
    sampler: Option<Sampler>,
    control: PlatformControl,
    dpi: f32,
    tracking_leave: bool,
    backdrop: bool,
    theme_pref: ThemePreference,
    /// Whether the current theme is dark. Follows the system when `theme_pref` says so.
    dark: bool,
    /// First half of a UTF-16 surrogate pair from `WM_CHAR`, awaiting the second.
    high_surrogate: Option<u16>,
    cursors: Cursors,
    /// The window title as last set; it says when the display is paused.
    title: &'static str,
    /// `None` if this build's version does not parse, which a build from this
    /// repository never produces.
    updater: Option<Updater>,
    closing: Closing,
    /// The registered message a Task Manager stand-in sends to bring this window
    /// forward (`instance`).
    raise_message: u32,
}

/// How far the app is along in closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    No,
    /// Closed with an update waiting and "Install updates automatically" on: the
    /// window is hidden and Setup is running. Restart Manager ends the process, or
    /// the close finishes if Setup gives up.
    InstallingUpdate,
    /// Logoff, shutdown or Restart Manager is closing the app: never install then.
    SessionEnding,
}

/// What a message handler decided. The borrow on [`State`] ends before anything
/// re-entrant runs: view events go through [`dispatch`], and a DPI move through
/// `SetWindowPos`, only after the handler has returned.
enum Outcome {
    Done(LRESULT),
    Default,
    Event(UiEvent),
    /// `WM_SIZE`: hand the view its new size and draw the frame right away, rather
    /// than leaving it to `WM_PAINT`. During a live drag the modal sizing loop only
    /// synthesizes `WM_PAINT` when its queue is empty, and the mouse keeps it busy,
    /// so the window would show the last frame at its old size (or nothing, since
    /// `ResizeBuffers` leaves the new buffers blank) until the pointer rests.
    Resized(UiEvent),
    /// `WM_DPICHANGED`: move to the suggested rectangle, which re-enters with `WM_SIZE`.
    Rescale(RECT),
    /// Tell the user something in a message box, once the state is released.
    Notify(String),
    /// Hide the window, once the state is released (`ShowWindow` re-enters).
    Hide,
    /// Restore the window if minimized and bring it to the front, for a Task
    /// Manager stand-in, answering 1.
    Raise,
}

fn win(context: &'static str) -> impl FnOnce(windows::core::Error) -> ShellError {
    move |source| ShellError::Win { context, source }
}

// Window class, window, DWM attributes, graphics, sampler, message loop: one setup
// sequence, read top to bottom. Splitting it would only scatter the order.
#[allow(clippy::too_many_lines)]
pub fn run(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    options: ShellOptions,
) -> Result<(), ShellError> {
    // A Task Manager stand-in looks for this.
    let _ = instance::mark();

    // SAFETY: plain process-wide setting; failure (already set) is harmless.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // SAFETY: system cursors need no module and live for the process.
    let cursors = unsafe {
        Cursors {
            arrow: LoadCursorW(None, IDC_ARROW).map_err(win("LoadCursorW"))?,
            size_we: LoadCursorW(None, IDC_SIZEWE).map_err(win("LoadCursorW"))?,
            ibeam: LoadCursorW(None, IDC_IBEAM).map_err(win("LoadCursorW"))?,
        }
    };

    // SAFETY: all structures are fully initialized; the class name is a static wide string.
    let hwnd = unsafe {
        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(win("GetModuleHandleW"))?.0);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: cursors.arrow,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassW(&raw const wc) == 0 {
            return Err(win("RegisterClassW")(windows::core::Error::from_thread()));
        }
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            CLASS_NAME,
            w!("open-task"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            INITIAL_SIZE.0,
            INITIAL_SIZE.1,
            None,
            None,
            Some(hinstance),
            None,
        )
        .map_err(win("CreateWindowExW"))?
    };
    let raise_message = instance::raise_message();
    instance::accept_raise(hwnd, raise_message);

    // Title bar follows the theme; Mica backdrop is best-effort (older builds refuse,
    // and we fall back to an opaque background).
    let dark = resolve_dark(options.theme);
    apply_title_bar_theme(hwnd, dark);
    // SAFETY: the attribute value is a local that outlives the call; sizes match.
    let backdrop = unsafe {
        let kind = DWMSBT_MAINWINDOW;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const kind).cast::<c_void>(),
            size_of::<i32>() as u32,
        )
        .is_ok()
    };

    // SAFETY: hwnd is valid.
    let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
    set_window_icons(hwnd, dpi as u32);
    let size_px = client_size(hwnd);
    let gfx = Gfx::new(hwnd, size_px, dpi).map_err(win("Gfx::new"))?;

    let mut app = App::new(theme_for(dark));
    app.set_backdrop(backdrop);
    app.set_settings(prefs::load());
    app.set_system_animations(prefs::system_animations());
    app.set_view(options.view);
    app.set_page(options.page);
    let _ = app.handle(UiEvent::Resize(to_dips_size(size_px, dpi)));
    let installation = ot_update::installation();
    tracing::info!(?installation, "installation");
    app.set_task_manager(TaskManager {
        replacement: task_manager::replacement(),
        install: installation.as_ref().map(|i| i.scope),
        pending: false,
    });
    let updater = start_updater(options.version, hwnd, installation);
    let can_install = updater.as_ref().is_some_and(Updater::can_install);
    app.set_update(UpdateView::new(options.version, can_install));

    // The sampler posts a message per publish. HWND is a pointer and therefore not
    // Send, so it crosses the thread as an integer.
    let hwnd_bits = hwnd.0 as isize;
    let sampler = Sampler::start_with_notify(probe, config, move || {
        // SAFETY: posting to a window handle is thread-safe; if the window is gone
        // the call fails harmlessly.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd_bits as *mut c_void)),
                WM_APP_SNAPSHOT,
                WPARAM(0),
                LPARAM(0),
            )
        };
    });

    let state = Box::new(RefCell::new(State {
        hwnd,
        gfx: Some(gfx),
        app,
        dl: DisplayList::new(),
        sampler: Some(sampler),
        control: PlatformControl,
        dpi,
        tracking_leave: false,
        backdrop,
        theme_pref: options.theme,
        dark,
        high_surrogate: None,
        cursors,
        title: TITLE,
        updater,
        closing: Closing::No,
        raise_message,
    }));
    let state_ptr = Box::into_raw(state);
    // SAFETY: hwnd is valid; the pointer stays alive until after the loop below.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
        SetTimer(Some(hwnd), TIMER_UPDATE, FIRST_CHECK_MS, None);
    }

    tracing::info!(backdrop, dark, dpi, ?size_px, "window up");

    // SAFETY: standard message loop; MSG is a plain out-struct.
    unsafe {
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&raw mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }

    // SAFETY: no more messages will be dispatched to this window; we own the box.
    unsafe {
        let mut state = Box::from_raw(state_ptr);
        // Stop sampling before the rest is torn down so no message is posted to a
        // dead window from the sampler thread.
        if let Some(mut s) = state.get_mut().sampler.take() {
            s.stop();
        }
        drop(state);
    }
    Ok(())
}

/// The updater, posting `WM_APP_UPDATE` to the window after every change. It
/// installs updates only if the installer put this copy where it runs
/// (`installation`).
fn start_updater(version: &str, hwnd: HWND, installation: Option<Installation>) -> Option<Updater> {
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
    // HWND is a pointer and therefore not Send, so it crosses as an integer.
    let hwnd_bits = hwnd.0 as isize;
    Some(Updater::new(config, platform, move || {
        // SAFETY: posting to a window handle is thread-safe; if the window is gone
        // the call fails harmlessly.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd_bits as *mut c_void)),
                WM_APP_UPDATE,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }))
}

/// Windows' "Choose your default app mode" setting. A missing value means light,
/// which is what Windows itself assumes.
fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the out-pointers reference locals that outlive the call; `size` matches.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    status.is_ok() && value == 0
}

/// Give the window the app icon at the sizes `dpi` wants: the big one for the
/// taskbar and Alt+Tab, the small one for the title bar. The exe carries a drawing
/// for each size, so none is scaled. A build without the icon resource keeps the
/// default icon.
fn set_window_icons(hwnd: HWND, dpi: u32) {
    // SAFETY: the module is this exe and the resource name is an integer id, as
    // MAKEINTRESOURCE makes; the icons replaced were loaded here (the class has
    // none), so they are ours to destroy.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        for (which, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = GetSystemMetricsForDpi(metric, dpi);
            let Ok(icon) = LoadImageW(
                Some(HINSTANCE(module.0)),
                PCWSTR(usize::from(APP_ICON) as *const u16),
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            ) else {
                continue;
            };
            let old = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(which as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
            if old.0 != 0 {
                let _ = DestroyIcon(HICON(old.0 as *mut c_void));
            }
        }
    }
}

fn resolve_dark(pref: ThemePreference) -> bool {
    match pref {
        ThemePreference::System => system_prefers_dark(),
        ThemePreference::Dark => true,
        ThemePreference::Light => false,
    }
}

fn apply_title_bar_theme(hwnd: HWND, dark: bool) {
    let flag = BOOL(i32::from(dark));
    // SAFETY: the value is a local that outlives the call; the size matches.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const flag).cast::<c_void>(),
            size_of::<BOOL>() as u32,
        );
    }
}

fn theme_for(dark: bool) -> Theme {
    if dark {
        Theme::dark()
    } else {
        Theme::light()
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    // SAFETY: hwnd is valid; RECT is a plain out-struct.
    unsafe {
        let _ = GetClientRect(hwnd, &raw mut r);
    }
    (
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

fn to_dips_size(px: (u32, u32), dpi: f32) -> Size {
    let s = 96.0 / dpi;
    Size::new(px.0 as f32 * s, px.1 as f32 * s)
}

fn to_dips_point(x: i32, y: i32, dpi: f32) -> Point {
    let s = 96.0 / dpi;
    Point::new(x as f32 * s, y as f32 * s)
}

fn to_px_point(p: Point, dpi: f32) -> POINT {
    let s = dpi / 96.0;
    POINT {
        x: (p.x * s).round() as i32,
        y: (p.y * s).round() as i32,
    }
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    let v = lp.0 as u32;
    (
        i32::from((v as u16).cast_signed()),
        i32::from(((v >> 16) as u16).cast_signed()),
    )
}

/// Screen coordinates (as `WM_CONTEXTMENU` and `WM_MOUSEWHEEL` give them) to DIPs
/// in the client area.
fn screen_to_dips(hwnd: HWND, x: i32, y: i32, dpi: f32) -> Point {
    let mut p = POINT { x, y };
    // SAFETY: hwnd is valid; POINT is a plain in-out struct.
    unsafe {
        let _ = ScreenToClient(hwnd, &raw mut p);
    }
    to_dips_point(p.x, p.y, dpi)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query of the keyboard state; no pointers.
    let state = unsafe { GetKeyState(i32::from(vk.0)) };
    state < 0
}

fn wheel_delta(wparam: WPARAM) -> f32 {
    f32::from((((wparam.0 >> 16) & 0xFFFF) as u16).cast_signed()) / WHEEL_DELTA as f32
}

fn invalidate(hwnd: HWND) {
    // SAFETY: hwnd is valid.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn state_from(hwnd: HWND) -> Option<&'static RefCell<State>> {
    // SAFETY: the pointer was stored by `run` and outlives every message dispatch;
    // it is only ever accessed from the window's thread.
    unsafe {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<State>;
        p.as_ref()
    }
}

fn repaint(st: &mut State) {
    if st.gfx.is_none() {
        match Gfx::new(st.hwnd, client_size(st.hwnd), st.dpi) {
            Ok(g) => {
                st.gfx = Some(g);
                st.app.set_backdrop(st.backdrop);
            }
            Err(e) => {
                tracing::error!(error = %e, "could not recreate graphics device");
                return;
            }
        }
    }
    st.app.paint(&mut st.dl);
    if let Some(gfx) = st.gfx.as_mut() {
        if let Err(e) = gfx.render(&st.dl) {
            tracing::error!(error = %e, "render failed; device will be recreated");
            st.gfx = None;
            invalidate(st.hwnd);
        }
    }
    // Rows are sliding: ask for the next frame. `Present` waits for the vertical
    // blank, so this runs at the display's rate and stops when the slide lands.
    if st.app.animating() {
        invalidate(st.hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(cell) = state_from(hwnd) else {
        // SAFETY: standard default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // A message that arrives while we are already inside a handler (Win32 re-enters
    // the procedure from calls like SetWindowPos) gets default handling rather than
    // a RefCell panic. Handlers are written so that matters as little as possible:
    // anything re-entrant happens after the borrow below has ended.
    let outcome = match cell.try_borrow_mut() {
        Ok(mut st) => handle_message(&mut st, hwnd, msg, wparam, lparam),
        Err(_) => Outcome::Default,
    };
    match outcome {
        Outcome::Done(r) => r,
        // SAFETY: standard default handling.
        Outcome::Default => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Outcome::Event(ev) => {
            dispatch(cell, hwnd, ev);
            LRESULT(0)
        }
        Outcome::Resized(ev) => {
            dispatch(cell, hwnd, ev);
            if let Ok(mut st) = cell.try_borrow_mut() {
                repaint(&mut st);
            }
            // The frame just presented is current; drop the invalidation that the
            // class styles and the view queued, so `WM_PAINT` does not draw it twice.
            // SAFETY: hwnd is valid; a null rectangle means the whole client area.
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        Outcome::Rescale(r) => {
            // SAFETY: hwnd is valid; flags are standard.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            // The icons follow the monitor's DPI too.
            // SAFETY: hwnd is valid.
            set_window_icons(hwnd, unsafe { GetDpiForWindow(hwnd) });
            invalidate(hwnd);
            LRESULT(0)
        }
        Outcome::Notify(text) => {
            notify(hwnd, &text);
            LRESULT(0)
        }
        Outcome::Hide => {
            // SAFETY: hwnd is valid.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        Outcome::Raise => {
            // SAFETY: hwnd is valid. The stand-in that asked handed this process the
            // right to take the foreground.
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(1)
        }
    }
}

/// Tell the user something in a message box.
fn notify(hwnd: HWND, text: &str) {
    let text = HSTRING::from(text);
    // SAFETY: strings outlive the call; hwnd is valid.
    unsafe {
        MessageBoxW(Some(hwnd), &text, w!("open-task"), MB_OK | MB_ICONWARNING);
    }
}

#[allow(clippy::too_many_lines)]
fn handle_message(st: &mut State, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Outcome {
    match msg {
        // A Task Manager stand-in asks this window to come forward. Not while it
        // is closing: then the stand-in opens its own.
        m if m == st.raise_message => {
            if st.closing == Closing::No {
                Outcome::Raise
            } else {
                Outcome::Done(LRESULT(0))
            }
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: validates the update region; we do not draw with the HDC.
            unsafe {
                let _ = BeginPaint(hwnd, &raw mut ps);
                let _ = EndPaint(hwnd, &raw const ps);
            }
            repaint(st);
            Outcome::Done(LRESULT(0))
        }
        WM_ERASEBKGND => Outcome::Done(LRESULT(1)),
        WM_SIZE => {
            if wparam.0 as u32 == SIZE_MINIMIZED {
                return Outcome::Done(LRESULT(0));
            }
            let (w, h) = lparam_xy(lparam);
            let px = (w.max(0) as u32, h.max(0) as u32);
            if let Some(g) = st.gfx.as_mut() {
                if let Err(e) = g.resize(px) {
                    tracing::error!(error = %e, "swap chain resize failed");
                    st.gfx = None;
                }
            }
            Outcome::Resized(UiEvent::Resize(to_dips_size(px, st.dpi)))
        }
        WM_DPICHANGED => {
            let new_dpi = ((wparam.0 >> 16) & 0xFFFF) as f32;
            st.dpi = new_dpi;
            if let Some(g) = st.gfx.as_mut() {
                g.set_dpi(new_dpi);
            }
            // SAFETY: lparam is a pointer to the suggested RECT for this message.
            Outcome::Rescale(unsafe { *(lparam.0 as *const RECT) })
        }
        WM_SETCURSOR => {
            if (lparam.0 & 0xFFFF) as u32 != HTCLIENT {
                return Outcome::Default;
            }
            let c = match st.app.cursor() {
                Cursor::Arrow => st.cursors.arrow,
                Cursor::ResizeColumn => st.cursors.size_we,
                Cursor::Text => st.cursors.ibeam,
            };
            // SAFETY: a cursor loaded in `run`.
            unsafe {
                SetCursor(Some(c));
            }
            Outcome::Done(LRESULT(1))
        }
        WM_MOUSEMOVE => {
            if !st.tracking_leave {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: struct is fully initialized.
                st.tracking_leave = unsafe { TrackMouseEvent(&raw mut tme) }.is_ok();
            }
            let (x, y) = lparam_xy(lparam);
            Outcome::Event(UiEvent::MouseMove(to_dips_point(x, y, st.dpi)))
        }
        WM_MOUSELEAVE => {
            st.tracking_leave = false;
            Outcome::Event(UiEvent::MouseLeave)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN => {
            let (x, y) = lparam_xy(lparam);
            let at = to_dips_point(x, y, st.dpi);
            // Capture during a left drag so a column resize follows the pointer
            // outside the window.
            // SAFETY: hwnd is valid; capture calls take no pointers.
            unsafe {
                if msg == WM_LBUTTONDOWN {
                    SetCapture(hwnd);
                } else if msg == WM_LBUTTONUP {
                    let _ = ReleaseCapture();
                }
            }
            let button = if msg == WM_RBUTTONDOWN {
                MouseButton::Right
            } else {
                MouseButton::Left
            };
            Outcome::Event(if msg == WM_LBUTTONUP {
                UiEvent::MouseUp { at, button }
            } else {
                UiEvent::MouseDown { at, button }
            })
        }
        WM_CONTEXTMENU => {
            let (x, y) = lparam_xy(lparam);
            // The keyboard (Shift+F10, the menu key) reports no position.
            let at = (x != -1 || y != -1).then(|| screen_to_dips(hwnd, x, y, st.dpi));
            Outcome::Event(UiEvent::ContextMenu { at })
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = wheel_delta(wparam);
            let (sx, sy) = lparam_xy(lparam);
            let at = screen_to_dips(hwnd, sx, sy, st.dpi);
            // A tilt wheel reports right as positive. Wheel away from the user is
            // positive and scrolls content up, or left with Shift held.
            let (lines, horizontal) = if msg == WM_MOUSEHWHEEL {
                (delta, true)
            } else {
                (-delta, key_down(VK_SHIFT))
            };
            Outcome::Event(UiEvent::Wheel {
                at,
                lines,
                horizontal,
            })
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let ctrl = key_down(VK_CONTROL);
            let shift = key_down(VK_SHIFT);
            let ev = match vk {
                v if v == VK_UP.0 => UiEvent::Key(Key::Up),
                v if v == VK_DOWN.0 => UiEvent::Key(Key::Down),
                v if v == VK_PRIOR.0 => UiEvent::Key(Key::PageUp),
                v if v == VK_NEXT.0 => UiEvent::Key(Key::PageDown),
                v if v == VK_HOME.0 => UiEvent::Key(Key::Home),
                v if v == VK_END.0 => UiEvent::Key(Key::End),
                v if v == VK_LEFT.0 => UiEvent::Key(Key::Left),
                v if v == VK_RIGHT.0 => UiEvent::Key(Key::Right),
                v if v == VK_BACK.0 && ctrl => UiEvent::Key(Key::WordBackspace),
                v if v == VK_BACK.0 => UiEvent::Key(Key::Backspace),
                v if v == VK_ESCAPE.0 => UiEvent::Key(Key::Escape),
                v if v == VK_RETURN.0 => UiEvent::Key(Key::Enter),
                // Delete ends the selected process; Shift+Delete its whole tree.
                // Process Explorer's bindings.
                v if v == VK_DELETE.0 && shift => {
                    UiEvent::Command(Command::Menu(MenuAction::EndTree))
                }
                v if v == VK_DELETE.0 => UiEvent::Command(Command::Menu(MenuAction::EndTask)),
                // Ctrl+T: Process Explorer's binding for the process tree.
                VK_T if ctrl => UiEvent::Command(Command::ToggleView),
                // Ctrl+M: the Map, the process table's area as a treemap.
                VK_M if ctrl => UiEvent::Command(Command::SetView(ViewMode::Map)),
                VK_H if ctrl => UiEvent::Command(Command::SetView(ViewMode::History)),
                VK_F if ctrl => UiEvent::Command(Command::Find),
                // Ctrl+Tab and Ctrl+Shift+Tab walk the pages, as in classic Task
                // Manager; Ctrl+1..9 jump to one.
                v if v == VK_TAB.0 && ctrl => {
                    UiEvent::Command(Command::StepPage(if shift { -1 } else { 1 }))
                }
                v @ VK_1..=VK_9 if ctrl => match Page::nth(usize::from(v - VK_1) + 1) {
                    Some(page) => UiEvent::Command(Command::SetPage(page)),
                    None => return Outcome::Default,
                },
                _ => return Outcome::Default,
            };
            Outcome::Event(ev)
        }
        WM_CHAR => {
            // One UTF-16 unit per message; a supplementary character arrives as
            // two. Control characters (Backspace, Enter, Escape, Ctrl+letter) are
            // handled as keys, not text.
            let unit = (wparam.0 & 0xFFFF) as u16;
            let c = if let Some(high) = st.high_surrogate.take() {
                char::decode_utf16([high, unit]).next().and_then(Result::ok)
            } else if (0xD800..=0xDBFF).contains(&unit) {
                st.high_surrogate = Some(unit);
                None
            } else if unit >= 0x20 && unit != 0x7F {
                char::from_u32(u32::from(unit))
            } else {
                None
            };
            match c {
                Some(c) => Outcome::Event(UiEvent::Char(c)),
                None => Outcome::Done(LRESULT(0)),
            }
        }
        WM_APP_SAMPLE => {
            // SAFETY: the pointer was made by `Box::into_raw` in `sample_cpu` and is
            // delivered exactly once.
            let outcome = unsafe { Box::from_raw(lparam.0 as *mut SampleOutcome) };
            let SampleOutcome {
                target,
                name,
                result,
            } = *outcome;
            match result {
                Ok(a) => {
                    tracing::info!(pid = target.pid, samples = a.samples, "CPU sample done");
                    st.app.set_attribution(Arc::new(a));
                    invalidate(hwnd);
                    Outcome::Done(LRESULT(0))
                }
                Err(e) => {
                    tracing::warn!(pid = target.pid, error = %e, "CPU sample failed");
                    st.app.sampling_failed(target);
                    invalidate(hwnd);
                    Outcome::Notify(format!("Could not sample {name}.\n\n{e}"))
                }
            }
        }
        WM_APP_TASK_MANAGER => {
            // SAFETY: the pointer was made by `Box::into_raw` in
            // `replace_task_manager` and is delivered exactly once.
            let outcome = unsafe { Box::from_raw(lparam.0 as *mut TaskManagerOutcome) };
            set_task_manager_pending(st, false);
            refresh_task_manager(st);
            invalidate(hwnd);
            match outcome.result {
                Elevated::Done => Outcome::Done(LRESULT(0)),
                Elevated::Cancelled => {
                    tracing::info!("permission to change Task Manager was not given");
                    Outcome::Done(LRESULT(0))
                }
                Elevated::Failed(e) => {
                    tracing::warn!(error = %e, "the Task Manager helper failed");
                    Outcome::Notify(task_manager_failure(outcome.on, &e))
                }
            }
        }
        // Coming back to the window: whatever changed Task Manager's replacement
        // meanwhile (Process Explorer, the installer) shows on the Settings page.
        WM_ACTIVATEAPP => {
            if wparam.0 != 0 && refresh_task_manager(st) {
                invalidate(hwnd);
            }
            Outcome::Default
        }
        WM_APP_SNAPSHOT => {
            if let Some(s) = &st.sampler {
                if st.app.set_snapshot(s.latest()) {
                    invalidate(hwnd);
                }
            }
            Outcome::Done(LRESULT(0))
        }
        WM_APP_UPDATE => {
            if let Some(u) = &st.updater {
                let status = u.status();
                // Installing as it closed, and Setup ended without closing it
                // (cancelled, or failed): finish closing.
                let installing = matches!(status, ot_update::Status::Installing { .. });
                if st.closing == Closing::InstallingUpdate && !installing {
                    tracing::info!(?status, "setup did not close the app; closing");
                    // SAFETY: posting to our own window.
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                if st.app.set_update_status(status) {
                    invalidate(hwnd);
                }
            }
            Outcome::Done(LRESULT(0))
        }
        WM_CLOSE => close(st),
        WM_TIMER if wparam.0 == TIMER_UPDATE => {
            // After the first tick, look hourly; the updater decides whether a day
            // has passed since the last check.
            // SAFETY: hwnd is valid; re-arming an existing timer replaces it.
            unsafe {
                SetTimer(Some(hwnd), TIMER_UPDATE, CHECK_TICK_MS, None);
            }
            let settings = st.app.settings();
            if let Some(u) = &st.updater {
                if settings.check_updates && u.due(CHECK_EVERY) {
                    let download = settings.download_updates || settings.install_updates;
                    u.check(false, download && u.can_install());
                }
            }
            Outcome::Done(LRESULT(0))
        }
        // Restart Manager (the installer replacing this exe) and logoff end the
        // session this way. Close as the user would; posting keeps it out of this
        // handler, and the message loop ends with the window.
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                tracing::info!(reason = lparam.0, "session ending; closing");
                st.closing = Closing::SessionEnding;
                // SAFETY: posting to our own window.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            Outcome::Done(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            // Sent for any system setting; re-reading these is cheap. The Settings
            // page shows Windows' animation effects, so a change repaints.
            let animations = prefs::system_animations();
            if animations != st.app.system_animations() {
                st.app.set_system_animations(animations);
                invalidate(hwnd);
            }
            if st.theme_pref == ThemePreference::System {
                let dark = system_prefers_dark();
                if dark != st.dark {
                    st.dark = dark;
                    apply_title_bar_theme(hwnd, dark);
                    st.app.set_theme(theme_for(dark));
                    invalidate(hwnd);
                }
            }
            Outcome::Default
        }
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            Outcome::Done(LRESULT(0))
        }
        // Everything else, including WM_RBUTTONUP, which DefWindowProc turns into
        // WM_CONTEXTMENU for us.
        _ => Outcome::Default,
    }
}

/// Feed an event to the view and carry out what it asks for. A menu choice comes
/// back as another event, so this loops until the view has nothing more to say.
fn dispatch(cell: &RefCell<State>, hwnd: HWND, ev: UiEvent) {
    let mut next = Some(ev);
    while let Some(ev) = next.take() {
        let (effect, title) = {
            let Ok(mut st) = cell.try_borrow_mut() else {
                return;
            };
            let reaction = st.app.handle(ev);
            if reaction.repaint {
                invalidate(hwnd);
            }
            let title = if st.app.paused() { TITLE_PAUSED } else { TITLE };
            let changed = std::mem::replace(&mut st.title, title) != title;
            (reaction.effect, changed.then_some(title))
        };
        // Setting the title sends WM_SETTEXT, so only with the state released.
        if let Some(title) = title {
            // SAFETY: hwnd is valid; the string outlives the call.
            unsafe {
                let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
            }
        }
        if let Some(effect) = effect {
            next = perform(cell, hwnd, effect);
        }
    }
}

fn perform(cell: &RefCell<State>, hwnd: HWND, effect: Effect) -> Option<UiEvent> {
    match effect {
        Effect::Menu { at, entries } => {
            show_menu(cell, hwnd, at, &entries).map(|a| UiEvent::Command(Command::Menu(a)))
        }
        Effect::Terminate { targets, label } => {
            terminate(cell, hwnd, &targets, &label);
            None
        }
        Effect::OpenFileLocation(path) => {
            open_file_location(hwnd, &path);
            None
        }
        Effect::SampleCpu { target, seconds } => {
            sample_cpu(cell, hwnd, target, seconds);
            None
        }
        Effect::SaveSettings(settings) => {
            prefs::save(&settings);
            // Downloading was just turned on with a release waiting: start now.
            let updater = cell.try_borrow().ok().and_then(|st| st.updater.clone());
            if let Some(u) = updater {
                let waiting = matches!(u.status(), ot_update::Status::Available { .. });
                let download = settings.download_updates || settings.install_updates;
                if download && waiting && u.can_install() {
                    u.download();
                }
            }
            None
        }
        Effect::Update(action) => {
            update(cell, hwnd, action);
            None
        }
        Effect::ReplaceTaskManager(on) => {
            replace_task_manager(cell, hwnd, on);
            None
        }
    }
}

/// Replace Task Manager (`on`) or restore it: directly when this process may write
/// the machine's registry, otherwise through a copy of this program started as
/// administrator. That runs on a worker thread, so the window stays live while
/// Windows asks for permission, and reports back with `WM_APP_TASK_MANAGER`. The
/// card shows what the registry says afterwards, whatever happened.
fn replace_task_manager(cell: &RefCell<State>, hwnd: HWND, on: bool) {
    let failure = match task_manager::change(on) {
        Ok(()) => None,
        Err(e) if task_manager::denied(&e) => {
            tracing::info!(
                on,
                "changing Task Manager needs administrator rights; asking"
            );
            if let Ok(mut st) = cell.try_borrow_mut() {
                set_task_manager_pending(&mut st, true);
            }
            invalidate(hwnd);
            let hwnd_bits = hwnd.0 as isize;
            let spawned = std::thread::Builder::new()
                .name("ot-task-manager".into())
                .spawn(move || {
                    let owner = HWND(hwnd_bits as *mut c_void);
                    let result = task_manager::change_elevated(on, owner);
                    let ptr = Box::into_raw(Box::new(TaskManagerOutcome { on, result }));
                    // SAFETY: posting to a window handle is thread-safe. If the
                    // window is gone the post fails and the box is reclaimed here.
                    let posted = unsafe {
                        PostMessageW(
                            Some(owner),
                            WM_APP_TASK_MANAGER,
                            WPARAM(0),
                            LPARAM(ptr as isize),
                        )
                    };
                    if posted.is_err() {
                        // SAFETY: the message was not delivered, so the box is ours.
                        drop(unsafe { Box::from_raw(ptr) });
                    }
                });
            match spawned {
                Ok(_) => return,
                Err(e) => {
                    if let Ok(mut st) = cell.try_borrow_mut() {
                        set_task_manager_pending(&mut st, false);
                    }
                    Some(e.to_string())
                }
            }
        }
        Err(e) => Some(e.message()),
    };
    if let Ok(mut st) = cell.try_borrow_mut() {
        refresh_task_manager(&mut st);
    }
    invalidate(hwnd);
    if let Some(reason) = failure {
        tracing::warn!(on, %reason, "could not change Task Manager");
        notify(hwnd, &task_manager_failure(on, &reason));
    }
}

/// The message box text for a failed replace or restore.
fn task_manager_failure(on: bool, reason: &dyn std::fmt::Display) -> String {
    let verb = if on { "replace" } else { "restore" };
    format!("Could not {verb} Task Manager.\n\n{reason}")
}

/// Read what Windows starts in Task Manager's place again. Returns whether the card
/// changed.
fn refresh_task_manager(st: &mut State) -> bool {
    let mut tm = st.app.task_manager().clone();
    tm.replacement = task_manager::replacement();
    st.app.set_task_manager(tm)
}

fn set_task_manager_pending(st: &mut State, pending: bool) {
    let mut tm = st.app.task_manager().clone();
    tm.pending = pending;
    let _ = st.app.set_task_manager(tm);
}

/// `WM_CLOSE`. With "Install updates automatically" on and a verified update
/// waiting, the window hides and Setup runs without relaunching the app: the next
/// start is the new version. The process stays until Setup's Restart Manager ends
/// it, which keeps the installer locked through Setup's elevation, as a click does.
/// Anything else closes as usual.
fn close(st: &mut State) -> Outcome {
    if st.closing != Closing::No {
        return Outcome::Default;
    }
    let settings = st.app.settings();
    let Some(u) = st.updater.clone() else {
        return Outcome::Default;
    };
    let ready = matches!(u.status(), ot_update::Status::Ready { .. });
    if !(settings.install_updates && ready && u.can_install()) {
        return Outcome::Default;
    }
    tracing::info!("installing the update as open-task closes");
    st.closing = Closing::InstallingUpdate;
    if let Some(mut s) = st.sampler.take() {
        s.stop();
    }
    u.install(false);
    Outcome::Hide
}

/// The update button was pressed. The updater works on its own threads and
/// reports back with `WM_APP_UPDATE`.
fn update(cell: &RefCell<State>, hwnd: HWND, action: UpdateAction) {
    let Some(u) = cell.try_borrow().ok().and_then(|st| st.updater.clone()) else {
        return;
    };
    match action {
        // Asked for: download straight away if this copy can install it.
        UpdateAction::Check => u.check(true, u.can_install()),
        UpdateAction::Download => u.download(),
        UpdateAction::Install => u.install(true),
        UpdateAction::OpenReleasePage => {
            if let Some(url) = u.release_page() {
                open_url(hwnd, &url);
            }
        }
    }
}

/// The default browser at `url`.
fn open_url(hwnd: HWND, url: &str) {
    // SAFETY: strings outlive the call; hwnd is valid.
    let r = unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            &HSTRING::from(url),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values up to 32 are error codes, by the API's odd convention.
    if r.0 as usize <= 32 {
        tracing::warn!(url, code = r.0 as usize, "could not open the release page");
    }
}

/// Sample a process's CPU on a worker thread and post the result back as
/// `WM_APP_SAMPLE`. The sampler blocks for the whole window, so it must not run on
/// this thread; the table keeps updating meanwhile.
fn sample_cpu(cell: &RefCell<State>, hwnd: HWND, target: ProcessKey, seconds: u32) {
    let (name, services) = {
        let Ok(st) = cell.try_borrow() else {
            return;
        };
        let snap = st.sampler.as_ref().map(Sampler::latest);
        let p = snap
            .as_ref()
            .and_then(|s| s.processes.iter().find(|p| p.key() == target));
        match p {
            Some(p) => (
                p.name().to_owned(),
                p.services
                    .iter()
                    .map(|s| s.name.to_string())
                    .collect::<Vec<_>>(),
            ),
            None => (format!("PID {}", target.pid), Vec::new()),
        }
    };
    let hwnd_bits = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("ot-cpu-sample".into())
        .spawn(move || {
            let result =
                PlatformSampler.sample(target, &services, Duration::from_secs(u64::from(seconds)));
            let outcome = Box::new(SampleOutcome {
                target,
                name,
                result,
            });
            let ptr = Box::into_raw(outcome);
            // SAFETY: posting to a window handle is thread-safe. If the window is
            // gone the post fails and the box is reclaimed here.
            let posted = unsafe {
                PostMessageW(
                    Some(HWND(hwnd_bits as *mut c_void)),
                    WM_APP_SAMPLE,
                    WPARAM(0),
                    LPARAM(ptr as isize),
                )
            };
            if posted.is_err() {
                // SAFETY: the message was not delivered, so we still own the box.
                drop(unsafe { Box::from_raw(ptr) });
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start the sampling thread");
        if let Ok(mut st) = cell.try_borrow_mut() {
            st.app.sampling_failed(target);
        }
        invalidate(hwnd);
    }
}

/// A native popup menu. Blocks until the user picks or dismisses; messages keep
/// flowing meanwhile, so the table under it stays live.
fn show_menu(
    cell: &RefCell<State>,
    hwnd: HWND,
    at: Point,
    entries: &[MenuEntry],
) -> Option<MenuAction> {
    let dpi = cell.try_borrow().map_or(96.0, |st| st.dpi);
    let mut p = to_px_point(at, dpi);
    // SAFETY: hwnd is valid; POINT is a plain in-out struct.
    unsafe {
        let _ = ClientToScreen(hwnd, &raw mut p);
    }
    // Item ids are 1-based positions in `entries`; 0 means dismissed.
    // SAFETY: the menu is destroyed before returning; AppendMenuW copies its label.
    let chosen = unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, e) in entries.iter().enumerate() {
            let r = match e {
                MenuEntry::Item { label, enabled, .. } => {
                    let flags = if *enabled {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    };
                    AppendMenuW(menu, flags, i + 1, &HSTRING::from(*label))
                }
                MenuEntry::Separator => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
            };
            if let Err(e) = r {
                tracing::warn!(error = %e, "AppendMenuW");
            }
        }
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let chosen = TrackPopupMenuEx(menu, flags.0, p.x, p.y, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0
    };
    let index = usize::try_from(chosen).ok()?.checked_sub(1)?;
    match entries.get(index)? {
        MenuEntry::Item { action, .. } => Some(*action),
        MenuEntry::Separator => None,
    }
}

/// Confirm, then end every target. Processes that are already gone are not
/// failures; anything else that fails is reported together at the end.
fn terminate(cell: &RefCell<State>, hwnd: HWND, targets: &[ProcessKey], label: &str) {
    let text = HSTRING::from(format!(
        "End {label}?\n\nAny unsaved data in it will be lost."
    ));
    // SAFETY: strings outlive the call; hwnd is valid.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            &text,
            w!("open-task"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    if answer != IDYES {
        return;
    }
    let mut failures = String::new();
    {
        let Ok(st) = cell.try_borrow() else {
            return;
        };
        for key in targets {
            match st.control.terminate(*key) {
                Ok(()) => tracing::info!(pid = key.pid, "terminated"),
                Err(ControlError::Gone) => {}
                Err(e) => {
                    tracing::warn!(pid = key.pid, error = %e, "terminate failed");
                    let _ = writeln!(failures, "PID {}: {e}", key.pid);
                }
            }
        }
    }
    if !failures.is_empty() {
        let text = HSTRING::from(format!("Could not end {label}.\n\n{failures}"));
        // SAFETY: strings outlive the call; hwnd is valid.
        unsafe {
            MessageBoxW(Some(hwnd), &text, w!("open-task"), MB_OK | MB_ICONERROR);
        }
    }
}

/// Explorer with the file selected.
fn open_file_location(hwnd: HWND, path: &str) {
    let args = HSTRING::from(format!("/select,\"{path}\""));
    // SAFETY: strings outlive the call; hwnd is valid.
    let r = unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            w!("explorer.exe"),
            &args,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values up to 32 are error codes, by the API's odd convention.
    if r.0 as usize <= 32 {
        tracing::warn!(path, code = r.0 as usize, "explorer.exe /select failed");
    }
}
