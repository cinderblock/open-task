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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ot_model::apps::InstalledApp;
use ot_model::attribution::Attribution;
use ot_model::battery::BatterySample;
use ot_model::connection::Connection;
use ot_model::cpu::CpuSample;
use ot_model::device::{AdapterSample, DiskSample, VolumeSample};
use ot_model::gpu::GpuSample;
use ot_model::hardware::Hardware;
use ot_model::memory::MemorySample;
use ot_model::process::{Priority, ProcessSample};
use ot_model::service::ServiceEntry;
use ot_model::session::SessionInfo;
use ot_model::startup::StartupEntry;
use ot_model::system::SystemFacts;
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
#[derive(Debug)]
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
    /// Graphics adapters, in the system's order.
    pub gpus: Vec<GpuSample>,
    /// The battery, on a machine that has one.
    pub battery: Option<BatterySample>,
    /// Mounted volumes, `C:` first. Refreshed every few seconds, not every pass.
    pub volumes: Vec<VolumeSample>,
    /// Logon sessions. Refreshed every few seconds.
    pub sessions: Vec<SessionInfo>,
    /// Every service of the machine, by name. Shared by pointer between passes
    /// while nothing in it changes.
    pub services: Arc<[ServiceEntry]>,
}

impl Default for ProbeOutput {
    fn default() -> Self {
        Self {
            cpu: CpuSample::default(),
            memory: MemorySample::default(),
            processes: Vec::new(),
            threads: Vec::new(),
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            battery: None,
            volumes: Vec::new(),
            sessions: Vec::new(),
            services: Vec::new().into(),
        }
    }
}

impl ProbeOutput {
    /// Empty the buffers while keeping their capacity.
    pub fn clear(&mut self) {
        self.processes.clear();
        self.threads.clear();
        self.disks.clear();
        self.adapters.clear();
        self.gpus.clear();
        self.battery = None;
        self.volumes.clear();
        self.sessions.clear();
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
    #[error("this action is not implemented on this platform yet")]
    Unsupported,
    /// The process has exited, or its PID now belongs to a different process. Either
    /// way there is nothing left to act on, and nothing was touched.
    #[error("the process is no longer running")]
    Gone,
    /// The caller lacks the rights: another user's process, a protected process,
    /// or an action that needs administrator rights (realtime priority, a
    /// service, another session).
    #[error("access denied; this needs open-task run as administrator")]
    NotPermitted,
    /// An OS call failed.
    #[error("{context}: {source}")]
    Os {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl ControlError {
    #[allow(dead_code)] // for the platform actions as they are built
    pub(crate) fn os(
        context: &'static str,
        e: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Os {
            context,
            source: std::io::Error::other(e),
        }
    }
}

/// Which logical processors a process may run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Affinity {
    /// Bit `i` set: the process may run on logical processor `i` (processor
    /// group 0).
    pub mask: u64,
    /// The processors that exist, in the same form.
    pub system: u64,
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

    /// Change the process's priority class.
    ///
    /// # Errors
    /// See [`ControlError`]. Realtime needs administrator rights.
    fn set_priority(&self, _key: ProcessKey, _priority: Priority) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Which processors the process may run on.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn affinity(&self, _key: ProcessKey) -> Result<Affinity, ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Restrict the process to the processors in `mask`, which must not be empty.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn set_affinity(&self, _key: ProcessKey, _mask: u64) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Freeze every thread of the process, as Process Explorer's Suspend does.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn suspend(&self, _key: ProcessKey) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Undo [`ProcessControl::suspend`].
    ///
    /// # Errors
    /// See [`ControlError`].
    fn resume(&self, _key: ProcessKey) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Turn the platform's power throttling (Windows' efficiency mode: `EcoQoS` and
    /// idle priority) on or off for the process.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn set_efficiency_mode(&self, _key: ProcessKey, _on: bool) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Write a full memory dump of the process into `dir`, named after the process,
    /// and return the file's path. Blocking: a large process takes seconds.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn write_dump(&self, _key: ProcessKey, _dir: &Path) -> Result<PathBuf, ControlError> {
        Err(ControlError::Unsupported)
    }
}

/// Actions on services, for the Services page. Starting and stopping need
/// administrator rights on Windows; the implementation reports
/// [`ControlError::NotPermitted`] when they are missing.
pub trait ServiceControl: Send + Sync + std::fmt::Debug {
    /// # Errors
    /// See [`ControlError`].
    fn start_service(&self, _name: &str) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Ask the service to stop and wait, briefly, for it to.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn stop_service(&self, _name: &str) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }
}

/// Actions on logon sessions, for the Users page.
pub trait SessionControl: Send + Sync + std::fmt::Debug {
    /// Disconnect the session, leaving its programs running.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn disconnect_session(&self, _id: u32) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }

    /// Sign the session out, ending its programs.
    ///
    /// # Errors
    /// See [`ControlError`].
    fn logoff_session(&self, _id: u32) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }
}

/// Actions on startup entries, for the Startup page.
pub trait StartupControl: Send + Sync + std::fmt::Debug {
    /// Let the entry run at sign-in, or stop it from running, without removing it.
    ///
    /// # Errors
    /// See [`ControlError`]. A machine-wide entry needs administrator rights.
    fn set_startup_enabled(&self, _entry: &StartupEntry, _on: bool) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }
}

/// Lists read on demand rather than sampled: what is installed, what starts at
/// sign-in, which endpoints are open, what the machine is. Each takes milliseconds
/// to tens of milliseconds; callers run them off the UI thread.
pub trait Inventory: Send + Sync + std::fmt::Debug {
    /// Programs that start at sign-in, enabled or not.
    fn startup_entries(&self) -> Vec<StartupEntry> {
        Vec::new()
    }

    /// Installed programs, by name.
    fn installed_apps(&self) -> Vec<InstalledApp> {
        Vec::new()
    }

    /// Every open TCP and UDP endpoint with its owning PID.
    fn connections(&self) -> Vec<Connection> {
        Vec::new()
    }

    /// The machine and its operating system.
    fn system_facts(&self) -> SystemFacts {
        SystemFacts::default()
    }
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
