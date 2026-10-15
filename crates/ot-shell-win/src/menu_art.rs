//! What native popup menus are not given by default: dark mode, icons beside the
//! items, and a header naming what the menu is for.
//!
//! **Dark mode.** Win32 menus draw dark only once the process asks, through
//! uxtheme exports known by ordinal, not name ([`set_dark`]). They are
//! undocumented but stable since Windows 10 1903, and are what Explorer and
//! Notepad++ use. The app's own theme is forced, so the menus follow open-task's
//! setting rather than the system's.
//!
//! **Icons.** An item's [`Icon`] is the same Segoe Fluent Icons glyph the view
//! draws, rendered by Direct2D into a WIC bitmap (premultiplied BGRA, which is
//! what a menu's `hbmpItem` wants) and copied into a DIB section. Bitmaps are kept
//! per glyph, size and color for the life of the thread: a dozen of them.
//!
//! **Header.** The program's own icon, its title in bold, then lines of detail and
//! its path, dimmer: rendered the same way into one bitmap, which the menu shows as
//! a bitmap item that cannot be chosen ([`header_bitmap`]). Not owner-drawn: a menu
//! with an owner-drawn item loses its theme, dark mode with it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use ot_paint::Icon;
use windows::core::{w, HSTRING, PCSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::HiDpi::{GetSystemMetricsForDpi, SystemParametersInfoForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    NONCLIENTMETRICSW, SM_CXSMICON, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};
use windows_numerics::Vector2;

/// `PreferredAppMode` for `SetPreferredAppMode`.
const FORCE_DARK: i32 = 2;
const FORCE_LIGHT: i32 = 3;

/// Make the menus `hwnd` owns dark or light, whatever the system's setting.
pub(crate) fn set_dark(hwnd: HWND, dark: bool) {
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, bool) -> bool;
    type Refresh = unsafe extern "system" fn();
    // SAFETY: uxtheme is a system DLL, loaded from System32 only, and never
    // unloaded. The ordinals are looked up, not assumed: a build without them
    // returns None and menus stay light. Their signatures are as Windows has
    // had them since 1903.
    unsafe {
        let Ok(uxtheme) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        else {
            return;
        };
        let ordinal = |n: usize| GetProcAddress(uxtheme, PCSTR(n as *const u8));
        let (Some(refresh_policy), Some(allow), Some(set), Some(flush)) =
            (ordinal(104), ordinal(133), ordinal(135), ordinal(136))
        else {
            tracing::warn!("uxtheme has no dark mode exports; menus stay light");
            return;
        };
        let refresh_policy: Refresh = std::mem::transmute(refresh_policy);
        let allow: AllowDarkModeForWindow = std::mem::transmute(allow);
        let set: SetPreferredAppMode = std::mem::transmute(set);
        let flush: Refresh = std::mem::transmute(flush);
        // The process's choice, then the window's leave to use it, then the
        // cached menu theme dropped so the next menu is drawn anew.
        set(if dark { FORCE_DARK } else { FORCE_LIGHT });
        allow(hwnd, dark);
        refresh_policy();
        flush();
    }
}

/// The inks a menu is drawn in, on a dark menu or a light one.
struct Ink {
    /// An item's text: the header's title, an enabled item's icon.
    text: D2D1_COLOR_F,
    /// The header's other lines.
    detail: D2D1_COLOR_F,
    /// A disabled item's icon.
    off: D2D1_COLOR_F,
}

fn ink(dark: bool) -> Ink {
    let (v, text, detail, off) = if dark {
        (1.0, 0.95, 0.6, 0.35)
    } else {
        (0.0, 0.9, 0.6, 0.35)
    };
    let gray = |a: f32| D2D1_COLOR_F {
        r: v,
        g: v,
        b: v,
        a,
    };
    Ink {
        text: gray(text),
        detail: gray(detail),
        off: gray(off),
    }
}

