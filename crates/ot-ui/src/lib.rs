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
mod settings;
pub mod sparkline;
mod steady;
pub mod table;
mod task_manager;
pub mod theme;
pub mod treemap;
mod update;
mod usage_map;
pub mod view;

pub use nav::Page;
pub use settings::Settings;
pub use task_manager::{Replacement, TaskManager};
pub use theme::Theme;
pub use update::{UpdateAction, UpdateView};
pub use view::{
    App, Command, Cursor, Effect, Key, MenuAction, MenuEntry, MouseButton, Reaction, UiEvent,
    ViewMode,
};
