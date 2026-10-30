//! Chart shapes drawn by the host, for the CPU and GPU choices of chart drawing.
//!
//! With [`ChartDrawing::Cpu`] the host draws each run of chart commands
//! ([`ot_paint::find_runs`]) into an image of its own with `ot-paint`'s chart
//! rasterizer, in device pixels, and hands the images to the plugin with the frame
//! ([`abi::Raster`]); the plugin draws an image where its run's commands were.
//! One rasterizer for GTK and Qt both, the one the Windows shell's CPU choice
//! uses. With [`ChartDrawing::Gpu`] a big run's image comes from shaders instead
//! ([`ChartGl`]), and the plugin cannot tell: a small one stays on the CPU, which is
//! cheaper. With [`ChartDrawing::Native`] there are no images and the plugin draws
//! the commands with its toolkit's painter (Cairo, `QPainter`).
//!
//! The GPU is offered only once it has been tried: a context, a renderer that is
//! not software, the shaders, and a test drawing that matches the rasterizer's
//! ([`prove`]). The trial is a process of its own ([`TRIAL_ARG`]), started when the
//! Settings page opens or a saved choice asks for the GPU. Loading a graphics driver
//! takes about 100 MB, on top of the 40 the whole app uses, which nobody should pay
//! for opening Settings; and a driver that crashes or hangs takes only the trial
//! with it, which matters in a task manager and for a choice saved across runs. The
//! app loads the driver itself only for someone who has picked the GPU
//! ([`ChartGl::start`]). If it fails then, or later, the frame is drawn on the CPU
//! and the choice goes from the Settings page.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ot_paint::chart_shapes::Shapes;
use ot_paint::{
    find_runs, ChartRaster, ChartRun, Color, DisplayList, Point, Rect, RunScratch, Size,
};
use ot_shell_abi as abi;
use ot_ui::{Capabilities, ChartDrawing};

use crate::chart_gl::{ChartGl, GlAccess};

/// The ways the Linux shell draws charts, in [`ChartDrawing::ALL`] order.
pub const OFFERED: &[ChartDrawing] = &[ChartDrawing::Cpu, ChartDrawing::Native];

/// [`OFFERED`] with a working GPU.
pub const OFFERED_WITH_GPU: &[ChartDrawing] =
    &[ChartDrawing::Gpu, ChartDrawing::Cpu, ChartDrawing::Native];

/// The smallest run the GPU draws. Reading an image back has a cost of its own,
/// whatever its size, which the small charts above the process table never earn
/// back: there the GPU cost 30 % more CPU than the rasterizer, against 25 % less on
/// the History view (`plans/qt-gpu-charts.md`). And they then leave the GPU asleep.
const GPU_MIN_PIXELS: u64 = 100_000;

/// How far apart, out of 255, a test drawing may be from the rasterizer's. The two
/// differ by a few at edges (at most 3 or 4 measured); a driver that draws
/// something else is much further off.
const TOLERANCE: u8 = 8;

/// `OT_CHARTS_CHECK`: compare the GPU's images with the rasterizer's this often.
const CHECK_EVERY: u32 = 240;

/// The argument that makes this program the trial, and nothing else.
pub const TRIAL_ARG: &str = "--try-gl";

/// How long a trial may take. Generous: the first context through WSL's D3D12
/// layer takes 15 s, and a shader cache can be cold. Past this the driver has hung.
const TRIAL_LIMIT: Duration = Duration::from_secs(40);

/// Where the GPU stands.
#[derive(Debug, Default)]
enum Gpu {
    /// The plugin has no context to give, or nobody has wanted one yet.
    #[default]
    Untried,
    /// A trial is running: its result comes here.
    Trying(Receiver<Result<String, String>>),
    /// The trial passed; the app has not loaded the driver.
    Proved,
    Running(Box<ChartGl>),
    /// Tried, and it will not do, for the rest of the run.
    Refused,
}

/// An image the GPU drew for a run.
#[derive(Debug, Default)]
struct GpuImage {
    pixels: Vec<u8>,
    size: (u32, u32),
    /// Whether this, not the run's raster, is the run's latest.
    current: bool,
}

