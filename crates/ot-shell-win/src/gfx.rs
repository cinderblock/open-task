//! Direct3D 11 + DXGI composition swap chain + Direct2D + DirectWrite.
//!
//! One `Gfx` per window. It owns every device object and a text-layout cache keyed
//! by a hash of `(text, style, box, alignment)`. On a task manager, 59 of every 60
//! frames draw the exact same strings, so the cache turns text into a lookup.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::ManuallyDrop;

use ot_paint::{
    Color, DisplayList, DrawCmd, FontFamily, FontWeight, HAlign, Icon, Point, Rect, Span, TextCmd,
    TextStyle, VAlign,
};
use windows::core::{Interface, Result, BOOL, HSTRING};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
    ID2D1PathGeometry1, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteInlineObject,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Vector2;

/// Frames a cached layout may go unused before it is evicted.
const LAYOUT_TTL_FRAMES: u64 = 120;

/// Layout width for editable fields: wide enough that nothing wraps or trims, so
/// the text can be measured and scrolled to keep its end visible.
const FIELD_LAYOUT_W: f32 = 1.0e6;
/// Caret width in DIPs.
const CARET_W: f32 = 1.0;

struct CachedLayout {
    layout: IDWriteTextLayout,
    last_used: u64,
}

#[derive(Clone)]
struct FontSet {
    format: IDWriteTextFormat,
    ellipsis: IDWriteInlineObject,
}

/// All GPU and text resources for one window.
/// What is known about one image path.
#[derive(Debug)]
enum ImageSlot {
    /// Asked for, not yet delivered.
    Pending,
    /// There is no image for this path (the file has no icon, or cannot be read).
    Missing,
    Ready(ID2D1Bitmap1),
}

/// The key an image path is cached under.
fn image_key(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

pub struct Gfx {
    _d3d: ID3D11Device,
    d2d_factory: ID2D1Factory1,
    _d2d_device: ID2D1Device,
    dc: ID2D1DeviceContext,
    swapchain: IDXGISwapChain1,
    /// The swap chain's buffer, which each frame is copied into.
    target: Option<ID2D1Bitmap1>,
    /// The frame as drawn so far, kept between frames: the device context draws
    /// here, and only where a frame differs from the last ([`Gfx::render`]).
    canvas: Option<ID2D1Bitmap1>,
    /// The last frame drawn, to find what the next one changes.
    last: DisplayList,
    /// Where the frame being drawn differs from the last, in DIPs.
    damage: Vec<Rect>,
    /// Draw the next frame whole: the canvas is new, or holds something stale.
    redraw_all: bool,
    // Composition objects must stay alive for the visual tree to keep existing.
    _dcomp: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _visual: IDCompositionVisual,

    dwrite: IDWriteFactory,
    brush: ID2D1SolidColorBrush,
    tabular: IDWriteTypography,
    ui_family: HSTRING,
    mono_family: HSTRING,
    icon_family: HSTRING,
    fonts: HashMap<TextStyle, FontSet>,
    layouts: HashMap<u64, CachedLayout>,
    utf16: Vec<u16>,
    /// Raster images by the hash of their path: a program's icon, loaded by the
    /// window off this thread and handed in with [`Gfx::add_image`].
    images: HashMap<u64, ImageSlot>,
    /// Paths this frame asked for that nobody has loaded yet.
    wanted: Vec<String>,

    frame: u64,
    dpi: f32,
    size_px: (u32, u32),
}

/// Damage over this share of the window is drawn as a whole frame instead.
const WHOLE_AT: f32 = 0.5;

impl std::fmt::Debug for Gfx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gfx")
            .field("size_px", &self.size_px)
            .field("dpi", &self.dpi)
            .field("layouts", &self.layouts.len())
            .finish_non_exhaustive()
    }
}

