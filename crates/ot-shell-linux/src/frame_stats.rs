//! `OT_FRAME_STATS`: how many frames the view made and what they cost, logged
//! every few seconds, as the Windows shell does. The plugin logs its own drawing
//! time beside it. A diagnostic, for telling where the time goes without a
//! profiler.

use std::time::{Duration, Instant};

use ot_ui::PaintKind;

/// How often the totals are logged and reset.
const PERIOD: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct FrameLog {
    since: Instant,
    frames: u32,
    ticks: u32,
    slides: u32,
    fulls: u32,
    paint: Duration,
    worst: Duration,
}

impl FrameLog {
    /// A log when `OT_FRAME_STATS` is set.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var_os("OT_FRAME_STATS")?;
        Some(Self {
            since: Instant::now(),
            frames: 0,
            ticks: 0,
            slides: 0,
            fulls: 0,
            paint: Duration::ZERO,
            worst: Duration::ZERO,
        })
    }

    /// One frame: what kind the view made, and how long making it took.
    pub fn add(&mut self, kind: PaintKind, paint: Duration) {
        self.frames += 1;
        match kind {
            PaintKind::Tick => self.ticks += 1,
            PaintKind::Slide => self.slides += 1,
            PaintKind::Full => self.fulls += 1,
        }
        self.paint += paint;
        self.worst = self.worst.max(paint);
        let elapsed = self.since.elapsed();
        if elapsed >= PERIOD {
            let per_frame = self.paint.checked_div(self.frames).unwrap_or_default();
            tracing::info!(
                frames = self.frames,
                fps = format!("{:.1}", f64::from(self.frames) / elapsed.as_secs_f64()),
                ticks = self.ticks,
                slides = self.slides,
                fulls = self.fulls,
                paint_avg_us = per_frame.as_micros(),
                paint_worst_us = self.worst.as_micros(),
                "frames"
            );
            *self = Self {
                since: Instant::now(),
                frames: 0,
                ticks: 0,
                slides: 0,
                fulls: 0,
                paint: Duration::ZERO,
                worst: Duration::ZERO,
            };
        }
    }
}
