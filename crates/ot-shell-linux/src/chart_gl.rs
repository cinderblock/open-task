//! Chart shapes on the GPU, through OpenGL: the Linux counterpart of the Windows
//! shell's Direct3D renderer.
//!
//! The plugin gives a current offscreen context and the way to look up functions
//! ([`GlAccess`]); everything else is here, in the host, which owns the display
//! list: the shaders, the buffers, and reading each run back into an image. The
//! image goes to the plugin as a CPU-drawn one does ([`abi::Raster`]), so the
//! window, its kept pixels and partial repaints are exactly as they are without
//! a GPU. What it saves is the rasterizing; the plugin's blit stays.
//!
//! A piece of a shape ([`Piece`]) is one instance of a quad. For a band its pixels
//! read the band's top and bottom at four samples of their column
//! ([`Columns`], worked out on the CPU); for a line they search the points, which
//! are ascending in x, for the segments in reach. Same coverage as
//! [`ot_paint::ChartRaster`]: four sub-samples across the pixel, exact vertically,
//! for a band; distance to the line for a graph. Blending is premultiplied
//! source-over. The points and the columns live in floating-point textures read
//! with `texelFetch`, which, unlike storage buffers, OpenGL ES 3.0 has too.
//!
//! The target's rows are the image's: nothing is flipped, row 0 of the target is
//! drawn at y 0 and `glReadPixels` gives it first. The shader swaps red and blue, so
//! reading RGBA, which every driver does without converting, gives the image's BGRA.
//!
//! OpenGL 3.3 core or ES 3.0, so it runs on every Qt the plugin supports. Loading
//! a driver takes a hundred megabytes, so [`ChartGl::start`] is only called in the
//! app for someone who picked this, and after a process of its own has tried
//! it ([`crate::charts`]).
//!
//! [`abi::Raster`]: ot_shell_abi::Raster

// OpenGL takes sizes as `int`; a window and its charts' data are far below that.
#![allow(clippy::cast_possible_wrap)]

use std::ffi::{c_char, c_void, CStr, CString};

use ot_paint::chart_shapes::{Columns, Piece, Shapes, NO_COLUMNS};

/// `OtPlugin.gl_make_current`.
pub type GlMakeCurrent = unsafe extern "C" fn() -> bool;
/// `OtPlugin.gl_proc`.
pub type GlProc = unsafe extern "C" fn(name: *const c_char) -> *const c_void;

/// The plugin's two functions for reaching its offscreen context.
#[derive(Debug, Clone, Copy)]
pub struct GlAccess {
    pub make_current: GlMakeCurrent,
    pub proc_address: GlProc,
}