#[derive(Debug, Default)]
pub struct Charts {
    /// `OT_CHARTS`, which overrides the user's choice, for measuring.
    forced: Option<ChartDrawing>,
    /// `OT_CHARTS_CHECK`, and the frames since the last check.
    check: bool,
    since_check: u32,
    access: Option<GlAccess>,
    gpu: Gpu,
    runs: Vec<ChartRun>,
    /// One per run, kept between frames for their allocations.
    rasters: Vec<ChartRaster>,
    images: Vec<GpuImage>,
    /// The shapes of the runs the GPU draws this frame, and each run's range of them.
    shapes: Shapes,
    ranges: Vec<Option<(u32, u32)>>,
    /// What the plugin is handed: one per run drawn, pointing into `rasters` and
    /// `images`.
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
            check: std::env::var_os("OT_CHARTS_CHECK").is_some(),
            ..Self::default()
        }
    }

    /// The plugin can give an OpenGL context.
    pub fn connect(&mut self, access: GlAccess) {
        self.access = Some(access);
    }

    /// Whether it is time for a trial: the plugin has a context to give, none has
    /// been tried, and either `saved` or `OT_CHARTS` asks for the GPU or the choice
    /// is about to be shown (`offering`).
    #[must_use]
    pub fn wants_trial(&self, saved: ChartDrawing, offering: bool) -> bool {
        self.access.is_some()
            && matches!(self.gpu, Gpu::Untried)
            && (offering || self.forced.unwrap_or(saved) == ChartDrawing::Gpu)
    }

    /// Start the trial, in the background. `wake` is called, from another thread,
    /// when its result is in for [`Charts::trial_result`].
    pub fn start_trial(&mut self, wake: unsafe extern "C" fn()) {
        let (send, receive) = mpsc::channel();
        let waiter = std::thread::Builder::new()
            .name("ot-gl-trial".to_owned())
            .spawn(move || {
                let _ = send.send(run_trial());
                // SAFETY: the plugin's wake, which any thread may call.
                unsafe { wake() };
            });
        self.gpu = match waiter {
            Ok(_) => Gpu::Trying(receive),
            Err(e) => {
                tracing::warn!(error = %e, "no GPU chart drawing: no thread for the trial");
                Gpu::Refused
            }
        };
    }

    /// Take the trial's result if it has come. True when that changed what is on
    /// offer.
    pub fn trial_result(&mut self) -> bool {
        let Gpu::Trying(result) = &self.gpu else {
            return false;
        };
        match result.try_recv() {
            Ok(Ok(renderer)) => {
                tracing::info!(%renderer, "the GPU can draw charts");
                self.gpu = Gpu::Proved;
                true
            }
            Ok(Err(why)) => {
                tracing::warn!(%why, "no GPU chart drawing");
                self.gpu = Gpu::Refused;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.gpu = Gpu::Refused;
                true
            }
        }
    }

    /// Load the driver in this process, for someone who has picked the GPU.
    fn start_gl(&mut self) {
        let Some(access) = self.access else {
            self.gpu = Gpu::Refused;
            return;
        };
        // SAFETY: the plugin's functions, on its UI thread, where this runs. The
        // trial has said what is good enough, so nothing is refused here for it.
        self.gpu = match unsafe { ChartGl::start(access, true) } {
            Ok(gl) => Gpu::Running(Box::new(gl)),
            Err(why) => {
                tracing::warn!(%why, "GPU chart drawing did not start; drawing charts on the CPU");
                Gpu::Refused
            }
        };
    }

    /// The ways of drawing charts on offer now.
    #[must_use]
    pub fn offered(&self) -> &'static [ChartDrawing] {
        if matches!(self.gpu, Gpu::Proved | Gpu::Running(_)) {
            OFFERED_WITH_GPU
        } else {
            OFFERED
        }
    }

    /// How charts are drawn, for the user's `saved` choice: `OT_CHARTS` if set,
    /// and either way one the shell has.
    #[must_use]
    pub fn mode(&self, caps: &Capabilities, saved: ChartDrawing) -> ChartDrawing {
        caps.chart_drawing(self.forced.unwrap_or(saved))
    }

    /// Draw `dl`'s chart runs for a window `size` DIPs across at `scale` device
    /// pixels a DIP, when `mode` is the CPU or the GPU; the images for the plugin,
    /// valid until the next call. None for any other mode.
    ///
    /// With `damage`, for a plugin that draws only that, a run that misses it
    /// keeps the image it had: the plugin will not draw it, and drawing a chart
    /// is the costliest part of a frame where only another chart moved.
    pub fn prepare(
        &mut self,
        dl: &DisplayList,
        mode: ChartDrawing,
        scale: f32,
        size: Size,
        damage: Option<&[Rect]>,
    ) -> &[abi::Raster] {
        self.out.clear();
        if mode == ChartDrawing::Native || !scale.is_finite() || scale <= 0.0 {
            return &self.out;
        }
        let size_px = (
            (size.w * scale).ceil().max(0.0) as u32,
            (size.h * scale).ceil().max(0.0) as u32,
        );
        find_runs(dl, scale, size_px, &mut self.runs);
        let count = self.runs.len();
        if self.rasters.len() < count {
            self.rasters.resize_with(count, ChartRaster::new);
            self.images.resize_with(count, GpuImage::default);
        }
        if mode == ChartDrawing::Gpu && matches!(self.gpu, Gpu::Proved) {
            self.start_gl();
        }
        let gpu = mode == ChartDrawing::Gpu && matches!(self.gpu, Gpu::Running(_));
        self.shapes.clear();
        self.ranges.clear();
        for (i, run) in self.runs.iter().enumerate() {
            let wanted = damage.is_none_or(|d| reaches(run.dip, d));
            let big = u64::from(run.px.w) * u64::from(run.px.h) >= GPU_MIN_PIXELS;
            self.ranges
                .push((wanted && gpu && big).then(|| self.shapes.add_run(dl, run, scale)));
            if wanted && !(gpu && big) {
                self.rasters[i].draw_run(dl, run, scale, &mut self.scratch);
                self.images[i].current = false;
            }
        }
        if self.ranges.iter().any(Option::is_some) {
            if let Err(lost) = self.draw_on_gpu() {
                tracing::warn!(why = %lost.0, "GPU chart drawing stopped; drawing charts on the CPU");
                self.gpu = Gpu::Refused;
                for (i, run) in self.runs.iter().enumerate() {
                    if self.ranges[i].is_some() {
                        self.rasters[i].draw_run(dl, run, scale, &mut self.scratch);
                        self.images[i].current = false;
                    }
                }
            } else if self.check {
                self.check_images(dl, scale);
            }
        }
        // Every run goes to the plugin, an empty one too (its commands are
        // outside the window or their clip): drawing it as nothing is right.
        for ((run, raster), image) in self.runs.iter().zip(&self.rasters).zip(&self.images) {
            let (width, height, pixels) = if image.current {
                (image.size.0, image.size.1, image.pixels.as_ptr())
            } else {
                (raster.width(), raster.height(), raster.pixels().as_ptr())
            };
            self.out.push(abi::Raster {
                first: run.start as u32,
                end: run.end as u32,
                rect: run.dip,
                width,
                height,
                pixels,
            });
        }
        &self.out
    }

    /// Draw the runs with a range this frame into their images.
    fn draw_on_gpu(&mut self) -> Result<(), crate::chart_gl::GlLost> {
        let Gpu::Running(gl) = &mut self.gpu else {
            return Ok(());
        };
        gl.upload(&self.shapes)?;
        for (i, run) in self.runs.iter().enumerate() {
            if let Some(range) = self.ranges[i] {
                let image = &mut self.images[i];
                gl.draw(range, run.px.w, run.px.h, &mut image.pixels)?;
                image.size = (run.px.w, run.px.h);
                image.current = true;
            }
        }
        Ok(())
    }

    /// `OT_CHARTS_CHECK`: how far the GPU's images are from the rasterizer's, every
    /// so often, for trying a driver.
    fn check_images(&mut self, dl: &DisplayList, scale: f32) {
        self.since_check += 1;
        if self.since_check < CHECK_EVERY {
            return;
        }
        self.since_check = 0;
        let mut raster = ChartRaster::new();
        for (i, run) in self.runs.iter().enumerate() {
            if self.ranges[i].is_some() {
                raster.draw_run(dl, run, scale, &mut self.scratch);
                let worst = worst_difference(raster.pixels(), &self.images[i].pixels);
                tracing::info!(
                    w = run.px.w,
                    h = run.px.h,
                    worst,
                    "chart images: GPU vs CPU"
                );
            }
        }
    }
}

