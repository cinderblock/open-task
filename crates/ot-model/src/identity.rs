//! Stable process identity.
//!
//! Operating systems recycle PIDs, and they recycle them fastest exactly when the
//! machine is busy — which is when a task manager is being used. A UI keyed on PID
//! alone will silently attribute a dead process's history to an unrelated new one.
//!
//! Every platform we target exposes a per-process creation stamp that, combined with
//! the PID, is unique for the uptime of the machine:
//!
//! | Platform | Source | Meaning |
//! | --- | --- | --- |
//! | Windows | `SYSTEM_PROCESS_INFORMATION::CreateTime` | 100 ns ticks since 1601 |
//! | Linux | field 22 of `/proc/<pid>/stat` | clock ticks since boot |
//! | macOS | `kinfo_proc::kp_proc.p_starttime` | microseconds since epoch |
//!
//! We keep that value opaque. Nothing above this module should interpret it; it is
//! only ever compared for equality.

/// Opaque, platform-defined process birth stamp. Compare only for equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ProcessKeyRaw(pub u64);

/// A process identity that remains correct across PID reuse.
///
/// This is the key for every per-process time series in the application. Two
/// processes that share a PID but not a birth stamp are different processes and must
/// never share history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessKey {
    /// The OS process id. Display this; do not key on it alone.
    pub pid: u32,
    /// Platform-defined creation stamp that disambiguates PID reuse.
    pub birth: ProcessKeyRaw,
}

impl ProcessKey {
    #[must_use]
    pub const fn new(pid: u32, birth: u64) -> Self {
        Self {
            pid,
            birth: ProcessKeyRaw(birth),
        }
    }

    /// The synthetic key for the kernel "process" that owns system-wide time.
    ///
    /// Windows reports PID 0 as "System Idle Process"; Linux has no PID 0 entry. We
    /// give it a fixed key so the two agree.
    #[must_use]
    pub const fn idle() -> Self {
        Self::new(0, 0)
    }
}
