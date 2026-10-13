//! Where the window was, per monitor layout.
//!
//! A layout is the set of connected monitors' rectangles and DPIs, so a laptop on
//! its own and the same laptop docked to two screens each keep their own place.
//! Monitor device names (`\\.\DISPLAY3`) are left out of it, since Windows
//! renumbers them, and so are work areas, since moving the taskbar is not a new
//! layout. The place is `GetWindowPlacement`'s normal rectangle and whether the
//! window was maximized, kept by `prefs` under the layout's key.

use std::ffi::c_void;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, MonitorFromRect, HDC, HMONITOR, MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::Threading::{GetStartupInfoW, STARTF_USESHOWWINDOW, STARTUPINFOW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SetWindowPlacement, ShowWindow, SHOW_WINDOW_CMD, SW_FORCEMINIMIZE, SW_HIDE,
    SW_MINIMIZE, SW_SHOWDEFAULT, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE,
    WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
};

use crate::prefs;

/// One monitor of a layout: its rectangle in virtual-screen pixels and its DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Monitor {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    dpi: u32,
}

/// Where the window was: the normal (restored) rectangle in workspace
/// coordinates, as `GetWindowPlacement` reports it, and whether it was maximized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    maximized: bool,
}

impl Placement {
    fn encode(self) -> String {
        format!(
            "{},{},{},{},{}",
            self.left,
            self.top,
            self.right,
            self.bottom,
            u8::from(self.maximized)
        )
    }

    /// `None` for anything [`Placement::encode`] did not write, or an empty or
    /// inside-out rectangle.
    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split(',').map(str::trim);
        let mut next = || parts.next()?.parse::<i32>().ok();
        let (left, top, right, bottom, maximized) = (next()?, next()?, next()?, next()?, next()?);
        if parts.next().is_some() || right <= left || bottom <= top || maximized > 1 {
            return None;
        }
        Some(Self {
            left,
            top,
            right,
            bottom,
            maximized: maximized == 1,
        })
    }
}

/// The key a layout's placement is kept under: each monitor as
/// `left,top,right,bottom@dpi`, in order, joined by `;`. The same monitors in the
/// same arrangement give the same key however Windows enumerates them.
fn layout_key(mut monitors: Vec<Monitor>) -> String {
    monitors.sort_unstable();
    monitors
        .iter()
        .map(|m| format!("{},{},{},{}@{}", m.left, m.top, m.right, m.bottom, m.dpi))
        .collect::<Vec<_>>()
        .join(";")
}

/// The connected monitors, as Windows enumerates them.
fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn each(
        monitor: HMONITOR,
        _: HDC,
        rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the `Vec` passed below, alive and unaliased for the
        // whole enumeration; `rect` is the monitor's rectangle, valid for this call.
        let (list, r) = unsafe { (&mut *(data.0 as *mut Vec<Monitor>), *rect) };
        let (mut dpi, mut dpi_y) = (0u32, 0u32);
        // SAFETY: a monitor handle Windows just gave us; the out-pointers are locals.
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut dpi_y) }
            .is_err()
        {
            dpi = 0;
        }
        list.push(Monitor {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
            dpi,
        });
        BOOL(1)
    }
    let mut list = Vec::new();
    // SAFETY: the callback only runs during this call, while `list` is alive.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(each),
            LPARAM((&raw mut list).cast::<c_void>() as isize),
        )
    };
    list
}

/// The current layout's key, or `None` if no monitor could be found.
fn current_layout() -> Option<String> {
    let monitors = monitors();
    (!monitors.is_empty()).then(|| layout_key(monitors))
}

/// Put a new, still hidden window where it was last left on this monitor layout.
/// Returns whether it was maximized, for [`show`].
///
/// Call before the window's state is attached: crossing to a monitor of another
/// DPI sends `WM_DPICHANGED`, which must go to `DefWindowProc` (which leaves the
/// size alone) rather than rescale the saved size to Windows' suggestion.
pub fn restore(hwnd: HWND) -> bool {
    let Some(layout) = current_layout() else {
        return false;
    };
    let Some(saved) = prefs::load_placement(&layout)
        .as_deref()
        .and_then(Placement::decode)
    else {
        return false;
    };
    let rect = RECT {
        left: saved.left,
        top: saved.top,
        right: saved.right,
        bottom: saved.bottom,
    };
    // The layout matched, so this should always be on a monitor; the check only
    // guards against a value edited by hand.
    // SAFETY: the rect is a local.
    if unsafe { MonitorFromRect(&raw const rect, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return false;
    }
    let wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: SW_HIDE.0 as u32,
        rcNormalPosition: rect,
        ..Default::default()
    };
    // SAFETY: hwnd is valid; the struct is a local with its length set.
    if let Err(e) = unsafe { SetWindowPlacement(hwnd, &raw const wp) } {
        tracing::warn!(error = %e, "could not restore the window placement");
        return false;
    }
    tracing::debug!(layout, placement = ?saved, "restored the window placement");
    saved.maximized
}

