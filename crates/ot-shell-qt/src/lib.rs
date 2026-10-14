//! Qt 6 plugin for open-task's Linux shell.
//!
//! The plugin is C++ (`src/plugin.cpp`), compiled by the build script against
//! Qt 6 Widgets and `ot-shell-abi`'s header. Qt has no C API, so the C++ speaks
//! the ABI itself; this crate exists so Cargo builds it like every other part of
//! open-task, and to export the entry point, since a Rust `cdylib` exports only
//! the symbols Rust declares.
//!
//! It owns the Qt event loop and the one window: a widget that draws the host's
//! display list with `QPainter`, and the menus and dialogs the host asks for,
//! shown with `open()` and `popup()` so that nothing blocks.
//!
//! On other platforms the crate is empty.

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// The C++ side's function table (`src/plugin.cpp`).
    fn ot_qt_plugin() -> *const ot_shell_abi::Plugin;
}

/// The plugin's entry point: its function table.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub extern "C" fn ot_shell_plugin() -> *const ot_shell_abi::Plugin {
    // SAFETY: the C++ returns a pointer to its static table.
    unsafe { ot_qt_plugin() }
}