/// `f` with the thread's [`Art`], made on first use; `None` if it cannot be.
fn with_art<T>(f: impl FnOnce(&mut Art) -> Option<T>) -> Option<T> {
    thread_local! {
        static ART: RefCell<Option<Art>> = const { RefCell::new(None) };
    }
    ART.with(|art| {
        let mut art = art.borrow_mut();
        if art.is_none() {
            *art = Art::new()
                .inspect_err(|e| tracing::warn!(error = %e, "menu art unavailable"))
                .ok();
        }
        f(art.as_mut()?)
    })
}

/// An icon's bitmap for a menu item at `dpi`, in the ink of an item enabled or
/// not, on a dark menu or a light one. Kept for the thread's life; `None` if it
/// could not be drawn, when the item goes without.
pub(crate) fn icon_bitmap(icon: Icon, dpi: u32, dark: bool, enabled: bool) -> Option<HBITMAP> {
    // SAFETY: plain query.
    let px = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(16) as u32;
    let ink = ink(dark);
    let color = if enabled { ink.text } else { ink.off };
    with_art(|art| {
        let key = (icon, px, color.r.to_bits(), color.a.to_bits());
        if let Some(&b) = art.icons.get(&key) {
            return Some(b);
        }
        let b = art
            .icon(icon, px, color)
            .inspect_err(|e| tracing::warn!(error = %e, ?icon, "menu icon"))
            .ok()?;
        art.icons.insert(key, b);
        Some(b)
    })
}

