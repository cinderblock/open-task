//! A rasterizer for chart shapes ([`DrawCmd::Band`](crate::DrawCmd::Band) and
//! [`DrawCmd::Graph`](crate::DrawCmd::Graph)), on the CPU.
//!
//! A general path rasterizer has to handle any outline; a chart's shapes are
//! functions of x, which makes antialiased coverage a short loop per pixel column.
//! This is the CPU renderer for them, and the reference for the GPU one, which
//! computes the same coverage in a pixel shader:
//!
//! - **Band:** [`SAMPLES`] sub-samples across each pixel column; at each, the top
//!   and bottom by linear interpolation between their points; the exact overlap
//!   of `[top, bottom]` with each pixel row; coverage is the mean over the samples.
//! - **Graph:** `d`, the distance from the pixel's center to the nearest segment;
//!   coverage is `clamp(width / 2 + 0.5 - d, 0, 1)`.
//!
//! Everything here is in the raster's own pixels: callers map DIPs to device
//! pixels and subtract the raster's origin ([`to_pixels`]). The result is 32-bit
//! premultiplied BGRA, rows top first, what a GPU bitmap takes as is.

use crate::color::Color;
use crate::geom::Point;

/// Sub-samples across a pixel column for a band's coverage.
pub const SAMPLES: u32 = 4;

/// A premultiplied BGRA pixel buffer chart shapes are drawn into, in order, with
/// source-over blending. Reused between frames: [`ChartRaster::reset`] keeps the
/// allocation.
#[derive(Debug, Default, Clone)]
pub struct ChartRaster {
    width: u32,
    height: u32,
    /// Premultiplied BGRA, four bytes a pixel; blended as one little-endian `u32`
    /// (blue in the low byte).
    px: Vec<u8>,
    /// For each column of the band being filled, the rows it covers whole.
    inner: Vec<(u32, u32)>,
    /// The segments of the line being drawn ([`Seg`]).
    segs: Vec<Seg>,
    /// The squared distance of each pixel in a column's run to the line.
    d2: Vec<f32>,
}

impl ChartRaster {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the raster `width` x `height`, every pixel transparent.
    pub fn reset(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.px.clear();
        self.px.resize(width as usize * height as usize * 4, 0);
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels: premultiplied BGRA, `width * 4` bytes a row, top row first.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.px
    }

    /// Blend `src` over pixel `i` (counted in pixels, not bytes).
    fn put(&mut self, i: usize, src: Src) {
        let at = &mut self.px[i * 4..i * 4 + 4];
        let dst = u32::from_le_bytes([at[0], at[1], at[2], at[3]]);
        at.copy_from_slice(&over(dst, src).to_le_bytes());
    }

