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
    /// Drawing the charts' images (the CPU choice of chart drawing).
    charts: Duration,
    /// Frames the plugin draws only in part, and the share of the window that
    /// is, summed over them.
    partials: u32,
    drawn: f32,
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
            charts: Duration::ZERO,
            partials: 0,
            drawn: 0.0,
        })
    }

    /// One frame: what kind the view made, how long making it took, how long
    /// drawing its charts' images took, and the share of the window the plugin
    /// is to draw when that is not all of it.
    pub fn add(&mut self, kind: PaintKind, paint: Duration, charts: Duration, drawn: Option<f32>) {
        self.frames += 1;
        match kind {
            PaintKind::Tick => self.ticks += 1,
            PaintKind::Slide => self.slides += 1,
            PaintKind::Full => self.fulls += 1,
        }
        self.paint += paint;
        self.worst = self.worst.max(paint);
        self.charts += charts;
        if let Some(d) = drawn {
            self.partials += 1;
            self.drawn += d;
        }
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
                charts_avg_us = self
                    .charts
                    .checked_div(self.frames)
                    .unwrap_or_default()
                    .as_micros(),
                partials = self.partials,
                partial_drawn_pct =
                    format!("{:.1}", 100.0 * self.drawn / self.partials.max(1) as f32),
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
                charts: Duration::ZERO,
                partials: 0,
                drawn: 0.0,
            };
        }
    }
}
