//! Chart shapes on the GPU: Direct3D 11 shaders that compute each pixel's coverage.
//!
//! The CPU's part is copying the shapes' points (in each run's pixels,
//! [`crate::charts::Shapes`]) into one buffer and a record per shape into another;
//! then, for each run, one instanced draw of a quad per shape into the run's
//! texture. The pixel shader finds the shape's top and bottom (a band) or nearest
//! segment (a graph) at its pixel by binary search over the points, which are
//! ascending in x, and computes the same coverage as
//! [`ot_paint::ChartRaster`]: four sub-samples across the pixel, exact vertically,
//! for a band; distance to the line for a graph. Blending is premultiplied
//! source-over, as Direct2D's.
//!
//! The shaders are compiled from source when the renderer starts (`D3DCompile`,
//! `d3dcompiler_47.dll`, part of Windows since 8.1), which takes a few
//! milliseconds; a failure there leaves the CPU renderer in charge.

use std::mem::size_of;

use windows::core::{s, Result, BOOL, PCSTR};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D11_SRV_DIMENSION_BUFFEREX, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
    ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11VertexShader,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BUFFEREX_SRV,
    D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_CPU_ACCESS_WRITE, D3D11_CULL_NONE,
    D3D11_FILL_SOLID, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE_DISCARD, D3D11_RASTERIZER_DESC,
    D3D11_RENDER_TARGET_BLEND_DESC, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_USAGE_DYNAMIC,
    D3D11_VIEWPORT,
};

use crate::charts::{Kind, Shapes};

/// The shaders. `Shape` must match [`GpuShape`] field for field.
const HLSL: &str = r"
struct Shape {
    float4 color;   // premultiplied
    float4 rect;    // x0, y0, x1, y1 in the run's pixels
    uint kind;      // 0 band, 1 graph
    uint aFirst;
    uint aCount;
    uint bFirst;
    uint bCount;
    float width;
    float2 pad;
};
StructuredBuffer<float2> points : register(t0);
StructuredBuffer<Shape> shapes : register(t1);
cbuffer Run : register(b0) {
    float2 size;
    uint firstShape;
    uint pad;
};

struct VOut {
    float4 pos : SV_Position;
    nointerpolation uint shape : SHAPE;
};

VOut vs(uint vid : SV_VertexID, uint iid : SV_InstanceID) {
    uint i = firstShape + iid;
    Shape s = shapes[i];
    float2 c = float2((vid & 1) ? s.rect.z : s.rect.x, (vid & 2) ? s.rect.w : s.rect.y);
    VOut o;
    o.pos = float4(c.x / size.x * 2.0 - 1.0, 1.0 - c.y / size.y * 2.0, 0.0, 1.0);
    o.shape = i;
    return o;
}

// The first of points[first .. first + count] whose x is at least `x`, relative to
// `first`; `count` when there is none.
uint lowerBound(uint first, uint count, float x) {
    uint lo = 0;
    uint hi = count;
    while (lo < hi) {
        uint mid = (lo + hi) / 2;
        if (points[first + mid].x < x) { lo = mid + 1; } else { hi = mid; }
    }
    return lo;
}

// The line through the points at `x`, by linear interpolation; the end values
// beyond either end. As ChartRaster's `at`.
float lineAt(uint first, uint count, float x) {
    uint i = lowerBound(first, count, x);
    if (i == 0) { return points[first].y; }
    if (i >= count) { return points[first + count - 1].y; }
    float2 a = points[first + i - 1];
    float2 b = points[first + i];
    float span = b.x - a.x;
    if (span <= 1.1920929e-7) { return b.y; }
    return a.y + (b.y - a.y) * min((x - a.x) / span, 1.0);
}

float segmentDistance(float2 p, float2 a, float2 b) {
    float2 d = b - a;
    float len2 = dot(d, d);
    float t = len2 > 0.0 ? saturate(dot(p - a, d) / len2) : 0.0;
    return length(a + t * d - p);
}