impl Gfx {
    /// Create all device objects and bind the swap chain to `hwnd` via composition.
    #[allow(clippy::too_many_lines)]
    pub fn new(hwnd: HWND, size_px: (u32, u32), dpi: f32) -> Result<Self> {
        let d3d = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d.cast()?;

        // SAFETY: all objects are valid; the descriptor is fully initialized.
        let (swapchain, dcomp, dcomp_target, visual) = unsafe {
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size_px.0.max(1),
                Height: size_px.1.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swapchain = factory.CreateSwapChainForComposition(&d3d, &raw const desc, None)?;

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let dcomp_target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(&visual)?;
            dcomp.Commit()?;
            (swapchain, dcomp, dcomp_target, visual)
        };

        // SAFETY: factory options are optional; device objects are valid.
        let (d2d_factory, d2d_device, dc, brush) = unsafe {
            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            dc.SetDpi(dpi, dpi);
            // ClearType needs an opaque background; over a translucent backdrop it
            // produces colour fringes. Grayscale is what WinUI does over Mica too.
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let white = D2D1_COLOR_F {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            };
            let brush = dc.CreateSolidColorBrush(&raw const white, None)?;
            (d2d_factory, d2d_device, dc, brush)
        };

        // SAFETY: the DirectWrite factory is process-wide shared; typography and
        // collection objects are valid for the calls made.
        let (dwrite, tabular, ui_family, mono_family, icon_family) = unsafe {
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let tabular = dwrite.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            let mut collection: Option<IDWriteFontCollection> = None;
            dwrite.GetSystemFontCollection(&raw mut collection, false)?;
            let pick = |candidates: &[&str]| -> HSTRING {
                for c in candidates {
                    let name = HSTRING::from(*c);
                    if let Some(col) = &collection {
                        let mut index = 0u32;
                        let mut exists = BOOL(0);
                        if col
                            .FindFamilyName(&name, &raw mut index, &raw mut exists)
                            .is_ok()
                            && exists.as_bool()
                        {
                            return name;
                        }
                    }
                }
                HSTRING::from(candidates[candidates.len() - 1])
            };
            let ui = pick(&["Segoe UI Variable", "Segoe UI"]);
            let mono = pick(&["Cascadia Mono", "Consolas"]);
            // Windows 11 ships Fluent; Windows 10 has MDL2, with the same codepoints
            // for every icon `glyph` maps.
            let icons = pick(&["Segoe Fluent Icons", "Segoe MDL2 Assets"]);
            (dwrite, tabular, ui, mono, icons)
        };

        let mut gfx = Self {
            _d3d: d3d,
            d2d_factory,
            _d2d_device: d2d_device,
            dc,
            swapchain,
            target: None,
            canvas: None,
            last: DisplayList::new(),
            damage: Vec::new(),
            redraw_all: true,
            _dcomp: dcomp,
            _dcomp_target: dcomp_target,
            _visual: visual,
            dwrite,
            brush,
            tabular,
            ui_family,
            mono_family,
            icon_family,
            fonts: HashMap::new(),
            layouts: HashMap::with_capacity(1024),
            utf16: Vec::with_capacity(128),
            images: HashMap::new(),
            wanted: Vec::new(),
            frame: 0,
            dpi,
            size_px,
        };
        gfx.bind_target()?;
        Ok(gfx)
    }