    /// Fill the region between `top` and `bottom` (each ascending in x) over the
    /// x-range both cover.
    pub fn fill_band(&mut self, top: &[Point], bottom: &[Point], color: Color) {
        let (Some(t0), Some(t1), Some(b0), Some(b1)) =
            (top.first(), top.last(), bottom.first(), bottom.last())
        else {
            return;
        };
        let x0 = t0.x.max(b0.x).max(0.0);
        let x1 = t1.x.min(b1.x).min(self.width as f32);
        // Also false for NaN: nothing to draw.
        if x1.partial_cmp(&x0) != Some(std::cmp::Ordering::Greater) || self.height == 0 {
            return;
        }
        let ink = ink(color);
        let (mut ti, mut bi) = (0, 0);
        let first = x0.floor() as u32;
        let last = (x1.ceil() as u32).min(self.width);
        let mut lo = [0.0f32; SAMPLES as usize];
        let mut hi = [0.0f32; SAMPLES as usize];
        self.inner.clear();
        for col in first..last {
            // The samples inside the band's x-range, with its top and bottom there.
            let mut n = 0;
            for k in 0..SAMPLES {
                let xs = col as f32 + (k as f32 + 0.5) / SAMPLES as f32;
                if xs < x0 || xs > x1 {
                    continue;
                }
                let t = at(top, &mut ti, xs);
                let b = at(bottom, &mut bi, xs);
                lo[n] = t.min(b);
                hi[n] = t.max(b);
                n += 1;
            }
            if n == 0 {
                self.inner.push((0, 0));
                continue;
            }
            let (lo, hi) = (&lo[..n], &hi[..n]);
            let top_px = lo.iter().copied().fold(f32::INFINITY, f32::min);
            let bottom_px = hi.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let r0 = top_px.floor().max(0.0) as u32;
            let r1 = (bottom_px.ceil().max(0.0) as u32).min(self.height);
            // Rows every sample covers whole: the same coverage all the way down.
            let inner0 = lo.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil();
            let inner1 = hi.iter().copied().fold(f32::INFINITY, f32::min).floor();
            // Rows every sample covers whole are filled below, a row at a time;
            // only a column with all its samples inside takes part.
            let (in0, in1) = if n == SAMPLES as usize && inner1 > inner0 {
                (
                    (inner0.max(0.0) as u32).clamp(r0, r1),
                    (inner1.max(0.0) as u32).clamp(r0, r1),
                )
            } else {
                (r1, r1)
            };
            for row in (r0..in0).chain(in1.max(in0)..r1) {
                let (y0, y1) = (row as f32, row as f32 + 1.0);
                let cov = lo
                    .iter()
                    .zip(hi)
                    .map(|(&l, &h)| (h.min(y1) - l.max(y0)).max(0.0))
                    .sum::<f32>()
                    / SAMPLES as f32;
                self.blend(col, row, ink, cov);
            }
            self.inner.push((in0, in1.max(in0)));
        }
        // The whole rows, as runs along each row: contiguous pixels, one color.
        let whole = scaled(ink, 256);
        let inner = std::mem::take(&mut self.inner);
        let top_row = inner.iter().map(|r| r.0).min().unwrap_or(0);
        let bottom_row = inner.iter().map(|r| r.1).max().unwrap_or(0);
        for row in top_row..bottom_row {
            let mut c = 0;
            while c < inner.len() {
                if !(inner[c].0..inner[c].1).contains(&row) {
                    c += 1;
                    continue;
                }
                let start = c;
                while c < inner.len() && (inner[c].0..inner[c].1).contains(&row) {
                    c += 1;
                }
                let at = row as usize * self.width as usize + first as usize + start;
                self.fill_span(at, c - start, whole);
            }
        }
        self.inner = inner;
    }

    /// Blend `src` over `len` pixels from pixel `at` along a row: byte by byte
    /// with the same weight, sixteen bytes a step, which compiles to vector code.
    fn fill_span(&mut self, at: usize, len: usize, src: Src) {
        let bytes = src.px.to_le_bytes();
        let mut pattern = [0u16; 16];
        for (k, p) in pattern.iter_mut().enumerate() {
            *p = u16::from(bytes[k % 4]);
        }
        // At most 256, so a byte times it fits in 16 bits.
        let keep = src.keep as u16;
        let span = &mut self.px[at * 4..(at + len) * 4];
        let mut chunks = span.chunks_exact_mut(16);
        for chunk in &mut chunks {
            for (d, &p) in chunk.iter_mut().zip(&pattern) {
                let v = p + ((u16::from(*d) * keep + 128) >> 8);
                *d = v.min(255) as u8;
            }
        }
        for (d, &p) in chunks.into_remainder().iter_mut().zip(&pattern) {
            let v = p + ((u16::from(*d) * keep + 128) >> 8);
            *d = v.min(255) as u8;
        }
    }

