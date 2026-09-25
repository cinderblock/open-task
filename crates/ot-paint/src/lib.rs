//! Portable draw-command layer.
//!
//! The UI describes a frame as a [`DisplayList`]: a flat list of fills, lines, text
//! runs and clips in device-independent pixels. A platform backend (Direct2D on
//! Windows; others later) rasterizes it. Nothing in this crate touches a GPU or an
//! OS API, so view-model code can be unit-tested by inspecting the list it produces.
//!
//! Design constraints that shaped this:
//! - **No per-frame allocation in steady state.** Points and strings live in arenas
//!   inside the list, and commands index into them. A list is cleared and refilled
//!   each frame, keeping its capacity.
//! - **Text is cacheable.** A backend can hash `(text, style)` to reuse layouts
//!   across frames, which is what makes a 1000-row table cheap when 59 of every 60
//!   frames draw identical strings.
//! - **Units are DIPs.** Backends apply DPI. The UI never sees physical pixels.

#![forbid(unsafe_code)]

pub mod color;
pub mod display;
pub mod geom;
pub mod text;

pub use color::Color;
pub use display::{DisplayList, DrawCmd, Span, TextCmd};
pub use geom::{Point, Rect, Size};
pub use text::{FontFamily, FontWeight, HAlign, TextStyle, VAlign};

/// A backend that can rasterize a display list into a window.
pub trait Renderer {
    /// Draw one frame. Called only when something changed; the backend must not
    /// assume a steady cadence.
    ///
    /// # Errors
    /// Backend-specific device or surface failures.
    fn render(
        &mut self,
        list: &DisplayList,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}
