//! Windows shell.
//!
//! A plain Win32 window whose client area is a `DirectComposition` swap chain drawn
//! with Direct2D and DirectWrite. There are no child controls: the whole content is
//! an [`ot_ui::App`] rendered from its display list. The window uses
//! `WS_EX_NOREDIRECTIONBITMAP` and a premultiplied-alpha swap chain so the Windows
//! 11 Mica backdrop shows through wherever the view leaves pixels transparent.
//!
//! Redraws happen only when something changed: a new snapshot (the sampler posts a
//! message), input, resize, or DPI change, plus the frames of a row slide after a
//! re-sort (150 ms, paced by the display). Idle cost is zero frames per second.
//!
//! The user's settings are read from and written to the registry by `prefs`. The
//! updater (`ot_update`) runs on its own threads and posts its progress back; the
//! installer it starts closes the window through Restart Manager (`WM_ENDSESSION`).
//!
//! [`launcher`] is the console launcher (`open-task.com`), which lets a terminal
//! wait for the command-line modes of this GUI program.
//!
//! [`task_manager`] makes Windows start open-task in Task Manager's place, as
//! Process Explorer's "Replace Task Manager" does; `instance` makes such a launch
//! bring an open window forward instead of opening another.
//!
//! On other platforms this crate compiles to a stub that returns
//! [`ShellError::Unsupported`], so the workspace builds everywhere.

#[cfg(windows)]
mod actions;
#[cfg(windows)]
mod gfx;
#[cfg(windows)]
mod icons;
#[cfg(windows)]
mod instance;
#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
mod prefs;
#[cfg(windows)]
mod run_dialog;
#[cfg(windows)]
pub mod task_manager;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod window;

use ot_core::SamplerConfig;
use ot_probe::SystemProbe;

pub use ot_ui::{Page, ViewMode};

/// Which theme to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreference {
    /// Follow the Windows "default app mode" setting, including live changes.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemePreference {
    /// Parse a command-line value. Unknown values fall back to `System`.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::System,
        }
    }
}

/// Startup options for the shell.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellOptions {
    pub theme: ThemePreference,
    /// How the process table starts out; Ctrl+T switches at runtime.
    pub view: ViewMode,
    /// The page shown first; the rail and Ctrl+Tab switch at runtime.
    pub page: Page,
    /// Whether `view` and `page` were asked for on the command line. When not,
    /// the window opens where the last session left off.
    pub view_given: bool,
    pub page_given: bool,
    /// This build's version, as `crates/ot-app/build.rs` made it: shown on the
    /// update button, and what updates are compared with.
    pub version: &'static str,
}

/// Why the shell could not run.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("the Windows shell only runs on Windows")]
    Unsupported,
    #[cfg(windows)]
    #[error("{context}: {source}")]
    Win {
        context: &'static str,
        #[source]
        source: windows::core::Error,
    },
}

/// Attach to the parent process's console, so a build without a console of its own
/// (`windows_subsystem = "windows"`) can still print in `--headless` mode when it is
/// launched from a terminal. A no-op when there is no parent console.
#[cfg(windows)]
pub fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: process-wide call with no pointers; failure just means no console.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Show a modal error box. For startup failures in a build that has no console to
/// print to.
#[cfg(windows)]
pub fn error_box(message: &str) {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    // SAFETY: both strings outlive the call; a null owner window is permitted.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("open-task"),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// For a launch in Task Manager's place ([`task_manager::is_stand_in`]): bring
/// forward an open-task window that is already open, as Task Manager does, waiting
/// a moment for one that is still starting. Returns whether one came forward, in
/// which case this process has nothing more to do. Call it before any slow setup.
#[cfg(windows)]
#[must_use]
pub fn raise_open_window() -> bool {
    instance::mark() && instance::raise_existing(window::CLASS_NAME)
}

/// Create the main window, start sampling, and run the message loop until the
/// window closes.
///
/// # Errors
/// Returns [`ShellError`] if window or graphics setup fails, or on non-Windows.
pub fn run(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    options: ShellOptions,
) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        window::run(probe, config, options)
    }
    #[cfg(not(windows))]
    {
        let _ = (probe, config, options);
        Err(ShellError::Unsupported)
    }
}
