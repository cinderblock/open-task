//! Per-process state.

use crate::identity::ProcessKey;
use crate::service::ServiceInfo;
use crate::units::{Bytes, Percent, Watts};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Elevation / integrity of a process, as far as we can tell without opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    /// The logon session the process runs in (Windows: session id; 0 is the
    /// services session). What the Users page groups by.
    pub session_id: u32,
    /// The instruction set the process runs, when it could be read.
    pub architecture: Architecture,
    /// The image's description from its version resource (`Google Chrome`), when
    /// readable. What Task Manager shows as a process's friendly name.
    pub description: Option<String>,
    /// The image's company from its version resource (`Google LLC`).
    pub company: Option<String>,
}

/// The instruction set a process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Architecture {
    X64,
    X86,
    Arm64,
    Arm,
    #[default]
    Unknown,
}

impl Architecture {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::X86 => "x86",
            Self::Arm64 => "ARM64",
            Self::Arm => "ARM",
            Self::Unknown => "",
        }
    }
}

/// A process's scheduling priority class, as Task Manager and Process Explorer
/// name them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub enum Priority {
    Idle,
    BelowNormal,
    #[default]
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    /// Every class, lowest first, for a menu.
    pub const ALL: [Self; 6] = [
        Self::Realtime,
        Self::High,
        Self::AboveNormal,
        Self::Normal,
        Self::BelowNormal,
        Self::Idle,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Low",
            Self::BelowNormal => "Below normal",
            Self::Normal => "Normal",
            Self::AboveNormal => "Above normal",
            Self::High => "High",
            Self::Realtime => "Realtime",
        }
    }

    /// The class a Windows base priority (4, 6, 8, 10, 13, 24) stands for. Values
    /// in between are rounded down to the class below them.
    #[must_use]
    pub const fn from_base(base: i32) -> Self {
        match base {
            i32::MIN..=5 => Self::Idle,
            6..=7 => Self::BelowNormal,
            8..=9 => Self::Normal,
            10..=12 => Self::AboveNormal,
            13..=23 => Self::High,
            _ => Self::Realtime,
        }
    }

    /// The Windows base priority of this class.
    #[must_use]
    pub const fn base(self) -> i32 {
        match self {
            Self::Idle => 4,
            Self::BelowNormal => 6,
            Self::Normal => 8,
            Self::AboveNormal => 10,
            Self::High => 13,
            Self::Realtime => 24,
        }
    }
}

/// What a process is to a person, as Task Manager groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ProcessKind {
    /// Has a window of its own on the desktop.
    App,
    /// Everything else that runs as a user.
    #[default]
    Background,
    /// Part of the operating system: a system image run by a system account.
    Windows,
}

impl ProcessKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "App",
            Self::Background => "Background",
            Self::Windows => "Windows",
        }
    }
}

/// A process's main window, when it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// The platform's window handle, opaque to everything above the probe; the
    /// shell hands it back to bring the window forward.
    pub handle: u64,
    /// The window's title.
    pub title: String,
    /// The window has stopped answering the system: what Task Manager shows as
    /// "Not responding".
    pub hung: bool,
}

/// Cumulative I/O counts and bytes since the process started, every kind of I/O
/// (file, network, device). Task Manager's Details page shows these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IoCounters {
    pub reads: u64,
    pub writes: u64,
    pub other: u64,
    pub read_bytes: Bytes,
    pub write_bytes: Bytes,
    pub other_bytes: Bytes,
}

/// Per-process values for one sampling pass.
///
/// Everything here is a rate or a level measured over the interval that just ended.
/// Raw monotonic counters stay in the probe layer; by the time a value reaches the UI
/// it has already been differenced.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProcessSample {
    /// Immutable facts, shared by pointer across samples.
    /// Not serialized: shared by pointer between passes, so the Flight Recorder
    /// writes each distinct value once in a table and restores it on read.
    #[serde(skip)]
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
    /// Whether the system is throttling the process to save power (Windows'
    /// efficiency mode, `EcoQoS`). `None` where it could not be read.
    pub efficiency_mode: Option<bool>,
    /// The process's main window, when it has one on the desktop. Shared by
    /// pointer between passes while unchanged.
    /// Not serialized: shared by pointer between passes, so the Flight Recorder
    /// writes each distinct value once in a table and restores it on read.
    #[serde(skip)]
    pub window: Option<Arc<WindowInfo>>,
    /// App, background or part of Windows.
    pub kind: ProcessKind,
    /// Scheduling priority class.
    pub priority: Priority,
    /// Page faults since the process started.
    pub page_faults: u32,
    /// The most physical memory the process has held at once.
    pub peak_working_set: Bytes,
    /// Virtual address space reserved or committed.
    pub virtual_size: Bytes,
    /// Kernel pool memory charged to the process.
    pub paged_pool: Bytes,
    pub nonpaged_pool: Bytes,
    /// I/O since the process started.
    pub io: IoCounters,
    /// The GPU engine the process is busiest on (`GPU 0 - 3D`), when it uses one.
    pub gpu_engine: Option<Arc<str>>,

    /// Services hosted by this process, as the platform's service manager reports
    /// them. Empty for an ordinary program. Shared by pointer between passes while
    /// the set is unchanged.
    /// Not serialized: shared by pointer between passes, so the Flight Recorder
    /// writes each distinct value once in a table and restores it on read.
    #[serde(skip)]
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
