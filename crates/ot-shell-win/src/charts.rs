//! Chart shapes ([`DrawCmd::Band`], [`DrawCmd::Graph`]) drawn off Direct2D's path
//! rasterizer.
//!
//! A run of consecutive chart commands is a [`ChartRun`]: before a frame's Direct2D
//! pass, each run it draws is rendered into a bitmap of its own, in device pixels,
//! by the GPU ([`crate::chart_gpu`]) or the CPU ([`ot_paint::ChartRaster`]); the
//! pass then draws the bitmap 1:1 where the run's commands were. `ot-paint` finds
//! the runs ([`find_runs`]) and turns their shapes into pixels both renderers take
//! ([`Shapes`]), cut into [`Piece`]s for the GPU.
//!
//! [`DrawCmd::Band`]: ot_paint::DrawCmd::Band
//! [`DrawCmd::Graph`]: ot_paint::DrawCmd::Graph

pub use ot_paint::chart_shapes::{Columns, Kind, Piece, Shapes};
pub use ot_paint::{find_runs, ChartRun};

/// How chart shapes are drawn. The choice is the user's (Settings), `OT_CHARTS`
/// overrides it.
pub use ot_ui::ChartDrawing;
