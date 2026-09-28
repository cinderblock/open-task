//! UI-agnostic view models.
//!
//! Everything here turns a [`ot_core::Snapshot`] plus some interaction state into an
//! [`ot_paint::DisplayList`]. No platform code, no GPU, no windows: the whole layer is
//! unit-testable, and every platform shell is a thin adapter around [`App`].

#![forbid(unsafe_code)]

mod charts;
pub mod format;
mod nav;
mod perf;
mod process_rows;
pub mod sparkline;
pub mod table;
pub mod theme;
pub mod view;

pub use nav::Page;
pub use theme::Theme;
pub use view::{
    App, Command, Cursor, Effect, Key, MenuAction, MenuEntry, MouseButton, Reaction, UiEvent,
    ViewMode,
};
