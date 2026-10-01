//! Per-process state.

use crate::identity::ProcessKey;
use crate::service::ServiceInfo;
use crate::units::{Bytes, Percent, Watts};
use std::sync::Arc;
use std::time::Duration;

/// Elevation / integrity of a process, as far as we can tell without opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Integrity {
    /// Sandboxed (`AppContainer`, or a low-integrity browser renderer).
    Low,
    /// Ordinary user process.
    #[default]
    Medium,
    /// Elevated / administrator.
    High,
    /// SYSTEM, or a kernel-mode owner.
    System,
    /// Could not be determined, usually because the process could not be opened.
    Unknown,
}

/// Fields that do not change over a process's lifetime.
///
/// Held behind an `Arc` and cloned by pointer into every sample, so a 1000-process
/// refresh does not re-allocate a thousand command line strings each pass. This is
/// the single most important allocation decision in the sampling path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessStatic {
    pub key: ProcessKey,
    /// Parent's identity, absent for a root or when the parent had already exited
    /// when this process was first seen.
    ///
    /// Operating systems only report the parent's PID, and PIDs are recycled. The
    /// probe resolves the PID against the live table once, at first sight, and only
    /// accepts a process created no later than this one, so a stranger that inherited
    /// the parent's PID is never adopted. A consumer can use this as a real identity.
    pub parent: Option<ProcessKey>,
    /// Executable file name only, e.g. `chrome.exe`.
    pub name: String,
    /// Full path to the image, when readable.
    pub image_path: Option<String>,
    /// Full command line, when readable.
    pub command_line: Option<String>,
    /// Owning user, formatted for display.
    pub user: Option<String>,
    /// Integrity / elevation level.
    pub integrity: Integrity,
    /// Wall-clock start time as a Unix timestamp in milliseconds, for display only.
    /// Identity uses [`ProcessKey`], never this.
    pub started_unix_ms: Option<i64>,
}

/// Per-process values for one sampling pass.
///
/// Everything here is a rate or a level measured over the interval that just ended.
/// Raw monotonic counters stay in the probe layer; by the time a value reaches the UI
/// it has already been differenced.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessSample {
    /// Immutable facts, shared by pointer across samples.
    pub statics: Arc<ProcessStatic>,

    /// CPU used over the last interval, as a share of one core. 400.0 means four
    /// cores fully saturated.
    pub cpu: Percent,
    /// CPU time used since the process started, user and kernel, all threads. What
    /// a process "has been using" over any stretch is the difference of two of
    /// these.
    pub cpu_time: Duration,
    /// Processor clock cycles used since the process started, all threads, where
    /// the platform counts them (Windows does); zero where it does not. A finer
    /// measure of the same thing as `cpu_time`: that is charged a clock tick at a
    /// time, this is counted exactly, so a short burst shows in it.
    pub cycles: u64,
    /// Private working set: physical memory this process alone is holding.
    pub working_set: Bytes,
    /// Private committed bytes, the closest thing to "how much will I get back".
    pub private_bytes: Bytes,

    /// Bytes read from disk over the interval.
    pub disk_read: Bytes,
    /// Bytes written to disk over the interval.
    pub disk_write: Bytes,
    /// Bytes received over the interval, where per-process attribution is available.
    pub net_rx: Bytes,
    /// Bytes sent over the interval.
    pub net_tx: Bytes,

    /// Live thread count.
    pub threads: u32,
    /// Open kernel handle / file descriptor count.
    pub handles: u32,

    /// Estimated energy attribution, where the platform models it.
    pub power: Option<Watts>,
    /// GPU utilization attributed to this process.
    pub gpu: Option<Percent>,

    /// True when the process is suspended (UWP lifecycle, `SIGSTOP`, debugger break).
    /// A suspended process at 0% CPU is idle by design, not stuck, and the diagnostics
    /// engine must not flag it.
    pub suspended: bool,

    /// Services hosted by this process, as the platform's service manager reports
    /// them. Empty for an ordinary program. Shared by pointer between passes while
    /// the set is unchanged.
    pub services: Arc<[ServiceInfo]>,
    /// Start and length of this process's rows in the snapshot's thread list, see
    /// [`ProcessSample::thread_range`]. Zero rows when the platform does not sample
    /// threads.
    pub thread_first: u32,
    pub thread_rows: u32,
}

impl ProcessSample {
    /// Convenience accessor; process identity is a property of the static half.
    #[must_use]
    pub fn key(&self) -> ProcessKey {
        self.statics.key
    }

    /// Convenience accessor for the display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.statics.name
    }

    /// Index range of this process's rows in the snapshot's thread list.
    #[must_use]
    pub fn thread_range(&self) -> std::ops::Range<usize> {
        let first = self.thread_first as usize;
        first..first + self.thread_rows as usize
    }

    /// Whether the platform's service manager says this process hosts services.
    #[must_use]
    pub fn is_service_host(&self) -> bool {
        !self.services.is_empty()
    }
}
