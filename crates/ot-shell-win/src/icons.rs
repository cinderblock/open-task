//! Program icons for the process table, extracted off the UI thread.
//!
//! The view draws a row's icon as an [`ot_paint::DrawCmd::Image`] naming the
//! program's path; the renderer keeps a bitmap per path and lists the paths it has
//! not seen ([`crate::gfx::Gfx::take_wanted`]). This module turns those into
//! pixels: one worker thread takes paths from a channel, asks the shell for the
//! file's icon (`SHGetFileInfoW` with `SHGFI_ICON`, the same icon Explorer shows,
//! which honors the file's own resources and the registered type), converts the
//! `HICON` to premultiplied BGRA through WIC (`CreateBitmapFromHICON` and a format
//! converter, which handles both alpha icons and the old masked kind), and posts the
//! result to the window as a boxed [`IconPixels`]. A file the shell cannot read
//! (another user's process from an unelevated open-task, a path that has gone) gets
//! the generic program icon instead (`SHGFI_USEFILEATTRIBUTES` on a made-up `.exe`
//! name), as Task Manager shows one.
//!
//! The large icon (32 px at 100 %) is asked for and scaled down to the row's 16
//! DIPs by Direct2D, which looks better at 125 % to 200 % than scaling the small
//! one up. A lookup costs a few milliseconds the first time a program is seen
//! (shell32 opens the file and reads its resources); the worker drains the channel
//! in the order the table asked, so the rows on screen come first.
//!
//! The shell APIs need COM; the worker initializes a single-threaded apartment of
//! its own.

use std::ffi::c_void;
use std::sync::mpsc::{self, Sender};

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, PostMessageW, HICON};

/// One extracted icon, posted to the window: `pbgra` is `width * height * 4`
/// bytes of premultiplied BGRA, or empty when there is no icon for the path.
#[derive(Debug)]
pub(crate) struct IconPixels {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub pbgra: Vec<u8>,
}

/// The worker's handle: ask for paths; results arrive as window messages.
#[derive(Debug)]
pub(crate) struct IconLoader {
    tx: Sender<String>,
}

impl IconLoader {
    /// Start the worker. Each result is posted to `hwnd` as `message` with a
    /// `Box<IconPixels>` in `lparam`, which the receiver owns.
    pub fn start(hwnd: HWND, message: u32) -> Self {
        let (tx, rx) = mpsc::channel::<String>();
        let hwnd_bits = hwnd.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("ot-icons".into())
            .spawn(move || {
                // SAFETY: a fresh thread with no COM state yet.
                let com = unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                };
                if com.is_err() {
                    tracing::warn!(hresult = ?com, "icon worker could not initialize COM");
                    return;
                }
                // SAFETY: COM is initialized on this thread.
                let wic: Option<IWICImagingFactory> = unsafe {
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                }
                .map_err(|e| tracing::warn!(error = %e, "no WIC factory; icons are off"))
                .ok();
                if let Some(wic) = wic {
                    for path in rx {
                        let pixels = extract(&wic, &path);
                        if !post(hwnd_bits, message, Box::new(pixels)) {
                            break;
                        }
                    }
                }
                // SAFETY: balances the CoInitializeEx above.
                unsafe { CoUninitialize() };
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the icon thread");
        }
        Self { tx }
    }

    /// Queue a path. Ignored once the worker is gone.
    pub fn request(&self, path: String) {
        let _ = self.tx.send(path);
    }
}

/// The icon for `path`, or the generic program icon when the file will not say.
fn extract(wic: &IWICImagingFactory, path: &str) -> IconPixels {
    let mut pixels = IconPixels {
        path: path.to_owned(),
        width: 0,
        height: 0,
        pbgra: Vec::new(),
    };
    let icon = file_icon(path, false).or_else(|| file_icon(path, true));
    if let Some(icon) = icon {
        if let Some((w, h, data)) = pixels_of(wic, icon) {
            pixels.width = w;
            pixels.height = h;
            pixels.pbgra = data;
        }
        // SAFETY: the icon came from SHGetFileInfoW, which gave us ownership.
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }
    pixels
}

/// Ask the shell for the file's large icon. With `generic`, the file is not
/// opened: the icon for its extension stands in.
fn file_icon(path: &str, generic: bool) -> Option<HICON> {
    let shown: &str = if generic { "program.exe" } else { path };
    let wide: Vec<u16> = shown.encode_utf16().chain(Some(0)).collect();
    let mut info = SHFILEINFOW::default();
    let mut flags = SHGFI_ICON | SHGFI_LARGEICON;
    let attributes = if generic {
        flags |= SHGFI_USEFILEATTRIBUTES;
        FILE_ATTRIBUTE_NORMAL
    } else {
        FILE_FLAGS_AND_ATTRIBUTES(0)
    };
    // SAFETY: the path is NUL-terminated and outlives the call; `info` is a
    // valid out-structure of the size given.
    let r = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            attributes,
            Some(&raw mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    (r != 0 && !info.hIcon.is_invalid()).then_some(info.hIcon)
}

/// Premultiplied BGRA pixels of an icon, with its size.
fn pixels_of(wic: &IWICImagingFactory, icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    convert(wic, icon)
        .map_err(|e| tracing::debug!(error = %e, "icon conversion failed"))
        .ok()
}

fn convert(wic: &IWICImagingFactory, icon: HICON) -> Result<(u32, u32, Vec<u8>)> {
    // SAFETY: `icon` is a valid icon handle for the duration; WIC copies what it
    // needs. The converter is initialized before it is read.
    unsafe {
        let source = wic.CreateBitmapFromHICON(icon)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let (mut width, mut height) = (0u32, 0u32);
        converter.GetSize(&raw mut width, &raw mut height)?;
        let stride = width * 4;
        let mut data = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut data)?;
        Ok((width, height, data))
    }
}

/// Post a boxed result; false if the window is gone (the box is reclaimed).
fn post(hwnd_bits: isize, message: u32, value: Box<IconPixels>) -> bool {
    let ptr = Box::into_raw(value);
    // SAFETY: posting to a window handle is thread-safe; if it fails the box was
    // not delivered and is still ours.
    let posted = unsafe {
        PostMessageW(
            Some(HWND(hwnd_bits as *mut c_void)),
            message,
            WPARAM(0),
            LPARAM(ptr as isize),
        )
    };
    if posted.is_err() {
        // SAFETY: not delivered, so this is the only owner.
        drop(unsafe { Box::from_raw(ptr) });
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_shell_has_an_icon_and_it_is_premultiplied_bgra() {
        // SAFETY: a test thread with no COM state.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .unwrap();
        // SAFETY: COM is initialized.
        let wic: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .expect("WIC factory");
        let p = extract(&wic, r"C:\Windows\explorer.exe");
        assert!(p.width >= 16 && p.height >= 16, "{}x{}", p.width, p.height);
        assert_eq!(p.pbgra.len(), (p.width * p.height * 4) as usize);
        // Premultiplied: no channel exceeds its alpha.
        assert!(p
            .pbgra
            .chunks_exact(4)
            .all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]));
        assert!(
            p.pbgra.chunks_exact(4).any(|px| px[3] > 0),
            "all transparent"
        );

        // A path that does not exist still gets the generic program icon.
        let g = extract(&wic, r"Z:\no\such\thing.exe");
        assert!(g.width >= 16, "generic icon expected");
        // SAFETY: balances the init above.
        unsafe { CoUninitialize() };
    }
}