    /// Resize the swap chain to the new client size in physical pixels.
    pub fn resize(&mut self, size_px: (u32, u32)) -> Result<()> {
        if size_px == self.size_px {
            return Ok(());
        }
        // SAFETY: the target must be released before the buffers can be resized.
        unsafe {
            self.dc.SetTarget(None::<&ID2D1Image>);
            self.target = None;
            self.canvas = None;
            self.swapchain.ResizeBuffers(
                0,
                size_px.0.max(1),
                size_px.1.max(1),
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
        }
        self.size_px = size_px;
        self.bind_target()
    }

    /// Change DPI. Layouts are in DIPs so the cache stays valid; only the target
    /// bitmap and context need to know.
    pub fn set_dpi(&mut self, dpi: f32) {
        if (dpi - self.dpi).abs() < f32::EPSILON {
            return;
        }
        self.dpi = dpi;
        // SAFETY: dc is valid.
        unsafe { self.dc.SetDpi(dpi, dpi) };
        // Rebind so the bitmap's DPI matches.
        let _ = self.bind_target();
    }

    /// Image paths the last frames drew that have not been loaded yet. The caller
    /// loads them (off this thread) and brings each back through
    /// [`Gfx::add_image`] or [`Gfx::image_missing`].
    pub fn take_wanted(&mut self) -> Vec<String> {
        std::mem::take(&mut self.wanted)
    }

    /// Hand in the pixels for `path`: `width` by `height`, 32-bit premultiplied
    /// BGRA, rows of `width * 4` bytes.
    pub fn add_image(&mut self, path: &str, width: u32, height: u32, pbgra: &[u8]) {
        let key = image_key(path);
        if width == 0 || height == 0 || pbgra.len() != (width * height * 4) as usize {
            self.images.insert(key, ImageSlot::Missing);
            return;
        }
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        let size = D2D_SIZE_U { width, height };
        // SAFETY: the pixel buffer is `height` rows of `width * 4` bytes, checked
        // above; D2D copies it during the call.
        let bitmap = unsafe {
            self.dc.CreateBitmap(
                size,
                Some(pbgra.as_ptr().cast()),
                width * 4,
                &raw const props,
            )
        };
        match bitmap {
            Ok(b) => {
                self.images.insert(key, ImageSlot::Ready(b));
                // Wherever the image was asked for, the canvas shows nothing yet.
                self.redraw_all = true;
            }
            Err(e) => {
                tracing::debug!(path, error = %e, "CreateBitmap failed for an icon");
                self.images.insert(key, ImageSlot::Missing);
            }
        }
    }

    /// There is no image for `path`; stop asking.
    pub fn image_missing(&mut self, path: &str) {
        self.images.insert(image_key(path), ImageSlot::Missing);
    }

    fn bind_target(&mut self) -> Result<()> {
        // SAFETY: buffer 0 exists on a valid swap chain; properties are complete.
        unsafe {
            let surface: IDXGISurface = self.swapchain.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };
            let bitmap = self
                .dc
                .CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?;
            self.target = Some(bitmap);
            // The canvas the frames are drawn on: the buffer's size and format, and
            // drawable, to be copied from.
            let props = D2D1_BITMAP_PROPERTIES1 {
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                ..props
            };
            let size = D2D_SIZE_U {
                width: self.size_px.0.max(1),
                height: self.size_px.1.max(1),
            };
            let canvas = self.dc.CreateBitmap(size, None, 0, &raw const props)?;
            self.dc.SetTarget(&canvas);
            self.canvas = Some(canvas);
        }
        self.redraw_all = true;
        Ok(())
    }

