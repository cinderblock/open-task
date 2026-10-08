//! `OT_FRAME_STATS`: what the frames cost, logged every few seconds.
//!
//! With the variable set, every frame's kind ([`PaintKind`]), whether it was drawn
//! in part, and the time each step took are added up and logged at info level
//! every [`PERIOD`], with the process's own CPU over the same time. A diagnostic,
//! for telling where a frame's time goes without a profiler.

use std::time::{Duration, Instant};

use ot_paint::Rect;
use ot_ui::PaintKind;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use crate::gfx::FrameStats;

/// How often the totals are logged and reset.
const PERIOD: Duration = Duration::from_secs(5);

/// Totals since the last log.
#[derive(Debug)]
pub struct FrameLog {
    since: Instant,
    cpu_at_start: Duration,
    frames: u32,
    ticks: u32,
    slides: u32,
    fulls: u32,
    partial: u32,
    damaged: f32,
    paint: Duration,
    diff: Duration,
    draw: Duration,
    draw_partial: Duration,
    copy: Duration,
    charts: Duration,
    present: Duration,
    rest: Duration,
    worst: Duration,
    /// The damage of the last tick frame drawn in part.
    tick_damage: Vec<Rect>,
}

impl FrameLog {
    /// A log when `OT_FRAME_STATS` is set.
    pub fn from_env() -> Option<Self> {
        std::env::var_os("OT_FRAME_STATS")?;
        tracing::info!("logging frame statistics every {} s", PERIOD.as_secs());
        Some(Self::new())
    }

    fn new() -> Self {
        Self {
            since: Instant::now(),
            cpu_at_start: process_cpu(),
            frames: 0,
            ticks: 0,
            slides: 0,
            fulls: 0,
            partial: 0,
            damaged: 0.0,
            paint: Duration::ZERO,
            diff: Duration::ZERO,
            draw: Duration::ZERO,
            draw_partial: Duration::ZERO,
            copy: Duration::ZERO,
            charts: Duration::ZERO,
            present: Duration::ZERO,
            rest: Duration::ZERO,
            worst: Duration::ZERO,
            tick_damage: Vec::new(),
        }
    }

    /// Add one frame: how the view made it, how long that took, and how the
    /// renderer went.
    pub fn add(&mut self, kind: PaintKind, paint: Duration, gfx: FrameStats, damage: &[Rect]) {
        self.frames += 1;
        if kind == PaintKind::Tick && gfx.partial {
            self.tick_damage.clear();
            self.tick_damage.extend_from_slice(damage);
        }
        match kind {
            PaintKind::Tick => self.ticks += 1,
            PaintKind::Slide => self.slides += 1,
            PaintKind::Full => self.fulls += 1,
        }
        if gfx.partial {
            self.partial += 1;
            self.draw_partial += gfx.draw;
        }
        self.damaged += gfx.damaged;
        self.paint += paint;
        self.diff += gfx.diff;
        self.draw += gfx.draw;
        self.copy += gfx.copy;
        self.charts += gfx.charts;
        self.present += gfx.present;
        self.rest += gfx.rest;
        let busy = paint + gfx.diff + gfx.charts + gfx.draw + gfx.copy + gfx.rest;
        self.worst = self.worst.max(busy);
        if self.since.elapsed() >= PERIOD {
            self.log();
            *self = Self::new();
        }
    }

    fn log(&self) {
        let secs = self.since.elapsed().as_secs_f64();
        let n = f64::from(self.frames.max(1));
        let ms = |d: Duration| d.as_secs_f64() * 1000.0 / n;
        let cpu = (process_cpu().saturating_sub(self.cpu_at_start)).as_secs_f64() / secs * 100.0;
        let whole = self.frames - self.partial;
        tracing::info!(
            fps = format!("{:.1}", f64::from(self.frames) / secs),
            tick = self.ticks,
            slide = self.slides,
            full = self.fulls,
            partial = self.partial,
            whole,
            damaged_pct = format!("{:.0}", f64::from(self.damaged) / n * 100.0),
            paint_ms = format!("{:.3}", ms(self.paint)),
            diff_ms = format!("{:.3}", ms(self.diff)),
            draw_ms = format!("{:.3}", ms(self.draw)),
            draw_partial_ms = format!(
                "{:.3}",
                self.draw_partial.as_secs_f64() * 1000.0 / f64::from(self.partial.max(1))
            ),
            draw_whole_ms = format!(
                "{:.3}",
                self.draw.saturating_sub(self.draw_partial).as_secs_f64() * 1000.0
                    / f64::from(whole.max(1))
            ),
            copy_ms = format!("{:.3}", ms(self.copy)),
            charts_ms = format!("{:.3}", ms(self.charts)),
            present_ms = format!("{:.3}", ms(self.present)),
            rest_ms = format!("{:.3}", ms(self.rest)),
            worst_ms = format!("{:.2}", self.worst.as_secs_f64() * 1000.0),
            cpu_pct_of_core = format!("{cpu:.1}"),
            tick_damage = ?self.tick_damage,
            "frames"
        );
    }
}

/// The process's CPU time so far, user and kernel.
fn process_cpu() -> Duration {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the pseudo-handle is always valid; the out-pointers are locals.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok.is_err() {
        return Duration::ZERO;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // FILETIME counts 100 ns.
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}
