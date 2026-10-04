//! Per-thread state.
//!
//! Threads are where a process's CPU actually goes. For a single-purpose program
//! that is a detail; for a shared service host it is the only way to tell which
//! service is busy. Thread rows are sampled every pass from the same system query
//! that produces the process list, so they cost no extra handles or syscalls.

use crate::units::Percent;
use serde::{Deserialize, Serialize};

/// Scheduler state of a thread at the instant of the sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ThreadState {
    Ready,
    Running,
    Waiting,
    /// Anything else: initializing, standby, transition, terminated.
    #[default]
    Other,
}

impl ThreadState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Waiting => "Waiting",
            Self::Other => "",
        }
    }
}

/// Why a waiting thread is waiting, as the platform's raw reason code. Only the
/// handful of reasons a task manager user cares about are named; the rest show as
/// a number, which is still enough to tell a suspended thread from a sleeping one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct WaitReason(pub u8);

impl WaitReason {
    /// Names for the common Windows `KWAIT_REASON` values.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        Some(match self.0 {
            4 | 11 => "Sleeping",
            5 | 12 => "Suspended",
            6 | 13 => "User request",
            15 => "Queue",
            37 => "ALPC",
            _ => return None,
        })
    }
}

/// Which service a thread was working for, when the platform tags threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ServiceTag {
    /// Not known: the platform has no tags, or this process could not be read.
    #[default]
    Unknown,
    /// Tagged as belonging to no service (the host's own threads, or a service that
    /// does not tag its work).
    None,
    /// Index into the owning process's service list.
    Service(u16),
}

/// One thread for one sampling pass.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThreadSample {
    /// OS thread id. Recycled like PIDs; identity is `(tid, birth)`.
    pub tid: u32,
    /// Platform-defined creation stamp, compared only for equality.
    pub birth: u64,
    /// CPU used over the last interval, as a share of one core.
    pub cpu: Percent,
    pub state: ThreadState,
    pub wait_reason: WaitReason,
    pub service: ServiceTag,
    /// Wall-clock start as Unix milliseconds, for display.
    pub started_unix_ms: Option<i64>,
}
