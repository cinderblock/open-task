//! Chart shapes ([`DrawCmd::Band`], [`DrawCmd::Graph`]) drawn off Direct2D's path
//! rasterizer.
//!
//! A run of consecutive chart commands is a [`ChartRun`]: before a frame's Direct2D
//! pass, each run it draws is rendered into a bitmap of its own, in device pixels,
//! by the GPU ([`crate::chart_gpu`]) or the CPU ([`ot_paint::ChartRaster`]); the
//! pass then draws the bitmap 1:1 where the run's commands were. This module finds
//! the runs and turns their shapes into pixels both renderers take, and cuts them
//! into [`Piece`]s for the GPU.

use ot_paint::{ChartRaster, Color, DisplayList, DrawCmd, Point, Rect};

/// How chart shapes are drawn. The choice is the user's (Settings), `OT_CHARTS`
/// overrides it.
pub use ot_ui::ChartDrawing;

/// A rectangle of whole device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PxRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl PxRect {
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// Consecutive chart commands `start..end` of a display list, drawn as one bitmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChartRun {
    pub start: usize,
    pub end: usize,
    /// Where the bitmap goes, in device pixels: the commands' bounds within their
    /// clip, grown out to whole pixels and kept inside the window.
    pub px: PxRect,
    /// `px` in DIPs.
    pub dip: Rect,
}

/// The chart runs of `dl`, for a window of `size_px` at `scale` device pixels a
/// DIP, into `out`.
pub fn find_runs(dl: &DisplayList, scale: f32, size_px: (u32, u32), out: &mut Vec<ChartRun>) {
    out.clear();
    let window = Rect::new(0.0, 0.0, size_px.0 as f32 / scale, size_px.1 as f32 / scale);
    let mut clips: Vec<Rect> = Vec::new();
    let mut open: Option<(usize, Rect)> = None;
    let close = |open: &mut Option<(usize, Rect)>, end: usize, out: &mut Vec<ChartRun>| {
        if let Some((start, bounds)) = open.take() {
            let px = to_px(bounds, scale, size_px);
            out.push(ChartRun {
                start,
                end,
                px,
                dip: Rect::new(
                    px.x as f32 / scale,
                    px.y as f32 / scale,
                    px.w as f32 / scale,
                    px.h as f32 / scale,
                ),
            });
        }
    };
    for (i, cmd) in dl.cmds().iter().enumerate() {
        match *cmd {
            DrawCmd::Band { .. } | DrawCmd::Graph { .. } => {
                let clip = clips.last().copied().unwrap_or(window);
                let b = dl.bounds(cmd).map_or(clip, |b| b.intersect(&clip));
                open = Some(match open {
                    Some((start, acc)) => (start, union(acc, b)),
                    None => (i, b),
                });
            }
            DrawCmd::PushClip(r) => {
                close(&mut open, i, out);
                let top = clips.last().copied().unwrap_or(window);
                clips.push(top.intersect(&r));
            }
            DrawCmd::PopClip => {
                close(&mut open, i, out);
                clips.pop();
            }
            _ => close(&mut open, i, out),
        }
    }
    close(&mut open, dl.cmds().len(), out);
}

/// `r` in device pixels, grown out to whole ones, within the window.
fn to_px(r: Rect, scale: f32, size_px: (u32, u32)) -> PxRect {
    if r.is_empty() {
        return PxRect::default();
    }
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let x0 = clamp((r.x * scale).floor(), size_px.0);
    let y0 = clamp((r.y * scale).floor(), size_px.1);
    let x1 = clamp((r.right() * scale).ceil(), size_px.0);
    let y1 = clamp((r.bottom() * scale).ceil(), size_px.1);
    PxRect {
        x: x0,
        y: y0,
        w: x1.saturating_sub(x0),
        h: y1.saturating_sub(y0),
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(
        x,
        y,
        a.right().max(b.right()) - x,
        a.bottom().max(b.bottom()) - y,
    )
}

/// What a chart shape is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Band,
    Graph,
}

/// One chart shape in its run's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    /// The band's top or the graph's line: a range of [`Shapes::points`].
    pub a: (u32, u32),
    /// The band's bottom; unused for a graph.
    pub b: (u32, u32),
    pub color: Color,
    /// A graph's width, in pixels.
    pub width: f32,
    /// The pixels the shape can touch, within the run: `[x0, y0, x1, y1]`.
    pub bounds: [f32; 4],
}

