//! Direct3D 11 + DXGI composition swap chain + Direct2D + DirectWrite.
//!
//! One `Gfx` per window. It owns every device object and a text-layout cache keyed
//! by a hash of `(text, style, box, alignment)`. On a task manager, 59 of every 60
//! frames draw the exact same strings, so the cache turns text into a lookup.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use ot_paint::{
    ChartRaster, Color, DisplayList, DrawCmd, FontFamily, FontWeight, HAlign, Icon, Point, Rect,
    Span, TextCmd, TextStyle, VAlign,
};
use windows::core::{Interface, Result, BOOL, HSTRING};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_RECT_U, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
    ID2D1PathGeometry1, ID2D1SolidColorBrush, ID2D1StrokeStyle1, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_NONE,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_FLAT,
    D2D1_DASH_STYLE_SOLID, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_LINE_JOIN_BEVEL, D2D1_MAP_OPTIONS_READ,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};

use crate::chart_gpu::GpuCharts;
use crate::charts::{find_runs, ChartDrawing, ChartRun, Shapes};
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
    d3d: ID3D11Device,
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
    /// With `OT_CHECK_DAMAGE` set, every partial frame is also drawn whole and the
    /// two compared ([`Gfx::check_damage`]).
    check: Option<DamageCheck>,
    /// How chart shapes are drawn, and their bitmaps ([`Charts`]).
    charts: Charts,
    // Composition objects must stay alive for the visual tree to keep existing.
    _dcomp: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _visual: IDCompositionVisual,

    dwrite: IDWriteFactory,
    brush: ID2D1SolidColorBrush,
    /// For polylines: bevelled joins. Direct2D's default, mitred, reaches up to five
    /// widths past a sharp corner, outside the area the damage diff gives a line
    /// ([`DisplayList::bounds`]), so a spike left pixels behind; a bevel stays
    /// within half the width of the points.
    line_style: ID2D1StrokeStyle1,
    tabular: IDWriteTypography,
    ui_family: HSTRING,
    mono_family: HSTRING,
    icon_family: HSTRING,
    fonts: HashMap<TextStyle, FontSet>,
    layouts: HashMap<u64, CachedLayout>,
    utf16: Vec<u16>,
    /// The segments of the path being built, reused between paths.
    path_pts: Vec<Vector2>,
    /// A band's outline for Direct2D, reused between bands.
    band_pts: Vec<Point>,
    /// Raster images by the hash of their path: a program's icon, loaded by the
    /// window off this thread and handed in with [`Gfx::add_image`].
    images: HashMap<u64, ImageSlot>,
    /// Paths this frame asked for that nobody has loaded yet.
    wanted: Vec<String>,

    frame: u64,
    /// How the last frame went, for frame statistics.
    stats: FrameStats,
    dpi: f32,
    size_px: (u32, u32),
}

