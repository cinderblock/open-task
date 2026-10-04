//! An immutable view of the system at one instant.
//!
//! The sampling core publishes one of these per pass and the UI reads whichever is
//! newest. It lives in the model, below the core, so the Flight Recorder can write
//! and read it without depending on the sampler: a recording is a sequence of these,
//! and replay publishes them where the sampler normally would.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::battery::BatterySample;
use crate::cpu::CpuSample;
use crate::device::{AdapterSample, DiskSample, VolumeSample};
use crate::gpu::GpuSample;
use crate::hardware::Hardware;
use crate::memory::MemorySample;
use crate::process::ProcessSample;
use crate::service::ServiceEntry;
use crate::session::SessionInfo;
use crate::thread::ThreadSample;
use crate::{Capabilities, Tick};

/// Everything the probe measured in one pass, plus timing.
///
/// Snapshots are published behind an `Arc` and never mutated. A reader that holds one
/// can take as long as it likes; the sampler simply publishes the next one alongside.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Every sampled thread; each process names its own range with
    /// [`ProcessSample::thread_range`]. Empty on platforms without thread sampling.
    pub threads: Vec<ThreadSample>,
    /// Physical disks, by number. Empty where the platform cannot report them.
    pub disks: Vec<DiskSample>,
    /// Connected network adapters, physical ones first.
    pub adapters: Vec<AdapterSample>,
    /// Graphics adapters, in the system's order. Empty where the platform cannot
    /// report them.
    pub gpus: Vec<GpuSample>,
    /// The battery, on a machine that has one.
    pub battery: Option<BatterySample>,
    /// Mounted volumes, `C:` first.
    pub volumes: Vec<VolumeSample>,
    /// Logon sessions, for the Users page.
    pub sessions: Vec<SessionInfo>,
    /// Every service of the machine, by name, shared by pointer between
    /// snapshots while unchanged.
    ///
    /// Not serialized: the Flight Recorder writes each distinct list once in a
    /// table and restores it on read.
    #[serde(skip)]
    pub services: Arc<[ServiceEntry]>,
    /// What the probe behind this snapshot can measure, so the UI can explain a
    /// missing column or attribution rather than show a blank.
    pub capabilities: Capabilities,
    /// Static facts about the machine, shared by every snapshot of a session.
    ///
    /// Not serialized: the Flight Recorder keeps it in the file's header.
    #[serde(skip)]
    pub hardware: Arc<Hardware>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            tick: Tick::default(),
            taken_at: None,
            interval: Duration::ZERO,
            probe_cost: Duration::ZERO,
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
            capabilities: Capabilities::default(),
            hardware: Arc::new(Hardware::default()),
        }
    }
}

impl Snapshot {
    /// True until the first real pass has been published.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.taken_at.is_none()
    }
}