/// How many columns of pixels a [`Piece`] spans. Narrow, so that a piece is not
/// much taller than its shape is there; wide enough that the pieces stay few (a
/// full History is a couple of thousand).
pub const PIECE_W: u32 = 8;

/// A strip of a shape, for the GPU: the pixels of some whole columns that the shape
/// can touch, and where in its points the searches for those pixels end up.
///
/// A shape's bounds are mostly empty: a History band is a thin ribbon across a
/// plot, its bounding box most of the plot. Drawn as one quad, every pixel of the
/// box ran the coverage shader, binary searches and all, nine bands and nine
/// hairlines deep. Cut into pieces only the ribbon's pixels run it, each once, and
/// each search starts already narrowed to the piece's few points. The coverage is
/// computed as before, so the pixels come out the same.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    /// `[x0, y0, x1, y1]` in the run's pixels: whole columns, cut to the shape's
    /// bounds, and the rows the shape can touch in them.
    pub rect: [f32; 4],
    /// Index into [`Shapes::shapes`].
    pub shape: u32,
    /// For the band's top (the graph's line) and its bottom, the range of answers
    /// the shader's lower-bound search can give for a pixel of the piece, relative
    /// to the line's first point: `lo..=hi`.
    pub a: (u32, u32),
    pub b: (u32, u32),
}

/// The index of the first point whose x is at least `x`; `pts.len()` when there is
/// none. The shader's `lowerBound`.
fn lower_bound(pts: &[Point], x: f32) -> u32 {
    pts.partition_point(|p| p.x < x) as u32
}

/// The line through `pts` at `x`, its end values beyond either end. The shader's
/// `lineAt`.
fn line_at(pts: &[Point], x: f32) -> f32 {
    let i = lower_bound(pts, x) as usize;
    if i == 0 {
        return pts[0].y;
    }
    let Some(&b) = pts.get(i) else {
        return pts[pts.len() - 1].y;
    };
    let a = pts[i - 1];
    let span = b.x - a.x;
    if span <= f32::EPSILON {
        return b.y;
    }
    a.y + (b.y - a.y) * ((x - a.x) / span).min(1.0)
}

/// The lowest and highest the line through `pts` is over `l..=r`: at its ends, and
/// at every point between, where a piecewise linear line turns.
fn extent(pts: &[Point], l: f32, r: f32) -> (f32, f32) {
    let (a, b) = (line_at(pts, l), line_at(pts, r));
    let (mut lo, mut hi) = (a.min(b), a.max(b));
    let first = pts.partition_point(|p| p.x < l);
    for p in pts[first..].iter().take_while(|p| p.x <= r) {
        lo = lo.min(p.y);
        hi = hi.max(p.y);
    }
    (lo, hi)
}

/// Slack, in pixels, on a piece's rows, so that rounding in [`extent`] can never
/// leave out a row the shader would give a sliver of coverage.
const ROW_SLACK: f32 = 0.01;

/// The shapes of one or more runs, in each run's own pixels (its top left at 0, 0).
#[derive(Debug, Default)]
pub struct Shapes {
    pub points: Vec<Point>,
    pub shapes: Vec<Shape>,
}

impl Shapes {
    pub fn clear(&mut self) {
        self.points.clear();
        self.shapes.clear();
    }

    /// Add `run`'s shapes from `dl`. Returns the range of [`Shapes::shapes`] they
    /// took.
    pub fn add_run(&mut self, dl: &DisplayList, run: &ChartRun, scale: f32) -> (u32, u32) {
        let first = self.shapes.len() as u32;
        let origin = Point::new(run.px.x as f32, run.px.y as f32);
        let (w, h) = (run.px.w as f32, run.px.h as f32);
        for cmd in &dl.cmds()[run.start..run.end] {
            let (kind, a, b, color, width) = match *cmd {
                DrawCmd::Band { top, bottom, color } => {
                    (Kind::Band, dl.points(top), dl.points(bottom), color, 0.0)
                }
                DrawCmd::Graph {
                    points,
                    color,
                    width,
                } => (
                    Kind::Graph,
                    dl.points(points),
                    &[][..],
                    color,
                    width * scale,
                ),
                _ => continue,
            };
            let a = self.push(a, scale, origin);
            let b = self.push(b, scale, origin);
            let bounds = dl.bounds(cmd).map_or([0.0, 0.0, w, h], |r| {
                [
                    (r.x * scale - origin.x).clamp(0.0, w),
                    (r.y * scale - origin.y).clamp(0.0, h),
                    (r.right() * scale - origin.x).clamp(0.0, w),
                    (r.bottom() * scale - origin.y).clamp(0.0, h),
                ]
            });
            self.shapes.push(Shape {
                kind,
                a,
                b,
                color,
                width,
                bounds,
            });
        }
        (first, self.shapes.len() as u32 - first)
    }

