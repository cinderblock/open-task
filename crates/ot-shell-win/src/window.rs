//! Win32 window, message loop, and translation of messages into UI events.
//!
//! The app itself, its feed, updater and workers, and every effect that needs no
//! window live in [`ot_shell_core::Shell`]. This module turns messages into
//! [`UiEvent`]s for it and carries out what it reports back ([`Out`]) with native
//! APIs: a popup menu, a message box, a shell verb, replacing Task Manager.
//! Those run with the window state unborrowed, because menus and message boxes
//! pump messages and those messages re-enter the window procedure.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_core::SamplerConfig;
use ot_paint::{DisplayList, Point, Size};
use ot_probe::SystemProbe;
use ot_shell_core::{
    theme_for, Ask, CloseOutcome, Closing, HostEffect, Notice, NoticeKind, Out, Shell, Start,
};
use ot_ui::{
    Capabilities, Command, Cursor, Key, MenuAction, MouseButton, Page, Settings, TaskManager,
    UiEvent,
};
use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_CLOAKED,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, InvalidateRect, ScreenToClient, ValidateRect};
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
    DispatchMessageW, GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowLongPtrW, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, LoadImageW, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenuEx,
    TranslateMessage, WindowFromPoint, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
    HCURSOR, HICON, HTCLIENT, HWND_NOTOPMOST, HWND_TOPMOST, ICON_BIG, ICON_SMALL, IDC_ARROW,
    IDC_CROSS, IDC_IBEAM, IDC_SIZEWE, IDYES, IMAGE_ICON, LR_DEFAULTCOLOR, MB_DEFBUTTON2,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_STYLE,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, SIZE_MINIMIZED, SM_CXICON, SM_CXSMICON,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_MINIMIZE, SW_RESTORE,
    SW_SHOW, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    WHEEL_DELTA, WM_ACTIVATEAPP, WM_APP, WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY,
    WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER,
    WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};

use crate::actions::{self, WinPlatform};
use crate::frame_stats::FrameLog;
use crate::gfx::Gfx;
use crate::icons;
use crate::menu_art;
use crate::placement;
use crate::prefs::WinPrefs;
use crate::task_manager::{self, Elevated};
use crate::tray::Tray;
use crate::{instance, prefs, run_dialog};
use crate::{ShellError, ShellOptions, ThemePreference};

/// The shell has something for the UI thread: a snapshot, the updater's
/// status, a worker's result. Read it with [`Shell::drain`].
const WM_APP_WAKE: u32 = WM_APP + 1;
/// The elevated helper that replaces or restores Task Manager finished; `lparam` is
/// a `Box<TaskManagerOutcome>`.
const WM_APP_TASK_MANAGER: u32 = WM_APP + 4;
/// The tray icon was clicked; `lparam` is the mouse message.
const WM_APP_TRAY: u32 = WM_APP + 7;
/// An icon the worker extracted: `lparam` is a `Box<icons::IconPixels>`.
const WM_APP_ICON: u32 = WM_APP + 8;

/// The timer for scheduled update checks: first shortly after start, so it does
/// not compete with the first frames, then hourly to see whether a day has passed.
const TIMER_UPDATE: usize = 1;
/// Re-reads the Connections page's list while it is showing.
const TIMER_REFRESH: usize = 2;
/// Paces the next frame when `Present` stops waiting for the display (see
/// [`FRAME_TOO_SOON`]).
const TIMER_FRAME: usize = 3;
/// A frame's worth of milliseconds at 60 Hz, for that fallback.
const FRAME_MS: u32 = 16;
/// Frames closer together than this cannot have waited for a vertical blank:
/// the display is off, the session locked, or the window out of composition.
const FRAME_TOO_SOON: Duration = Duration::from_millis(4);
const FIRST_CHECK_MS: u32 = 10_000;
const CHECK_TICK_MS: u32 = 3_600_000;