/// Show the window for the first time. A maximized window comes back maximized,
/// unless whatever started the app asked for it minimized (a shortcut's "Run:
/// Minimized"), when it starts minimized and restores to maximized.
pub fn show(hwnd: HWND, maximized: bool) {
    if !maximized {
        // SAFETY: hwnd is valid.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
        }
        return;
    }
    let mut si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: the struct is a local with its size set.
    unsafe { GetStartupInfoW(&raw mut si) };
    let asked = SHOW_WINDOW_CMD(i32::from(si.wShowWindow));
    let minimized = si.dwFlags.contains(STARTF_USESHOWWINDOW)
        && [
            SW_MINIMIZE,
            SW_SHOWMINIMIZED,
            SW_SHOWMINNOACTIVE,
            SW_FORCEMINIMIZE,
        ]
        .contains(&asked);
    if !minimized {
        // SAFETY: hwnd is valid.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        }
        return;
    }
    let mut wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: hwnd is valid; the struct is a local with its length set.
    unsafe {
        if GetWindowPlacement(hwnd, &raw mut wp).is_ok() {
            wp.showCmd = SW_SHOWMINNOACTIVE.0 as u32;
            wp.flags = WPF_RESTORETOMAXIMIZED;
            let _ = SetWindowPlacement(hwnd, &raw const wp);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
        }
    }
}

/// Remember where the window is now, for the current monitor layout. A minimized
/// (or hidden to the tray) window is remembered by where it restores to.
pub fn save(hwnd: HWND) {
    let Some(layout) = current_layout() else {
        return;
    };
    let mut wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: hwnd is valid; the struct is a local with its length set.
    if let Err(e) = unsafe { GetWindowPlacement(hwnd, &raw mut wp) } {
        tracing::warn!(error = %e, "could not read the window placement");
        return;
    }
    let r = wp.rcNormalPosition;
    let maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
        || (wp.showCmd == SW_SHOWMINIMIZED.0 as u32 && wp.flags.contains(WPF_RESTORETOMAXIMIZED));
    let placement = Placement {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
        maximized,
    };
    if r.right > r.left && r.bottom > r.top {
        prefs::save_placement(&layout, &placement.encode());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: i32, top: i32, right: i32, bottom: i32, dpi: u32) -> Monitor {
        Monitor {
            left,
            top,
            right,
            bottom,
            dpi,
        }
    }

    #[test]
    fn layout_key_ignores_enumeration_order() {
        let a = monitor(0, 0, 2560, 1440, 144);
        let b = monitor(2560, 0, 4480, 1080, 96);
        assert_eq!(layout_key(vec![a, b]), layout_key(vec![b, a]));
        assert_eq!(
            layout_key(vec![b, a]),
            "0,0,2560,1440@144;2560,0,4480,1080@96"
        );
    }

    #[test]
    fn layout_key_tells_layouts_apart() {
        let laptop = monitor(0, 0, 1920, 1200, 120);
        let docked = [laptop, monitor(-2560, 0, 0, 1440, 96)];
        assert_ne!(layout_key(vec![laptop]), layout_key(docked.to_vec()));
        // The same screen at another scale is another layout: the saved size was
        // in its pixels.
        assert_ne!(
            layout_key(vec![laptop]),
            layout_key(vec![monitor(0, 0, 1920, 1200, 144)])
        );
    }

    #[test]
    fn placement_round_trips() {
        for p in [
            Placement {
                left: -2400,
                top: 40,
                right: -1220,
                bottom: 800,
                maximized: false,
            },
            Placement {
                left: 100,
                top: 100,
                right: 1280,
                bottom: 860,
                maximized: true,
            },
        ] {
            assert_eq!(Placement::decode(&p.encode()), Some(p));
        }
    }

    #[test]
    fn placement_rejects_bad_values() {
        for text in [
            "",
            "1,2,3",
            "1,2,3,4,5",
            "0,0,100,100,0,9",
            "100,0,50,100,0",
            "0,100,100,100,0",
            "a,0,100,100,0",
        ] {
            assert_eq!(Placement::decode(text), None, "{text:?}");
        }
    }
}