    fn push(&mut self, pts: &[Point], scale: f32, origin: Point) -> (u32, u32) {
        let start = self.points.len() as u32;
        self.points.extend(
            pts.iter()
                .map(|p| Point::new(p.x * scale - origin.x, p.y * scale - origin.y)),
        );
        (start, pts.len() as u32)
    }

    fn slice(&self, (start, len): (u32, u32)) -> &[Point] {
        &self.points[start as usize..(start + len) as usize]
    }

    /// Cut shape `index` into pieces [`PIECE_W`] columns wide onto `out`, or, with
    /// `whole`, into one piece that is its bounds, its searches over all its points
    /// (how shapes were drawn before pieces, kept to check them against). A piece
    /// holds every pixel of its columns that the shape gives any coverage, and no
    /// pixel is in two pieces of a shape.
    pub fn pieces(&self, index: usize, whole: bool, out: &mut Vec<Piece>) {
        let it = &self.shapes[index];
        let shape = index as u32;
        let [bx0, by0, bx1, by1] = it.bounds;
        let (top, bottom) = (self.slice(it.a), self.slice(it.b));
        let band = it.kind == Kind::Band;
        if bx1 <= bx0 || by1 <= by0 || top.is_empty() || (band && bottom.is_empty()) {
            return;
        }
        if whole {
            out.push(Piece {
                rect: it.bounds,
                shape,
                a: (0, top.len() as u32),
                b: (0, bottom.len() as u32),
            });
            return;
        }
        // How far from a pixel's center the shape reaches: a line's half width and
        // the fringe; a band is sampled inside its pixel's column.
        let reach = if band { 0.0 } else { it.width * 0.5 + 0.5 };
        let last = bx1.ceil() as u32;
        let mut c0 = bx0.floor() as u32;
        while c0 < last {
            let c1 = (c0 + PIECE_W).min(last);
            // Every x a pixel of these columns looks at: a band's samples, inside
            // the columns; a line's segments, within reach of the centers.
            let (left, right) = (c0 as f32 - reach, c1 as f32 + reach);
            let (mut lo, mut hi) = extent(top, left, right);
            let mut range_b = (0, 0);
            if band {
                let (blo, bhi) = extent(bottom, left, right);
                lo = lo.min(blo);
                hi = hi.max(bhi);
                range_b = (lower_bound(bottom, left), lower_bound(bottom, right));
            }
            let y0 = (lo - reach - ROW_SLACK).floor().max(by0);
            let y1 = (hi + reach + ROW_SLACK).ceil().min(by1);
            if y1 > y0 {
                out.push(Piece {
                    rect: [(c0 as f32).max(bx0), y0, (c1 as f32).min(bx1), y1],
                    shape,
                    a: (lower_bound(top, left), lower_bound(top, right)),
                    b: range_b,
                });
            }
            c0 = c1;
        }
    }