/// What the Task Manager helper's thread hands back to the window.
struct TaskManagerOutcome {
    /// Replacing (`true`) or restoring.
    on: bool,
    result: Elevated,
}

pub(crate) const CLASS_NAME: PCWSTR = w!("OpenTaskMainWindow");
/// The app icon's resource id, as `ot-app`'s build script writes it.
const APP_ICON: u16 = 1;
const INITIAL_SIZE: (i32, i32) = (1180, 760);

/// Virtual-key codes for letters are their upper-case ASCII values.
const VK_F: u16 = b'F' as u16;
const VK_H: u16 = b'H' as u16;
const VK_M: u16 = b'M' as u16;
const VK_N: u16 = b'N' as u16;
const VK_T: u16 = b'T' as u16;
/// The digit keys above the letters, `1` to `9`.
const VK_1: u16 = b'1' as u16;
const VK_9: u16 = b'9' as u16;

#[derive(Clone, Copy)]
struct Cursors {
    arrow: HCURSOR,
    size_we: HCURSOR,
    ibeam: HCURSOR,
    cross: HCURSOR,
}

// The flags are independent facts about the window's moment; an enum would
// only obscure them.
#[allow(clippy::struct_excessive_bools)]
struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    shell: Shell,
    dl: DisplayList,
    /// When the last frame was presented, to notice `Present` not waiting.
    last_frame: Option<Instant>,
    /// With `OT_FRAME_STATS` set: what the frames cost.
    frame_log: Option<FrameLog>,
    dpi: f32,
    tracking_leave: bool,
    backdrop: bool,
    theme_pref: ThemePreference,
    /// Whether the current theme is dark. Follows the system when `theme_pref` says so.
    dark: bool,
    /// First half of a UTF-16 surrogate pair from `WM_CHAR`, awaiting the second.
    high_surrogate: Option<u16>,
    cursors: Cursors,
    /// The notification-area icon with its CPU meter.
    tray: Option<Tray>,
    /// Extracts program icons the renderer asks for.
    icons: icons::IconLoader,
    /// The crosshair is out: the mouse is captured until the button comes up.
    picking: bool,
    /// The registered message a Task Manager stand-in sends to bring this window
    /// forward (`instance`).
    raise_message: u32,
}

/// What a message handler decided. The borrow on [`State`] ends before anything
/// re-entrant runs: view events go through [`dispatch`], the shell's requests
/// through [`carry_out`], and a DPI move through `SetWindowPos`, only after the
/// handler has returned.
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
    /// Show the window (restoring it, or unhiding it from the tray) and bring it
    /// to the front, answering 0.
    Show,
    /// The tray icon's menu, at the pointer.
    TrayMenu,
    /// What the shell reported, to carry out once the state is released.
    Outs(Vec<Out>),
}

fn win(context: &'static str) -> impl FnOnce(windows::core::Error) -> ShellError {
    move |source| ShellError::Win { context, source }
}

