//! Chart shapes drawn by the host, for the CPU choice of chart drawing.
//!
//! With [`ChartDrawing::Cpu`] the host draws each run of chart commands
//! ([`ot_paint::find_runs`]) into an image of its own with `ot-paint`'s chart
//! rasterizer, in device pixels, and hands the images to the plugin with the frame
//! ([`abi::Raster`]); the plugin draws an image where its run's commands were.
//! One rasterizer for GTK and Qt both, the one the Windows shell's CPU choice
//! uses. With [`ChartDrawing::Native`] there are no images and the plugin draws
//! the commands with its toolkit's painter (Cairo, `QPainter`).

use ot_paint::{find_runs, ChartRaster, ChartRun, DisplayList, RunScratch, Size};
use ot_shell_abi as abi;
use ot_ui::{Capabilities, ChartDrawing};

/// The ways the Linux shell draws charts, in [`ChartDrawing::ALL`] order.
pub const OFFERED: &[ChartDrawing] = &[ChartDrawing::Cpu, ChartDrawing::Native];

#[derive(Debug, Default)]
pub struct Charts {
    /// `OT_CHARTS`, which overrides the user's choice, for measuring.
    forced: Option<ChartDrawing>,
    runs: Vec<ChartRun>,
    /// One per run, kept between frames for their allocations.
    rasters: Vec<ChartRaster>,
    /// What the plugin is handed: one per run drawn, pointing into `rasters`.
    out: Vec<abi::Raster>,
    scratch: RunScratch,
}

impl Charts {
    /// With `OT_CHARTS` (`cpu`, `native`, `cairo`, `qpainter`) read.
    #[must_use]
    pub fn from_env() -> Self {
        let forced = std::env::var("OT_CHARTS")
            .ok()
            .and_then(|v| ChartDrawing::parse(&v));
        if let Some(f) = forced {
            tracing::info!(drawing = ?f, "chart drawing set by OT_CHARTS");
        }
        Self {
            forced,
            ..Self::default()
        }
    }

    /// How charts are drawn, for the user's `saved` choice: `OT_CHARTS` if set,
    /// and either way one the shell has.
    #[must_use]
    pub fn mode(&self, caps: &Capabilities, saved: ChartDrawing) -> ChartDrawing {
        caps.chart_drawing(self.forced.unwrap_or(saved))
    }

    /// Draw `dl`'s chart runs for a window `size` DIPs across at `scale` device
    /// pixels a DIP, when `mode` is the CPU; the images for the plugin, valid
    /// until the next call. None for any other mode.
    pub fn prepare(
        &mut self,
        dl: &DisplayList,
        mode: ChartDrawing,
        scale: f32,
        size: Size,
    ) -> &[abi::Raster] {
        self.out.clear();
        if mode != ChartDrawing::Cpu || !scale.is_finite() || scale <= 0.0 {
            return &self.out;
        }
        let size_px = (
            (size.w * scale).ceil().max(0.0) as u32,
            (size.h * scale).ceil().max(0.0) as u32,
        );
        find_runs(dl, scale, size_px, &mut self.runs);
        if self.rasters.len() < self.runs.len() {
            self.rasters.resize_with(self.runs.len(), ChartRaster::new);
        }
        for (run, raster) in self.runs.iter().zip(&mut self.rasters) {
            raster.draw_run(dl, run, scale, &mut self.scratch);
        }
        // Every run goes to the plugin, an empty one too (its commands are
        // outside the window or their clip): drawing it as nothing is right.
        for (run, raster) in self.runs.iter().zip(&self.rasters) {
            self.out.push(abi::Raster {
                first: run.start as u32,
                end: run.end as u32,
                rect: run.dip,
                width: raster.width(),
                height: raster.height(),
                pixels: raster.pixels().as_ptr(),
            });
        }
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::{Color, Point, Rect};

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

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
        dl.graph([p(20.0, 100.0), p(30.0, 120.5)], Color::WHITE, 1.0);
        dl
    }

    const LINUX: Capabilities = Capabilities {
        chart_drawings: OFFERED,
        ..Capabilities::WINDOWS
    };

    #[test]
    fn the_cpu_hands_the_plugin_an_image_per_run_in_device_pixels() {
        let dl = frame();
        let mut charts = Charts::default();
        let size = Size::new(400.0, 300.0);
        let out = charts.prepare(&dl, ChartDrawing::Cpu, 2.0, size).to_vec();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].first, out[0].end), (2, 4));
        assert_eq!((out[1].first, out[1].end), (5, 6));
        // The clip, 100 x 50 DIPs at (10, 10), cut to the shapes: whole device
        // pixels, the rect their DIPs.
        let r = out[0];
        assert_eq!(r.width, 200);
        assert!((r.rect.x - 10.0).abs() < 1e-4 && (r.rect.w - 100.0).abs() < 1e-4);
        assert!((r.rect.h * 2.0 - r.height as f32).abs() < 1e-3);
        // SAFETY: the pixels the raster holds, valid until the next prepare.
        let px = unsafe { std::slice::from_raw_parts(r.pixels, (r.width * r.height * 4) as usize) };
        assert!(px.contains(&255), "something was drawn");

        // Native: the plugin draws the commands; no images.
        assert!(charts
            .prepare(&dl, ChartDrawing::Native, 2.0, size)
            .is_empty());
    }

    #[test]
    fn the_gpu_saved_by_default_draws_on_the_cpu_here() {
        let charts = Charts::default();
        assert_eq!(charts.mode(&LINUX, ChartDrawing::Gpu), ChartDrawing::Cpu);
        assert_eq!(
            charts.mode(&LINUX, ChartDrawing::Native),
            ChartDrawing::Native
        );
        let forced = Charts {
            forced: Some(ChartDrawing::Native),
            ..Charts::default()
        };
        assert_eq!(forced.mode(&LINUX, ChartDrawing::Cpu), ChartDrawing::Native);
    }
}