/// Run the trial: this program again, as [`TRIAL_ARG`]. What it printed, as the
/// renderer's name or why not.
fn run_trial() -> Result<String, String> {
    // Not the path it was started from, which an update may have replaced.
    let mut child = Command::new("/proc/self/exe")
        .arg(TRIAL_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the trial did not start: {e}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TRIAL_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the driver did not answer in {TRIAL_LIMIT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("the trial was lost: {e}")),
        }
    };
    let mut said = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut said);
    }
    let said = said.trim().to_owned();
    match status.code() {
        Some(0) => Ok(said),
        Some(_) if !said.is_empty() => Err(said),
        _ => Err(format!("the trial failed: {status}")),
    }
}

/// The trial's work, in its own process, with the plugin's context at hand: start
/// the renderer, as this run's settings allow (`OT_CHARTS=gpu` also takes a software
/// renderer, for testing), and have it draw a test image close to the rasterizer's.
/// The renderer's name, or why not.
///
/// # Errors
/// Why the GPU will not do, in words.
///
/// # Safety
/// `access` must be the plugin's, called on its UI thread.
pub unsafe fn prove(access: GlAccess) -> Result<String, String> {
    let software = Charts::from_env().forced == Some(ChartDrawing::Gpu);
    // SAFETY: the caller's promise.
    let mut gl = unsafe { ChartGl::start(access, software) }?;
    let p = Point::new;
    let mut dl = DisplayList::new();
    let color = Color::hex(0x39_87_E5).with_alpha(0.9);
    dl.band(
        [p(0.0, 20.0), p(64.0, 4.0)],
        [p(0.0, 30.0), p(64.0, 30.0)],
        color,
    );
    dl.graph(
        [p(0.0, 18.0), p(30.0, 26.0), p(64.0, 2.0)],
        Color::WHITE,
        1.5,
    );
    let mut runs = Vec::new();
    find_runs(&dl, 1.0, (64, 32), &mut runs);
    let run = runs.first().ok_or("no test drawing")?;
    let mut shapes = Shapes::default();
    let range = shapes.add_run(&dl, run, 1.0);
    let mut image = Vec::new();
    gl.upload(&shapes)
        .and_then(|()| gl.draw(range, run.px.w, run.px.h, &mut image))
        .map_err(|e| e.0)?;
    let mut raster = ChartRaster::new();
    raster.draw_run(&dl, run, 1.0, &mut RunScratch::default());
    match worst_difference(raster.pixels(), &image) {
        d if d <= TOLERANCE => Ok(gl.renderer().to_owned()),
        d => Err(format!("its test drawing is {d}/255 off the rasterizer's")),
    }
}

