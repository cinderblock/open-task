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

use ot_model::cpu::CpuSample;
use ot_model::memory::MemorySample;
use ot_model::process::ProcessSample;

mod imp;

pub use imp::PlatformProbe;

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

/// Which metrics this platform can actually supply.
///
/// The UI uses this to hide columns rather than show a grid of dashes. A column that
/// can never have data on this machine is worse than no column at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// A flag set is the honest shape for this; enums would add nothing but ceremony.
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub per_process_cpu: bool,
    pub per_process_disk: bool,
    pub per_process_network: bool,
    pub per_process_gpu: bool,
    pub per_process_power: bool,
    pub core_frequency: bool,
    pub package_power: bool,
    pub thermals: bool,
    pub hybrid_core_kinds: bool,
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
}

impl ProbeOutput {
    /// Empty the buffers while keeping their capacity.
    pub fn clear(&mut self) {
        self.processes.clear();
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

    /// Take one pass, refilling `out`.
    ///
    /// # Errors
    /// Returns [`ProbeError`] if the platform is unsupported or an OS call fails.
    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError>;
}
