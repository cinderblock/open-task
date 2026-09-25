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

/// Create the main window, start sampling, and run the message loop until the
/// window closes.
///
/// # Errors
/// Returns [`ShellError`] if window or graphics setup fails, or on non-Windows.
pub fn run(probe: Box<dyn SystemProbe>, config: SamplerConfig) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        window::run(probe, config)
    }
    #[cfg(not(windows))]
    {
        let _ = (probe, config);
        Err(ShellError::Unsupported)
    }
}
