//! Linux shell.
//!
//! `open-task` links no GUI toolkit, so the same binary runs on a desktop and on
//! a server with no display libraries at all. The window comes from a plugin,
//! loaded at run time: GTK 4 (`ot-shell-gtk`) or Qt 6 (`ot-shell-qt`), each a
//! shared object that speaks the C ABI in `ot-shell-abi`. A release binary
//! carries both inside it ([`Embedded`]); which one runs is chosen per desktop
//! ([`select`]), and the other is the fallback when the first one's libraries
//! are missing.
//!
//! This crate is the host side: it loads the plugin ([`loader`]), feeds the
//! plugin's input to [`ot_shell_core::Shell`] and carries out what the shell
//! reports, keeps the settings in a file ([`prefs`]), and does what the desktop
//! does for an app without a toolkit's help ([`desktop`]).
//!
//! On other platforms only [`select`] and the stubs compile.

pub mod chart_gl;
pub mod charts;
#[cfg(target_os = "linux")]
pub mod desktop;
#[cfg(target_os = "linux")]
mod frame_stats;
#[cfg(target_os = "linux")]
mod host;
pub mod keys;
#[cfg(target_os = "linux")]
pub mod loader;
#[cfg(target_os = "linux")]
mod partial;
pub mod prefs;
pub mod select;

pub use ot_shell_core::{ShellOptions, ThemePreference};
pub use ot_ui::{Page, ViewMode};
pub use select::Toolkit;

#[cfg(target_os = "linux")]
pub use loader::Embedded;

/// Why the window could not be shown.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("the Linux shell only runs on Linux")]
    Unsupported,
    /// No toolkit could be loaded, or none could open a window: why, for each.
    #[error("no toolkit could open a window ({})", .0.join("; "))]
    NoToolkit(Vec<String>),
    #[error(transparent)]
    Start(#[from] ot_shell_core::StartError),
    /// Internal: a second start was attempted without a probe.
    #[error("the probe was already used")]
    Spent,
}

/// [`charts::TRIAL_ARG`]: be the GPU's trial, a process with nothing else to do,
/// and print what came of it. The exit status for the process.
#[cfg(target_os = "linux")]
#[must_use]
pub fn gl_trial(embedded: &[Embedded]) -> i32 {
    // SAFETY: a plain request: not to outlive the app if it goes first.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
    host::gl_trial(embedded)
}

/// Whether this session can show a window: a Wayland or X11 display to open
/// it on.
#[must_use]
pub fn display_available() -> bool {
    select::display_available(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    )
}

/// The toolkit asked for with `OPEN_TASK_TOOLKIT`, if any.
#[must_use]
pub fn toolkit_from_env() -> Option<Toolkit> {
    std::env::var("OPEN_TASK_TOOLKIT")
        .ok()
        .and_then(|t| Toolkit::parse(&t))
}

/// Open the window with the first toolkit that will, start sampling, and run
/// until the window closes. `asked` is the toolkit the user named, if any.
///
/// # Errors
/// [`RunError::NoToolkit`] when no plugin could be loaded or open a window: the
/// caller can still run headless.
#[cfg(target_os = "linux")]
pub fn run(
    embedded: &[Embedded],
    asked: Option<Toolkit>,
    probe: Box<dyn ot_probe::SystemProbe>,
    config: ot_core::SamplerConfig,
    options: &ShellOptions,
) -> Result<(), RunError> {
    host::run(embedded, asked, probe, config, options)
}