    /// Draw one frame and present it.
    ///
    /// The frame is drawn into a canvas that keeps it, and only where it differs
    /// from the last one ([`DisplayList::damage_since`]): each damaged rectangle is
    /// clipped to and the commands that touch it replayed, so a frame where only a
    /// chart moved does not redraw the table's text. The canvas is then copied to
    /// the swap chain's buffer and presented.
    pub fn render(&mut self, dl: &DisplayList) -> Result<()> {
        self.frame += 1;
        let mut damage = std::mem::take(&mut self.damage);
        let partial = !self.redraw_all && dl.damage_since(&self.last, &mut damage);
        let window = self.size_px.0 as f32 * self.size_px.1 as f32;
        let scale = self.dpi / 96.0;
        let damaged: f32 = damage.iter().map(|r| r.w * r.h * scale * scale).sum();
        // Damage over most of the window costs as much as drawing it whole, and
        // more once rectangles overlap.
        let partial = partial && damaged < window * WHOLE_AT;
        // SAFETY: every call is on a valid device context between BeginDraw and
        // EndDraw; all pointers passed point at locals that outlive the call.
        let drawn = unsafe {
            if partial {
                self.draw_damage(dl, &damage)
            } else {
                self.draw_all(dl)
            }
        };
        self.damage = damage;
        drawn?;
        // SAFETY: the canvas and the target are the same size and format; the
        // canvas's drawing has been flushed by EndDraw.
        unsafe {
            if let (Some(target), Some(canvas)) = (&self.target, &self.canvas) {
                target.CopyFromBitmap(None, canvas, None)?;
            }
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        self.last.copy_from(dl);
        self.redraw_all = false;
        self.evict_layouts();
        Ok(())
    }

    /// Draw every command into the canvas.
    unsafe fn draw_all(&mut self, dl: &DisplayList) -> Result<()> {
        // SAFETY: as for `render`.
        unsafe {
            self.dc.BeginDraw();
            for cmd in dl.cmds() {
                self.draw_cmd(dl, cmd)?;
            }
            self.dc.EndDraw(None, None)
        }
    }

    /// Draw the commands that touch each of `damage` into the canvas, clipped to
    /// it, over what the canvas holds from the last frame.
    unsafe fn draw_damage(&mut self, dl: &DisplayList, damage: &[Rect]) -> Result<()> {
        // Text this frame skips is still on screen: its layout stays cached.
        self.touch_layouts(dl);
        if damage.is_empty() {
            return Ok(());
        }
        // SAFETY: as for `render`.
        unsafe {
            self.dc.BeginDraw();
            for &area in damage {
                // Out to whole pixels, so the clip's edge leaves no pixel half drawn.
                let area = self.to_pixel_edges(area);
                let clip = rectf(area);
                self.dc
                    .PushAxisAlignedClip(&raw const clip, D2D1_ANTIALIAS_MODE_ALIASED);
                for cmd in dl.cmds() {
                    // Clips always, to keep the stack; clears and anything else
                    // that reaches into the area, in order.
                    let touches = match cmd {
                        DrawCmd::PushClip(_) | DrawCmd::PopClip => true,
                        _ => dl
                            .bounds(cmd)
                            .is_none_or(|b| !b.intersect(&area).is_empty()),
                    };
                    if touches {
                        self.draw_cmd(dl, cmd)?;
                    }
                }
                self.dc.PopAxisAlignedClip();
            }
            self.dc.EndDraw(None, None)
        }
    }

    /// `r` grown out to the nearest device pixels, in DIPs.
    fn to_pixel_edges(&self, r: Rect) -> Rect {
        let s = self.dpi / 96.0;
        let (x, y) = ((r.x * s).floor() / s, (r.y * s).floor() / s);
        let (right, bottom) = ((r.right() * s).ceil() / s, (r.bottom() * s).ceil() / s);
        Rect::new(x, y, right - x, bottom - y)
    }

    /// Mark the layouts of every text in `dl` used this frame, drawn or not.
    fn touch_layouts(&mut self, dl: &DisplayList) {
        for cmd in dl.cmds() {
            if let DrawCmd::Text(t) = cmd {
                let key = layout_key(dl.str(t.text), t);
                if let Some(c) = self.layouts.get_mut(&key) {
                    c.last_used = self.frame;
                }
            }
        }
    }

    /// Draw one command on the device context, between `BeginDraw` and `EndDraw`.
    #[allow(clippy::too_many_lines)]
    unsafe fn draw_cmd(&mut self, dl: &DisplayList, cmd: &DrawCmd) -> Result<()> {
        // SAFETY: as for `render`.
        unsafe {
            match *cmd {
                DrawCmd::Clear(c) => {
                    let col = color(c);
                    self.dc.Clear(Some(&raw const col));
                }
                DrawCmd::FillRect { rect, color: c } => {
                    self.set_color(c);
                    let r = rectf(rect);
                    self.dc.FillRectangle(&raw const r, &self.brush);
                }
                DrawCmd::FillRoundRect {
                    rect,
                    radius,
                    color: c,
                } => {
                    self.set_color(c);
                    let rr = D2D1_ROUNDED_RECT {
                        rect: rectf(rect),
                        radiusX: radius,
                        radiusY: radius,
                    };
                    self.dc.FillRoundedRectangle(&raw const rr, &self.brush);
                }
                DrawCmd::StrokeRect {
                    rect,
                    color: c,
                    width,
                } => {
                    self.set_color(c);
                    // Inset by half the stroke so the outline stays inside `rect`.
                    let r = rectf(rect.inset(width * 0.5, width * 0.5));
                    self.dc
                        .DrawRectangle(&raw const r, &self.brush, width, None);
                }
                DrawCmd::StrokeRoundRect {
                    rect,
                    radius,
                    color: c,
                    width,
                } => {
                    self.set_color(c);
                    let rr = D2D1_ROUNDED_RECT {
                        rect: rectf(rect.inset(width * 0.5, width * 0.5)),
                        radiusX: radius,
                        radiusY: radius,
                    };
                    self.dc
                        .DrawRoundedRectangle(&raw const rr, &self.brush, width, None);
                }
                DrawCmd::Line {
                    from,
                    to,
                    color: c,
                    width,
                } => {
                    self.set_color(c);
                    self.dc.DrawLine(v2(from), v2(to), &self.brush, width, None);
                }
                DrawCmd::Polyline {
                    points,
                    color: c,
                    width,
                } => {
                    let geom = self.path(dl.points(points), false)?;
                    self.set_color(c);
                    self.dc.DrawGeometry(&geom, &self.brush, width, None);
                }
                DrawCmd::FillPolygon { points, color: c } => {
                    let geom = self.path(dl.points(points), true)?;
                    self.set_color(c);
                    self.dc.FillGeometry(&geom, &self.brush, None);
                }
                DrawCmd::Text(t) if t.field => {
                    // Measure, then shift left so the end stays in the box, and
                    // put the caret right after the last character.
                    let layout = self.layout_for(dl.str(t.text), &t)?;
                    let mut m = DWRITE_TEXT_METRICS::default();
                    layout.GetMetrics(&raw mut m)?;
                    let width = m.widthIncludingTrailingWhitespace;
                    let shift = (width - t.rect.w).max(0.0);
                    let clip = rectf(t.rect);
                    self.dc
                        .PushAxisAlignedClip(&raw const clip, D2D1_ANTIALIAS_MODE_ALIASED);
                    self.set_color(t.color);
                    self.dc.DrawTextLayout(
                        Vector2 {
                            X: t.rect.x - shift,
                            Y: t.rect.y,
                        },
                        &layout,
                        &self.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                    if t.caret {
                        let caret = rectf(Rect::new(
                            (t.rect.x - shift + width).round(),
                            t.rect.y + m.top,
                            CARET_W,
                            m.height,
                        ));
                        self.dc.FillRectangle(&raw const caret, &self.brush);
                    }
                    self.dc.PopAxisAlignedClip();
                }
                DrawCmd::Text(t) => {
                    let layout = self.layout_for(dl.str(t.text), &t)?;
                    self.set_color(t.color);
                    self.dc.DrawTextLayout(
                        Vector2 {
                            X: t.rect.x,
                            Y: t.rect.y,
                        },
                        &layout,
                        &self.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                }
                DrawCmd::Icon {
                    icon,
                    rect,
                    size,
                    color: c,
                } => {
                    let mut utf8 = [0u8; 4];
                    let text = glyph(icon).encode_utf8(&mut utf8);
                    let t = TextCmd {
                        text: Span::default(),
                        rect,
                        style: TextStyle {
                            family: FontFamily::Icons,
                            size,
                            weight: FontWeight::Regular,
                            tabular_numbers: false,
                        },
                        color: c,
                        halign: HAlign::Center,
                        valign: VAlign::Middle,
                        ellipsis: false,
                        field: false,
                        caret: false,
                    };
                    let layout = self.layout_for(text, &t)?;
                    self.set_color(c);
                    self.dc.DrawTextLayout(
                        Vector2 {
                            X: rect.x,
                            Y: rect.y,
                        },
                        &layout,
                        &self.brush,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    );
                }
                DrawCmd::Image { path, rect } => {
                    let path = dl.str(path);
                    let key = image_key(path);
                    match self.images.get(&key) {
                        Some(ImageSlot::Ready(bitmap)) => {
                            let dest = rectf(rect);
                            self.dc.DrawBitmap(
                                bitmap,
                                Some(&raw const dest),
                                1.0,
                                D2D1_INTERPOLATION_MODE_LINEAR,
                                None,
                                None,
                            );
                        }
                        Some(ImageSlot::Pending | ImageSlot::Missing) => {}
                        None => {
                            self.images.insert(key, ImageSlot::Pending);
                            self.wanted.push(path.to_owned());
                        }
                    }
                }
                DrawCmd::PushClip(rect) => {
                    let r = rectf(rect);
                    self.dc
                        .PushAxisAlignedClip(&raw const r, D2D1_ANTIALIAS_MODE_ALIASED);
                }
                DrawCmd::PopClip => self.dc.PopAxisAlignedClip(),
            }
        }
        Ok(())
    }

    unsafe fn set_color(&self, c: Color) {
        // SAFETY: brush is valid; the color struct outlives the call.
        let col = color(c);
        unsafe { self.brush.SetColor(&raw const col) };
    }

    fn path(&self, pts: &[Point], closed: bool) -> Result<ID2D1PathGeometry1> {
        // SAFETY: factory is valid; the sink is closed before the geometry is used.
        unsafe {
            let geom = self.d2d_factory.CreatePathGeometry()?;
            let sink = geom.Open()?;
            sink.BeginFigure(
                v2(pts[0]),
                if closed {
                    D2D1_FIGURE_BEGIN_FILLED
                } else {
                    D2D1_FIGURE_BEGIN_HOLLOW
                },
            );
            for p in &pts[1..] {
                sink.AddLine(v2(*p));
            }
            sink.EndFigure(if closed {
                D2D1_FIGURE_END_CLOSED
            } else {
                D2D1_FIGURE_END_OPEN
            });
            sink.Close()?;
            Ok(geom)
        }
    }

    fn font_for(&mut self, style: TextStyle) -> Result<FontSet> {
        if !self.fonts.contains_key(&style) {
            let family = match style.family {
                FontFamily::Ui => &self.ui_family,
                FontFamily::Mono => &self.mono_family,
                FontFamily::Icons => &self.icon_family,
            };
            let weight = match style.weight {
                FontWeight::Regular => DWRITE_FONT_WEIGHT_NORMAL,
                FontWeight::SemiBold => DWRITE_FONT_WEIGHT_SEMI_BOLD,
                FontWeight::Bold => DWRITE_FONT_WEIGHT_BOLD,
            };
            // SAFETY: factory is valid; strings outlive the call.
            let set = unsafe {
                let format = self.dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.size,
                    &HSTRING::from("en-us"),
                )?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
                FontSet { format, ellipsis }
            };
            self.fonts.insert(style, set);
        }
        Ok(self.fonts[&style].clone())
    }

    fn layout_for(&mut self, text: &str, t: &TextCmd) -> Result<IDWriteTextLayout> {
        let key = layout_key(text, t);
        if let Some(c) = self.layouts.get_mut(&key) {
            c.last_used = self.frame;
            return Ok(c.layout.clone());
        }

        self.utf16.clear();
        self.utf16.extend(text.encode_utf16());
        let len = self.utf16.len() as u32;
        // A field lays out at its natural width and is scrolled at draw time.
        let w = if t.field { FIELD_LAYOUT_W } else { t.rect.w };
        let h = t.rect.h;
        let (halign, valign, ellipsis, tabular) =
            (t.halign, t.valign, t.ellipsis, t.style.tabular_numbers);

        let font = self.font_for(t.style)?;
        // SAFETY: factory, format and inline object are valid; the UTF-16 buffer
        // outlives the call (DirectWrite copies the text).
        let layout = unsafe {
            let layout = self
                .dwrite
                .CreateTextLayout(&self.utf16, &font.format, w, h)?;
            layout.SetTextAlignment(match halign {
                HAlign::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                HAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                HAlign::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            layout.SetParagraphAlignment(match valign {
                VAlign::Top => DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
                VAlign::Middle => DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                VAlign::Bottom => DWRITE_PARAGRAPH_ALIGNMENT_FAR,
            })?;
            if ellipsis {
                let trim = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                layout.SetTrimming(&raw const trim, &font.ellipsis)?;
            }
            if tabular {
                layout.SetTypography(
                    &self.tabular,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: len,
                    },
                )?;
            }
            layout
        };

        self.layouts.insert(
            key,
            CachedLayout {
                layout: layout.clone(),
                last_used: self.frame,
            },
        );
        Ok(layout)
    }

    fn evict_layouts(&mut self) {
        if !self.frame.is_multiple_of(LAYOUT_TTL_FRAMES) {
            return;
        }
        let cutoff = self.frame.saturating_sub(LAYOUT_TTL_FRAMES);
        self.layouts.retain(|_, c| c.last_used >= cutoff);
    }
}

fn create_d3d_device() -> Result<ID3D11Device> {
    let mut last = None;
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        // SAFETY: out-pointer is a valid local; other pointers are optional and absent.
        let r = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                None,
            )
        };
        match (r, device) {
            (Ok(()), Some(d)) => {
                if driver == D3D_DRIVER_TYPE_WARP {
                    tracing::warn!("no hardware D3D11 device; using WARP software rasterizer");
                }
                return Ok(d);
            }
            (Err(e), _) => last = Some(e),
            (Ok(()), None) => {}
        }
    }
    Err(last
        .unwrap_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)))
}

