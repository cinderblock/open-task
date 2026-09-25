//! Win32 window, message loop, and translation of messages into UI events.

use std::cell::RefCell;
use std::ffi::c_void;

use ot_core::{Sampler, SamplerConfig};
use ot_paint::{DisplayList, Point, Size};
use ot_probe::SystemProbe;
use ot_ui::{App, Key, MouseButton, Theme, UiEvent};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, ScreenToClient, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWLP_USERDATA, IDC_ARROW, MSG, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_SHOWDEFAULT, WHEEL_DELTA, WM_APP, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETTINGCHANGE, WM_SIZE, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP,
    WS_OVERLAPPEDWINDOW,
};

use crate::gfx::Gfx;
use crate::{ShellError, ShellOptions, ThemePreference};

/// Posted by the sampler thread after each publish.
const WM_APP_SNAPSHOT: u32 = WM_APP + 1;

const CLASS_NAME: PCWSTR = w!("OpenTaskMainWindow");
const INITIAL_SIZE: (i32, i32) = (1180, 760);

struct State {
    hwnd: HWND,
    gfx: Option<Gfx>,
    app: App,
    dl: DisplayList,
    sampler: Option<Sampler>,
    dpi: f32,
    tracking_leave: bool,
    backdrop: bool,
    theme_pref: ThemePreference,
    /// Whether the current theme is dark. Follows the system when `theme_pref` says so.
    dark: bool,
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
    // SAFETY: plain process-wide setting; failure (already set) is harmless.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // SAFETY: all structures are fully initialized; the class name is a static wide string.
    let hwnd = unsafe {
        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(win("GetModuleHandleW"))?.0);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(win("LoadCursorW"))?,
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
    let size_px = client_size(hwnd);
    let gfx = Gfx::new(hwnd, size_px, dpi).map_err(win("Gfx::new"))?;

    let mut app = App::new(theme_for(dark));
    app.set_backdrop(backdrop);
    app.handle(UiEvent::Resize(to_dips_size(size_px, dpi)));

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
        dpi,
        tracking_leave: false,
        backdrop,
        theme_pref: options.theme,
        dark,
    }));
    let state_ptr = Box::into_raw(state);
    // SAFETY: hwnd is valid; the pointer stays alive until after the loop below.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
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

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    let v = lp.0 as u32;
    (
        i32::from((v as u16).cast_signed()),
        i32::from(((v >> 16) as u16).cast_signed()),
    )
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
}

#[allow(clippy::too_many_lines)]
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(cell) = state_from(hwnd) else {
        // SAFETY: standard default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // A message that arrives while we are already inside a handler (Win32 re-enters
    // the procedure from calls like SetWindowPos) gets default handling rather than
    // a RefCell panic. Handlers below are written so that matters as little as
    // possible: state is updated before any re-entrant call is made.
    let Ok(mut guard) = cell.try_borrow_mut() else {
        // SAFETY: standard default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let st: &mut State = &mut guard;

    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: validates the update region; we do not draw with the HDC.
            unsafe {
                let _ = BeginPaint(hwnd, &raw mut ps);
                let _ = EndPaint(hwnd, &raw const ps);
            }
            repaint(st);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if wparam.0 as u32 != SIZE_MINIMIZED {
                let (w, h) = lparam_xy(lparam);
                let px = (w.max(0) as u32, h.max(0) as u32);
                if let Some(g) = st.gfx.as_mut() {
                    if let Err(e) = g.resize(px) {
                        tracing::error!(error = %e, "swap chain resize failed");
                        st.gfx = None;
                    }
                }
                st.app.handle(UiEvent::Resize(to_dips_size(px, st.dpi)));
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let new_dpi = ((wparam.0 >> 16) & 0xFFFF) as f32;
            st.dpi = new_dpi;
            if let Some(g) = st.gfx.as_mut() {
                g.set_dpi(new_dpi);
            }
            // SAFETY: lparam is a pointer to the suggested RECT for this message.
            let r = unsafe { *(lparam.0 as *const RECT) };
            // SetWindowPos re-enters with WM_SIZE; release our borrow first so that
            // handler runs instead of being skipped.
            drop(guard);
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
            invalidate(hwnd);
            LRESULT(0)
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
            if st
                .app
                .handle(UiEvent::MouseMove(to_dips_point(x, y, st.dpi)))
            {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            st.tracking_leave = false;
            if st.app.handle(UiEvent::MouseLeave) {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP => {
            let (x, y) = lparam_xy(lparam);
            let at = to_dips_point(x, y, st.dpi);
            let button = if msg == WM_LBUTTONDOWN || msg == WM_LBUTTONUP {
                MouseButton::Left
            } else {
                MouseButton::Right
            };
            let ev = if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
                UiEvent::MouseDown { at, button }
            } else {
                UiEvent::MouseUp { at, button }
            };
            if st.app.handle(ev) {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta =
                f32::from((((wparam.0 >> 16) & 0xFFFF) as u16).cast_signed()) / WHEEL_DELTA as f32;
            let (sx, sy) = lparam_xy(lparam);
            let mut p = POINT { x: sx, y: sy };
            // SAFETY: hwnd is valid; POINT is a plain in-out struct.
            unsafe {
                let _ = ScreenToClient(hwnd, &raw mut p);
            }
            let at = to_dips_point(p.x, p.y, st.dpi);
            // Wheel away from the user is positive and scrolls content up.
            if st.app.handle(UiEvent::Wheel { at, lines: -delta }) {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            let key = match vk {
                v if v == VK_UP.0 => Some(Key::Up),
                v if v == VK_DOWN.0 => Some(Key::Down),
                v if v == VK_PRIOR.0 => Some(Key::PageUp),
                v if v == VK_NEXT.0 => Some(Key::PageDown),
                v if v == VK_HOME.0 => Some(Key::Home),
                v if v == VK_END.0 => Some(Key::End),
                _ => None,
            };
            match key {
                Some(k) => {
                    if st.app.handle(UiEvent::Key(k)) {
                        invalidate(hwnd);
                    }
                    LRESULT(0)
                }
                // SAFETY: standard default handling.
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }
        WM_APP_SNAPSHOT => {
            if let Some(s) = &st.sampler {
                if st.app.set_snapshot(s.latest()) {
                    invalidate(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // Sent for any system setting; re-reading one registry value is cheap.
            if st.theme_pref == ThemePreference::System {
                let dark = system_prefers_dark();
                if dark != st.dark {
                    st.dark = dark;
                    apply_title_bar_theme(hwnd, dark);
                    st.app.set_theme(theme_for(dark));
                    invalidate(hwnd);
                }
            }
            // SAFETY: standard default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: standard default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