    /// Draw shapes `range` (from [`Shapes::add_run`]) into `raster`, `w` x `h`.
    pub fn rasterize(&self, range: (u32, u32), w: u32, h: u32, raster: &mut ChartRaster) {
        raster.reset(w, h);
        for s in &self.shapes[range.0 as usize..(range.0 + range.1) as usize] {
            match s.kind {
                Kind::Band => raster.fill_band(self.slice(s.a), self.slice(s.b), s.color),
                Kind::Graph => raster.stroke_graph(self.slice(s.a), s.width, s.color),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    /// A card with a chart in a clip, text after it, and a second chart.
    fn frame() -> DisplayList {
        let mut dl = DisplayList::new();
        dl.fill_rect(Rect::new(0.0, 0.0, 400.0, 300.0), Color::BLACK);
        dl.push_clip(Rect::new(10.0, 10.0, 100.0, 50.0));
        dl.band(
            [p(0.0, 30.0), p(200.0, 20.0)],
            [p(0.0, 60.0), p(200.0, 60.0)],
            Color::WHITE,
        );
        dl.graph([p(0.0, 30.0), p(200.0, 20.0)], Color::WHITE, 1.5);
        dl.pop_clip();
        dl.label(
            "CPU",
            Rect::new(10.0, 70.0, 50.0, 20.0),
            ot_paint::TextStyle::default(),
            Color::WHITE,
        );
        dl.graph([p(20.0, 100.0), p(30.0, 120.5)], Color::WHITE, 1.0);
        dl
    }

    #[test]
    fn runs_are_consecutive_chart_commands_cut_to_their_clip() {
        let mut runs = Vec::new();
        find_runs(&frame(), 1.0, (400, 300), &mut runs);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].start, runs[0].end), (2, 4));
        // The band reaches past the clip either side and below: the run is the clip
        // there, and down from the line's top (y 20, less half its width and the
        // fringe) above.
        assert_eq!(
            runs[0].px,
            PxRect {
                x: 10,
                y: 18,
                w: 100,
                h: 42
            }
        );
        // The lone line: its points, half its width and the fringe, out to pixels.
        assert_eq!((runs[1].start, runs[1].end), (6, 7));
        assert_eq!(
            runs[1].px,
            PxRect {
                x: 18,
                y: 98,
                w: 14,
                h: 24
            }
        );
    }

    #[test]
    fn runs_are_in_device_pixels_at_a_scale() {
        let mut runs = Vec::new();
        find_runs(&frame(), 1.5, (600, 450), &mut runs);
        assert_eq!(
            runs[0].px,
            PxRect {
                x: 15,
                y: 27,
                w: 150,
                h: 63
            }
        );
        assert!((runs[0].dip.x - 10.0).abs() < 1e-4 && (runs[0].dip.w - 100.0).abs() < 1e-4);
        // Kept inside the window.
        find_runs(&frame(), 1.0, (50, 40), &mut runs);
        assert_eq!(
            runs[0].px,
            PxRect {
                x: 10,
                y: 18,
                w: 40,
                h: 22
            }
        );
    }

    #[test]
    fn shapes_are_in_the_runs_own_pixels() {
        let dl = frame();
        let mut runs = Vec::new();
        find_runs(&dl, 2.0, (800, 600), &mut runs);
        let mut shapes = Shapes::default();
        let range = shapes.add_run(&dl, &runs[0], 2.0);
        assert_eq!(range, (0, 2));
        let band = shapes.shapes[0];
        assert_eq!(band.kind, Kind::Band);
        // DIP (0, 30) at 2x, less the run's origin (20, 36).
        assert_eq!(
            runs[0].px,
            PxRect {
                x: 20,
                y: 36,
                w: 200,
                h: 84
            }
        );
        assert_eq!(shapes.slice(band.a)[0], p(-20.0, 24.0));
        let line = shapes.shapes[1];
        assert_eq!(line.kind, Kind::Graph);
        assert!((line.width - 3.0).abs() < 1e-6);
        assert_eq!(line.bounds[0], 0.0, "bounds are kept inside the run");

        let mut raster = ChartRaster::new();
        shapes.rasterize(range, runs[0].px.w, runs[0].px.h, &mut raster);
        assert_eq!(raster.pixels().len(), (200 * 84 * 4) as usize);
        // Inside the band, below the line: white, opaque.
        let i = ((80 * 200 + 100) * 4) as usize;
        assert_eq!(&raster.pixels()[i..i + 4], &[255, 255, 255, 255]);
    }