/// A bitmap this module made for one menu, deleted with it.
pub(crate) struct Owned(pub HBITMAP);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the bitmap is this value's alone, and its menu is gone.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// The header's widest text, in DIPs, before it is cut with an ellipsis (a path
/// in its middle, keeping the file's name).
const MAX_W: f32 = 420.0;
/// Its program icon, the space around it, and above and below the text, in DIPs.
const ICON: f32 = 32.0;
const PAD: f32 = 8.0;
const GAP: f32 = 10.0;
const V_PAD: f32 = 6.0;

/// The header of a menu: the program's icon (from `path`), `title` in bold, then
/// `lines` and `path`, dimmer; at `dpi`, for a dark menu or a light one.
pub(crate) fn header_bitmap(
    title: &str,
    lines: &[String],
    path: Option<&str>,
    dpi: u32,
    dark: bool,
) -> Option<Owned> {
    with_art(|art| {
        art.header(title, lines, path, dpi, dark)
            .inspect_err(|e| tracing::warn!(error = %e, "menu header"))
            .ok()
    })
}

/// What draws the menus' pictures, and the icons it has drawn.
struct Art {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
    icon_family: HSTRING,
    icons: HashMap<(Icon, u32, u32, u32), HBITMAP>,
}

impl Art {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: COM on the UI thread, as an apartment; already so is fine, and
        // it stays up for the life of the thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // SAFETY: factory creation with valid arguments.
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let icon_family = icon_family(&dwrite);
            Ok(Self {
                d2d,
                dwrite,
                wic,
                icon_family,
                icons: HashMap::new(),
            })
        }
    }

    /// A transparent `w` by `h` pixel canvas to draw on, one pixel a DIP (the
    /// sizes asked are already the DPI's).
    fn canvas(&self, w: u32, h: u32) -> windows::core::Result<(IWICBitmap, ID2D1RenderTarget)> {
        // SAFETY: plain creation calls with valid arguments.
        unsafe {
            let bitmap = self.wic.CreateBitmap(
                w,
                h,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt = self
                .d2d
                .CreateWicBitmapRenderTarget(&bitmap, &raw const props)?;
            // ClearType needs an opaque background to blend with; there is none.
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok((bitmap, rt))
        }
    }

    /// `icon` in `color`, `px` square.
    fn icon(&self, icon: Icon, px: u32, color: D2D1_COLOR_F) -> windows::core::Result<HBITMAP> {
        let (bitmap, rt) = self.canvas(px, px)?;
        // SAFETY: the target and what is drawn on it are this thread's.
        unsafe {
            let format = self.format(&self.icon_family, DWRITE_FONT_WEIGHT_NORMAL, px as f32)?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let brush = rt.CreateSolidColorBrush(&raw const color, None)?;
            let mut utf16 = [0u16; 2];
            let text = crate::gfx::glyph(icon).encode_utf16(&mut utf16);
            let rect = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: px as f32,
                bottom: px as f32,
            };
            rt.BeginDraw();
            rt.Clear(None);
            rt.DrawText(
                text,
                &format,
                &raw const rect,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            rt.EndDraw(None, None)?;
        }
        dib_of(&bitmap, px, px)
    }

    /// The menu's header ([`header_bitmap`]).
    fn header(
        &self,
        title: &str,
        lines: &[String],
        path: Option<&str>,
        dpi: u32,
        dark: bool,
    ) -> windows::core::Result<Owned> {
        let scale = dpi as f32 / 96.0;
        let (face, size) = menu_font(dpi);
        let ink = ink(dark);
        let bold = self.format(&face, DWRITE_FONT_WEIGHT_SEMI_BOLD, size)?;
        let normal = self.format(&face, DWRITE_FONT_WEIGHT_NORMAL, size)?;
        let max_w = MAX_W * scale;
        // Each line laid out with its ink: the title, the details, the path
        // (cut in its middle, at a backslash, should it not fit).
        let mut laid: Vec<(IDWriteTextLayout, D2D1_COLOR_F)> = Vec::new();
        laid.push((self.line(title, &bold, max_w, false)?, ink.text));
        for l in lines {
            laid.push((self.line(l, &normal, max_w, false)?, ink.detail));
        }
        if let Some(p) = path {
            laid.push((self.line(p, &normal, max_w, true)?, ink.detail));
        }
        let mut text_w = 0f32;
        let mut heights = Vec::with_capacity(laid.len());
        for (layout, _) in &laid {
            let mut m = DWRITE_TEXT_METRICS::default();
            // SAFETY: an out-pointer to a local.
            unsafe { layout.GetMetrics(&raw mut m)? };
            text_w = text_w.max(m.widthIncludingTrailingWhitespace.min(max_w));
            heights.push(m.height);
        }
        let text_h: f32 = heights.iter().sum();
        let icon = ICON * scale;
        let left = (PAD + GAP) * scale + icon;
        let w = (left + text_w + PAD * scale).ceil();
        let h = (text_h.max(icon) + 2.0 * V_PAD * scale).ceil();
        let (bitmap, rt) = self.canvas(w as u32, h as u32)?;
        // The program's icon, as the table shows it: for a process whose file
        // is unknown, the generic program icon, which a made-up name gets.
        let pixels = Some(crate::icons::extract(
            &self.wic,
            path.unwrap_or("program.exe"),
        ))
        .filter(|p| p.width > 0 && p.height > 0);
        // SAFETY: the target, and what is drawn on it, are this thread's; the
        // icon's pixels are `height` rows of `width * 4` bytes, copied by D2D.
        unsafe {
            rt.BeginDraw();
            rt.Clear(None);
            if let Some(p) = pixels {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let size = D2D_SIZE_U {
                    width: p.width,
                    height: p.height,
                };
                let image = rt.CreateBitmap(
                    size,
                    Some(p.pbgra.as_ptr().cast()),
                    p.width * 4,
                    &raw const props,
                )?;
                let top = ((h - icon) / 2.0).round();
                let x = (PAD * scale).round();
                let dest = D2D_RECT_F {
                    left: x,
                    top,
                    right: x + icon,
                    bottom: top + icon,
                };
                rt.DrawBitmap(
                    &image,
                    Some(&raw const dest),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
            let mut y = ((h - text_h) / 2.0).round();
            for ((layout, color), height) in laid.iter().zip(&heights) {
                let brush = rt.CreateSolidColorBrush(&raw const *color, None)?;
                rt.DrawTextLayout(
                    Vector2 { X: left, Y: y },
                    layout,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                y += height;
            }
            rt.EndDraw(None, None)?;
        }
        dib_of(&bitmap, w as u32, h as u32).map(Owned)
    }

    fn format(
        &self,
        family: &HSTRING,
        weight: DWRITE_FONT_WEIGHT,
        size: f32,
    ) -> windows::core::Result<IDWriteTextFormat> {
        // SAFETY: plain creation call.
        unsafe {
            self.dwrite.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
        }
    }

    /// One line of text, no wider than `max_w`: cut with an ellipsis at its end,
    /// or for a path, before its last component.
    fn line(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        max_w: f32,
        path: bool,
    ) -> windows::core::Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain creation calls; the trimming is a local.
        unsafe {
            let layout = self
                .dwrite
                .CreateTextLayout(&wide, format, max_w, f32::MAX)?;
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let trim = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: if path { u32::from('\\') } else { 0 },
                delimiterCount: u32::from(path),
            };
            let sign = self.dwrite.CreateEllipsisTrimmingSign(format)?;
            layout.SetTrimming(&raw const trim, &sign)?;
            Ok(layout)
        }
    }
}

impl Drop for Art {
    fn drop(&mut self) {
        for b in self.icons.values() {
            // SAFETY: each bitmap was made here and is no longer in a menu.
            unsafe {
                let _ = DeleteObject((*b).into());
            }
        }
    }
}

/// The menu font's face and size in pixels at `dpi`; Segoe UI 9 pt if the system
/// will not say.
fn menu_font(dpi: u32) -> (HSTRING, f32) {
    let mut m = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: the struct is sized as the call expects.
    let ok = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            m.cbSize,
            Some((&raw mut m).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0).0,
            dpi,
        )
    }
    .is_ok();
    let f = &m.lfMenuFont;
    let len = f.lfFaceName.iter().position(|&c| c == 0).unwrap_or(0);
    if !ok || len == 0 || f.lfHeight == 0 {
        return (HSTRING::from("Segoe UI"), 12.0 * dpi as f32 / 96.0);
    }
    (
        HSTRING::from_wide(&f.lfFaceName[..len]),
        f.lfHeight.unsigned_abs() as f32,
    )
}