macro_rules! gl_functions {
    ($($field:ident = $name:literal : fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// The OpenGL functions used, looked up through the plugin.
        struct Api { $($field: unsafe extern "system" fn($($arg),*) $(-> $ret)?,)* }

        impl Api {
            /// Every function, or the name of the first the driver lacks.
            ///
            /// # Safety
            /// `get` is the current context's function lookup.
            unsafe fn load(get: GlProc) -> Result<Self, String> {
                Ok(Self { $($field: {
                    // SAFETY: the name is NUL-terminated; a non-null result is the
                    // function of that name, whose type is the one declared here.
                    unsafe {
                        let p = get(concat!($name, "\0").as_ptr().cast());
                        if p.is_null() {
                            return Err(format!("no {}", $name));
                        }
                        std::mem::transmute::<
                            *const c_void,
                            unsafe extern "system" fn($($arg),*) $(-> $ret)?,
                        >(p)
                    }
                },)* })
            }
        }
    };
}

/// A `GLenum`.
type Enum = u32;

gl_functions! {
    active_texture = "glActiveTexture": fn(Enum);
    attach_shader = "glAttachShader": fn(u32, u32);
    bind_buffer = "glBindBuffer": fn(Enum, u32);
    bind_framebuffer = "glBindFramebuffer": fn(Enum, u32);
    bind_texture = "glBindTexture": fn(Enum, u32);
    bind_vertex_array = "glBindVertexArray": fn(u32);
    blend_func_separate = "glBlendFuncSeparate": fn(Enum, Enum, Enum, Enum);
    buffer_data = "glBufferData": fn(Enum, isize, *const c_void, Enum);
    check_framebuffer_status = "glCheckFramebufferStatus": fn(Enum) -> Enum;
    clear = "glClear": fn(u32);
    clear_color = "glClearColor": fn(f32, f32, f32, f32);
    compile_shader = "glCompileShader": fn(u32);
    create_program = "glCreateProgram": fn() -> u32;
    create_shader = "glCreateShader": fn(Enum) -> u32;
    delete_shader = "glDeleteShader": fn(u32);
    delete_textures = "glDeleteTextures": fn(i32, *const u32);
    disable = "glDisable": fn(Enum);
    draw_arrays_instanced = "glDrawArraysInstanced": fn(Enum, i32, i32, i32);
    enable = "glEnable": fn(Enum);
    enable_vertex_attrib_array = "glEnableVertexAttribArray": fn(u32);
    framebuffer_texture_2d = "glFramebufferTexture2D": fn(Enum, Enum, Enum, u32, i32);
    gen_buffers = "glGenBuffers": fn(i32, *mut u32);
    gen_framebuffers = "glGenFramebuffers": fn(i32, *mut u32);
    gen_textures = "glGenTextures": fn(i32, *mut u32);
    gen_vertex_arrays = "glGenVertexArrays": fn(i32, *mut u32);
    get_error = "glGetError": fn() -> Enum;
    get_integerv = "glGetIntegerv": fn(Enum, *mut i32);
    get_program_info_log = "glGetProgramInfoLog": fn(u32, i32, *mut i32, *mut c_char);
    get_programiv = "glGetProgramiv": fn(u32, Enum, *mut i32);
    get_shader_info_log = "glGetShaderInfoLog": fn(u32, i32, *mut i32, *mut c_char);
    get_shaderiv = "glGetShaderiv": fn(u32, Enum, *mut i32);
    get_string = "glGetString": fn(Enum) -> *const u8;
    get_uniform_location = "glGetUniformLocation": fn(u32, *const c_char) -> i32;
    link_program = "glLinkProgram": fn(u32);
    pixel_storei = "glPixelStorei": fn(Enum, i32);
    read_pixels = "glReadPixels": fn(i32, i32, i32, i32, Enum, Enum, *mut c_void);
    shader_source = "glShaderSource": fn(u32, i32, *const *const c_char, *const i32);
    tex_image_2d = "glTexImage2D": fn(Enum, i32, i32, i32, i32, i32, Enum, Enum, *const c_void);
    tex_parameteri = "glTexParameteri": fn(Enum, Enum, i32);
    tex_sub_image_2d = "glTexSubImage2D": fn(Enum, i32, i32, i32, i32, i32, Enum, Enum, *const c_void);
    uniform_1i = "glUniform1i": fn(i32, i32);
    uniform_2f = "glUniform2f": fn(i32, f32, f32);
    use_program = "glUseProgram": fn(u32);
    vertex_attrib_divisor = "glVertexAttribDivisor": fn(u32, u32);
    vertex_attrib_i_pointer = "glVertexAttribIPointer": fn(u32, i32, Enum, i32, *const c_void);
    vertex_attrib_pointer = "glVertexAttribPointer": fn(u32, i32, Enum, u8, i32, *const c_void);
    viewport = "glViewport": fn(i32, i32, i32, i32);
}

const ARRAY_BUFFER: Enum = 0x8892;
const BLEND: Enum = 0x0BE2;
const COLOR_ATTACHMENT0: Enum = 0x8CE0;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const COMPILE_STATUS: Enum = 0x8B81;
const DITHER: Enum = 0x0BD0;
const FLOAT: Enum = 0x1406;
const FRAGMENT_SHADER: Enum = 0x8B30;
const FRAMEBUFFER: Enum = 0x8D40;
const FRAMEBUFFER_COMPLETE: Enum = 0x8CD5;
const LINK_STATUS: Enum = 0x8B82;
const MAG_FILTER: Enum = 0x2800;
const MAX_TEXTURE_SIZE: Enum = 0x0D33;
const MIN_FILTER: Enum = 0x2801;
const NEAREST: i32 = 0x2600;
const ONE: Enum = 1;
const ONE_MINUS_SRC_ALPHA: Enum = 0x0303;
const PACK_ALIGNMENT: Enum = 0x0D05;
const RENDERER: Enum = 0x1F01;
const RG: Enum = 0x8227;
const RG32F: i32 = 0x8230;
const RGBA: Enum = 0x1908;
const RGBA32F: i32 = 0x8814;
const RGBA8: i32 = 0x8058;
const STREAM_DRAW: Enum = 0x88E0;
const TEXTURE0: Enum = 0x84C0;
const TEXTURE_2D: Enum = 0x0DE1;
const TRIANGLE_STRIP: Enum = 5;
const UNSIGNED_BYTE: Enum = 0x1401;
const UNSIGNED_INT: Enum = 0x1405;
const VERSION: Enum = 0x1F02;
const VERTEX_SHADER: Enum = 0x8B31;

/// The data textures' width, in texels; a point takes one, a column two. Every
/// OpenGL 3 implementation has textures at least this big.
const DATA_W: usize = 1024;

/// The shaders' first lines, by the kind of OpenGL the context is.
const HEAD_CORE: &str = "#version 330 core\n";
const HEAD_ES: &str =
    "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n";

/// Attribute locations and layouts must match [`GlPiece`].
const VERTEX: &str = r"
layout(location = 0) in vec4 rect;    // x0, y0, x1, y1 in the run's pixels
layout(location = 1) in vec4 color;   // premultiplied
layout(location = 2) in uvec4 line;   // first point, count, the search's lo and hi
layout(location = 3) in uvec2 samples; // a band's first column's samples, that column
layout(location = 4) in float width;
uniform vec2 size;
flat out vec4 vColor;
flat out uvec4 vLine;
flat out uvec2 vSamples;
flat out float vWidth;
void main() {
    int v = gl_VertexID;
    vec2 c = vec2((v & 1) != 0 ? rect.z : rect.x, (v & 2) != 0 ? rect.w : rect.y);
    gl_Position = vec4(c / size * 2.0 - 1.0, 0.0, 1.0);
    vColor = color;
    vLine = line;
    vSamples = samples;
    vWidth = width;
}
";

const FRAGMENT: &str = r"
uniform sampler2D points;   // RG: x, y
uniform sampler2D columns;  // RGBA: tops, then bottoms, a column's in two texels
flat in vec4 vColor;
flat in uvec4 vLine;
flat in uvec2 vSamples;
flat in float vWidth;
out vec4 fragColor;

vec2 pointAt(uint i) {
    return texelFetch(points, ivec2(int(i & 1023u), int(i >> 10)), 0).xy;
}

// The first of the line's points whose x is at least `x`, relative to `first`; the
// answer is known to be in `lo..=hi`.
uint lowerBound(uint first, uint lo, uint hi, float x) {
    while (lo < hi) {
        uint mid = (lo + hi) / 2u;
        if (pointAt(first + mid).x < x) { lo = mid + 1u; } else { hi = mid; }
    }
    return lo;
}

float segmentDistance(vec2 p, vec2 a, vec2 b) {
    vec2 d = b - a;
    float len2 = dot(d, d);
    float t = len2 > 0.0 ? clamp(dot(p - a, d) / len2, 0.0, 1.0) : 0.0;
    return length(a + t * d - p);
}

void main() {
    vec2 px = gl_FragCoord.xy;
    float cov = 0.0;
    if (vSamples.x != 0xFFFFFFFFu) {
        float row = floor(px.y);
        uint c = (vSamples.x + (uint(px.x) - vSamples.y)) * 2u;
        ivec2 at = ivec2(int(c & 1023u), int(c >> 10));
        vec4 t = texelFetch(columns, at, 0);
        vec4 b = texelFetch(columns, at + ivec2(1, 0), 0);
        vec4 k = max(min(max(t, b), row + 1.0) - max(min(t, b), row), 0.0);
        cov = (((k.x + k.y) + k.z) + k.w) / 4.0;
    } else {
        float reach = vWidth * 0.5 + 0.5;
        uint i = lowerBound(vLine.x, vLine.z, vLine.w, px.x - reach);
        uint j = i == 0u ? 0u : i - 1u;
        float d = 1e9;
        for (; j + 1u < vLine.y; j++) {
            vec2 a = pointAt(vLine.x + j);
            if (a.x > px.x + reach) { break; }
            d = min(d, segmentDistance(px, a, pointAt(vLine.x + j + 1u)));
        }
        cov = clamp(reach - d, 0.0, 1.0);
    }
    if (cov <= 0.0) { discard; }
    fragColor = (vColor * cov).bgra;
}
";

/// One piece as the vertex shader takes it: all that its pixels need of its shape,
/// so there is no table of shapes to size for the largest page.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct GlPiece {
    rect: [f32; 4],
    color: [f32; 4],
    first: u32,
    count: u32,
    lo: u32,
    hi: u32,
    samples: u32,
    column: u32,
    width: f32,
    pad: f32,
}

