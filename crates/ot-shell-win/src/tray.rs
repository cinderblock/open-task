//! The notification-area icon: a live CPU meter, as Task Manager's.
//!
//! The icon is drawn, not loaded: a 16 by 16 bitmap with a bar whose height is the
//! CPU utilization, in Task Manager's green on a dark field, remade whenever the
//! rounded percentage changes (once a second at most, a few microseconds). The
//! tooltip carries the numbers. A left click shows the window, restoring it if it
//! was minimized or hidden; a right click offers the same, "Always on top", and
//! "Exit". With "Hide when minimized" on, minimizing hides the window and the
//! icon is the way back.

use std::ffi::c_void;
use std::fmt::Write as _;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

const SIZE: i32 = 16;
/// Task Manager's tray colors: a dark field, a green bar.
const FIELD: u32 = 0xFF_1E_1E_1E;
const BAR: u32 = 0xFF_2E_C4_6A;
const FRAME: u32 = 0xFF_55_55_55;

/// The icon as it stands.
#[derive(Debug)]
pub(crate) struct Tray {
    hwnd: HWND,
    message: u32,
    icon: Option<HICON>,
    /// The percentage drawn, so an unchanged value draws nothing.
    shown: Option<u32>,
    added: bool,
    tip: String,
}

impl Tray {
    /// An icon for `hwnd`, posting `message` for mouse events on it. Added to the
    /// tray on the first [`Tray::update`].
    pub fn new(hwnd: HWND, message: u32) -> Self {
        Self {
            hwnd,
            message,
            icon: None,
            shown: None,
            added: false,
            tip: String::new(),
        }
    }

    /// Draw the meter for `cpu` percent and set the tooltip. Cheap when the
    /// rounded value has not changed.
    pub fn update(&mut self, cpu: f32, memory_percent: Option<f32>) {
        let pct = cpu.clamp(0.0, 100.0).round() as u32;
        let tip_changed = {
            let mut tip = String::new();
            let _ = write!(tip, "open-task\nCPU {pct}%");
            if let Some(m) = memory_percent {
                let _ = write!(tip, ", memory {m:.0}%");
            }
            if tip == self.tip {
                false
            } else {
                self.tip = tip;
                true
            }
        };
        if self.shown == Some(pct) && self.added && !tip_changed {
            return;
        }
        if self.shown != Some(pct) || self.icon.is_none() {
            if let Some(icon) = draw(pct) {
                if let Some(old) = self.icon.replace(icon) {
                    // SAFETY: made by CreateIconIndirect, no longer shown once the
                    // next notify call replaces it below.
                    unsafe {
                        let _ = DestroyIcon(old);
                    }
                }
                self.shown = Some(pct);
            }
        }
        let Some(icon) = self.icon else {
            return;
        };
        let mut data = self.data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.hIcon = icon;
        data.uCallbackMessage = self.message;
        let tip: Vec<u16> = self.tip.encode_utf16().take(127).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        // SAFETY: the structure is fully initialized and its size set.
        let ok = unsafe {
            Shell_NotifyIconW(
                if self.added { NIM_MODIFY } else { NIM_ADD },
                &raw const data,
            )
        };
        if ok.as_bool() {
            self.added = true;
        } else if !self.added {
            tracing::warn!("could not add the tray icon");
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    /// The pointer's position for a menu opened from the icon.
    pub fn cursor() -> POINT {
        let mut p = POINT::default();
        // SAFETY: valid out-struct.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&raw mut p);
        }
        p
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.added {
            let data = self.data();
            // SAFETY: removes the icon this window added.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &raw const data);
            }
        }
        if let Some(icon) = self.icon.take() {
            // SAFETY: ours; destroyed once.
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

/// A 16 by 16 icon with a bar `pct` percent of the way up.
fn draw(pct: u32) -> Option<HICON> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE,
            // Negative: top-down rows.
            biHeight: -SIZE,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: the header describes a 32-bit top-down bitmap; `bits` receives its
    // pixel memory, which belongs to the bitmap and is written below while it
    // lives. The screen DC is released; the bitmaps are deleted once the icon
    // (which copies them) is made.
    unsafe {
        let dc = GetDC(None);
        let color: HBITMAP = CreateDIBSection(
            Some(dc),
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        )
        .ok()?;
        ReleaseDC(None, dc);
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        let px = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (SIZE * SIZE) as usize);
        let bar_h =
            ((f64::from(pct) / 100.0 * f64::from(SIZE - 2)).round() as i32).clamp(0, SIZE - 2);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let edge = x == 0 || y == 0 || x == SIZE - 1 || y == SIZE - 1;
                let in_bar = !edge && y >= SIZE - 1 - bar_h;
                px[(y * SIZE + x) as usize] = if edge {
                    FRAME
                } else if in_bar {
                    BAR
                } else {
                    FIELD
                };
            }
        }
        // The mask is ignored for a 32-bit color bitmap but must exist.
        let mask = CreateBitmap(SIZE, SIZE, 1, 1, None);
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&raw const ii).ok();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

/// For callers that need the message constant's type.
#[allow(dead_code)]
const _: PCWSTR = PCWSTR::null();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_draw_at_every_level() {
        for pct in [0, 1, 50, 99, 100] {
            let icon = draw(pct).expect("icon");
            // SAFETY: ours.
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}
