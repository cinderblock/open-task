//! Placeholder backend for platforms without an implementation yet.
//!
//! Compiles everywhere so the workspace, CI matrix and UI can be exercised on Linux
//! and macOS before their real probes exist. Every call reports
//! [`ProbeError::Unsupported`] rather than fabricating data.

use crate::{Capabilities, ProbeError, ProbeOutput, SystemProbe};

/// Probe that measures nothing.
#[derive(Debug, Default)]
pub struct StubProbe;

impl StubProbe {
    /// Construct the stub. Never fails.
    ///
    /// # Errors
    /// Infallible; the signature matches the Windows probe so callers need no `cfg`.
    pub fn new() -> Result<Self, ProbeError> {
        Ok(Self)
    }

    /// Name of the platform this stub stands in for, for log messages.
    #[must_use]
    pub const fn platform_name() -> &'static str {
        if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "unknown"
        }
    }
}

impl SystemProbe for StubProbe {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        out.clear();
        Err(ProbeError::Unsupported(Self::platform_name()))
    }
}
