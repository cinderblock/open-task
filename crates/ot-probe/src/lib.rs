//! Platform sampling backends.
//!
//! Everything platform-specific in open-task lives behind [`SystemProbe`]. The core
//! never calls an OS API directly, which is what keeps the Linux and macOS ports to
//! this crate and lets the Flight Recorder stand in for a real machine.
//!
//! # Platform status
//!
//! | Platform | Status |
//! | --- | --- |
//! | Windows | implemented against `NtQuerySystemInformation` |
//! | Linux | stub; returns [`ProbeError::Unsupported`] |
//! | macOS | stub; returns [`ProbeError::Unsupported`] |

use std::time::Duration;

use ot_model::attribution::Attribution;
use ot_model::cpu::CpuSample;
use ot_model::device::{AdapterSample, DiskSample};
use ot_model::hardware::Hardware;
use ot_model::memory::MemorySample;
use ot_model::process::ProcessSample;
use ot_model::thread::ThreadSample;
pub use ot_model::Capabilities;
use ot_model::ProcessKey;

mod imp;

pub use imp::{PlatformControl, PlatformProbe, PlatformSampler};

/// Why a sampling pass could not produce data.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// This platform has no implementation yet.
    #[error("{0} is not implemented on this platform yet")]
    Unsupported(&'static str),
    /// An OS call failed.
    #[error("{context}: {source}")]
    Os {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl ProbeError {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn os(context: &'static str, source: std::io::Error) -> Self {
        Self::Os { context, source }
    }
}

/// One full sampling pass.
///
/// Reused between passes so a steady state does not allocate. The sampler calls
/// [`ProbeOutput::clear`] and the probe refills it in place.
#[derive(Debug, Default)]
pub struct ProbeOutput {
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub processes: Vec<ProcessSample>,
    /// Every sampled thread, grouped by process: each process names its range.
    pub threads: Vec<ThreadSample>,
    /// Physical disks, by number.
    pub disks: Vec<DiskSample>,
    /// Connected network adapters, physical ones first.
    pub adapters: Vec<AdapterSample>,
}

impl ProbeOutput {
    /// Empty the buffers while keeping their capacity.
    pub fn clear(&mut self) {
        self.processes.clear();
        self.threads.clear();
        self.disks.clear();
        self.adapters.clear();
        self.cpu.cores.clear();
        self.memory = MemorySample::default();
    }
}

/// A source of system measurements.
///
/// Implementations are stateful: they hold the previous pass's raw counters so they
/// can hand back rates rather than monotonic totals.
pub trait SystemProbe: Send + std::fmt::Debug {
    /// What this platform can measure. Constant for the life of the probe.
    fn capabilities(&self) -> Capabilities;

    /// Facts about the machine that do not change while it runs. Asked for once,
    /// when sampling starts.
    fn hardware(&self) -> Hardware {
        Hardware::default()
    }

    /// Take one pass, refilling `out`.
    ///
    /// # Errors
    /// Returns [`ProbeError`] if the platform is unsupported or an OS call fails.
    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError>;
}

/// Why an action on a process did not happen.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// This platform has no implementation yet.
    #[error("process actions are not implemented on this platform yet")]
    Unsupported,
    /// The process has exited, or its PID now belongs to a different process. Either
    /// way there is nothing left to act on, and nothing was touched.
    #[error("the process is no longer running")]
    Gone,
    /// An OS call failed; typically access denied for another user's process.
    #[error("{context}: {source}")]
    Os {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// Actions on processes.
///
/// Separate from [`SystemProbe`] so the UI thread can act while the sampler thread
/// samples. Every action is keyed by [`ProcessKey`], never a bare PID, and the
/// implementation must verify the identity before acting: a PID that has been
/// recycled since the snapshot was taken belongs to a stranger.
pub trait ProcessControl: Send + Sync + std::fmt::Debug {
    /// Kill the process. Fails with [`ControlError::Gone`] if it already exited or
    /// the PID has been reused.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn terminate(&self, key: ProcessKey) -> Result<(), ControlError>;
}

/// Why a CPU sample could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum SampleError {
    /// This platform has no implementation yet.
    #[error("CPU sampling is not implemented on this platform yet")]
    Unsupported,
    /// This process may not sample: on Windows it lacks `SeSystemProfilePrivilege`
    /// or the right to control trace sessions, which administrators have when
    /// elevated.
    #[error("CPU sampling needs open-task to run as administrator (it needs the system profiling privilege)")]
    NotPermitted,
    /// The process exited, or its PID was recycled, before sampling began.
    #[error("the process is no longer running")]
    Gone,
    /// An OS call failed.
    #[error("{context}: {source}")]
    Os {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SampleError {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn os(
        context: &'static str,
        e: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Os {
            context,
            source: std::io::Error::other(e),
        }
    }

    #[cfg(windows)]
    pub(crate) fn os_code(
        context: &'static str,
        code: windows::Win32::Foundation::WIN32_ERROR,
    ) -> Self {
        Self::Os {
            context,
            source: std::io::Error::from_raw_os_error(code.0.cast_signed()),
        }
    }
}

/// On-demand CPU sampling of one process: which modules its threads were
/// executing over a short window, and for a service with a known trace provider,
/// which clients it served. See [`Attribution`].
///
/// Blocking: a call takes about `duration`. Callers run it off the UI thread.
/// Implementations must be read-only with respect to the target: no suspension,
/// no debugger, no memory writes.
pub trait CpuSampler: Send + Sync + std::fmt::Debug {
    /// Sample `target` for `duration`. `services` are the names of the services
    /// the target hosts, for the client report.
    ///
    /// # Errors
    /// See [`SampleError`].
    fn sample(
        &self,
        target: ProcessKey,
        services: &[String],
        duration: Duration,
    ) -> Result<Attribution, SampleError>;
}