float4 ps(VOut v) : SV_Target {
    Shape s = shapes[v.shape];
    float2 px = v.pos.xy;
    float cov = 0.0;
    if (s.kind == 0) {
        float x0 = max(max(points[s.aFirst].x, points[s.bFirst].x), 0.0);
        float x1 = min(points[s.aFirst + s.aCount - 1].x, points[s.bFirst + s.bCount - 1].x);
        float col = floor(px.x);
        float row = floor(px.y);
        [unroll] for (uint k = 0; k < 4; k++) {
            float xs = col + (k + 0.5) / 4.0;
            if (xs >= x0 && xs <= x1) {
                float t = lineAt(s.aFirst, s.aCount, xs);
                float b = lineAt(s.bFirst, s.bCount, xs);
                cov += max(min(max(t, b), row + 1.0) - max(min(t, b), row), 0.0);
            }
        }
        cov /= 4.0;
    } else {
        float reach = s.width * 0.5 + 0.5;
        uint i = lowerBound(s.aFirst, s.aCount, px.x - reach);
        uint j = i == 0 ? 0 : i - 1;
        float d = 1e9;
        for (; j + 1 < s.aCount; j++) {
            float2 a = points[s.aFirst + j];
            if (a.x > px.x + reach) { break; }
            d = min(d, segmentDistance(px, a, points[s.aFirst + j + 1]));
        }
        cov = saturate(reach - d);
    }
    if (cov <= 0.0) { discard; }
    return s.color * cov;
}
";

/// One shape as the shaders read it (`Shape` in [`HLSL`]).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuShape {
    color: [f32; 4],
    rect: [f32; 4],
    kind: u32,
    a_first: u32,
    a_count: u32,
    b_first: u32,
    b_count: u32,
    width: f32,
    pad: [f32; 2],
}

/// The constant buffer (`Run` in [`HLSL`]).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct RunConstants {
    size: [f32; 2],
    first_shape: u32,
    pad: u32,
}

/// A structured buffer that grows as needed, with its view.
struct Structured {
    buffer: ID3D11Buffer,
    view: ID3D11ShaderResourceView,
    capacity: u32,
}

/// The Direct3D objects the chart shaders need. One per [`crate::gfx::Gfx`].
pub struct GpuCharts {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    constants: ID3D11Buffer,
    points: Option<Structured>,
    shapes: Option<Structured>,
    upload: Vec<GpuShape>,
}

impl std::fmt::Debug for GpuCharts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuCharts").finish_non_exhaustive()
    }
}

impl GpuCharts {
    /// Compile the shaders and make the fixed state, on `device`.
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let vs_code = compile(s!("vs"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps"), s!("ps_5_0"))?;
        // SAFETY: the descriptors are fully initialized locals; the bytecode blobs
        // outlive the calls.
        unsafe {
            let ctx = device.GetImmediateContext()?;
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vs_code), None, Some(&raw mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&ps_code), None, Some(&raw mut ps))?;
            let mut targets = [D3D11_RENDER_TARGET_BLEND_DESC::default(); 8];
            targets[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: BOOL(1),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let blend_desc = D3D11_BLEND_DESC {
                AlphaToCoverageEnable: BOOL(0),
                IndependentBlendEnable: BOOL(0),
                RenderTarget: targets,
            };
            let mut blend = None;
            device.CreateBlendState(&raw const blend_desc, Some(&raw mut blend))?;
            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: BOOL(1),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&raw const raster_desc, Some(&raw mut raster))?;
            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<RunConstants>().next_multiple_of(16) as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut constants = None;
            device.CreateBuffer(&raw const cb_desc, None, Some(&raw mut constants))?;
            Ok(Self {
                device: device.clone(),
                ctx,
                vs: vs.ok_or_else(fail)?,
                ps: ps.ok_or_else(fail)?,
                blend: blend.ok_or_else(fail)?,
                raster: raster.ok_or_else(fail)?,
                constants: constants.ok_or_else(fail)?,
                points: None,
                shapes: None,
                upload: Vec::new(),
            })
        }
    }

    /// Hand the shapes of every run this frame draws to the GPU: one copy of the
    /// points and one of the shape records, before any [`GpuCharts::draw`].
    pub fn upload(&mut self, shapes: &Shapes) -> Result<()> {
        self.upload.clear();
        self.upload.extend(shapes.shapes.iter().map(|s| {
            let a = s.color.a.clamp(0.0, 1.0);
            GpuShape {
                color: [s.color.r * a, s.color.g * a, s.color.b * a, a],
                rect: s.bounds,
                kind: match s.kind {
                    Kind::Band => 0,
                    Kind::Graph => 1,
                },
                a_first: s.a.0,
                a_count: s.a.1,
                b_first: s.b.0,
                b_count: s.b.1,
                width: s.width,
                pad: [0.0; 2],
            }
        }));
        let points: Vec<[f32; 2]> = shapes.points.iter().map(|p| [p.x, p.y]).collect();
        fill(&self.device, &self.ctx, &mut self.points, &points)?;
        let upload = std::mem::take(&mut self.upload);
        let r = fill(&self.device, &self.ctx, &mut self.shapes, &upload);
        self.upload = upload;
        r
    }

    /// Draw shapes `range` (from [`Shapes::add_run`], uploaded) into `target`,
    /// `w` x `h` pixels, cleared first.
    pub fn draw(
        &mut self,
        target: &ID3D11RenderTargetView,
        w: u32,
        h: u32,
        range: (u32, u32),
    ) -> Result<()> {
        let (Some(points), Some(shapes)) = (&self.points, &self.shapes) else {
            return Ok(());
        };
        // SAFETY: every object is ours and alive; the mapped constant buffer is
        // written within its size and unmapped before the draw.
        unsafe {
            let ctx = &self.ctx;
            ctx.ClearRenderTargetView(target, &[0.0; 4]);
            if range.1 == 0 {
                return Ok(());
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(
                &self.constants,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&raw mut mapped),
            )?;
            mapped
                .pData
                .cast::<RunConstants>()
                .write_unaligned(RunConstants {
                    size: [w as f32, h as f32],
                    first_shape: range.0,
                    pad: 0,
                });
            ctx.Unmap(&self.constants, 0);

            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            let views = [Some(points.view.clone()), Some(shapes.view.clone())];
            ctx.VSSetShaderResources(0, Some(&views));
            ctx.PSSetShaderResources(0, Some(&views));
            let cbs = [Some(self.constants.clone())];
            ctx.VSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetConstantBuffers(0, Some(&cbs));
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.OMSetBlendState(&self.blend, None, u32::MAX);
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.DrawInstanced(4, range.1, 0, 0);
        }
        Ok(())
    }

    /// Unbind what [`GpuCharts::draw`] bound, so Direct2D can read the targets.
    pub fn finish(&mut self) {
        // SAFETY: unbinding is always valid.
        unsafe {
            self.ctx.OMSetRenderTargets(None, None);
            let none = [None, None];
            self.ctx.VSSetShaderResources(0, Some(&none));
            self.ctx.PSSetShaderResources(0, Some(&none));
        }
    }
}

