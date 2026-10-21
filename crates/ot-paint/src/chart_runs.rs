//! Runs of chart shapes ([`DrawCmd::Band`], [`DrawCmd::Graph`]) in a display list,
//! each drawn into a bitmap of its own instead of by the backend's path renderer.
//!
//! A run is consecutive chart commands under one clip. A backend that draws charts
//! off its path renderer finds the runs ([`find_runs`]), renders each into a bitmap
//! in device pixels (on the CPU with [`ChartRaster::draw_run`], or on the GPU), and
//! draws the bitmap 1:1 where the run's commands were, skipping them.

use crate::chart_raster::{to_pixels, ChartRaster};
use crate::display::{DisplayList, DrawCmd};
use crate::geom::{Point, Rect};

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

/// Points in a run's pixels, kept between runs so drawing one allocates nothing.
#[derive(Debug, Default, Clone)]
pub struct RunScratch {
    a: Vec<Point>,
    b: Vec<Point>,
}

impl ChartRaster {
    /// Make the raster `run`'s size and draw its commands from `dl` into it, at
    /// `scale` device pixels a DIP: what goes at `run.px`.
    pub fn draw_run(
        &mut self,
        dl: &DisplayList,
        run: &ChartRun,
        scale: f32,
        scratch: &mut RunScratch,
    ) {
        self.reset(run.px.w, run.px.h);
        let origin = Point::new(run.px.x as f32, run.px.y as f32);
        for cmd in &dl.cmds()[run.start..run.end] {
            match *cmd {
                DrawCmd::Band { top, bottom, color } => {
                    to_pixels(dl.points(top), scale, origin, &mut scratch.a);
                    to_pixels(dl.points(bottom), scale, origin, &mut scratch.b);
                    self.fill_band(&scratch.a, &scratch.b, color);
                }
                DrawCmd::Graph {
                    points,
                    color,
                    width,
                } => {
                    to_pixels(dl.points(points), scale, origin, &mut scratch.a);
                    self.stroke_graph(&scratch.a, width * scale, color);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, TextStyle};

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
            TextStyle::default(),
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
    fn a_run_draws_in_its_own_device_pixels() {
        let dl = frame();
        let mut runs = Vec::new();
        find_runs(&dl, 2.0, (800, 600), &mut runs);
        assert_eq!(
            runs[0].px,
            PxRect {
                x: 20,
                y: 36,
                w: 200,
                h: 84
            }
        );
        let mut raster = ChartRaster::new();
        raster.draw_run(&dl, &runs[0], 2.0, &mut RunScratch::default());
        assert_eq!((raster.width(), raster.height()), (200, 84));
        // Inside the band, below the line: white, opaque.
        let i = ((80 * 200 + 100) * 4) as usize;
        assert_eq!(&raster.pixels()[i..i + 4], &[255, 255, 255, 255]);
        // Above the line, at the run's top left: nothing.
        assert_eq!(&raster.pixels()[0..4], &[0, 0, 0, 0]);
    }
}