const _: () = assert!(size_of::<GlPiece>() == 64);
// `vSamples.x != 0xFFFFFFFFu` in the shader.
const _: () = assert!(NO_COLUMNS == u32::MAX);

/// A texture of points or columns, growing by rows.
#[derive(Debug, Default)]
struct DataTexture {
    id: u32,
    rows: usize,
}

/// A failure the renderer cannot go on from: the context is gone or a call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlLost(pub String);

/// The OpenGL objects the chart shaders need.
pub struct ChartGl {
    api: Api,
    access: GlAccess,
    renderer: String,
    program: u32,
    size_at: i32,
    framebuffer: u32,
    target: u32,
    target_size: (i32, i32),
    array: u32,
    buffer: u32,
    points: DataTexture,
    columns: DataTexture,
    /// Texture rows the driver allows, for points and for columns alike.
    max_rows: usize,
    pieces: Vec<GlPiece>,
    cut: Vec<Piece>,
    samples: Columns,
    /// Where each shape's pieces start (one more than there are shapes).
    starts: Vec<u32>,
    flat: Vec<[f32; 2]>,
}

impl std::fmt::Debug for ChartGl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChartGl").finish_non_exhaustive()
    }
}

/// Whether a renderer's name says it draws on the CPU. Shaders run on the CPU are
/// far dearer than the rasterizer they would replace.
#[must_use]
pub fn is_software(renderer: &str) -> bool {
    let name = renderer.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "swrast", "software", "basic render"]
        .iter()
        .any(|s| name.contains(s))
}