// Window class, window, DWM attributes, graphics, shell, message loop: one setup
// sequence, read top to bottom. Splitting it would only scatter the order.
#[allow(clippy::too_many_lines)]
pub fn run(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    options: &ShellOptions,
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
            cross: LoadCursorW(None, IDC_CROSS).map_err(win("LoadCursorW"))?,
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
    // Where it was left on these monitors, before the DPI and size are read below
    // and before the state is attached (see `placement::restore`).
    let maximized = placement::restore(hwnd);
    let raise_message = instance::raise_message();
    instance::accept_raise(hwnd, raise_message);

    // Title bar follows the theme; Mica backdrop is best-effort (older builds refuse,
    // and we fall back to an opaque background).
    let dark = options.theme.dark(system_prefers_dark());
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

    let installation = ot_update::installation();
    // The shell wakes the window with a posted message. HWND is a pointer and
    // therefore not Send, so it crosses threads as an integer.
    let hwnd_bits = hwnd.0 as isize;
    let waker = Arc::new(move || {
        // SAFETY: posting to a window handle is thread-safe; if the window is gone
        // the call fails harmlessly.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd_bits as *mut c_void)),
                WM_APP_WAKE,
                WPARAM(0),
                LPARAM(0),
            )
        };
    });
    let mut shell = Shell::start(Start {
        probe,
        config,
        options,
        theme: theme_for(dark),
        backdrop,
        system_animations: prefs::system_animations(),
        capabilities: Capabilities::WINDOWS,
        task_manager: TaskManager {
            replacement: task_manager::replacement(),
            install: installation.as_ref().map(|i| i.scope),
            pending: false,
        },
        installation,
        prefs: Box::new(WinPrefs),
        platform: Arc::new(WinPlatform),
        waker,
    })?;
    let _ = shell
        .app_mut()
        .handle(UiEvent::Resize(to_dips_size(size_px, dpi)));
    let title = shell.title().to_owned();
    let settings = shell.app().settings();

    let state = Box::new(RefCell::new(State {
        hwnd,
        gfx: Some(gfx),
        shell,
        dl: DisplayList::new(),
        last_frame: None,
        frame_log: FrameLog::from_env(),
        dpi,
        tracking_leave: false,
        backdrop,
        theme_pref: options.theme,
        dark,
        high_surrogate: None,
        cursors,
        tray: Some(Tray::new(hwnd, WM_APP_TRAY)),
        icons: icons::IconLoader::start(hwnd, WM_APP_ICON),
        picking: false,
        raise_message,
    }));
    let state_ptr = Box::into_raw(state);
    // SAFETY: hwnd is valid; the pointer stays alive until after the loop below.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
        placement::show(hwnd, maximized);
        SetTimer(Some(hwnd), TIMER_UPDATE, FIRST_CHECK_MS, None);
    }
    // SAFETY: the state was just stored; the window is up.
    if let Some(cell) = unsafe { state_ptr.as_ref() } {
        apply_settings(cell, hwnd, &settings);
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
        state.get_mut().shell.stop_feed();
        drop(state);
    }
    Ok(())
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

