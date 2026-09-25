//! An immutable view of the system at one instant.

use std::time::{Duration, SystemTime};

use ot_model::cpu::CpuSample;
use ot_model::memory::MemorySample;
use ot_model::process::ProcessSample;
use ot_model::Tick;

/// Everything the probe measured in one pass, plus timing.
///
/// Snapshots are published behind an `Arc` and never mutated. A reader that holds one
/// can take as long as it likes; the sampler simply publishes the next one alongside.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Which pass produced this snapshot.
    pub tick: Tick,
    /// Wall-clock time the pass completed. For display and for the recorder.
    pub taken_at: Option<SystemTime>,
    /// How long the previous interval actually was. Rates in this snapshot are
    /// measured over this duration, which is not necessarily the configured one.
    pub interval: Duration,
    /// How long the probe took to produce this pass. The app's own overhead, shown
    /// honestly.
    pub probe_cost: Duration,
    pub cpu: CpuSample,
    pub memory: MemorySample,
    /// All processes, in the order the OS returned them. Sorting is the UI's job.
    pub processes: Vec<ProcessSample>,
}

impl Snapshot {
    /// True until the first real pass has been published.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.taken_at.is_none()
    }
}