impl ChartGl {
    /// Make the context current, load OpenGL, and compile the shaders. The error
    /// says why not, for the log; `software` allows a renderer that is on the CPU.
    ///
    /// # Errors
    /// No context, a function missing, a software renderer, or the shaders
    /// failing to build.
    ///
    /// # Safety
    /// `access` must be the plugin's, called on its UI thread.
    pub unsafe fn start(access: GlAccess, software: bool) -> Result<Self, String> {
        // SAFETY: the caller's promise about `access`.
        if !unsafe { (access.make_current)() } {
            return Err("no OpenGL context".to_owned());
        }
        // SAFETY: the context is current, and every call below is on this thread
        // with arguments that are valid locals.
        unsafe {
            let api = Api::load(access.proc_address)?;
            let name = gl_string(&api, RENDERER);
            let version = gl_string(&api, VERSION);
            tracing::info!(renderer = %name, %version, "chart drawing: OpenGL");
            if is_software(&name) && !software {
                return Err(format!("{name} draws on the CPU"));
            }
            let head = if version.starts_with("OpenGL ES") {
                HEAD_ES
            } else {
                HEAD_CORE
            };
            let program = link(&api, head)?;
            (api.use_program)(program);
            let at = |name: &CStr| (api.get_uniform_location)(program, name.as_ptr());
            (api.uniform_1i)(at(c"points"), 0);
            (api.uniform_1i)(at(c"columns"), 1);
            let size_at = at(c"size");

            let (mut array, mut buffer, mut framebuffer, mut target) = (0, 0, 0, 0);
            (api.gen_vertex_arrays)(1, &raw mut array);
            (api.bind_vertex_array)(array);
            (api.gen_buffers)(1, &raw mut buffer);
            (api.bind_buffer)(ARRAY_BUFFER, buffer);
            for location in 0..5 {
                (api.enable_vertex_attrib_array)(location);
                (api.vertex_attrib_divisor)(location, 1);
            }
            (api.gen_framebuffers)(1, &raw mut framebuffer);
            (api.gen_textures)(1, &raw mut target);
            (api.pixel_storei)(PACK_ALIGNMENT, 4);
            (api.enable)(BLEND);
            (api.disable)(DITHER);
            (api.blend_func_separate)(ONE, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA);
            (api.clear_color)(0.0, 0.0, 0.0, 0.0);
            let mut max = 0;
            (api.get_integerv)(MAX_TEXTURE_SIZE, &raw mut max);
            let error = (api.get_error)();
            if error != 0 {
                return Err(format!("OpenGL error {error:#x} while starting"));
            }
            Ok(Self {
                api,
                access,
                renderer: name,
                program,
                size_at,
                framebuffer,
                target,
                target_size: (0, 0),
                array,
                buffer,
                points: DataTexture::default(),
                columns: DataTexture::default(),
                max_rows: usize::try_from(max).unwrap_or(0),
                pieces: Vec::new(),
                cut: Vec::new(),
                samples: Columns::new(),
                starts: Vec::new(),
                flat: Vec::new(),
            })
        }
    }