/// The Segoe Fluent Icons / MDL2 Assets codepoint for each icon, picked by
/// rendering the candidates (see `plans/performance-view.md`).
fn glyph(icon: Icon) -> char {
    match icon {
        Icon::Menu => '\u{E700}',        // GlobalNavigationButton
        Icon::Summary => '\u{F246}',     // ViewDashboard, tiles of a dashboard
        Icon::Processes => '\u{E71D}',   // AllApps, drawn as a checklist
        Icon::Performance => '\u{E9D9}', // Diagnostic, a pulse in a box
        Icon::Settings => '\u{E713}',    // Setting, a gear
        Icon::Update => '\u{E895}',      // Sync, Windows Update's own
        Icon::Users => '\u{E716}',       // People
        Icon::Services => '\u{E9F5}',    // Processing, two gears
        Icon::Startup => '\u{E7E8}',     // PowerButton
        Icon::Connections => '\u{E774}', // Globe
        Icon::Apps => '\u{E7B8}',        // Package
        Icon::System => '\u{E7F4}',      // Devices, a monitor
        Icon::Play => '\u{E768}',        // Play
        Icon::Pause => '\u{E769}',       // Pause
        Icon::Previous => '\u{E892}',    // Previous
        Icon::Next => '\u{E893}',        // Next
    }
}

fn layout_key(text: &str, t: &TextCmd) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    t.style.hash(&mut h);
    t.rect.w.to_bits().hash(&mut h);
    t.rect.h.to_bits().hash(&mut h);
    t.halign.hash(&mut h);
    t.valign.hash(&mut h);
    t.ellipsis.hash(&mut h);
    t.field.hash(&mut h);
    h.finish()
}

fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn rectf(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

fn v2(p: Point) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}