/// The largest difference between two images' bytes; the most there is when they
/// are not the same size.
fn worst_difference(a: &[u8], b: &[u8]) -> u8 {
    if a.len() != b.len() {
        return u8::MAX;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Whether `area` comes within a DIP of any of `damage`: the plugin's clip to
/// the damage is rounded out to whole pixels, so an image just outside it could
/// show a sliver.
fn reaches(area: Rect, damage: &[Rect]) -> bool {
    damage
        .iter()
        .any(|d| !area.intersect(&d.inset(-1.0, -1.0)).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = charts
            .prepare(&dl, ChartDrawing::Cpu, 2.0, size, None)
            .to_vec();
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
            .prepare(&dl, ChartDrawing::Native, 2.0, size, None)
            .is_empty());
    }

    #[test]
    fn a_partial_frame_redraws_only_the_images_the_damage_reaches() {
        let dl = frame();
        let mut charts = Charts::default();
        let size = Size::new(400.0, 300.0);
        charts.prepare(&dl, ChartDrawing::Cpu, 1.0, size, None);
        let drawn = |c: &Charts, i: usize| c.rasters[i].pixels().iter().any(|&b| b != 0);
        assert!(drawn(&charts, 0) && drawn(&charts, 1));

        // Blank both images, then damage only the second run's area.
        for r in &mut charts.rasters {
            *r = ChartRaster::new();
        }
        let damage = [Rect::new(18.0, 98.0, 14.0, 25.0)];
        let out = charts
            .prepare(&dl, ChartDrawing::Cpu, 1.0, size, Some(&damage))
            .to_vec();
        // Both runs still go to the plugin, which skips the first's commands.
        assert_eq!(out.len(), 2);
        assert!(!drawn(&charts, 0), "the first run missed the damage");
        assert!(drawn(&charts, 1));

        // Damage a DIP short of the first run's clip still redraws it.
        let near = [Rect::new(110.5, 20.0, 5.0, 5.0)];
        charts.prepare(&dl, ChartDrawing::Cpu, 1.0, size, Some(&near));
        assert!(drawn(&charts, 0));
    }

    /// A context that never comes.
    unsafe extern "C" fn no_context() -> bool {
        false
    }

    unsafe extern "C" fn no_function(_: *const std::ffi::c_char) -> *const std::ffi::c_void {
        std::ptr::null()
    }

    const NO_GL: GlAccess = GlAccess {
        make_current: no_context,
        proc_address: no_function,
    };

    #[test]
    fn the_gpu_is_tried_only_for_someone_who_may_use_it() {
        let mut charts = Charts::default();
        assert!(
            !charts.wants_trial(ChartDrawing::Gpu, true),
            "the plugin has no context to give"
        );
        charts.connect(NO_GL);
        assert!(
            !charts.wants_trial(ChartDrawing::Cpu, false),
            "the CPU's user"
        );
        assert!(
            charts.wants_trial(ChartDrawing::Cpu, true),
            "Settings is opening"
        );
        assert!(
            charts.wants_trial(ChartDrawing::Gpu, false),
            "the saved choice"
        );
        let forced = Charts {
            forced: Some(ChartDrawing::Gpu),
            ..Charts::default()
        };
        assert!(
            !forced.wants_trial(ChartDrawing::Cpu, false),
            "no plugin yet"
        );
        let mut forced = forced;
        forced.connect(NO_GL);
        assert!(forced.wants_trial(ChartDrawing::Cpu, false), "OT_CHARTS");
    }

    /// A trial that has finished with `result`.
    fn after_trial(result: Result<String, String>) -> Charts {
        let (send, receive) = mpsc::channel();
        send.send(result).expect("the receiver is here");
        let mut charts = Charts {
            gpu: Gpu::Trying(receive),
            ..Charts::default()
        };
        charts.connect(NO_GL);
        assert_eq!(charts.offered(), OFFERED, "not while it runs");
        assert!(charts.trial_result(), "what is on offer may change");
        assert!(!charts.trial_result(), "once");
        charts
    }

    #[test]
    fn a_passed_trial_offers_the_gpu_and_a_failed_one_does_not() {
        assert_eq!(
            after_trial(Ok("A GPU".to_owned())).offered(),
            OFFERED_WITH_GPU
        );
        let refused = after_trial(Err("no".to_owned()));
        assert_eq!(refused.offered(), OFFERED);
        assert!(!refused.wants_trial(ChartDrawing::Gpu, true), "not again");
    }

    #[test]
    fn the_driver_loads_only_when_the_gpu_is_drawn_with_and_failing_there_is_the_cpu() {
        let mut charts = after_trial(Ok("A GPU".to_owned()));
        let dl = frame();
        let size = Size::new(400.0, 300.0);
        charts.prepare(&dl, ChartDrawing::Cpu, 1.0, size, None);
        assert!(
            matches!(charts.gpu, Gpu::Proved),
            "the CPU's user pays nothing"
        );

        // The context that passed the trial is not there for the app: the frame
        // still comes, on the CPU, and the choice goes.
        let out = charts
            .prepare(&dl, ChartDrawing::Gpu, 1.0, size, None)
            .to_vec();
        assert_eq!(out.len(), 2);
        assert!(!out[0].pixels.is_null());
        assert!(charts.images.iter().all(|i| !i.current));
        assert_eq!(charts.offered(), OFFERED);
    }

    #[test]
    fn images_compare_by_their_bytes() {
        assert_eq!(worst_difference(&[1, 2, 3], &[1, 9, 0]), 7);
        assert_eq!(worst_difference(&[1, 2], &[1, 2, 3]), u8::MAX);
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