    /// What the driver calls the GPU.
    #[must_use]
    pub fn renderer(&self) -> &str {
        &self.renderer
    }

    /// Hand the shapes of every run this frame draws to the GPU, before any
    /// [`ChartGl::draw`].
    ///
    /// # Errors
    /// The context is gone, or the shapes do not fit the driver's textures.
    pub fn upload(&mut self, shapes: &Shapes) -> Result<(), GlLost> {
        // SAFETY: the plugin's function, on its UI thread.
        if !unsafe { (self.access.make_current)() } {
            return Err(GlLost("the OpenGL context is gone".to_owned()));
        }
        self.cut.clear();
        self.samples.clear();
        self.starts.clear();
        self.pieces.clear();
        for (i, s) in shapes.shapes.iter().enumerate() {
            self.starts.push(self.pieces.len() as u32);
            let from = self.cut.len();
            shapes.pieces(i, false, &mut self.cut, &mut self.samples);
            let a = s.color.a.clamp(0.0, 1.0);
            let color = [s.color.r * a, s.color.g * a, s.color.b * a, a];
            self.pieces.extend(self.cut[from..].iter().map(|p| GlPiece {
                rect: p.rect,
                color,
                first: s.a.0,
                count: s.a.1,
                lo: p.a.0,
                hi: p.a.1,
                samples: p.columns,
                column: p.column,
                width: s.width,
                pad: 0.0,
            }));
        }
        self.starts.push(self.pieces.len() as u32);
        self.flat.clear();
        self.flat.extend(shapes.points.iter().map(|p| [p.x, p.y]));
        let point_rows = self.flat.len().div_ceil(DATA_W).max(1);
        let column_rows = (self.samples.len() * 2).div_ceil(DATA_W).max(1);
        if point_rows.max(column_rows) > self.max_rows {
            return Err(GlLost(format!(
                "{} points and {} columns do not fit a texture",
                self.flat.len(),
                self.samples.len()
            )));
        }
        // A whole number of rows: the last is padded, as the upload needs.
        self.flat.resize(point_rows * DATA_W, [0.0; 2]);
        self.samples.resize(column_rows * DATA_W / 2, [[0.0; 4]; 2]);
        // SAFETY: the context is current; the pointers are to live slices of the
        // sizes given, and the textures hold at least those rows.
        unsafe {
            let api = &self.api;
            (api.bind_buffer)(ARRAY_BUFFER, self.buffer);
            (api.buffer_data)(
                ARRAY_BUFFER,
                std::mem::size_of_val(&self.pieces[..]) as isize,
                self.pieces.as_ptr().cast(),
                STREAM_DRAW,
            );
            (api.active_texture)(TEXTURE0);
            fill(
                api,
                &mut self.points,
                (RG32F, RG),
                point_rows,
                self.flat.as_ptr().cast(),
            );
            (api.active_texture)(TEXTURE0 + 1);
            fill(
                api,
                &mut self.columns,
                (RGBA32F, RGBA),
                column_rows,
                self.samples.as_ptr().cast(),
            );
            (api.use_program)(self.program);
            (api.bind_vertex_array)(self.array);
            (api.bind_framebuffer)(FRAMEBUFFER, self.framebuffer);
        }
        Ok(())
    }

