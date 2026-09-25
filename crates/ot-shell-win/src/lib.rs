//! Windows shell.
//!
//! A plain Win32 window whose client area is a `DirectComposition` swap chain drawn
//! with Direct2D and DirectWrite. There are no child controls: the whole content is
//! an [`ot_ui::App`] rendered from its display list. The window uses
//! `WS_EX_NOREDIRECTIONBITMAP` and a premultiplied-alpha swap chain so the Windows
//! 11 Mica backdrop shows through wherever the view leaves pixels transparent.
//!
//! Redraws happen only when something changed: a new snapshot (the sampler posts a
//! message), input, resize, or DPI change. Idle cost is zero frames per second.
//!
//! On other platforms this crate compiles to a stub that returns
//! [`ShellError::Unsupported`], so the workspace builds everywhere.

#[cfg(windows)]
mod gfx;
#[cfg(windows)]
mod window;

use ot_core::SamplerConfig;
use ot_probe::SystemProbe;

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
