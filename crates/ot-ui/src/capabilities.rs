//! What the platform and its shell can do, so the app offers only what works.
//!
//! The Settings page was written on Windows, where the shell can do everything it
//! lists. Elsewhere some of it is missing for now (no tray, no updater, no switching
//! to another app's window) and some for good (GTK 4 cannot keep a window on top).
//! The shell says which with [`Capabilities`], and a card for something missing is
//! not shown, the way the Task Manager card is not shown where there is no Task
//! Manager to replace. A card comes back as soon as its shell says it can.

use crate::settings::ChartDrawing;

/// Where the system's animation setting comes from, which row animation follows
/// until the user chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationSource {
    /// Windows' animation effects.
    Windows,
    /// The desktop's setting, as the toolkit reads it (GTK's `gtk-enable-animations`).
    Desktop,
    /// Nothing this shell can read: rows animate unless turned off here, and the
    /// card says nothing about the system.
    Unknown,
}

/// What draws the general paths, and the charts when [`ChartDrawing::Native`] is
/// picked: Direct2D on Windows, the toolkit's painter on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRenderer {
    Direct2D,
    /// GTK's.
    Cairo,
    /// Qt's.
    QPainter,
}

impl PathRenderer {
    /// Its name, as the chart drawing card lists it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Direct2D => "Direct2D",
            Self::Cairo => "Cairo",
            Self::QPainter => "QPainter",
        }
    }
}

/// What the platform and its shell can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// The system's name, the heading of the Settings section for it.
    pub system: &'static str,
    /// Where row animation's system setting comes from.
    pub animations: AnimationSource,
    /// The ways of drawing charts this shell has, in [`ChartDrawing::ALL`] order.
    pub chart_drawings: &'static [ChartDrawing],
    /// What [`ChartDrawing::Native`] is here.
    pub paths: PathRenderer,
    /// The window can be kept above every other.
    pub keep_above: bool,
    /// There is a tray icon to bring a hidden window back, so minimizing can hide it.
    pub tray: bool,
    /// Switch to can bring another app's window forward (so minimizing on it means
    /// something).
    pub switch_to: bool,
    /// The updater can check for, download and install releases.
    pub updates: bool,
    /// A copy can be started with administrator (root) rights.
    pub run_elevated: bool,
}

impl Capabilities {
    /// Windows, where the shell does everything the app offers.
    pub const WINDOWS: Self = Self {
        system: "Windows",
        animations: AnimationSource::Windows,
        chart_drawings: &ChartDrawing::ALL,
        paths: PathRenderer::Direct2D,
        keep_above: true,
        tray: true,
        switch_to: true,
        updates: true,
        run_elevated: true,
    };

    /// How charts are drawn when the user saved `saved`: that, if this shell has
    /// it, else the next one it has after it in [`ChartDrawing::ALL`] order (the GPU,
    /// the default, falls to the CPU), else the first it has.
    #[must_use]
    pub fn chart_drawing(&self, saved: ChartDrawing) -> ChartDrawing {
        let has = |d: &ChartDrawing| self.chart_drawings.contains(d);
        if has(&saved) {
            return saved;
        }
        ChartDrawing::ALL
            .iter()
            .skip_while(|&&d| d != saved)
            .find(|d| has(d))
            .or_else(|| self.chart_drawings.first())
            .copied()
            .unwrap_or(saved)
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::WINDOWS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_chart_drawing_this_shell_lacks_falls_to_the_next_it_has() {
        let linux = Capabilities {
            chart_drawings: &[ChartDrawing::Cpu, ChartDrawing::Native],
            ..Capabilities::WINDOWS
        };
        assert_eq!(linux.chart_drawing(ChartDrawing::Gpu), ChartDrawing::Cpu);
        assert_eq!(
            linux.chart_drawing(ChartDrawing::Native),
            ChartDrawing::Native
        );
        let only_native = Capabilities {
            chart_drawings: &[ChartDrawing::Native],
            ..Capabilities::WINDOWS
        };
        assert_eq!(
            only_native.chart_drawing(ChartDrawing::Gpu),
            ChartDrawing::Native
        );
        let gpu_only = Capabilities {
            chart_drawings: &[ChartDrawing::Gpu],
            ..Capabilities::WINDOWS
        };
        // Nothing after Native: the first there is.
        assert_eq!(
            gpu_only.chart_drawing(ChartDrawing::Native),
            ChartDrawing::Gpu
        );
        for d in ChartDrawing::ALL {
            assert_eq!(Capabilities::WINDOWS.chart_drawing(d), d);
        }
    }
}