    /// Draw shapes `range` (from [`Shapes::add_run`], uploaded) into `image`, made
    /// `w` x `h` pixels of premultiplied BGRA, top row first.
    ///
    /// # Errors
    /// OpenGL reported an error, or the target could not be made.
    pub fn draw(
        &mut self,
        range: (u32, u32),
        w: u32,
        h: u32,
        image: &mut Vec<u8>,
    ) -> Result<(), GlLost> {
        image.resize(w as usize * h as usize * 4, 0);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let (w, h) = (w as i32, h as i32);
        let at = |i: u32| self.starts.get(i as usize).copied().unwrap_or(0);
        let first = at(range.0);
        let count = at(range.0 + range.1).saturating_sub(first);
        self.grow_target(w, h)?;
        // SAFETY: the context is current (`upload`, which `draw` follows); the
        // pixels written are `w * h` RGBA, the size of `image`.
        unsafe {
            let api = &self.api;
            (api.viewport)(0, 0, w, h);
            (api.uniform_2f)(self.size_at, w as f32, h as f32);
            (api.clear)(COLOR_BUFFER_BIT);
            if count > 0 {
                piece_attributes(api, first as usize * size_of::<GlPiece>());
                (api.draw_arrays_instanced)(TRIANGLE_STRIP, 0, 4, count as i32);
            }
            (api.read_pixels)(0, 0, w, h, RGBA, UNSIGNED_BYTE, image.as_mut_ptr().cast());
            match (api.get_error)() {
                0 => Ok(()),
                e => Err(GlLost(format!("OpenGL error {e:#x}"))),
            }
        }
    }

    /// Make the target at least `w` x `h`.
    fn grow_target(&mut self, w: i32, h: i32) -> Result<(), GlLost> {
        if w <= self.target_size.0 && h <= self.target_size.1 {
            return Ok(());
        }
        // Room to grow, so a chart's bounds moving by a pixel makes no new target.
        let grown = |v: i32| (v + 63) & !63;
        let size = (
            grown(w).max(self.target_size.0),
            grown(h).max(self.target_size.1),
        );
        // SAFETY: the context is current; no data is passed.
        let status = unsafe {
            let api = &self.api;
            (api.active_texture)(TEXTURE0 + 2);
            (api.bind_texture)(TEXTURE_2D, self.target);
            (api.tex_parameteri)(TEXTURE_2D, MIN_FILTER, NEAREST);
            (api.tex_parameteri)(TEXTURE_2D, MAG_FILTER, NEAREST);
            (api.tex_image_2d)(
                TEXTURE_2D,
                0,
                RGBA8,
                size.0,
                size.1,
                0,
                RGBA,
                UNSIGNED_BYTE,
                std::ptr::null(),
            );
            (api.framebuffer_texture_2d)(
                FRAMEBUFFER,
                COLOR_ATTACHMENT0,
                TEXTURE_2D,
                self.target,
                0,
            );
            (api.check_framebuffer_status)(FRAMEBUFFER)
        };
        if status != FRAMEBUFFER_COMPLETE {
            return Err(GlLost(format!("framebuffer status {status:#x}")));
        }
        self.target_size = size;
        Ok(())
    }
}

/// The attributes of the pieces from `base` bytes into the buffer.
///
/// # Safety
/// The context is current, its vertex array and buffer bound.
unsafe fn piece_attributes(api: &Api, base: usize) {
    let stride = size_of::<GlPiece>() as i32;
    let at = |offset: usize| (base + offset) as *const c_void;
    // SAFETY: as the caller promises; with a buffer bound the pointers are offsets.
    unsafe {
        (api.vertex_attrib_pointer)(0, 4, FLOAT, 0, stride, at(0));
        (api.vertex_attrib_pointer)(1, 4, FLOAT, 0, stride, at(16));
        (api.vertex_attrib_i_pointer)(2, 4, UNSIGNED_INT, stride, at(32));
        (api.vertex_attrib_i_pointer)(3, 2, UNSIGNED_INT, stride, at(48));
        (api.vertex_attrib_pointer)(4, 1, FLOAT, 0, stride, at(56));
    }
}