/// How one [`Gfx::render`] went, for `OT_FRAME_STATS`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Drawn in part, over the last frame, rather than whole.
    pub partial: bool,
    /// The share of the window drawn: 1 for a whole frame.
    pub damaged: f32,
    /// Comparing the display list with the last one.
    pub diff: Duration,
    /// Drawing into the canvas, `EndDraw` included.
    pub draw: Duration,
    /// Copying the canvas to the swap chain's buffer.
    pub copy: Duration,
    /// Drawing chart runs into their bitmaps (GPU or CPU), before the draw.
    pub charts: Duration,
    /// `Present`, which may wait for the display.
    pub present: Duration,
    /// Keeping the display list and evicting layouts, after `Present`.
    pub rest: Duration,
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
        // `OT_WARP` set: software only, to tell a GPU driver's faults from ours.
        Self::on_device(
            hwnd,
            size_px,
            dpi,
            created3d_device(std::env::var_os("OT_WARP").is_some())?,
        )
    }

    /// As [`Gfx::new`], on `d3d`.
    #[allow(clippy::too_many_lines)]
    fn on_device(hwnd: HWND, size_px: (u32, u32), dpi: f32, d3d: ID3D11Device) -> Result<Self> {
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
        let (d2d_factory, d2d_device, dc, brush, line_style) = unsafe {
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
            let line = D2D1_STROKE_STYLE_PROPERTIES1 {
                startCap: D2D1_CAP_STYLE_FLAT,
                endCap: D2D1_CAP_STYLE_FLAT,
                dashCap: D2D1_CAP_STYLE_FLAT,
                lineJoin: D2D1_LINE_JOIN_BEVEL,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
                transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
            };
            let line_style = d2d_factory.CreateStrokeStyle(&raw const line, None)?;
            (d2d_factory, d2d_device, dc, brush, line_style)
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
            d3d,
            d2d_factory,
            _d2d_device: d2d_device,
            dc,
            swapchain,
            target: None,
            canvas: None,
            last: DisplayList::new(),
            damage: Vec::new(),
            redraw_all: true,
            check: DamageCheck::from_env(),
            charts: Charts::new(),
            _dcomp: dcomp,
            _dcomp_target: dcomp_target,
            _visual: visual,
            dwrite,
            brush,
            line_style,
            tabular,
            ui_family,
            mono_family,
            icon_family,
            fonts: HashMap::new(),
            layouts: HashMap::with_capacity(1024),
            utf16: Vec::with_capacity(128),
            path_pts: Vec::with_capacity(1024),
            band_pts: Vec::new(),
            images: HashMap::new(),
            wanted: Vec::new(),
            frame: 0,
            stats: FrameStats::default(),
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
        if let Some(c) = self.check.as_mut() {
            // Made again at the new size and DPI.
            c.size = (0, 0);
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
    /// the swap chain's buffer and presented, on the `sync`th vertical blank from
    /// the last (1 for every one, 2 for every other).
    pub fn render(&mut self, dl: &DisplayList, sync: u32) -> Result<()> {
        self.frame += 1;
        let started = Instant::now();
        let mut damage = std::mem::take(&mut self.damage);
        let partial = !self.redraw_all && dl.damage_since(&self.last, &mut damage);
        let diffed = Instant::now();
        let window = self.size_px.0 as f32 * self.size_px.1 as f32;
        let scale = self.dpi / 96.0;
        let damaged: f32 = damage.iter().map(|r| r.w * r.h * scale * scale).sum();
        // Damage over most of the window costs as much as drawing it whole, and
        // more once rectangles overlap.
        let partial = partial && damaged < window * WHOLE_AT;
        // Chart runs into their bitmaps, before Direct2D's pass, which draws them.
        let charting = Instant::now();
        let all = !partial || self.check.is_some();
        if let Err(e) = self.prepare_charts(dl, if all { None } else { Some(&damage) }) {
            tracing::warn!(error = %e, "chart drawing failed; drawing charts with Direct2D");
            self.charts.fall_back(ChartDrawing::Direct2D);
            self.charts.runs.clear();
        }
        let charts = charting.elapsed();
        // SAFETY: every call is on a valid device context between BeginDraw and
        // EndDraw; all pointers passed point at locals that outlive the call.
        let drawn = unsafe {
            if partial {
                self.draw_damage(dl, &damage)
            } else {
                self.draw_all(dl)
            }
        };
        if partial && drawn.is_ok() && self.check.is_some() {
            // SAFETY: as above; the canvas's drawing has been flushed by EndDraw.
            if let Err(e) = unsafe { self.check_damage(dl, &damage) } {
                tracing::warn!(error = %e, "damage check failed");
            }
        }
        self.damage = damage;
        drawn?;
        let drawn_at = Instant::now();
        // SAFETY: the canvas and the target are the same size and format; the
        // canvas's drawing has been flushed by EndDraw.
        unsafe {
            if let (Some(target), Some(canvas)) = (&self.target, &self.canvas) {
                target.CopyFromBitmap(None, canvas, None)?;
            }
        }
        let copied = Instant::now();
        // SAFETY: the swap chain is valid.
        unsafe { self.swapchain.Present(sync, DXGI_PRESENT(0)).ok()? };
        let presented = Instant::now();
        self.last.copy_from(dl);
        self.redraw_all = false;
        self.evict_layouts();
        self.stats = FrameStats {
            partial,
            damaged: if partial {
                damaged / window.max(1.0)
            } else {
                1.0
            },
            diff: diffed - started,
            draw: drawn_at - diffed,
            copy: copied - drawn_at,
            charts,
            present: presented - copied,
            rest: presented.elapsed(),
        };
        Ok(())
    }

    /// How the last frame went.
    #[must_use]
    pub fn last_stats(&self) -> FrameStats {
        self.stats
    }

    /// Where the last frame differed from the one before, in DIPs; meaningful when
    /// it was drawn in part.
    #[must_use]
    pub fn last_damage(&self) -> &[Rect] {
        &self.damage
    }

    /// Draw every command into the canvas.
    unsafe fn draw_all(&mut self, dl: &DisplayList) -> Result<()> {
        // SAFETY: as for `render`.
        unsafe {
            self.dc.BeginDraw();
            let drawn = self.draw_cmds(dl, None);
            self.dc.EndDraw(None, None)?;
            drawn
        }
    }

    /// Draw `dl`'s commands in order, or only those reaching into `area`, with
    /// each chart run as its bitmap, between `BeginDraw` and `EndDraw`.
    unsafe fn draw_cmds(&mut self, dl: &DisplayList, area: Option<Rect>) -> Result<()> {
        let cmds = dl.cmds();
        let mut i = 0;
        let mut run = 0;
        while i < cmds.len() {
            if let Some(r) = self.charts.runs.get(run).copied() {
                if r.start == i {
                    run += 1;
                    i = r.end;
                    if area.is_none_or(|a| !r.dip.intersect(&a).is_empty()) {
                        // SAFETY: as for `render`.
                        unsafe { self.draw_run(run - 1, r) };
                    }
                    continue;
                }
            }
            let cmd = &cmds[i];
            i += 1;
            // Clips always, to keep the stack; clears and anything else that
            // reaches into the area, in order.
            let touches = area.is_none_or(|area| match cmd {
                DrawCmd::PushClip(_) | DrawCmd::PopClip => true,
                _ => dl
                    .bounds(cmd)
                    .is_none_or(|b| !b.intersect(&area).is_empty()),
            });
            if touches {
                // SAFETY: as for `render`.
                unsafe { self.draw_cmd(dl, cmd)? };
            }
        }
        Ok(())
    }

    /// Draw chart run `i`'s bitmap in its place, 1:1 with the device's pixels.
    unsafe fn draw_run(&self, i: usize, run: ChartRun) {
        let Some(Some(slot)) = self.charts.slots.get(i) else {
            return;
        };
        if !self.charts.ready.get(i).copied().unwrap_or(false) {
            return;
        }
        let dest = rectf(run.dip);
        let src = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: run.px.w as f32,
            bottom: run.px.h as f32,
        };
        // SAFETY: between BeginDraw and EndDraw; the rects are locals.
        unsafe {
            self.dc.DrawBitmap(
                &slot.bitmap,
                Some(&raw const dest),
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                Some(&raw const src),
                None,
            );
        }
    }

    /// Choose how chart shapes are drawn (the user's setting; `OT_CHARTS`
    /// overrides it).
    pub fn set_chart_drawing(&mut self, choice: ChartDrawing) {
        if self.charts.choose(choice) {
            self.redraw_all = true;
        }
    }

    /// Find this frame's chart runs and draw into their bitmaps the ones it needs:
    /// every one for a whole frame, those reaching into `damage` for a partial one.
    fn prepare_charts(&mut self, dl: &DisplayList, damage: Option<&[Rect]>) -> Result<()> {
        let charts = &mut self.charts;
        charts.runs.clear();
        let mode = charts.mode();
        if mode == ChartDrawing::Direct2D {
            return Ok(());
        }
        if mode == ChartDrawing::Gpu && charts.gpu.is_none() {
            match GpuCharts::new(&self.d3d) {
                Ok(g) => charts.gpu = Some(g),
                Err(e) => {
                    tracing::warn!(error = %e, "no GPU chart drawing; drawing charts on the CPU");
                    charts.fall_back(ChartDrawing::Cpu);
                }
            }
        }
        let mode = charts.mode();
        let scale = self.dpi / 96.0;
        find_runs(dl, scale, self.size_px, &mut charts.runs);
        let n = charts.runs.len();
        charts.ready.clear();
        charts.ready.resize(n, false);
        charts.ranges.clear();
        charts.ranges.resize(n, (0, 0));
        charts.shapes.clear();
        let mut any = false;
        for (i, run) in charts.runs.iter().enumerate() {
            let wanted = !run.px.is_empty()
                && damage.is_none_or(|d| d.iter().any(|a| !run.dip.intersect(a).is_empty()));
            if wanted {
                charts.ranges[i] = charts.shapes.add_run(dl, run, scale);
                charts.ready[i] = true;
                any = true;
            }
        }
        if !any {
            return Ok(());
        }
        self.draw_runs(mode)
    }

    /// Draw the runs [`Gfx::prepare_charts`] marked ready into their bitmaps, with
    /// `mode`.
    fn draw_runs(&mut self, mode: ChartDrawing) -> Result<()> {
        let charts = &mut self.charts;
        let n = charts.runs.len();
        if charts.slots.len() < n {
            charts.slots.resize_with(n, || None);
        }
        for i in 0..n {
            if !charts.ready[i] {
                continue;
            }
            let run = charts.runs[i];
            charts.slots[i] = Some(slot_for(
                &self.dc,
                &self.d3d,
                charts.slots[i].take(),
                run.px.w,
                run.px.h,
                mode == ChartDrawing::Gpu,
            )?);
        }
        match mode {
            ChartDrawing::Gpu => {
                let Some(gpu) = charts.gpu.as_mut() else {
                    return Ok(());
                };
                gpu.upload(&charts.shapes)?;
                for i in 0..n {
                    let (true, Some(slot)) = (charts.ready[i], &charts.slots[i]) else {
                        continue;
                    };
                    if let Some(rtv) = &slot.target {
                        let run = charts.runs[i];
                        gpu.draw(rtv, run.px.w, run.px.h, charts.ranges[i])?;
                    }
                }
                gpu.finish();
            }
            ChartDrawing::Cpu => {
                for i in 0..n {
                    let (true, Some(slot)) = (charts.ready[i], &charts.slots[i]) else {
                        continue;
                    };
                    let run = charts.runs[i];
                    charts.shapes.rasterize(
                        charts.ranges[i],
                        run.px.w,
                        run.px.h,
                        &mut charts.raster,
                    );
                    let dst = D2D_RECT_U {
                        left: 0,
                        top: 0,
                        right: run.px.w,
                        bottom: run.px.h,
                    };
                    // SAFETY: the raster holds `h` rows of `w * 4` bytes, the rect's
                    // size, within the bitmap.
                    unsafe {
                        slot.bitmap.CopyFromMemory(
                            Some(&raw const dst),
                            charts.raster.pixels().as_ptr().cast(),
                            run.px.w * 4,
                        )?;
                    }
                }
            }
            ChartDrawing::Direct2D => {}
        }
        Ok(())
    }

    /// Draw the commands that touch each of `damage` into the canvas, clipped to
    /// it, over what the canvas holds from the last frame.
    unsafe fn draw_damage(&mut self, dl: &DisplayList, damage: &[Rect]) -> Result<()> {
        // Text this frame skips is still on screen: its layout stays cached. Often
        // enough that nothing on screen ages out ([`Gfx::evict_layouts`]), not every
        // frame: hashing every string is a good part of a frame that only moved a
        // chart.
        if self.frame.is_multiple_of(LAYOUT_TTL_FRAMES / 4) {
            self.touch_layouts(dl);
        }
        if damage.is_empty() {
            return Ok(());
        }
        // SAFETY: as for `render`.
        unsafe {
            self.dc.BeginDraw();
            let mut drawn = Ok(());
            for &area in damage {
                // Out to whole pixels, so the clip's edge leaves no pixel half drawn.
                let area = self.to_pixel_edges(area);
                let clip = rectf(area);
                self.dc
                    .PushAxisAlignedClip(&raw const clip, D2D1_ANTIALIAS_MODE_ALIASED);
                drawn = self.draw_cmds(dl, Some(area));
                self.dc.PopAxisAlignedClip();
                if drawn.is_err() {
                    break;
                }
            }
            self.dc.EndDraw(None, None)?;
            drawn
        }
    }

    /// Draw `dl` whole into a second bitmap and compare it with the canvas, just
    /// drawn in part over the last frame: where they differ, the partial drawing is
    /// wrong. Each such frame is logged and the first few dumped as BMPs (canvas,
    /// whole, difference) beside a text file of the damage and the commands under
    /// the difference, in [`DamageCheck::dir`].
    unsafe fn check_damage(&mut self, dl: &DisplayList, damage: &[Rect]) -> Result<()> {
        let (width, height) = (self.size_px.0.max(1), self.size_px.1.max(1));
        // SAFETY: as for `render`; the mapped rows are `pitch` apart and at least
        // `width * 4` bytes long, for `height` rows.
        unsafe {
            if self
                .check
                .as_ref()
                .is_some_and(|c| c.size != (width, height))
            {
                let props = |options| D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: self.dpi,
                    dpiY: self.dpi,
                    bitmapOptions: options,
                    colorContext: ManuallyDrop::new(None),
                };
                let size = D2D_SIZE_U { width, height };
                let target = props(D2D1_BITMAP_OPTIONS_TARGET);
                let full = self.dc.CreateBitmap(size, None, 0, &raw const target)?;
                let read = props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
                let read_a = self.dc.CreateBitmap(size, None, 0, &raw const read)?;
                let read_b = self.dc.CreateBitmap(size, None, 0, &raw const read)?;
                if let Some(c) = self.check.as_mut() {
                    c.size = (width, height);
                    c.bitmaps = Some((full, read_a, read_b));
                }
            }
            let (Some((full, read_a, read_b)), Some(canvas)) = (
                self.check.as_ref().and_then(|c| c.bitmaps.clone()),
                self.canvas.clone(),
            ) else {
                return Ok(());
            };
            self.dc.SetTarget(&full);
            let whole = self.draw_all(dl);
            self.dc.SetTarget(&canvas);
            whole?;
            read_a.CopyFromBitmap(None, &canvas, None)?;
            read_b.CopyFromBitmap(None, &full, None)?;
            let row = (width * 4) as usize;
            let mut drawn = vec![0u8; row * height as usize];
            let mut whole_px = vec![0u8; row * height as usize];
            for (bitmap, out) in [(&read_a, &mut drawn), (&read_b, &mut whole_px)] {
                let mapped = bitmap.Map(D2D1_MAP_OPTIONS_READ)?;
                for y in 0..height as usize {
                    let src = mapped.bits.add(y * mapped.pitch as usize);
                    std::ptr::copy_nonoverlapping(src, out[y * row..].as_mut_ptr(), row);
                }
                bitmap.Unmap()?;
            }
            self.report_damage_check(dl, damage, (width, height), &drawn, &whole_px);
        }
        Ok(())
    }

    /// Compare the canvas `drawn` with the whole frame `whole` (`size` pixels, BGRA) and
    /// report where they differ.
    fn report_damage_check(
        &mut self,
        dl: &DisplayList,
        damage: &[Rect],
        size: (u32, u32),
        drawn: &[u8],
        whole: &[u8],
    ) {
        use std::fmt::Write as _;
        let (width, height) = (size.0 as usize, size.1 as usize);
        let (mut count, mut worst) = (0usize, 0u8);
        let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                let diff = drawn[at..at + 4]
                    .iter()
                    .zip(&whole[at..at + 4])
                    .map(|(p, q)| p.abs_diff(*q))
                    .max()
                    .unwrap_or(0);
                if diff > 0 {
                    count += 1;
                    worst = worst.max(diff);
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                }
            }
        }
        let frame = self.frame;
        let Some(check) = self.check.as_mut() else {
            return;
        };
        check.frames += 1;
        if check.frames.is_multiple_of(1000) {
            tracing::info!(bad = check.bad, of = check.frames, "damage check");
        }
        if count == 0 {
            return;
        }
        check.bad += 1;
        let dip = 96.0 / self.dpi;
        let bbox = Rect::new(
            left as f32 * dip,
            top as f32 * dip,
            (right + 1 - left) as f32 * dip,
            (bottom + 1 - top) as f32 * dip,
        );
        tracing::warn!(
            frame,
            pixels = count,
            worst,
            ?bbox,
            ?damage,
            bad = check.bad,
            of = check.frames,
            "partial frame differs from the whole frame"
        );
        if check.dumped >= DamageCheck::MAX_DUMPS {
            return;
        }
        check.dumped += 1;
        let _ = std::fs::create_dir_all(&check.dir);
        let stem = check.dir.join(format!("frame-{frame:06}"));
        let marked: Vec<u8> = drawn
            .chunks_exact(4)
            .zip(whole.chunks_exact(4))
            .flat_map(|(p, q)| {
                if p == q {
                    [q[0] / 3, q[1] / 3, q[2] / 3, 255]
                } else {
                    [255, 0, 255, 255]
                }
            })
            .collect();
        for (name, px) in [("canvas", drawn), ("whole", whole), ("diff", &marked[..])] {
            let _ = write_bmp(&stem.with_extension(format!("{name}.bmp")), size, px);
        }
        let mut txt = String::new();
        let _ = writeln!(txt, "frame {frame}");
        let _ = writeln!(txt, "pixels differing: {count}, worst channel {worst}");
        let _ = writeln!(txt, "difference bbox (DIPs): {bbox:?}");
        let _ = writeln!(txt, "damage (DIPs): {damage:?}");
        let _ = writeln!(txt, "\ncommands touching the difference:");
        let prev = self.last.cmds();
        for (i, cmd) in dl.cmds().iter().enumerate() {
            let hit = |list: &DisplayList, c: &DrawCmd| {
                list.bounds(c)
                    .is_none_or(|r| !r.intersect(&bbox).is_empty())
            };
            if !hit(dl, cmd) && !prev.get(i).is_some_and(|p| hit(&self.last, p)) {
                continue;
            }
            let now = describe(dl, cmd);
            match prev.get(i).map(|p| describe(&self.last, p)) {
                Some(then) if then != now => {
                    let _ = writeln!(txt, "{i}: {now}\n    was {then}");
                }
                _ => {
                    let _ = writeln!(txt, "{i}: {now}");
                }
            }
        }
        let _ = std::fs::write(stem.with_extension("txt"), txt);
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
                }
                | DrawCmd::Graph {
                    points,
                    color: c,
                    width,
                } => {
                    let geom = self.path(dl.points(points), false)?;
                    self.set_color(c);
                    self.dc
                        .DrawGeometry(&geom, &self.brush, width, &self.line_style);
                }
                DrawCmd::Band {
                    top,
                    bottom,
                    color: c,
                } => {
                    // Along the top, then back along the bottom.
                    let mut pts = std::mem::take(&mut self.band_pts);
                    pts.clear();
                    pts.extend_from_slice(dl.points(top));
                    pts.extend(dl.points(bottom).iter().rev());
                    let geom = self.path(&pts, true);
                    self.band_pts = pts;
                    let geom = geom?;
                    self.set_color(c);
                    self.dc.FillGeometry(&geom, &self.brush, None);
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

    /// A path through `pts`, built with one call for all the segments (a chart's
    /// line has a point a DIP, and a call each was most of the cost of building it).
    fn path(&mut self, pts: &[Point], closed: bool) -> Result<ID2D1PathGeometry1> {
        self.path_pts.clear();
        self.path_pts.extend(pts[1..].iter().map(|&p| v2(p)));
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
            sink.AddLines(&self.path_pts);
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

fn created3d_device(warp_only: bool) -> Result<ID3D11Device> {
    let mut last = None;
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        if warp_only && driver == D3D_DRIVER_TYPE_HARDWARE {
            continue;
        }
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

/// The `OT_CHECK_DAMAGE` diagnostic ([`Gfx::check_damage`]).
struct DamageCheck {
    /// Where dumps go: the variable's value when it is a path, else
    /// `%TEMP%\open-task-damage-check`.
    dir: std::path::PathBuf,
    /// The window size the bitmaps were made for.
    size: (u32, u32),
    /// The whole frame, and two bitmaps the CPU reads the canvas and it through.
    bitmaps: Option<(ID2D1Bitmap1, ID2D1Bitmap1, ID2D1Bitmap1)>,
    frames: u64,
    bad: u64,
    dumped: u32,
}

impl DamageCheck {
    const MAX_DUMPS: u32 = 20;

    fn from_env() -> Option<Self> {
        let v = std::env::var_os("OT_CHECK_DAMAGE")?;
        let dir = if v.is_empty() || v == "1" {
            std::env::temp_dir().join("open-task-damage-check")
        } else {
            std::path::PathBuf::from(v)
        };
        tracing::info!(dir = %dir.display(), "checking every partial frame against a whole one");
        Some(Self {
            dir,
            size: (0, 0),
            bitmaps: None,
            frames: 0,
            bad: 0,
            dumped: 0,
        })
    }
}

/// One command, with what it indexes resolved, for the damage check's report.
fn describe(dl: &DisplayList, cmd: &DrawCmd) -> String {
    let bounds = dl.bounds(cmd);
    match *cmd {
        DrawCmd::Text(t) => format!(
            "Text {:?} at {:?}, color {:?}",
            dl.str(t.text),
            t.rect,
            t.color
        ),
        DrawCmd::Polyline {
            points,
            color,
            width,
        } => format!(
            "Polyline of {} points, width {width}, color {color:?}, bounds {bounds:?}",
            dl.points(points).len()
        ),
        DrawCmd::FillPolygon { points, color } => format!(
            "FillPolygon of {} points, color {color:?}, bounds {bounds:?}",
            dl.points(points).len()
        ),
        DrawCmd::Image { path, rect } => format!("Image {:?} at {rect:?}", dl.str(path)),
        other => format!("{other:?}"),
    }
}

/// Write `px` (`size` pixels, BGRA, top row first) as a 32-bit BMP.
fn write_bmp(path: &std::path::Path, size: (u32, u32), px: &[u8]) -> std::io::Result<()> {
    let (w, h) = size;
    let data = w * h * 4;
    let mut out = Vec::with_capacity(54 + data as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.cast_signed().to_le_bytes());
    // Negative: the top row comes first.
    out.extend_from_slice(&(-h.cast_signed()).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    out.extend_from_slice(px);
    std::fs::write(path, out)
}

/// How chart shapes are drawn, and what drawing them needs between frames.
struct Charts {
    /// The user's choice ([`Gfx::set_chart_drawing`]).
    choice: ChartDrawing,
    /// `OT_CHARTS`, which overrides it.
    forced: Option<ChartDrawing>,
    /// What to use instead after the choice failed (the GPU's shaders, say).
    fallback: Option<ChartDrawing>,
    gpu: Option<GpuCharts>,
    raster: ChartRaster,
    shapes: Shapes,
    /// This frame's runs, whether each was drawn this frame, its shapes, and its
    /// bitmap, by the run's place.
    runs: Vec<ChartRun>,
    ready: Vec<bool>,
    ranges: Vec<(u32, u32)>,
    slots: Vec<Option<RunSlot>>,
}

impl Charts {
    fn new() -> Self {
        let forced = std::env::var("OT_CHARTS")
            .ok()
            .and_then(|v| ChartDrawing::parse(&v));
        if let Some(f) = forced {
            tracing::info!(drawing = ?f, "chart drawing set by OT_CHARTS");
        }
        Self {
            choice: ChartDrawing::default(),
            forced,
            fallback: None,
            gpu: None,
            raster: ChartRaster::new(),
            shapes: Shapes::default(),
            runs: Vec::new(),
            ready: Vec::new(),
            ranges: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// What draws the charts now.
    fn mode(&self) -> ChartDrawing {
        let wanted = self.forced.unwrap_or(self.choice);
        match self.fallback {
            Some(f) if wanted == ChartDrawing::Gpu || f == ChartDrawing::Direct2D => f,
            _ => wanted,
        }
    }

    /// Take the user's choice. Returns whether what draws the charts changed.
    fn choose(&mut self, choice: ChartDrawing) -> bool {
        let before = self.mode();
        self.choice = choice;
        // A new choice gets a fresh try.
        self.fallback = None;
        let changed = self.mode() != before;
        if changed {
            self.slots.clear();
        }
        changed
    }

    /// Stop using what failed: draw with `instead` from now on.
    fn fall_back(&mut self, instead: ChartDrawing) {
        self.fallback = Some(instead);
        self.slots.clear();
    }
}

/// A chart run's bitmap: the GPU draws into its texture through `target`, the CPU
/// copies its pixels in; Direct2D draws it.
struct RunSlot {
    bitmap: ID2D1Bitmap1,
    target: Option<ID3D11RenderTargetView>,
    /// The size made, in pixels: at least the run's.
    w: u32,
    h: u32,
}

/// A bitmap for a `w` x `h` run: `slot` again when it is big enough and of the
/// right kind, else a new one with room to grow.
fn slot_for(
    dc: &ID2D1DeviceContext,
    device: &ID3D11Device,
    slot: Option<RunSlot>,
    w: u32,
    h: u32,
    gpu: bool,
) -> Result<RunSlot> {
    if let Some(s) = slot {
        if s.w >= w && s.h >= h && s.target.is_some() == gpu {
            return Ok(s);
        }
    }
    // Room to grow, so a chart's bounds moving by a pixel does not make a new one.
    let (w, h) = (
        w.next_multiple_of(64).max(64),
        h.next_multiple_of(64).max(64),
    );
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // One bitmap pixel a DIP: the draw maps it 1:1 to the device's pixels.
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: ManuallyDrop::new(None),
    };
    // SAFETY: the descriptors are complete locals; out-pointers are locals.
    unsafe {
        if !gpu {
            let size = D2D_SIZE_U {
                width: w,
                height: h,
            };
            let bitmap = dc.CreateBitmap(size, None, 0, &raw const props)?;
            return Ok(RunSlot {
                bitmap,
                target: None,
                w,
                h,
            });
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&raw const desc, None, Some(&raw mut texture))?;
        let texture = texture.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
        })?;
        let mut target = None;
        device.CreateRenderTargetView(&texture, None, Some(&raw mut target))?;
        let surface: IDXGISurface = texture.cast()?;
        let bitmap = dc.CreateBitmapFromDxgiSurface(&surface, Some(&raw const props))?;
        Ok(RunSlot {
            bitmap,
            target,
            w,
            h,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::{Color, Point};
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
    };

    impl Gfx {
        /// The canvas's pixels, premultiplied BGRA, top row first.
        fn canvas_pixels(&self) -> Vec<u8> {
            let (w, h) = self.size_px;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };
            let mut out = vec![0u8; (w * h * 4) as usize];
            // SAFETY: the mapped rows are `pitch` apart, each at least `w * 4` bytes.
            unsafe {
                let read = self
                    .dc
                    .CreateBitmap(
                        D2D_SIZE_U {
                            width: w,
                            height: h,
                        },
                        None,
                        0,
                        &raw const props,
                    )
                    .unwrap();
                read.CopyFromBitmap(None, self.canvas.as_ref().unwrap(), None)
                    .unwrap();
                let m = read.Map(D2D1_MAP_OPTIONS_READ).unwrap();
                for y in 0..h as usize {
                    std::ptr::copy_nonoverlapping(
                        m.bits.add(y * m.pitch as usize),
                        out[y * (w as usize) * 4..].as_mut_ptr(),
                        w as usize * 4,
                    );
                }
                read.Unmap().unwrap();
            }
            out
        }
    }

    /// A frame of chart shapes at awkward places: a stack of bands with hairlines,
    /// a spiky line with its wash and envelope, steep and flat segments, all at
    /// fractional coordinates, in a clip, over an opaque background.
    fn charts_frame(dl: &mut DisplayList) {
        dl.clear();
        dl.clear_to(Color::hex(0x20_20_20));
        dl.push_clip(Rect::new(10.3, 8.7, 280.0, 180.0));
        let xs: Vec<f32> = (0..120).map(|i| 6.0 + i as f32 * 2.37).collect();
        let wave =
            |i: usize, k: f32| 0.5 + 0.45 * ((i as f32 * 0.31 + k).sin() * (i as f32 * 0.07).cos());
        let mut lower: Vec<Point> = xs.iter().map(|&x| Point::new(x, 185.5)).collect();
        let colors = [0x39_87_E5, 0xD9_59_26, 0x19_9E_70, 0xC9_85_00];
        for (b, c) in colors.into_iter().enumerate() {
            let upper: Vec<Point> = lower
                .iter()
                .enumerate()
                .map(|(i, p)| Point::new(p.x, p.y - 30.0 * wave(i, b as f32)))
                .collect();
            dl.band(
                upper.iter().copied(),
                lower.iter().copied(),
                Color::hex(c).with_alpha(0.9),
            );
            dl.graph(upper.iter().copied(), Color::hex(0x20_20_20), 1.0);
            lower = upper;
        }
        dl.pop_clip();
        dl.push_clip(Rect::new(20.0, 200.0, 260.0, 90.0));
        let line: Vec<Point> = (0..90)
            .map(|i| {
                let spike = if i % 17 == 0 { -60.0 } else { 0.0 };
                Point::new(18.0 + i as f32 * 3.1, 270.0 - 40.0 * wave(i, 2.0) + spike)
            })
            .collect();
        dl.band(
            line.iter().copied(),
            [
                Point::new(18.0, 290.0),
                Point::new(18.0 + 89.0 * 3.1, 290.0),
            ],
            Color::hex(0x60_CD_FF).with_alpha(0.15),
        );
        dl.band(
            line.iter().map(|p| Point::new(p.x, p.y - 6.0)),
            line.iter().map(|p| Point::new(p.x, p.y + 5.0)),
            Color::hex(0x60_CD_FF).with_alpha(0.25),
        );
        dl.graph(line.iter().copied(), Color::hex(0x60_CD_FF), 1.5);
        dl.pop_clip();
    }

    /// How two frames differ: the largest channel difference, the mean, and the
    /// share of pixels more than 8 apart.
    fn compare(a: &[u8], b: &[u8]) -> (u8, f64, f64) {
        let mut worst = 0u8;
        let mut sum = 0u64;
        let mut far = 0usize;
        for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            let d = p
                .iter()
                .zip(q)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            worst = worst.max(d);
            sum += u64::from(d);
            far += usize::from(d > 8);
        }
        let n = (a.len() / 4) as f64;
        (worst, sum as f64 / n, far as f64 / n)
    }

    #[test]
    fn the_gpu_and_cpu_chart_renderers_draw_what_direct2d_draws() {
        // SAFETY: a hidden popup of a system class, destroyed below.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("chart parity"),
                WS_POPUP,
                0,
                0,
                450,
                450,
                None,
                None,
                None,
                None,
            )
        }
        .expect("a window");
        let mut dl = DisplayList::new();
        charts_frame(&mut dl);
        for dpi in [96.0, 144.0] {
            let size = ((300.0 * dpi / 96.0) as u32, (300.0 * dpi / 96.0) as u32);
            // WARP: the same pixels on every machine, CI's included.
            let device = created3d_device(true).expect("WARP");
            let mut gfx = Gfx::on_device(hwnd, size, dpi, device).expect("graphics");
            let mut frames = Vec::new();
            for mode in [ChartDrawing::Direct2D, ChartDrawing::Cpu, ChartDrawing::Gpu] {
                gfx.charts.forced = Some(mode);
                gfx.redraw_all = true;
                gfx.render(&dl, 0).expect("a frame");
                assert_eq!(gfx.charts.mode(), mode, "no fallback");
                frames.push(gfx.canvas_pixels());
            }
            let (d2d, cpu, gpu) = (&frames[0], &frames[1], &frames[2]);
            let cpu_gpu = compare(cpu, gpu);
            let cpu_d2d = compare(cpu, d2d);
            let gpu_d2d = compare(gpu, d2d);
            eprintln!("dpi {dpi}: cpu/gpu {cpu_gpu:?} cpu/d2d {cpu_d2d:?} gpu/d2d {gpu_d2d:?}");
            // `OT_PARITY_DUMP=<dir>`: the three frames as BMPs, to look at.
            if let Some(dir) = std::env::var_os("OT_PARITY_DUMP") {
                let dir = std::path::PathBuf::from(dir);
                let _ = std::fs::create_dir_all(&dir);
                for (name, px) in [("d2d", d2d), ("cpu", cpu), ("gpu", gpu)] {
                    let _ = write_bmp(&dir.join(format!("{name}-{dpi}.bmp")), size, px);
                }
            }
            // The two of ours compute the same coverage: rounding apart.
            assert!(cpu_gpu.0 <= 3, "cpu/gpu {cpu_gpu:?}");
            // Against Direct2D's antialiasing: the same shapes, edges a little
            // apart (a different coverage filter), nothing missing or extra.
            for (name, d) in [("cpu", cpu_d2d), ("gpu", gpu_d2d)] {
                assert!(d.1 < 1.5, "{name}/d2d mean {d:?}");
                assert!(d.2 < 0.03, "{name}/d2d pixels far apart {d:?}");
            }
        }
        // SAFETY: created above.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