/// Copy `data` into `slot`'s structured buffer, growing it as needed.
fn fill<T: Copy>(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    slot: &mut Option<Structured>,
    data: &[T],
) -> Result<()> {
    let stride = size_of::<T>() as u32;
    let need = (data.len() as u32).max(1);
    if slot.as_ref().is_none_or(|s| s.capacity < need) {
        // Room to grow: the History's points grow for its first minutes.
        let capacity = need.next_power_of_two().max(1024);
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: capacity * stride,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
            StructureByteStride: stride,
        };
        let view_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
            ViewDimension: D3D11_SRV_DIMENSION_BUFFEREX,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                BufferEx: D3D11_BUFFEREX_SRV {
                    FirstElement: 0,
                    NumElements: capacity,
                    Flags: 0,
                },
            },
        };
        // SAFETY: the descriptors are complete; out-pointers are locals.
        unsafe {
            let mut buffer = None;
            device.CreateBuffer(&raw const desc, None, Some(&raw mut buffer))?;
            let buffer = buffer.ok_or_else(fail)?;
            let mut view = None;
            device.CreateShaderResourceView(
                &buffer,
                Some(&raw const view_desc),
                Some(&raw mut view),
            )?;
            *slot = Some(Structured {
                buffer,
                view: view.ok_or_else(fail)?,
                capacity,
            });
        }
    }
    let Some(s) = slot else {
        return Ok(());
    };
    // SAFETY: the buffer holds `capacity >= data.len()` elements of `T`; written
    // between Map and Unmap.
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(
            &s.buffer,
            0,
            D3D11_MAP_WRITE_DISCARD,
            0,
            Some(&raw mut mapped),
        )?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.pData.cast::<T>(), data.len());
        ctx.Unmap(&s.buffer, 0);
    }
    Ok(())
}

/// Compile `entry` of [`HLSL`] for `target`.
fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    // SAFETY: the source is a valid buffer of the given length; out-pointers are
    // locals.
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            s!("charts.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &raw mut code,
            Some(&raw mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .as_ref()
            .map(|b| String::from_utf8_lossy(blob_bytes(b)).into_owned());
        tracing::warn!(error = %e, compiler = ?msg, "chart shaders did not compile");
        return Err(e);
    }
    code.ok_or_else(fail)
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    // SAFETY: the blob owns `GetBufferSize` bytes at `GetBufferPointer` for as long
    // as it lives, which outlasts the borrow.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

fn fail() -> windows::core::Error {
    windows::core::Error::from_hresult(E_FAIL)
}

// The record the shaders read is 64 bytes, a multiple of 16.
const _: () = assert!(size_of::<GpuShape>() == 64);