    /// Draw a line `width` pixels wide through `points` (ascending in x).
    ///
    /// Column by column: the segments that come within reach of a column are a few
    /// neighbours, since the points ascend in x; each pixel takes its distance to
    /// the nearest of them, so a joint is blended once.
    pub fn stroke_graph(&mut self, points: &[Point], width: f32, color: Color) {
        let n = points.len();
        if n < 2 || self.width == 0 || self.height == 0 || width <= 0.0 {
            return;
        }
        let reach = width * 0.5 + 0.5;
        let reach2 = reach * reach;
        let ink = ink(color);
        let mut segs = std::mem::take(&mut self.segs);
        segs.clear();
        segs.extend(points.windows(2).map(|w| Seg::new(w[0], w[1])));
        let c0 = (points[0].x - reach).floor().max(0.0) as u32;
        let c1 = ((points[n - 1].x + reach).ceil().max(0.0) as u32).min(self.width);
        let mut first = 0;
        for col in c0..c1 {
            let cx = col as f32 + 0.5;
            let (left, right) = (cx - reach, cx + reach);
            // Segment j runs from point j to point j + 1.
            while first + 1 < segs.len() && points[first + 1].x < left {
                first += 1;
            }
            let mut last = first;
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            while last < segs.len() && points[last].x <= right {
                let (a, b) = (points[last], points[last + 1]);
                lo = lo.min(a.y.min(b.y));
                hi = hi.max(a.y.max(b.y));
                last += 1;
            }
            if last == first {
                continue;
            }
            let r0 = (lo - reach).floor().max(0.0) as u32;
            let r1 = ((hi + reach).ceil().max(0.0) as u32).min(self.height);
            if r1 <= r0 {
                continue;
            }
            // Each nearby segment's squared distance to every pixel center in the
            // column's run of rows, the nearest kept: one plain loop over the rows a
            // segment, which compiles to vector code.
            let mut d2 = std::mem::take(&mut self.d2);
            d2.clear();
            d2.resize((r1 - r0) as usize, f32::INFINITY);
            let top = r0 as f32 + 0.5;
            for s in &segs[first..last] {
                let px = cx - s.a.x;
                let py0 = top - s.a.y;
                for (k, d) in d2.iter_mut().enumerate() {
                    let py = py0 + k as f32;
                    let t = ((px * s.dx + py * s.dy) * s.inv_len2).clamp(0.0, 1.0);
                    let (qx, qy) = (t * s.dx - px, t * s.dy - py);
                    *d = d.min(qx * qx + qy * qy);
                }
            }
            for (k, &d) in d2.iter().enumerate() {
                if d < reach2 {
                    self.blend(col, r0 + k as u32, ink, reach - d.sqrt());
                }
            }
            self.d2 = d2;
        }
        self.segs = segs;
    }

    /// Source-over `ink` at coverage `cov` (clamped to 0..1) onto one pixel.
    fn blend(&mut self, col: u32, row: u32, ink: [u32; 4], cov: f32) {
        let c = (cov.clamp(0.0, 1.0) * 256.0 + 0.5) as u32;
        if c == 0 {
            return;
        }
        self.put(
            row as usize * self.width as usize + col as usize,
            scaled(ink, c),
        );
    }
}

/// `points`, in DIPs, as pixels of a raster whose top left is `origin` (device
/// pixels) at `scale` device pixels a DIP, into `out`.
pub fn to_pixels(points: &[Point], scale: f32, origin: Point, out: &mut Vec<Point>) {
    out.clear();
    out.extend(
        points
            .iter()
            .map(|p| Point::new(p.x * scale - origin.x, p.y * scale - origin.y)),
    );
}

/// `color` premultiplied, in BGRA order, 0..=255.
fn ink(c: Color) -> [u32; 4] {
    let a = c.a.clamp(0.0, 1.0);
    let q = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u32;
    [q(c.b), q(c.g), q(c.r), (a * 255.0 + 0.5) as u32]
}

/// A premultiplied pixel to blend: the pixel itself, and how much of what is under
/// it shows through, of 256.
#[derive(Debug, Clone, Copy)]
struct Src {
    px: u32,
    keep: u32,
}

/// `ink` at coverage `cover` of 256, packed.
fn scaled(ink: [u32; 4], cover: u32) -> Src {
    let [blue, green, red, alpha] = ink.map(|v| ((v * cover + 128) >> 8).min(255));
    Src {
        px: blue | (green << 8) | (red << 16) | (alpha << 24),
        // 256 - alpha, rounding so an opaque source leaves nothing under it.
        keep: 256 - (alpha + (alpha >> 7)),
    }
}

/// Source-over `src` onto the packed BGRA pixel `dst`: two channels a multiply.
fn over(dst: u32, src: Src) -> u32 {
    let rb = (((dst & 0x00FF_00FF) * src.keep) >> 8) & 0x00FF_00FF;
    let ag = (((dst >> 8) & 0x00FF_00FF) * src.keep) & 0xFF00_FF00;
    let under = rb | ag;
    // Channel by channel, so a rounding overflow saturates rather than carries.
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let v = ((src.px >> shift) & 0xFF) + ((under >> shift) & 0xFF);
        out |= v.min(255) << shift;
    }
    out
}

