//! GTK 4 plugin for open-task's Linux shell.
//!
//! A shared object that open-task loads at run time (`ot-shell-linux`) and finds
//! through `ot_shell_plugin`, speaking the C ABI in `ot-shell-abi`. It owns the
//! `GLib` main loop and the one window: a [`canvas::Canvas`] that draws the host's
//! display list with GSK nodes and Pango text, and the menus and dialogs the host
//! asks for, all shown without blocking.
//!
//! GTK 4.6 is the floor. Nothing newer is called, so the plugin loads on Ubuntu
//! 22.04 and RHEL 9 as well as on current desktops.
//!
//! On other platforms the crate is empty.

#[cfg(target_os = "linux")]
mod canvas;
#[cfg(target_os = "linux")]
mod dialogs;
#[cfg(target_os = "linux")]
mod draw;
#[cfg(target_os = "linux")]
mod plugin;

/// The plugin's entry point: its function table.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub extern "C" fn ot_shell_plugin() -> *const ot_shell_abi::Plugin {
    &raw const plugin::PLUGIN
}