/// The title bar, and the menus, dark or light.
fn apply_title_bar_theme(hwnd: HWND, dark: bool) {
    menu_art::set_dark(hwnd, dark);
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

/// Where the pointer is in the client area, in DIPs, if it is over the window and
/// not over another one in front of it.
fn pointer_over(hwnd: HWND, dpi: f32) -> Option<Point> {
    let mut screen = POINT::default();
    let mut client = RECT::default();
    // SAFETY: hwnd is valid; the out-structs are valid.
    unsafe {
        GetCursorPos(&raw mut screen).ok()?;
        GetClientRect(hwnd, &raw mut client).ok()?;
        if WindowFromPoint(screen) != hwnd {
            return None;
        }
    }
    let p = screen_to_dips(hwnd, screen.x, screen.y, dpi);
    let size = to_dips_size(
        (
            u32::try_from(client.right).unwrap_or(0),
            u32::try_from(client.bottom).unwrap_or(0),
        ),
        dpi,
    );
    (p.x >= 0.0 && p.y >= 0.0 && p.x < size.w && p.y < size.h).then_some(p)
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

fn post_close(hwnd: HWND) {
    // SAFETY: posting to our own window.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
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

/// Whether any of the window can be seen: shown, not minimized, and on the
/// current virtual desktop.
fn seen(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: hwnd is valid; DWMWA_CLOAKED writes one DWORD to the pointer.
    unsafe {
        IsWindowVisible(hwnd).as_bool()
            && !IsIconic(hwnd).as_bool()
            && (DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                (&raw mut cloaked).cast(),
                size_of::<u32>() as u32,
            )
            .is_err()
                || cloaked == 0)
    }
}

fn repaint(st: &mut State) {
    // Minimized, hidden in the tray or on another virtual desktop: nothing to
    // draw for. Each snapshot still asks for a frame; the first after the window
    // shows again (restoring sends one at once, the next snapshot otherwise)
    // draws what changed meanwhile.
    if !seen(st.hwnd) {
        return;
    }
    if st.gfx.is_none() {
        match Gfx::new(st.hwnd, client_size(st.hwnd), st.dpi) {
            Ok(mut g) => {
                g.set_chart_drawing(st.shell.app().settings().chart_drawing);
                st.gfx = Some(g);
                st.shell.app_mut().set_backdrop(st.backdrop);
            }
            Err(e) => {
                tracing::error!(error = %e, "could not recreate graphics device");
                return;
            }
        }
    }
    let painting = Instant::now();
    st.shell.app_mut().paint(&mut st.dl);
    let paint = painting.elapsed();
    // In the background the window gets every other vertical blank: still smooth,
    // at half the cost, for a window glanced at rather than watched.
    // SAFETY: plain query; no pointers.
    let foreground = unsafe { GetForegroundWindow() } == st.hwnd;
    let sync = if foreground { 1 } else { 2 };
    if let Some(gfx) = st.gfx.as_mut() {
        if let Err(e) = gfx.render(&st.dl, sync) {
            tracing::error!(error = %e, "render failed; device will be recreated");
            st.gfx = None;
            invalidate(st.hwnd);
        } else {
            if let Some(log) = st.frame_log.as_mut() {
                log.add(
                    st.shell.app().last_paint(),
                    paint,
                    gfx.last_stats(),
                    gfx.last_damage(),
                );
            }
            // Icons the frame wanted and nobody has loaded: the worker gets them.
            for path in gfx.take_wanted() {
                st.icons.request(path);
            }
        }
    }
    let presented = Instant::now();
    let too_soon = st
        .last_frame
        .replace(presented)
        .is_some_and(|t| presented.duration_since(t) < FRAME_TOO_SOON);
    // Rows are sliding or charts scrolling: ask for the next frame. `Present`
    // waits for the vertical blank, so this runs at the display's rate and stops
    // when the motion does. Not while nothing of the window can be seen:
    // minimized, hidden in the tray, or on another virtual desktop (cloaked);
    // showing it again paints it. Should `Present` stop waiting anyway, a timer
    // paces the frames rather than letting them spin.
    if seen(st.hwnd) && st.shell.app().animating() {
        if too_soon {
            // SAFETY: hwnd is valid; the timer is ours.
            unsafe {
                SetTimer(Some(st.hwnd), TIMER_FRAME, FRAME_MS, None);
            }
        } else {
            invalidate(st.hwnd);
        }
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
            show_window(hwnd);
            LRESULT(1)
        }
        Outcome::Show => {
            show_window(hwnd);
            LRESULT(0)
        }
        Outcome::TrayMenu => {
            tray_menu(cell, hwnd);
            LRESULT(0)
        }
        Outcome::Outs(outs) => {
            carry_out(cell, hwnd, outs);
            LRESULT(0)
        }
    }
}

/// Show the window, restoring it if minimized or unhiding it from the tray, and
/// bring it to the front.
fn show_window(hwnd: HWND) {
    // SAFETY: hwnd is valid.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// The tray icon's menu: show the window, always on top, exit.
fn tray_menu(cell: &RefCell<State>, hwnd: HWND) {
    let topmost = cell
        .try_borrow()
        .is_ok_and(|st| st.shell.app().settings().always_on_top);
    let p = Tray::cursor();
    // SAFETY: the menu is destroyed before returning; labels are static. The
    // window is brought to the foreground first, as a tray menu must be, so it
    // closes when the pointer leaves it.
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, 1, w!("Open open-task"));
        let flags = if topmost {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(menu, flags, 2, w!("Always on top"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("Exit"));
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let chosen = TrackPopupMenuEx(menu, flags.0, p.x, p.y, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0
    };
    match chosen {
        1 => show_window(hwnd),
        2 => {
            let mut outs = Vec::new();
            if let Ok(mut st) = cell.try_borrow_mut() {
                st.shell.toggle_always_on_top(&mut outs);
            }
            carry_out(cell, hwnd, outs);
        }
        3 => post_close(hwnd),
        _ => {}
    }
}

/// Make the settings that live in the shell take effect: the window's always-on-top
/// state and how charts are drawn.
fn apply_settings(cell: &RefCell<State>, hwnd: HWND, settings: &Settings) {
    // SAFETY: hwnd is valid; the flags leave position and size alone.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(if settings.always_on_top {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            }),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    if let Ok(mut st) = cell.try_borrow_mut() {
        if let Some(gfx) = st.gfx.as_mut() {
            gfx.set_chart_drawing(settings.chart_drawing);
        }
    }
    invalidate(hwnd);
}

/// Tell the user something in a message box.
fn notify(hwnd: HWND, text: &str) {
    message_box(hwnd, text, MB_ICONWARNING);
}

fn message_box(hwnd: HWND, text: &str, icon: MESSAGEBOX_STYLE) {
    let text = HSTRING::from(text);
    // SAFETY: strings outlive the call; hwnd is valid.
    unsafe {
        MessageBoxW(Some(hwnd), &text, w!("open-task"), MB_OK | icon);
    }
}

#[allow(clippy::too_many_lines)]
fn handle_message(st: &mut State, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Outcome {
    match msg {
        // A Task Manager stand-in asks this window to come forward. Not while it
        // is closing: then the stand-in opens its own.
        m if m == st.raise_message => {
            if st.shell.closing() == Closing::No {
                Outcome::Raise
            } else {
                Outcome::Done(LRESULT(0))
            }
        }
        WM_PAINT => {
            // The frame is drawn through Direct2D, never a DC: validating the update
            // region is all `BeginPaint`/`EndPaint` did for us, and they took and
            // released a DC every frame (about a twentieth of the app's CPU in a
            // profile at 60 Hz).
            // SAFETY: hwnd is valid.
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            repaint(st);
            Outcome::Done(LRESULT(0))
        }
        WM_ERASEBKGND => Outcome::Done(LRESULT(1)),
        WM_SIZE => {
            if wparam.0 as u32 == SIZE_MINIMIZED {
                // With "Hide when minimized" on, the tray icon is the way back.
                return if st.shell.app().settings().hide_when_minimized
                    && st.shell.closing() == Closing::No
                {
                    Outcome::Hide
                } else {
                    Outcome::Done(LRESULT(0))
                };
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
            let c = if st.picking {
                st.cursors.cross
            } else {
                match st.shell.app().cursor() {
                    Cursor::Arrow => st.cursors.arrow,
                    Cursor::ResizeColumn => st.cursors.size_we,
                    Cursor::Text => st.cursors.ibeam,
                }
            };
            // SAFETY: a cursor loaded in `run`.
            unsafe {
                SetCursor(Some(c));
            }
            Outcome::Done(LRESULT(1))
        }
        WM_MOUSEMOVE if st.picking => {
            // SAFETY: a cursor loaded in `run`.
            unsafe {
                SetCursor(Some(st.cursors.cross));
            }
            Outcome::Done(LRESULT(0))
        }
        WM_LBUTTONUP if st.picking => {
            // The crosshair lands: the process of the window under the pointer.
            st.picking = false;
            let mut p = POINT::default();
            // SAFETY: capture is ours to release; the out-struct is valid.
            unsafe {
                let _ = ReleaseCapture();
                let _ = GetCursorPos(&raw mut p);
                SetCursor(Some(st.cursors.arrow));
            }
            if let Some(pid) = actions::pid_at(p) {
                if st.shell.app_mut().select_pid(pid) {
                    invalidate(hwnd);
                } else {
                    return Outcome::Notify(format!(
                        "The window belongs to PID {pid}, which is not in the table yet."
                    ));
                }
            }
            Outcome::Done(LRESULT(0))
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
                // Escape with the crosshair out puts it away.
                v if v == VK_ESCAPE.0 && st.picking => {
                    st.picking = false;
                    // SAFETY: capture is ours to release.
                    unsafe {
                        let _ = ReleaseCapture();
                        SetCursor(Some(st.cursors.arrow));
                    }
                    return Outcome::Done(LRESULT(0));
                }
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
                VK_M if ctrl => UiEvent::Command(Command::SetView(ot_ui::ViewMode::Map)),
                VK_H if ctrl => UiEvent::Command(Command::SetView(ot_ui::ViewMode::History)),
                VK_F if ctrl => UiEvent::Command(Command::Find),
                // Ctrl+N: the Run dialog, "Run new task".
                VK_N if ctrl => UiEvent::Command(Command::RunTask),
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
        WM_APP_WAKE => {
            let mut outs = Vec::new();
            st.shell.drain(&mut outs);
            Outcome::Outs(outs)
        }
        WM_APP_ICON => {
            // SAFETY: the pointer was made by `Box::into_raw` in the icon worker
            // and is delivered exactly once.
            let pixels = unsafe { Box::from_raw(lparam.0 as *mut icons::IconPixels) };
            if let Some(gfx) = st.gfx.as_mut() {
                if pixels.pbgra.is_empty() {
                    gfx.image_missing(&pixels.path);
                } else {
                    gfx.add_image(&pixels.path, pixels.width, pixels.height, &pixels.pbgra);
                }
                invalidate(hwnd);
            }
            Outcome::Done(LRESULT(0))
        }
        WM_APP_TRAY => match (lparam.0 & 0xFFFF) as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => Outcome::Show,
            WM_RBUTTONUP => Outcome::TrayMenu,
            _ => Outcome::Done(LRESULT(0)),
        },
        WM_TIMER if wparam.0 == TIMER_FRAME => {
            // SAFETY: hwnd is valid; the timer is ours.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_FRAME);
            }
            invalidate(hwnd);
            Outcome::Done(LRESULT(0))
        }
        WM_TIMER if wparam.0 == TIMER_REFRESH => {
            // SAFETY: hwnd is valid; the timer is ours.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_REFRESH);
            }
            st.shell.refresh();
            Outcome::Done(LRESULT(0))
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
        WM_CLOSE => close(st),
        // The end of a move or resize by the user. Saving here as well as on close
        // keeps the place through a crash, and keeps a monitor layout's place
        // from being overwritten by where Windows moves the window when the
        // layout changes.
        WM_EXITSIZEMOVE => {
            placement::save(hwnd);
            Outcome::Default
        }
        WM_TIMER if wparam.0 == TIMER_UPDATE => {
            // After the first tick, look hourly; the shell decides whether a day
            // has passed since the last check.
            // SAFETY: hwnd is valid; re-arming an existing timer replaces it.
            unsafe {
                SetTimer(Some(hwnd), TIMER_UPDATE, CHECK_TICK_MS, None);
            }
            st.shell.update_tick();
            Outcome::Done(LRESULT(0))
        }
        // Restart Manager (the installer replacing this exe) and logoff end the
        // session this way. Close as the user would; posting keeps it out of this
        // handler, and the message loop ends with the window.
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                tracing::info!(reason = lparam.0, "session ending; closing");
                st.shell.set_closing(Closing::SessionEnding);
                post_close(hwnd);
            }
            Outcome::Done(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            // Sent for any system setting; re-reading these is cheap. The Settings
            // page shows Windows' animation effects, so a change repaints.
            let animations = prefs::system_animations();
            if animations != st.shell.app().system_animations() {
                st.shell.app_mut().set_system_animations(animations);
                invalidate(hwnd);
            }
            if st.theme_pref == ThemePreference::System {
                let dark = system_prefers_dark();
                if dark != st.dark {
                    st.dark = dark;
                    apply_title_bar_theme(hwnd, dark);
                    st.shell.app_mut().set_theme(theme_for(dark));
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

/// Feed an event to the shell and carry out what it reports.
fn dispatch(cell: &RefCell<State>, hwnd: HWND, ev: UiEvent) {
    let mut outs = Vec::new();
    {
        let Ok(mut st) = cell.try_borrow_mut() else {
            return;
        };
        st.shell.handle(ev, &mut outs);
    }
    carry_out(cell, hwnd, outs);
}

/// Carry out what the shell reported, in order, with the state unborrowed. An
/// answer to a menu or a question goes back to the shell, which may report more;
/// those are carried out in turn until there is nothing left.
fn carry_out(cell: &RefCell<State>, hwnd: HWND, outs: Vec<Out>) {
    let mut queue = VecDeque::from(outs);
    let mut more = Vec::new();
    while let Some(out) = queue.pop_front() {
        match out {
            Out::Repaint => invalidate(hwnd),
            Out::Title(title) => {
                // SAFETY: hwnd is valid; the string outlives the call.
                unsafe {
                    let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
                }
            }
            Out::Notice(notice) => show_notice(hwnd, &notice),
            Out::Ask(ask) => answer(cell, hwnd, ask, &mut more),
            Out::Do(effect) => host_effect(cell, hwnd, effect),
            Out::Settings(settings) => apply_settings(cell, hwnd, &settings),
            Out::Meters { cpu, memory } => {
                if let Ok(mut st) = cell.try_borrow_mut() {
                    if let Some(tray) = &mut st.tray {
                        tray.update(cpu, memory);
                    }
                }
            }
            Out::RefreshIn(after) => {
                let ms = u32::try_from(after.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: hwnd is valid; re-arming replaces the timer.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_REFRESH, ms, None);
                }
            }
            Out::Close => post_close(hwnd),
        }
        queue.extend(more.drain(..));
    }
}

fn show_notice(hwnd: HWND, notice: &Notice) {
    let icon = match notice.kind {
        NoticeKind::Info => MB_ICONINFORMATION,
        NoticeKind::Warning => MB_ICONWARNING,
        NoticeKind::Error => MB_ICONERROR,
    };
    message_box(hwnd, &notice.text, icon);
}

/// Ask the user, modally, and hand the answer to the shell.
fn answer(cell: &RefCell<State>, hwnd: HWND, ask: Ask, more: &mut Vec<Out>) {
    match ask {
        Ask::Menu { at, nodes } => {
            let (dpi, dark) = cell
                .try_borrow()
                .map_or((96.0, false), |st| (st.dpi, st.dark));
            let mut p = to_px_point(at, dpi);
            // SAFETY: hwnd is valid; POINT is a plain in-out struct.
            unsafe {
                let _ = ClientToScreen(hwnd, &raw mut p);
            }
            let chosen = actions::show_menu(hwnd, p, &nodes, dpi.round() as u32, dark);
            let at = pointer_over(hwnd, dpi);
            if let Ok(mut st) = cell.try_borrow_mut() {
                st.shell.menu_closed(chosen, at, more);
            }
        }
        Ask::Confirm { question, then } => {
            if confirm(hwnd, &question) {
                if let Ok(mut st) = cell.try_borrow_mut() {
                    st.shell.confirmed(then, more);
                }
            }
        }
        Ask::RecordPath => {
            if let Some(path) = actions::save_recording_dialog(hwnd) {
                if let Ok(mut st) = cell.try_borrow_mut() {
                    st.shell.record_to(&path, more);
                }
            }
        }
        Ask::Run { initial } => run_task(cell, hwnd, &initial),
    }
}

/// What the shell leaves to the window.
fn host_effect(cell: &RefCell<State>, hwnd: HWND, effect: HostEffect) {
    match effect {
        HostEffect::OpenFileLocation(path) => open_file_location(hwnd, &path),
        HostEffect::SwitchTo(handle) => actions::switch_to(handle),
        HostEffect::Minimize => {
            // "Hide when minimized" applies as for any minimize.
            // SAFETY: hwnd is valid.
            unsafe {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
        }
        HostEffect::OpenUrl(url) => actions::open(hwnd, &url),
        HostEffect::Properties(path) => actions::properties(hwnd, &path),
        HostEffect::CopyText(text) => {
            if let Err(e) = actions::copy_text(hwnd, &text) {
                notify(hwnd, &format!("Could not copy to the clipboard.\n\n{e}"));
            }
        }
        HostEffect::RunAsAdministrator => match actions::relaunch_elevated(hwnd) {
            Ok(true) => {
                tracing::info!("an elevated copy is starting; closing this one");
                if let Ok(mut st) = cell.try_borrow_mut() {
                    st.shell.set_closing(Closing::Relaunching);
                }
                post_close(hwnd);
            }
            Ok(false) => tracing::info!("permission to run as administrator was not given"),
            Err(e) => notify(hwnd, &format!("Could not start as administrator.\n\n{e}")),
        },
        HostEffect::PickWindow => {
            if let Ok(mut st) = cell.try_borrow_mut() {
                st.picking = true;
                // SAFETY: hwnd is valid; the cursor was loaded in `run`.
                unsafe {
                    SetCapture(hwnd);
                    SetCursor(Some(st.cursors.cross));
                }
            }
        }
        HostEffect::OpenServices => actions::open(hwnd, "services.msc"),
        HostEffect::OpenInstallLocation {
            location,
            uninstall,
        } => match actions::install_folder(location.as_deref(), uninstall.as_deref()) {
            Some(folder) => actions::open(hwnd, &folder.to_string_lossy()),
            None => notify(hwnd, "The program's folder could not be worked out."),
        },
        HostEffect::ReplaceTaskManager(on) => replace_task_manager(cell, hwnd, on),
    }
}

/// A yes/no question with No as the default.
fn confirm(hwnd: HWND, question: &str) -> bool {
    let text = HSTRING::from(question);
    // SAFETY: strings outlive the call; hwnd is valid.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            &text,
            w!("open-task"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    };
    answer == IDYES
}

/// The Run dialog, then the program it names: through the shell as administrator
/// when asked, else as a plain process, with the shell as the fallback for a
/// document or a URL.
fn run_task(cell: &RefCell<State>, hwnd: HWND, initial: &str) {
    let Some(request) = run_dialog::ask(hwnd, initial) else {
        return;
    };
    if let Ok(mut st) = cell.try_borrow_mut() {
        st.shell.set_last_run(&request.command);
    }
    let (file, params) = actions::split_command(&request.command);
    let result = if request.elevated {
        actions::shell_start(hwnd, &file, &params, None, true)
    } else {
        actions::launch(&request.command, None)
            .or_else(|_| actions::shell_start(hwnd, &file, &params, None, false))
    };
    if let Err(e) = result {
        notify(
            hwnd,
            &format!("Could not start {}.\n\n{e}", request.command),
        );
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
    let mut tm = st.shell.app().task_manager().clone();
    tm.replacement = task_manager::replacement();
    st.shell.app_mut().set_task_manager(tm)
}

fn set_task_manager_pending(st: &mut State, pending: bool) {
    let mut tm = st.shell.app().task_manager().clone();
    tm.pending = pending;
    let _ = st.shell.app_mut().set_task_manager(tm);
}

/// `WM_CLOSE`. Saves where the window is, then lets the shell decide: with an
/// update waiting to install as the app closes, the window hides and Setup runs
/// without relaunching the app (the next start is the new version). The process
/// stays until Setup's Restart Manager ends it, which keeps the installer locked
/// through Setup's elevation, as a click does. Anything else closes as usual.
fn close(st: &mut State) -> Outcome {
    placement::save(st.hwnd);
    st.tray = None;
    match st.shell.close() {
        CloseOutcome::Close => Outcome::Default,
        CloseOutcome::Hide => Outcome::Hide,
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