/// Put `rows` rows at `data` in `texture`, on the active unit, growing it as needed.
///
/// # Safety
/// The context is current; `data` holds `rows * DATA_W` texels of `format`.
unsafe fn fill(
    api: &Api,
    texture: &mut DataTexture,
    (internal, format): (i32, Enum),
    rows: usize,
    data: *const c_void,
) {
    // SAFETY: as the caller promises.
    unsafe {
        if rows > texture.rows {
            if texture.id != 0 {
                (api.delete_textures)(1, &raw const texture.id);
            }
            // Room to grow: the History's points grow for its first minutes.
            texture.rows = rows.next_power_of_two().max(16);
            (api.gen_textures)(1, &raw mut texture.id);
            (api.bind_texture)(TEXTURE_2D, texture.id);
            (api.tex_parameteri)(TEXTURE_2D, MIN_FILTER, NEAREST);
            (api.tex_parameteri)(TEXTURE_2D, MAG_FILTER, NEAREST);
            (api.tex_image_2d)(
                TEXTURE_2D,
                0,
                internal,
                DATA_W as i32,
                texture.rows as i32,
                0,
                format,
                FLOAT,
                std::ptr::null(),
            );
        }
        (api.bind_texture)(TEXTURE_2D, texture.id);
        (api.tex_sub_image_2d)(
            TEXTURE_2D,
            0,
            0,
            0,
            DATA_W as i32,
            rows as i32,
            format,
            FLOAT,
            data,
        );
    }
}

/// # Safety
/// The context is current.
unsafe fn gl_string(api: &Api, name: Enum) -> String {
    // SAFETY: as the caller promises; a non-null result is NUL-terminated.
    unsafe {
        let p = (api.get_string)(name);
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p.cast()).to_string_lossy().into_owned()
        }
    }
}

/// # Safety
/// The context is current.
unsafe fn compile(api: &Api, kind: Enum, head: &str, body: &str) -> Result<u32, String> {
    let text = CString::new(format!("{head}{body}")).map_err(|e| e.to_string())?;
    let source = text.as_ptr();
    // SAFETY: as the caller promises; the source is NUL-terminated and outlives
    // the calls, the log's size is given.
    unsafe {
        let shader = (api.create_shader)(kind);
        (api.shader_source)(shader, 1, &raw const source, std::ptr::null());
        (api.compile_shader)(shader);
        let mut ok = 0;
        (api.get_shaderiv)(shader, COMPILE_STATUS, &raw mut ok);
        if ok != 0 {
            return Ok(shader);
        }
        let mut log = [0u8; 1024];
        let mut n = 0;
        (api.get_shader_info_log)(shader, 1024, &raw mut n, log.as_mut_ptr().cast());
        Err(format!(
            "a chart shader did not compile: {}",
            String::from_utf8_lossy(&log[..usize::try_from(n).unwrap_or(0)])
        ))
    }
}

/// # Safety
/// The context is current.
unsafe fn link(api: &Api, head: &str) -> Result<u32, String> {
    // SAFETY: as the caller promises; the log's size is given.
    unsafe {
        let vertex = compile(api, VERTEX_SHADER, head, VERTEX)?;
        let fragment = compile(api, FRAGMENT_SHADER, head, FRAGMENT)?;
        let program = (api.create_program)();
        (api.attach_shader)(program, vertex);
        (api.attach_shader)(program, fragment);
        (api.link_program)(program);
        (api.delete_shader)(vertex);
        (api.delete_shader)(fragment);
        let mut ok = 0;
        (api.get_programiv)(program, LINK_STATUS, &raw mut ok);
        if ok != 0 {
            return Ok(program);
        }
        let mut log = [0u8; 1024];
        let mut n = 0;
        (api.get_program_info_log)(program, 1024, &raw mut n, log.as_mut_ptr().cast());
        Err(format!(
            "the chart shaders did not link: {}",
            String::from_utf8_lossy(&log[..usize::try_from(n).unwrap_or(0)])
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_renderer_on_the_cpu_is_known_by_its_name() {
        assert!(is_software("llvmpipe (LLVM 20.1.2, 256 bits)"));
        assert!(is_software("softpipe"));
        assert!(is_software("Software Rasterizer"));
        assert!(is_software("D3D12 (Microsoft Basic Render Driver)"));
        assert!(!is_software(
            "AMD Radeon RX 6700 XT (radeonsi, navi22, LLVM 19.1.1)"
        ));
        assert!(!is_software("D3D12 (Intel(R) UHD Graphics)"));
    }

    #[test]
    fn data_textures_are_addressed_as_the_shader_does() {
        // `i & 1023`, `i >> 10`.
        assert_eq!(DATA_W, 1024);
        assert!(FRAGMENT.contains("i & 1023u") && FRAGMENT.contains("i >> 10"));
    }
}