/// The line through `pts` (ascending in x) at `x`, by linear interpolation; the end
/// values beyond either end. `cursor` is where the last lookup ended: lookups at
/// increasing x walk forward from it.
fn at(pts: &[Point], cursor: &mut usize, x: f32) -> f32 {
    while *cursor + 1 < pts.len() && pts[*cursor + 1].x < x {
        *cursor += 1;
    }
    let a = pts[*cursor];
    let Some(&b) = pts.get(*cursor + 1) else {
        return a.y;
    };
    if x <= a.x {
        return a.y;
    }
    let span = b.x - a.x;
    if span <= f32::EPSILON {
        return b.y;
    }
    a.y + (b.y - a.y) * ((x - a.x) / span).min(1.0)
}

/// A line segment, with what finding a point's distance to it needs.
#[derive(Debug, Clone, Copy)]
struct Seg {
    a: Point,
    dx: f32,
    dy: f32,
    /// One over the squared length; zero for a point.
    inv_len2: f32,
}

impl Seg {
    fn new(a: Point, b: Point) -> Self {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len2 = dx * dx + dy * dy;
        Self {
            a,
            dx,
            dy,
            inv_len2: if len2 > 0.0 { 1.0 / len2 } else { 0.0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    /// The alpha of pixel (`col`, `row`), 0..1.
    fn alpha(r: &ChartRaster, col: u32, row: u32) -> f32 {
        f32::from(r.pixels()[((row * r.width() + col) * 4 + 3) as usize]) / 255.0
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn a_flat_band_covers_whole_rows_and_half_rows_at_half_pixel_edges() {
        let mut r = ChartRaster::new();
        r.reset(10, 10);
        r.fill_band(
            &[p(0.0, 2.5), p(10.0, 2.5)],
            &[p(0.0, 6.0), p(10.0, 6.0)],
            Color::WHITE,
        );
        for col in 0..10 {
            assert!(near(alpha(&r, col, 1), 0.0));
            assert!(near(alpha(&r, col, 2), 0.5), "half row at the top edge");
            for row in 3..6 {
                assert!(near(alpha(&r, col, row), 1.0));
            }
            assert!(near(alpha(&r, col, 6), 0.0));
        }
        // White, premultiplied: every channel equals alpha.
        let i = ((4 * 10 + 4) * 4) as usize;
        assert_eq!(&r.pixels()[i..i + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn a_band_starting_mid_pixel_covers_that_column_by_its_samples_inside() {
        let mut r = ChartRaster::new();
        r.reset(10, 4);
        r.fill_band(
            &[p(3.5, 0.0), p(10.0, 0.0)],
            &[p(0.0, 4.0), p(10.0, 4.0)],
            Color::WHITE,
        );
        assert!(near(alpha(&r, 2, 1), 0.0));
        assert!(near(alpha(&r, 3, 1), 0.5), "two of four samples");
        assert!(near(alpha(&r, 4, 1), 1.0));
    }

    #[test]
    fn a_sloped_band_edge_covers_the_area_under_it() {
        // Top from y 0 at x 0 to y 4 at x 4: the pixel at column 1, row 1 sits on
        // the diagonal, half covered.
        let mut r = ChartRaster::new();
        r.reset(4, 4);
        r.fill_band(
            &[p(0.0, 0.0), p(4.0, 4.0)],
            &[p(0.0, 4.0), p(4.0, 4.0)],
            Color::WHITE,
        );
        assert!(near(alpha(&r, 1, 1), 0.5));
        assert!(near(alpha(&r, 1, 3), 1.0));
        assert!(near(alpha(&r, 3, 0), 0.0));
    }

    #[test]
    fn translucent_bands_blend_source_over() {
        let mut r = ChartRaster::new();
        r.reset(2, 2);
        let half = Color::rgba(1.0, 0.0, 0.0, 0.5);
        for _ in 0..2 {
            r.fill_band(
                &[p(0.0, 0.0), p(2.0, 0.0)],
                &[p(0.0, 2.0), p(2.0, 2.0)],
                half,
            );
        }
        // 0.5 over 0.5 is 0.75.
        assert!(near(alpha(&r, 0, 0), 0.75));
        // BGRA: red is the third byte, equal to alpha for pure red.
        assert_eq!(r.pixels()[2], r.pixels()[3]);
    }

    #[test]
    fn a_line_on_a_pixel_center_covers_its_row_and_fades_by_distance() {
        let mut r = ChartRaster::new();
        r.reset(10, 10);
        r.stroke_graph(&[p(0.0, 5.5), p(10.0, 5.5)], 1.5, Color::WHITE);
        assert!(near(alpha(&r, 5, 5), 1.0));
        // A pixel center one away: 0.75 + 0.5 - 1.
        assert!(near(alpha(&r, 5, 4), 0.25));
        assert!(near(alpha(&r, 5, 6), 0.25));
        assert!(near(alpha(&r, 5, 3), 0.0));
    }

    #[test]
    fn a_line_joint_is_blended_once() {
        let mut r = ChartRaster::new();
        r.reset(10, 10);
        let half = Color::WHITE.with_alpha(0.5);
        r.stroke_graph(&[p(0.0, 2.5), p(5.5, 5.5), p(10.0, 2.5)], 1.0, half);
        // Both segments end on the joint's pixel; it gets the color's alpha, not
        // the alpha of two layers.
        assert!(near(alpha(&r, 5, 5), 0.5));
    }

    /// A full History chart at 96 DPI: nine stacked bands with hairlines across
    /// 670 x 290 pixels, a point a pixel. `cargo test -p ot-paint --release --
    /// --ignored --nocapture history_sized` prints the time a frame: the fastest of
    /// several batches, since a busy machine slows some.
    #[test]
    #[ignore = "a timing, not a check"]
    fn history_sized_frame_timing() {
        use std::time::{Duration, Instant};
        let (w, h) = (670u32, 290u32);
        let mut edges = vec![(0..=w)
            .map(|i| p(i as f32 + 0.3, h as f32))
            .collect::<Vec<_>>()];
        for b in 0..9 {
            let next = edges[b]
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let k = (i as f32 * 0.05 + b as f32).sin();
                    p(q.x, q.y - 12.0 - 10.0 * k.abs())
                })
                .collect();
            edges.push(next);
        }
        let mut raster = ChartRaster::new();
        let (mut best_bands, mut best_lines) = (Duration::MAX, Duration::MAX);
        for _ in 0..15 {
            let (mut bands, mut lines) = (Duration::ZERO, Duration::ZERO);
            for _ in 0..30 {
                raster.reset(w, h);
                for b in 0..9 {
                    let t0 = Instant::now();
                    raster.fill_band(&edges[b + 1], &edges[b], Color::rgba(0.4, 0.6, 0.9, 0.9));
                    let t1 = Instant::now();
                    raster.stroke_graph(&edges[b + 1], 1.0, Color::rgb(0.12, 0.12, 0.12));
                    bands += t1 - t0;
                    lines += t1.elapsed();
                }
            }
            best_bands = best_bands.min(bands / 30);
            best_lines = best_lines.min(lines / 30);
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!(
            "history-sized frame: {:.3} ms (bands {:.3}, lines {:.3})",
            ms(best_bands + best_lines),
            ms(best_bands),
            ms(best_lines)
        );
    }

    #[test]
    fn shapes_outside_the_raster_draw_nothing_and_do_not_panic() {
        let mut r = ChartRaster::new();
        r.reset(4, 4);
        r.fill_band(
            &[p(-10.0, -5.0), p(-1.0, -5.0)],
            &[p(-10.0, 9.0), p(-1.0, 9.0)],
            Color::WHITE,
        );
        r.stroke_graph(&[p(10.0, 10.0), p(20.0, 30.0)], 2.0, Color::WHITE);
        r.fill_band(&[p(0.0, 0.0)], &[p(0.0, 4.0), p(4.0, 4.0)], Color::WHITE);
        assert!(r.pixels().iter().all(|&b| b == 0));
    }
}