    /// Shapes like the History's and the summary charts', and awkward ones: a band
    /// whose top and bottom have their own xs and ends, steps (two points at one
    /// x), spikes, lines of several widths, points off the run on either side.
    fn awkward() -> DisplayList {
        let mut dl = DisplayList::new();
        dl.push_clip(Rect::new(0.0, 0.0, 300.0, 200.0));
        let wave = |i: usize, k: f32| ((i as f32 * 0.37 + k).sin() * (i as f32 * 0.05).cos()).abs();
        let mut lower: Vec<Point> = (0..=160)
            .map(|i| p(-10.0 + i as f32 * 2.0, 190.0))
            .collect();
        for k in 0..5 {
            let upper: Vec<Point> = lower
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let spike = if i % 23 == 0 { 40.0 } else { 0.0 };
                    p(q.x, q.y - 4.0 - 25.0 * wave(i, k as f32) - spike)
                })
                .collect();
            dl.band(upper.iter().copied(), lower.iter().copied(), Color::WHITE);
            dl.graph(upper.iter().copied(), Color::WHITE, 0.5 + k as f32 * 0.6);
            lower = upper;
        }
        // Its own xs, top and bottom, and a step in the top.
        dl.band(
            [p(20.3, 50.0), p(80.0, 20.0), p(80.0, 70.0), p(140.7, 30.0)],
            [p(10.0, 90.0), p(61.1, 95.5), p(150.0, 80.0)],
            Color::WHITE,
        );
        // A steep line, nearly vertical.
        dl.graph(
            [p(200.0, 10.0), p(200.4, 180.0), p(203.0, 20.0)],
            Color::WHITE,
            1.5,
        );
        dl.pop_clip();
        dl
    }

    #[test]
    fn pieces_hold_every_pixel_a_shape_covers_once() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let dl = awkward();
            let mut runs = Vec::new();
            find_runs(&dl, scale, (600, 400), &mut runs);
            let mut shapes = Shapes::default();
            let range = shapes.add_run(&dl, &runs[0], scale);
            let (w, h) = (runs[0].px.w, runs[0].px.h);
            let mut raster = ChartRaster::new();
            let mut pieces = Vec::new();
            for i in range.0..range.0 + range.1 {
                let i = i as usize;
                pieces.clear();
                shapes.pieces(i, false, &mut pieces);
                let mut whole = Vec::new();
                shapes.pieces(i, true, &mut whole);
                // The shape alone, with the coverage the shader computes.
                raster.reset(w, h);
                shapes.rasterize((i as u32, 1), w, h, &mut raster);
                let px = raster.pixels();
                let mut covered = 0usize;
                let center_in = |r: &[f32; 4], x: u32, y: u32| {
                    let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                    cx >= r[0] && cx < r[2] && cy >= r[1] && cy < r[3]
                };
                for y in 0..h {
                    for x in 0..w {
                        let n = pieces.iter().filter(|q| center_in(&q.rect, x, y)).count();
                        assert!(n <= 1, "scale {scale} shape {i}: ({x}, {y}) in {n} pieces");
                        let alpha = px[((y * w + x) * 4 + 3) as usize];
                        let in_whole = whole.iter().any(|q| center_in(&q.rect, x, y));
                        if alpha > 0 && in_whole {
                            covered += 1;
                            assert_eq!(
                                n, 1,
                                "scale {scale} shape {i}: ({x}, {y}) covered ({alpha}) but in no piece"
                            );
                        }
                        // Nothing the whole quad leaves out is drawn either.
                        assert!(in_whole || n == 0, "scale {scale} shape {i}: ({x}, {y})");
                    }
                }
                assert!(covered > 0, "scale {scale} shape {i} draws something");
                // A piece's searches cover where its pixels look.
                let s = shapes.shapes[i];
                for q in &pieces {
                    assert!(q.a.0 <= q.a.1 && q.a.1 <= s.a.1, "{q:?}");
                    assert!(q.b.0 <= q.b.1 && q.b.1 <= s.b.1, "{q:?}");
                }
            }
        }
    }

    #[test]
    fn pieces_are_much_smaller_than_the_bounds() {
        let dl = awkward();
        let mut runs = Vec::new();
        find_runs(&dl, 1.0, (600, 400), &mut runs);
        let mut shapes = Shapes::default();
        let range = shapes.add_run(&dl, &runs[0], 1.0);
        let area = |pieces: &[Piece]| -> f32 {
            pieces
                .iter()
                .map(|q| (q.rect[2] - q.rect[0]) * (q.rect[3] - q.rect[1]))
                .sum()
        };
        let (mut cut, mut whole) = (Vec::new(), Vec::new());
        for i in range.0..range.0 + range.1 {
            shapes.pieces(i as usize, false, &mut cut);
            shapes.pieces(i as usize, true, &mut whole);
        }
        let (cut, whole) = (area(&cut), area(&whole));
        assert!(cut < whole * 0.4, "pieces {cut} px, bounds {whole} px");
    }
}
