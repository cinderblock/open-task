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
    px: Vec<u8>,
    /// A graph's coverage, the strongest from any of its segments, before it is
    /// blended once: blending each segment would darken every joint.
    cover: Vec<f32>,
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
        let ink = premultiply(color);
        let (mut ti, mut bi) = (0, 0);
        let first = x0.floor() as u32;
        let last = (x1.ceil() as u32).min(self.width);
        let mut lo = [0.0f32; SAMPLES as usize];
        let mut hi = [0.0f32; SAMPLES as usize];
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
            let whole = n as f32 / SAMPLES as f32;
            for row in r0..r1 {
                let (y0, y1) = (row as f32, row as f32 + 1.0);
                let cov = if y0 >= inner0 && y1 <= inner1 {
                    whole
                } else {
                    lo.iter()
                        .zip(hi)
                        .map(|(&l, &h)| (h.min(y1) - l.max(y0)).max(0.0))
                        .sum::<f32>()
                        / SAMPLES as f32
                };
                self.blend(col, row, ink, cov);
            }
        }
    }

    /// Draw a line `width` pixels wide through `points` (ascending in x).
    pub fn stroke_graph(&mut self, points: &[Point], width: f32, color: Color) {
        if points.len() < 2 || self.width == 0 || self.height == 0 || width <= 0.0 {
            return;
        }
        let reach = width * 0.5 + 0.5;
        // The pixels the line can touch.
        let (mut x0, mut y0, mut x1, mut y1) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for p in points {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        let c0 = (x0 - reach).floor().max(0.0) as u32;
        let r0 = (y0 - reach).floor().max(0.0) as u32;
        let c1 = ((x1 + reach).ceil().max(0.0) as u32).min(self.width);
        let r1 = ((y1 + reach).ceil().max(0.0) as u32).min(self.height);
        if c1 <= c0 || r1 <= r0 {
            return;
        }
        let (bw, bh) = ((c1 - c0) as usize, (r1 - r0) as usize);
        let mut cover = std::mem::take(&mut self.cover);
        cover.clear();
        cover.resize(bw * bh, 0.0);
        for seg in points.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let sc0 = ((a.x.min(b.x) - reach).floor().max(c0 as f32) as u32).min(c1);
            let sc1 = ((a.x.max(b.x) + reach).ceil().max(c0 as f32) as u32).min(c1);
            let sr0 = ((a.y.min(b.y) - reach).floor().max(r0 as f32) as u32).min(r1);
            let sr1 = ((a.y.max(b.y) + reach).ceil().max(r0 as f32) as u32).min(r1);
            for row in sr0..sr1 {
                for col in sc0..sc1 {
                    let p = Point::new(col as f32 + 0.5, row as f32 + 0.5);
                    let cov = (reach - distance_to_segment(p, a, b)).clamp(0.0, 1.0);
                    let i = (row - r0) as usize * bw + (col - c0) as usize;
                    if cov > cover[i] {
                        cover[i] = cov;
                    }
                }
            }
        }
        let ink = premultiply(color);
        for row in 0..bh {
            for col in 0..bw {
                let cov = cover[row * bw + col];
                if cov > 0.0 {
                    self.blend(c0 + col as u32, r0 + row as u32, ink, cov);
                }
            }
        }
        self.cover = cover;
    }

    /// Source-over `ink` (premultiplied BGRA, 0..1) at `cov` onto one pixel.
    fn blend(&mut self, col: u32, row: u32, ink: [f32; 4], cov: f32) {
        if cov <= 0.0 {
            return;
        }
        let i = (row as usize * self.width as usize + col as usize) * 4;
        let keep = 1.0 - ink[3] * cov;
        for (dst, ink) in self.px[i..i + 4].iter_mut().zip(ink) {
            let v = ink * cov + f32::from(*dst) / 255.0 * keep;
            *dst = (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
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

/// `color` premultiplied, in BGRA order.
fn premultiply(c: Color) -> [f32; 4] {
    let a = c.a.clamp(0.0, 1.0);
    [c.b * a, c.g * a, c.r * a, a]
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

/// The distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (a.x + t * dx - p.x, a.y + t * dy - p.y);
    (qx * qx + qy * qy).sqrt()
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
