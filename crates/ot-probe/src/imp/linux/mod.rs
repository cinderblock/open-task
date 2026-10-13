//! The Linux backend: `/proc` and `/sys`, plus a few syscalls through `libc`.
//!
//! See `plans/linux-probe.md` for how each Windows concept maps onto what Linux
//! exposes.

mod control;
mod cpu;
mod fs;
mod process;

use std::time::Duration;

use ot_model::attribution::Attribution;
use ot_model::memory::MemorySample;
use ot_model::units::Bytes;
use ot_model::ProcessKey;

use crate::{Capabilities, CpuSampler, ProbeError, ProbeOutput, SampleError, SystemProbe};

pub use control::LinuxControl;

/// Whether this process runs as root.
#[must_use]
pub fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// The Linux implementation of [`SystemProbe`].
#[derive(Debug)]
pub struct LinuxProbe {
    cpu: cpu::CpuProbe,
    processes: process::ProcessProbe,
    hardware: ot_model::hardware::Hardware,
    buf: Vec<u8>,
}

impl LinuxProbe {
    /// Construct the probe and read the processor topology.
    ///
    /// # Errors
    /// Returns [`ProbeError::Os`] if `/proc` cannot be read: it is not mounted, or
    /// this is not Linux.
    pub fn new() -> Result<Self, ProbeError> {
        std::fs::metadata("/proc/stat").map_err(|e| ProbeError::os("reading /proc", e))?;
        let hardware = cpu::hardware();
        Ok(Self {
            cpu: cpu::CpuProbe::new(),
            processes: process::ProcessProbe::new(
                hardware.logical_processors,
                hardware.boot_unix_ms,
            ),
            hardware,
            buf: Vec::with_capacity(2048),
        })
    }

    fn sample_memory(&mut self) -> Result<MemorySample, ProbeError> {
        fs::read_into("/proc/meminfo", &mut self.buf)
            .map_err(|e| ProbeError::os("reading /proc/meminfo", e))?;
        Ok(parse_meminfo(&self.buf))
    }
}

/// `/proc/meminfo`, mapped onto the Windows-shaped model. See the plan for why
/// each field is what it is.
fn parse_meminfo(buf: &[u8]) -> MemorySample {
    let mut m = MemorySample::default();
    let (mut swap_total, mut swap_free, mut dirty, mut writeback) = (0, 0, 0, 0);
    let mut zswap = None;
    for line in buf.split(|&b| b == b'\n') {
        let Some((key, value)) = fs::key_value(line) else {
            continue;
        };
        let Some(v) = fs::kb_value(value) else {
            continue;
        };
        match key {
            b"MemTotal" => m.total = Bytes(v),
            b"MemFree" => m.free = Some(Bytes(v)),
            b"MemAvailable" => m.available = Bytes(v),
            b"Cached" => m.cached = Bytes(v),
            b"SwapTotal" => swap_total = v,
            b"SwapFree" => swap_free = v,
            b"Dirty" => dirty = v,
            b"Writeback" => writeback = v,
            b"SUnreclaim" => m.nonpaged_pool = Some(Bytes(v)),
            b"Committed_AS" => m.committed = Bytes(v),
            b"Zswap" => zswap = Some(Bytes(v)),
            _ => {}
        }
    }
    // The kernel's own CommitLimit only binds under `vm.overcommit_memory = 2`;
    // the model's limit is physical plus swap, as on Windows.
    m.commit_limit = Bytes(m.total.get() + swap_total);
    m.swap_used = (swap_total > 0).then(|| Bytes(swap_total.saturating_sub(swap_free)));
    m.modified = Some(Bytes(dirty + writeback));
    m.compressed = zswap;
    m
}

impl SystemProbe for LinuxProbe {
    fn hardware(&self) -> ot_model::hardware::Hardware {
        self.hardware.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            per_process_cpu: true,
            per_process_disk: true,
            core_frequency: self.cpu.has_frequency(),
            hybrid_core_kinds: self.cpu.hybrid(),
            threads: true,
            services: self.processes.has_services(),
            elevated: is_elevated(),
            ..Capabilities::default()
        }
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        out.clear();
        self.cpu
            .sample(&mut out.cpu)
            .map_err(|e| ProbeError::os("reading /proc/stat", e))?;
        self.processes.sample(&mut out.processes, &mut out.threads);
        out.memory = self.sample_memory()?;
        Ok(())
    }
}

/// CPU sampling by module. Not built yet on Linux (`perf_event_open` would be the
/// way).
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxSampler;

impl CpuSampler for LinuxSampler {
    fn sample(
        &self,
        _target: ProcessKey,
        _services: &[String],
        _duration: Duration,
    ) -> Result<Attribution, SampleError> {
        Err(SampleError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_maps_onto_the_model() {
        let m = parse_meminfo(
            b"MemTotal:       16000000 kB\nMemFree:         1000000 kB\n\
              MemAvailable:    8000000 kB\nCached:          5000000 kB\n\
              SwapTotal:       4000000 kB\nSwapFree:        3000000 kB\n\
              Dirty:               100 kB\nWriteback:            20 kB\n\
              SUnreclaim:       200000 kB\nCommitted_AS:   9000000 kB\n",
        );
        let kb = |n: u64| Bytes(n * 1024);
        assert_eq!(m.total, kb(16_000_000));
        assert_eq!(m.available, kb(8_000_000));
        assert_eq!(m.in_use(), kb(8_000_000));
        assert_eq!(m.cached, kb(5_000_000));
        assert_eq!(m.free, Some(kb(1_000_000)));
        assert_eq!(m.commit_limit, kb(20_000_000));
        assert_eq!(m.committed, kb(9_000_000));
        assert_eq!(m.swap_used, Some(kb(1_000_000)));
        assert_eq!(m.modified, Some(kb(120)));
        assert_eq!(m.nonpaged_pool, Some(kb(200_000)));
        assert_eq!(m.compressed, None);
    }

    #[test]
    fn a_full_pass_succeeds() {
        let mut probe = LinuxProbe::new().unwrap();
        let mut out = ProbeOutput::default();
        probe.sample(&mut out).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        probe.sample(&mut out).unwrap();
        assert_ne!(out.cpu.cores.len(), 0);
        assert!(out.memory.total.get() > 0);
        assert!(out.memory.available <= out.memory.total);
        assert!(out.processes.len() > 1);
        assert_ne!(out.threads.len(), 0);
    }
}