/// The icon font: Fluent on Windows 11, MDL2 on 10 (the same codepoints).
fn icon_family(dwrite: &IDWriteFactory) -> HSTRING {
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: an out-pointer to a local.
    let _ = unsafe { dwrite.GetSystemFontCollection(&raw mut collection, false) };
    for name in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
        let name = HSTRING::from(name);
        let mut index = 0u32;
        let mut exists = windows::core::BOOL(0);
        // SAFETY: out-pointers to locals.
        let found = collection.as_ref().is_some_and(|c| unsafe {
            c.FindFamilyName(&name, &raw mut index, &raw mut exists)
                .is_ok()
                && exists.as_bool()
        });
        if found {
            return name;
        }
    }
    HSTRING::from("Segoe MDL2 Assets")
}

/// `bitmap` (`w` by `h`, premultiplied BGRA) copied into a top-down 32 bpp DIB
/// section, which is what a menu takes.
fn dib_of(bitmap: &IWICBitmap, w: u32, h: u32) -> windows::core::Result<HBITMAP> {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    // SAFETY: the buffer is `h` rows of `w * 4` bytes.
    unsafe { bitmap.CopyPixels(std::ptr::null(), w * 4, &mut pixels)? };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w.cast_signed(),
            // Negative: rows top to bottom, as WIC gave them.
            biHeight: -h.cast_signed(),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: the header describes a bitmap of exactly `pixels.len()` bytes,
    // which are copied into the section's memory once it exists.
    unsafe {
        let b = CreateDIBSection(
            None,
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        )?;
        if bits.is_null() {
            let _ = DeleteObject(b.into());
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        Ok(b)
    }
}
