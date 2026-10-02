//! Windows backend.
//!
//! Built on `NtQuerySystemInformation`, the same undocumented-but-stable call that
//! Task Manager and Process Explorer use. One syscall returns every process with its
//! CPU times, memory counters, I/O transfer counts, thread and handle counts, and
//! creation time. It needs no handle to any individual process, so it works on
//! protected and elevated processes from an unelevated caller, and it is dramatically
//! cheaper than `OpenProcess` + `GetProcessTimes` + `GetProcessMemoryInfo` per PID.
//!
//! Per-core load comes from `SystemProcessorPerformanceInformation` from the same
//! call. Memory comes from `GlobalMemoryStatusEx` and `GetPerformanceInfo`. Hybrid
//! core classes come from `GetLogicalProcessorInformationEx`.
//!
//! The process structure is declared by hand in [`nt`] because the public SDK hides
//! its fields behind `Reserved` names; see that module for the layout pins.
//!
//! Image path, command line, user and integrity need a handle per process. They are
//! collected once per process lifetime by [`details`], at first sight when the pass's
//! time budget allows and from a queue over the following passes otherwise, so a
//! first pass over hundreds of processes stays quick.
//!
//! # Known gaps (tracked in the plan)
//! - Only the first processor group (up to 64 logical processors) is sampled.
//! - Per-core frequency is not reported. `CallNtPowerInformation` is known to be
//!   stale on modern Windows; the correct source is the PDH counter
//!   `\Processor Information(*)\% Processor Performance` scaled by base frequency.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::memory::MemorySample;
use ot_model::process::{
    Integrity, IoCounters, Priority, ProcessKind, ProcessSample, ProcessStatic,
};
use ot_model::service::ServiceInfo;
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState, WaitReason};
use ot_model::{Bytes, Hertz, Percent, ProcessKey};

use windows::Wdk::System::SystemInformation::{
    NtQuerySystemInformation, SystemProcessInformation, SystemProcessorPerformanceInformation,
    SYSTEM_INFORMATION_CLASS,
};
use windows::Win32::Foundation::{
    NTSTATUS, STATUS_INFO_LENGTH_MISMATCH, STATUS_SUCCESS, UNICODE_STRING,
};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetSystemInfo, GlobalMemoryStatusEx, RelationProcessorCore,
    MEMORYSTATUSEX, SYSTEM_INFO, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

use crate::{Capabilities, ProbeError, ProbeOutput, SystemProbe};

mod access;
mod battery;
mod connections;
mod control;
mod counters;
mod details;
mod gpu;
mod hardware;
mod installed;
mod network;
mod nt;
mod profile;
mod services;
mod sessions;
mod smbios;
mod startup;
mod storage;
mod system;
mod tags;
mod verinfo;
mod volumes;
mod windows_list;

pub use control::WindowsControl;
use counters::{DiskRates, PerfCounters};
use details::DetailProbe;
use network::NetProbe;
use nt::{SystemProcessInformation, SystemThreadInformation};
pub use profile::WindowsSampler;
use services::ServiceProbe;
use tags::{OwnedHandle, TagProbe};

/// Difference between the Windows FILETIME epoch (1601-01-01) and the Unix epoch, in
/// 100 ns units.
const FILETIME_UNIX_OFFSET_100NS: i64 = 116_444_736_000_000_000;

/// Time per pass spent collecting per-process details (path, command line, user).
/// The sampler thread pays this, not the UI. A full first pass on a busy machine
/// takes a few passes to fill in; each later pass only sees a handful of new
/// processes and finishes well inside the budget.
const DETAIL_BUDGET: Duration = Duration::from_millis(20);

/// Hard cap on thread rows per pass, so a runaway process cannot make the snapshot
/// arbitrarily large. Processes past the cap report no thread rows that pass.
const MAX_THREAD_ROWS: usize = 65_536;

/// Raw monotonic counters from the previous pass, per process.
#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    /// Kernel + user time in 100 ns units.
    cpu_100ns: u64,
    read_bytes: u64,
    write_bytes: u64,
}

/// What a thread's service tag read produced, remembered per thread lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagState {
    /// Not read yet (no budget, or the process handle is not open yet).
    Unresolved,
    /// Read failed for this thread; do not retry.
    Failed,
    /// The raw tag. Zero means untagged.
    Tag(u32),
}

/// Per-thread memory between passes.
#[derive(Debug, Clone, Copy)]
struct ThreadPrev {
    birth: u64,
    cpu_100ns: u64,
    last_seen: u64,
    tag: TagState,
}

/// Everything we remember about a process between passes.
#[derive(Debug)]
struct Tracked {
    statics: Arc<ProcessStatic>,
    prev: Counters,
    /// Pass number this entry was last observed in, for reaping exited processes.
    last_seen: u64,
    /// Services the SCM says run here; shared with the published sample until the
    /// set changes.
    services: Arc<[ServiceInfo]>,
    /// Threads by TID.
    threads: HashMap<u32, ThreadPrev>,
    /// Handle for reading thread tags, opened once for a service host when tags
    /// are available. `tag_tried` stops a refused open from being retried.
    tag_handle: Option<OwnedHandle>,
    tag_tried: bool,
}

/// Per-core raw times from the previous pass.
#[derive(Debug, Clone, Copy, Default)]
struct CoreTimes {
    idle: u64,
    kernel: u64,
    user: u64,
}

/// A byte buffer with 8-byte alignment, because the structures the kernel writes
/// into it are 8-byte aligned and `Vec<u8>` only promises 1.
#[derive(Debug, Default)]
pub(super) struct AlignedBuf(Vec<u64>);

impl AlignedBuf {
    pub(super) fn len_bytes(&self) -> usize {
        self.0.len() * 8
    }

    pub(super) fn resize_bytes(&mut self, bytes: usize) {
        self.0.resize(bytes.div_ceil(8), 0);
    }

    /// 8-byte-aligned base pointer. Callers use `byte_add` for byte offsets.
    pub(super) fn as_mut_ptr(&mut self) -> *mut u64 {
        self.0.as_mut_ptr()
    }

    /// 8-byte-aligned base pointer. Callers use `byte_add` for byte offsets.
    pub(super) fn as_ptr(&self) -> *const u64 {
        self.0.as_ptr()
    }
}

/// The Windows implementation of [`SystemProbe`].
#[derive(Debug)]
pub struct WindowsProbe {
    /// Reused across passes so steady state does not allocate.
    proc_buf: AlignedBuf,
    core_buf: AlignedBuf,
    tracked: HashMap<ProcessKey, Tracked>,
    /// Indices into the pass's output of processes first seen in that pass, whose
    /// parent hints still need resolving.
    new_this_pass: Vec<u32>,
    /// PID to output index for the current pass; only filled when needed, at most
    /// once per pass (`by_pid_pass` records which).
    by_pid: HashMap<u32, u32>,
    by_pid_pass: u64,
    details: DetailProbe,
    services: ServiceProbe,
    /// Present only when service hosts can be read; see [`tags`].
    tags: Option<TagProbe>,
    /// Whether on-demand CPU sampling can run; see [`profile::can_sample`].
    can_sample: bool,
    /// The shared empty list every non-host process points at.
    no_services: Arc<[ServiceInfo]>,
    /// Processes seen while the details budget was spent; drained on later passes.
    pending_details: Vec<ProcessKey>,
    prev_cores: Vec<CoreTimes>,
    /// Physical core index and class for each logical processor, computed once.
    topology: Vec<(u32, CoreKind)>,
    logical_count: u32,
    /// Static facts, read once.
    hardware: ot_model::hardware::Hardware,
    /// Performance counters for the clock and the memory lists, if PDH works here.
    counters: Option<PerfCounters>,
    /// Each logical processor's performance as a percentage of base, this pass.
    performance: Vec<Option<f64>>,
    /// This pass's disk counters, reused.
    disk_rates: Vec<DiskRates>,
    /// Facts per disk number, read once per disk.
    disk_infos: HashMap<u32, Arc<ot_model::device::DiskInfo>>,
    net: NetProbe,
    last_pass: Option<Instant>,
    pass: u64,
}

impl WindowsProbe {
    /// Construct the probe and discover CPU topology.
    ///
    /// # Errors
    /// Returns [`ProbeError::Os`] if topology discovery fails.
    pub fn new() -> Result<Self, ProbeError> {
        let mut si = SYSTEM_INFO::default();
        // SAFETY: `si` is a valid, writable SYSTEM_INFO.
        unsafe { GetSystemInfo(&raw mut si) };
        let logical_count = si.dwNumberOfProcessors;

        let topology = discover_topology(logical_count)?;

        Ok(Self {
            proc_buf: AlignedBuf::default(),
            core_buf: AlignedBuf::default(),
            tracked: HashMap::with_capacity(512),
            new_this_pass: Vec::new(),
            by_pid: HashMap::new(),
            by_pid_pass: 0,
            details: DetailProbe::new(),
            services: ServiceProbe::new(),
            tags: TagProbe::new(),
            can_sample: profile::can_sample(),
            no_services: Vec::new().into(),
            pending_details: Vec::new(),
            prev_cores: vec![CoreTimes::default(); logical_count as usize],
            topology,
            logical_count,
            hardware: hardware::read(logical_count),
            counters: PerfCounters::open(),
            performance: vec![None; logical_count as usize],
            disk_rates: Vec::new(),
            disk_infos: HashMap::new(),
            net: NetProbe::default(),
            last_pass: None,
            pass: 0,
        })
    }

    fn sample_cores(&mut self, out: &mut CpuSample) -> Result<(), ProbeError> {
        let n = self.logical_count as usize;
        let need = n * size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>();
        if self.core_buf.len_bytes() < need {
            self.core_buf.resize_bytes(need);
        }

        let mut returned = 0u32;
        // SAFETY: buffer is at least `need` bytes and 8-byte aligned; `returned` is a
        // valid out-pointer. The kernel writes at most `need` bytes.
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessorPerformanceInformation,
                self.core_buf.as_mut_ptr().cast(),
                need as u32,
                &raw mut returned,
            )
        };
        if status != STATUS_SUCCESS {
            return Err(ProbeError::os(
                "SystemProcessorPerformanceInformation",
                nt_error(status),
            ));
        }
        let count =
            (returned as usize / size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>()).min(n);

        // SAFETY: the kernel filled `count` contiguous, aligned structures.
        let cores = unsafe {
            std::slice::from_raw_parts(
                self.core_buf
                    .as_ptr()
                    .cast::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>(),
                count,
            )
        };

        out.cores.clear();
        out.cores.reserve(count);
        let mut total = 0.0f32;

        for (i, c) in cores.iter().enumerate() {
            let now = CoreTimes {
                idle: c.IdleTime as u64,
                kernel: c.KernelTime as u64,
                user: c.UserTime as u64,
            };
            let prev = self.prev_cores[i];
            self.prev_cores[i] = now;

            // KernelTime includes IdleTime, so total = kernel + user and busy = total - idle.
            let d_total = now.kernel.wrapping_sub(prev.kernel) + now.user.wrapping_sub(prev.user);
            let d_idle = now.idle.wrapping_sub(prev.idle);
            let usage = if self.last_pass.is_none() || d_total == 0 {
                Percent::ZERO
            } else {
                let busy = d_total.saturating_sub(d_idle) as f32 / d_total as f32;
                Percent::from_ratio(busy).clamped(100.0)
            };
            total += usage.get();

            let (physical, kind) = self
                .topology
                .get(i)
                .copied()
                .unwrap_or((i as u32, CoreKind::Unknown));
            // The clock is the base clock scaled by the processor's performance
            // counter, as Task Manager computes "Speed".
            let frequency = self
                .performance
                .get(i)
                .copied()
                .flatten()
                .zip(self.hardware.base_frequency)
                .map(|(pct, base)| Hertz((base.0 as f64 * pct / 100.0) as u64));
            out.cores.push(LogicalCore {
                index: i as u32,
                physical,
                kind,
                usage,
                frequency,
            });
        }

        out.total = if count == 0 {
            Percent::ZERO
        } else {
            Percent(total / count as f32)
        };
        out.package_power = None;
        out.hotspot_celsius = None;
        Ok(())
    }

    // One pass over the kernel's process list, read top to bottom: counters,
    // identity, details, services, threads. Splitting it would scatter the
    // per-entry bookkeeping across helpers that all need the same locals.
    #[allow(clippy::too_many_lines)]
    fn sample_processes(
        &mut self,
        out: &mut Vec<ProcessSample>,
        threads_out: &mut Vec<ThreadSample>,
        wall_100ns: u64,
    ) -> Result<(), ProbeError> {
        let len = query_growing(
            SystemProcessInformation,
            &mut self.proc_buf,
            "SystemProcessInformation",
        )?;
        self.pass += 1;
        let pass = self.pass;
        let max_cpu = self.logical_count as f32 * 100.0;
        let first_pass = self.last_pass.is_none();
        let deadline = Instant::now() + DETAIL_BUDGET;

        out.clear();
        threads_out.clear();
        self.new_this_pass.clear();
        self.services.refresh();

        let base = self.proc_buf.as_ptr();
        let mut offset = 0usize;
        loop {
            if offset + size_of::<SystemProcessInformation>() > len {
                break;
            }
            // SAFETY: `offset` is within the `len` bytes the kernel wrote, the buffer is
            // 8-byte aligned and the kernel aligns each entry to 8 as well. The reference
            // does not outlive this loop iteration.
            let p = unsafe { &*base.byte_add(offset).cast::<SystemProcessInformation>() };

            let pid = p.UniqueProcessId.0 as usize as u32;
            let key = ProcessKey::new(pid, p.CreateTime as u64);
            let now = Counters {
                cpu_100ns: (p.KernelTime as u64).wrapping_add(p.UserTime as u64),
                read_bytes: p.ReadTransferCount as u64,
                write_bytes: p.WriteTransferCount as u64,
            };

            let details = &mut self.details;
            let pending = &mut self.pending_details;
            let no_services = &self.no_services;
            let entry = self.tracked.entry(key).or_insert_with(|| {
                let mut statics = build_statics(p, key);
                // Details at first sight while the budget lasts; the rest queue up.
                if Instant::now() < deadline {
                    details.query(pid).apply(&mut statics);
                } else {
                    pending.push(key);
                }
                Tracked {
                    statics: Arc::new(statics),
                    prev: now,
                    last_seen: pass,
                    services: Arc::clone(no_services),
                    threads: HashMap::new(),
                    tag_handle: None,
                    tag_tried: false,
                }
            });

            // Services hosted here, republished by pointer while unchanged.
            match self.services.services_of(pid) {
                Some(list) if entry.services[..] != *list => entry.services = Arc::from(list),
                None if !entry.services.is_empty() => {
                    entry.services = Arc::clone(no_services);
                }
                _ => {}
            }

            // Thread rows follow the process entry in the same buffer.
            let thread_first = threads_out.len() as u32;
            let n_threads = p.NumberOfThreads as usize;
            let t_off = offset + size_of::<SystemProcessInformation>();
            let t_bytes = n_threads * size_of::<SystemThreadInformation>();
            let fits_entry = p.NextEntryOffset == 0
                || size_of::<SystemProcessInformation>() + t_bytes <= p.NextEntryOffset as usize;
            // PID 0 is the idle accounting, one entry per core all with TID 0: not
            // threads in any useful sense, and they would collide by id.
            if pid != 0
                && t_off + t_bytes <= len
                && fits_entry
                && threads_out.len() + n_threads <= MAX_THREAD_ROWS
            {
                // SAFETY: `n_threads` structures lie within the bytes the kernel
                // wrote, at an 8-byte-aligned offset, and the slice does not outlive
                // this iteration.
                let ts = unsafe {
                    std::slice::from_raw_parts(
                        base.byte_add(t_off).cast::<SystemThreadInformation>(),
                        n_threads,
                    )
                };
                sample_threads(
                    entry,
                    pid,
                    ts,
                    ThreadPass {
                        pass,
                        wall_100ns,
                        rates: !first_pass && wall_100ns != 0,
                        max_cpu,
                        deadline,
                    },
                    self.tags.as_mut(),
                    threads_out,
                );
            }
            let thread_rows = threads_out.len() as u32 - thread_first;
            // A freshly inserted entry already carries this pass number.
            let seen_before = entry.last_seen != pass;
            let prev = if seen_before { entry.prev } else { now };
            entry.prev = now;
            entry.last_seen = pass;
            if !seen_before {
                self.new_this_pass.push(out.len() as u32);
            }

            let (cpu, disk_read, disk_write) = if first_pass || !seen_before || wall_100ns == 0 {
                (Percent::ZERO, Bytes::ZERO, Bytes::ZERO)
            } else {
                let d_cpu = now.cpu_100ns.wrapping_sub(prev.cpu_100ns);
                let pct = d_cpu as f64 / wall_100ns as f64 * 100.0;
                (
                    Percent(pct as f32).clamped(max_cpu),
                    Bytes(now.read_bytes.wrapping_sub(prev.read_bytes)),
                    Bytes(now.write_bytes.wrapping_sub(prev.write_bytes)),
                )
            };

            out.push(ProcessSample {
                statics: Arc::clone(&entry.statics),
                cpu,
                // Kernel plus user time, in 100 ns units.
                cpu_time: Duration::from_nanos(now.cpu_100ns.saturating_mul(100)),
                cycles: p.CycleTime,
                working_set: Bytes(p.WorkingSetSize as u64),
                private_bytes: Bytes(p.PrivatePageCount as u64),
                disk_read,
                disk_write,
                net_rx: Bytes::ZERO,
                net_tx: Bytes::ZERO,
                threads: p.NumberOfThreads,
                handles: p.HandleCount,
                power: None,
                gpu: None,
                suspended: false,
                efficiency_mode: None,
                window: None,
                kind: ProcessKind::Background,
                priority: Priority::from_base(p.BasePriority),
                page_faults: p.PageFaultCount,
                peak_working_set: Bytes(p.PeakWorkingSetSize as u64),
                virtual_size: Bytes(p.VirtualSize as u64),
                paged_pool: Bytes(p.QuotaPagedPoolUsage as u64),
                nonpaged_pool: Bytes(p.QuotaNonPagedPoolUsage as u64),
                io: IoCounters {
                    reads: p.ReadOperationCount as u64,
                    writes: p.WriteOperationCount as u64,
                    other: p.OtherOperationCount as u64,
                    read_bytes: Bytes(p.ReadTransferCount as u64),
                    write_bytes: Bytes(p.WriteTransferCount as u64),
                    other_bytes: Bytes(p.OtherTransferCount as u64),
                },
                gpu_engine: None,
                services: Arc::clone(&entry.services),
                thread_first,
                thread_rows,
            });

            if p.NextEntryOffset == 0 {
                break;
            }
            offset += p.NextEntryOffset as usize;
        }

        self.resolve_parents(out);
        self.collect_pending_details(out, deadline);

        // Reap processes that were not in this pass. Their statics Arcs may still be
        // held by history buffers upstream; that is fine and intended.
        let tags = &mut self.tags;
        self.tracked.retain(|k, t| {
            let keep = t.last_seen == pass;
            if !keep {
                if let Some(tags) = tags {
                    tags.forget(k.pid);
                }
            }
            keep
        });
        Ok(())
    }

    /// Turn this pass's disk counters into samples, reading each disk's facts the
    /// first time it is seen (or when its volumes change).
    fn sample_disks(&mut self, out: &mut Vec<ot_model::device::DiskSample>) {
        out.clear();
        for r in &self.disk_rates {
            let Some(idle) = r.idle_pct else {
                continue;
            };
            let name = storage::display_name(r.number, &r.letters);
            let info = self
                .disk_infos
                .entry(r.number)
                .and_modify(|i| {
                    if i.name != name {
                        *i = Arc::new(storage::disk_info(r.number, &r.letters));
                    }
                })
                .or_insert_with(|| Arc::new(storage::disk_info(r.number, &r.letters)))
                .clone();
            let per_sec = |v: Option<f64>| Bytes(v.unwrap_or(0.0).max(0.0) as u64);
            out.push(ot_model::device::DiskSample {
                info,
                active: Percent((100.0 - idle).clamp(0.0, 100.0) as f32),
                read_per_sec: per_sec(r.read_per_sec),
                write_per_sec: per_sec(r.write_per_sec),
                response_ms: r.sec_per_transfer.map(|s| (s * 1000.0) as f32),
            });
        }
        let present: Vec<u32> = out.iter().map(|d| d.info.number).collect();
        self.disk_infos.retain(|n, _| present.contains(n));
    }

    /// Refill `by_pid` for this pass's output. PIDs are unique within one pass.
    fn index_by_pid(&mut self, out: &[ProcessSample]) {
        if self.by_pid_pass == self.pass {
            return;
        }
        self.by_pid_pass = self.pass;
        self.by_pid.clear();
        for (i, p) in out.iter().enumerate() {
            self.by_pid.insert(p.key().pid, i as u32);
        }
    }

    /// Collect details for processes that were queued when an earlier pass ran out
    /// of budget, until this pass's budget is spent too. Both the tracked entry and
    /// this pass's output get the new statics, so the UI sees them now.
    fn collect_pending_details(&mut self, out: &mut [ProcessSample], deadline: Instant) {
        if self.pending_details.is_empty() {
            return;
        }
        self.index_by_pid(out);
        let mut done = 0u32;
        while Instant::now() < deadline {
            let Some(key) = self.pending_details.pop() else {
                break;
            };
            // Exited before its turn came: nothing to learn.
            let Some(t) = self.tracked.get_mut(&key) else {
                continue;
            };
            let mut statics = (*t.statics).clone();
            self.details.query(key.pid).apply(&mut statics);
            let statics = Arc::new(statics);
            t.statics = Arc::clone(&statics);
            if let Some(&i) = self.by_pid.get(&key.pid) {
                if out[i as usize].key() == key {
                    out[i as usize].statics = statics;
                }
            }
            done += 1;
        }
        if done > 0 {
            tracing::debug!(
                done,
                left = self.pending_details.len(),
                "process details collected"
            );
        }
    }

    /// Turn the PID-only parent hint of every process first seen this pass into a
    /// real identity, or drop it.
    ///
    /// The kernel reports only the parent's PID, and PIDs are recycled. The process
    /// currently holding that PID is the real parent only if it was created no later
    /// than the child; otherwise the parent exited, a stranger inherited its PID, and
    /// the child is a root. Runs once per process lifetime, so steady state is free.
    fn resolve_parents(&mut self, out: &mut [ProcessSample]) {
        if self.new_this_pass.is_empty() {
            return;
        }
        self.index_by_pid(out);
        for &i in &self.new_this_pass {
            let child = &out[i as usize];
            let child_key = child.key();
            let Some(hint) = child.statics.parent else {
                continue;
            };
            // `birth` is `CreateTime` on Windows. Ordering it is meaningful here, in
            // the layer that defined it; everything above treats it as opaque.
            let resolved = self
                .by_pid
                .get(&hint.pid)
                .map(|&j| out[j as usize].key())
                .filter(|parent| parent.birth <= child_key.birth);
            if resolved == Some(hint) {
                continue;
            }
            let statics = Arc::new(ProcessStatic {
                parent: resolved,
                ..(*child.statics).clone()
            });
            if let Some(t) = self.tracked.get_mut(&child_key) {
                t.statics = Arc::clone(&statics);
            }
            out[i as usize].statics = statics;
        }
    }
}

impl SystemProbe for WindowsProbe {
    fn hardware(&self) -> ot_model::hardware::Hardware {
        self.hardware.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            per_process_cpu: true,
            per_process_disk: true,
            per_process_network: false,
            per_process_gpu: false,
            per_process_power: false,
            core_frequency: self.hardware.base_frequency.is_some()
                && self.counters.as_ref().is_some_and(PerfCounters::has_clock),
            package_power: false,
            thermals: false,
            hybrid_core_kinds: self.topology.iter().any(|(_, k)| *k != CoreKind::Unknown),
            threads: true,
            services: self.services.available(),
            service_tags: self.tags.is_some(),
            cpu_sampling: self.can_sample,
            gpu: false,
            sessions: false,
            service_list: false,
            elevated: access::is_elevated(),
        }
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        let now = Instant::now();
        let wall_100ns = self
            .last_pass
            .map_or(0, |prev| (now.duration_since(prev).as_nanos() / 100) as u64);

        // Counters first: the core loop reads this pass's clock from them.
        self.performance.fill(None);
        let collected = self.counters.as_mut().is_some_and(PerfCounters::collect);
        if collected {
            if let Some(c) = self.counters.as_mut() {
                c.processor_performance(&mut self.performance);
            }
        }

        self.sample_cores(&mut out.cpu)?;
        self.sample_processes(&mut out.processes, &mut out.threads, wall_100ns)?;
        out.memory = sample_memory()?;
        if let Some(c) = self.counters.as_mut().filter(|_| collected) {
            let lists = c.memory();
            out.memory.modified = lists.modified;
            out.memory.standby = lists.standby;
            out.memory.free = lists.free;
            c.disks(&mut self.disk_rates);
            self.sample_disks(&mut out.disks);
        }
        self.net.sample(&mut out.adapters);

        self.last_pass = Some(now);
        Ok(())
    }
}

/// Pass-wide constants for thread sampling.
#[derive(Debug, Clone, Copy)]
struct ThreadPass {
    pass: u64,
    wall_100ns: u64,
    /// Whether rates can be computed this pass (not the first, interval known).
    rates: bool,
    max_cpu: f32,
    /// Service tag reads stop once this passes; the rest wait for the next pass.
    deadline: Instant,
}

/// Difference each thread's CPU time against the previous pass, read service tags
/// for a service host when possible, and append the rows.
///
/// Thread identity is `(tid, CreateTime)`: a TID recycled within the same process
/// starts over rather than inheriting the old thread's counters or tag.
fn sample_threads(
    entry: &mut Tracked,
    pid: u32,
    ts: &[SystemThreadInformation],
    pp: ThreadPass,
    mut tags: Option<&mut TagProbe>,
    out: &mut Vec<ThreadSample>,
) {
    // Tags are only meaningful in a service host, and only readable when elevated.
    let want_tags = !entry.services.is_empty() && tags.is_some();
    if want_tags && entry.tag_handle.is_none() && !entry.tag_tried {
        entry.tag_tried = true;
        entry.tag_handle = TagProbe::open(pid);
    }

    for t in ts {
        let tid = t.ClientId.UniqueThread.0 as usize as u32;
        let birth = t.CreateTime as u64;
        let cpu_now = (t.KernelTime as u64).wrapping_add(t.UserTime as u64);
        let tp = entry.threads.entry(tid).or_insert(ThreadPrev {
            birth,
            cpu_100ns: cpu_now,
            last_seen: 0,
            tag: TagState::Unresolved,
        });
        if tp.birth != birth {
            *tp = ThreadPrev {
                birth,
                cpu_100ns: cpu_now,
                last_seen: 0,
                tag: TagState::Unresolved,
            };
        }
        let continuous = tp.last_seen + 1 == pp.pass;
        let prev = tp.cpu_100ns;
        tp.cpu_100ns = cpu_now;
        tp.last_seen = pp.pass;

        let cpu = if continuous && pp.rates {
            let d = cpu_now.wrapping_sub(prev);
            Percent((d as f64 / pp.wall_100ns as f64 * 100.0) as f32).clamped(pp.max_cpu)
        } else {
            Percent::ZERO
        };

        if want_tags && tp.tag == TagState::Unresolved && Instant::now() < pp.deadline {
            if let Some(h) = &entry.tag_handle {
                tp.tag = match TagProbe::thread_tag(h.0, tid) {
                    Some(tag) => TagState::Tag(tag),
                    None => TagState::Failed,
                };
            }
        }
        let service = match tp.tag {
            TagState::Tag(0) if want_tags => ServiceTag::None,
            TagState::Tag(tag) if want_tags => tags
                .as_mut()
                .and_then(|t| t.name(pid, tag))
                .and_then(|name| entry.services.iter().position(|s| *s.name == *name))
                .map_or(ServiceTag::Unknown, |i| ServiceTag::Service(i as u16)),
            _ => ServiceTag::Unknown,
        };

        let state = match t.ThreadState {
            1 => ThreadState::Ready,
            2 => ThreadState::Running,
            5 => ThreadState::Waiting,
            _ => ThreadState::Other,
        };
        let started_unix_ms =
            (t.CreateTime > 0).then(|| (t.CreateTime - FILETIME_UNIX_OFFSET_100NS) / 10_000);
        out.push(ThreadSample {
            tid,
            birth,
            cpu,
            state,
            wait_reason: WaitReason(t.WaitReason.min(255) as u8),
            service,
            started_unix_ms,
        });
    }
    entry.threads.retain(|_, t| t.last_seen == pp.pass);
}

/// Build the immutable half of a process record from its first observation.
fn build_statics(p: &SystemProcessInformation, key: ProcessKey) -> ProcessStatic {
    let mut name = unicode_to_string(&p.ImageName);
    if name.is_empty() {
        name = match key.pid {
            0 => "System Idle Process".to_owned(),
            4 => "System".to_owned(),
            _ => format!("<pid {}>", key.pid),
        };
    }

    let parent_pid = p.InheritedFromUniqueProcessId.0 as usize as u32;
    // Only the parent's PID is known here. `resolve_parents` replaces this hint with
    // the parent's full identity (or `None`) before the pass is published.
    let parent = (parent_pid != key.pid).then(|| ProcessKey::new(parent_pid, 0));

    let started_unix_ms =
        (p.CreateTime > 0).then(|| (p.CreateTime - FILETIME_UNIX_OFFSET_100NS) / 10_000);

    // Details (path, command line, user, integrity) are filled in by `DetailProbe`.
    ProcessStatic {
        key,
        parent,
        name,
        image_path: None,
        command_line: None,
        user: None,
        integrity: Integrity::Unknown,
        started_unix_ms,
        session_id: p.SessionId,
        architecture: ot_model::process::Architecture::Unknown,
        description: None,
        company: None,
    }
}

fn sample_memory() -> Result<MemorySample, ProbeError> {
    let mut ms = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `ms` is valid and `dwLength` is set, as the API requires.
    unsafe { GlobalMemoryStatusEx(&raw mut ms) }
        .map_err(|e| ProbeError::os("GlobalMemoryStatusEx", std::io::Error::other(e)))?;

    let mut pi = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: `pi` is valid and `cb` matches its size.
    unsafe { GetPerformanceInfo(&raw mut pi, pi.cb) }
        .map_err(|e| ProbeError::os("GetPerformanceInfo", std::io::Error::other(e)))?;

    let page = pi.PageSize as u64;
    Ok(MemorySample {
        total: Bytes(ms.ullTotalPhys),
        available: Bytes(ms.ullAvailPhys),
        cached: Bytes(pi.SystemCache as u64 * page),
        committed: Bytes(pi.CommitTotal as u64 * page),
        commit_limit: Bytes(pi.CommitLimit as u64 * page),
        compressed: None,
        swap_used: None,
        // The memory lists come from the performance counters, filled in by `sample`.
        modified: None,
        standby: None,
        free: None,
        paged_pool: Some(Bytes(pi.KernelPaged as u64 * page)),
        nonpaged_pool: Some(Bytes(pi.KernelNonpaged as u64 * page)),
    })
}

/// Map each logical processor to its physical core and efficiency class.
fn discover_topology(logical_count: u32) -> Result<Vec<(u32, CoreKind)>, ProbeError> {
    let mut len = 0u32;
    // SAFETY: a null buffer with zero length is the documented way to query size.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &raw mut len) };
    let mut buf = AlignedBuf::default();
    buf.resize_bytes(len as usize);

    // SAFETY: buffer is `len` bytes, 8-byte aligned, and `len` is the size the API asked for.
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr().cast()),
            &raw mut len,
        )
    }
    .map_err(|e| ProbeError::os("GetLogicalProcessorInformationEx", std::io::Error::other(e)))?;

    let mut topo = vec![(u32::MAX, CoreKind::Unknown); logical_count as usize];
    let mut classes: Vec<u8> = vec![0; logical_count as usize];
    let mut physical = 0u32;
    let base = buf.as_ptr();
    let mut offset = 0usize;
    while offset + size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= len as usize {
        // SAFETY: within the bytes the API wrote; entries are 8-byte aligned.
        let info = unsafe {
            &*base
                .byte_add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()
        };
        if info.Relationship == RelationProcessorCore {
            // SAFETY: Relationship says the Processor member of the union is active.
            let proc_rel = unsafe { info.Anonymous.Processor };
            // Only group 0 is represented in `topology` for now (see module docs).
            let mask = proc_rel.GroupMask[0].Mask;
            let group = proc_rel.GroupMask[0].Group;
            if group == 0 {
                for bit in 0..64u32 {
                    if mask & (1usize << bit) != 0 {
                        if let Some(slot) = topo.get_mut(bit as usize) {
                            slot.0 = physical;
                            classes[bit as usize] = proc_rel.EfficiencyClass;
                        }
                    }
                }
            }
            physical += 1;
        }
        if info.Size == 0 {
            break;
        }
        offset += info.Size as usize;
    }

    // EfficiencyClass is relative: a higher value is more performant. On a homogeneous
    // part every core reports 0 and the distinction is meaningless.
    let max_class = classes.iter().copied().max().unwrap_or(0);
    let min_class = classes.iter().copied().min().unwrap_or(0);
    if max_class != min_class {
        for (slot, class) in topo.iter_mut().zip(&classes) {
            slot.1 = if *class == max_class {
                CoreKind::Performance
            } else {
                CoreKind::Efficiency
            };
        }
    }
    for (i, slot) in topo.iter_mut().enumerate() {
        if slot.0 == u32::MAX {
            slot.0 = i as u32;
        }
    }
    Ok(topo)
}

/// Call `NtQuerySystemInformation`, growing `buf` until the result fits.
///
/// Returns the number of bytes written.
pub(super) fn query_growing(
    class: SYSTEM_INFORMATION_CLASS,
    buf: &mut AlignedBuf,
    context: &'static str,
) -> Result<usize, ProbeError> {
    if buf.len_bytes() == 0 {
        buf.resize_bytes(256 * 1024);
    }
    loop {
        let mut needed = 0u32;
        // SAFETY: buffer pointer and length agree; `needed` is a valid out-pointer.
        let status = unsafe {
            NtQuerySystemInformation(
                class,
                buf.as_mut_ptr().cast(),
                buf.len_bytes() as u32,
                &raw mut needed,
            )
        };
        if status == STATUS_SUCCESS {
            return Ok(needed as usize);
        }
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // The process table can grow between the size query and the fill, so add
            // headroom instead of sizing exactly and looping again.
            let target = (needed as usize).max(buf.len_bytes() * 2) + 64 * 1024;
            buf.resize_bytes(target);
            continue;
        }
        return Err(ProbeError::os(context, nt_error(status)));
    }
}

fn nt_error(status: NTSTATUS) -> std::io::Error {
    std::io::Error::other(format!("NTSTATUS 0x{:08X}", status.0 as u32))
}

pub(super) fn unicode_to_string(u: &UNICODE_STRING) -> String {
    if u.Buffer.is_null() || u.Length == 0 {
        return String::new();
    }
    // SAFETY: the kernel places the string inside the same buffer as the structure that
    // references it, and `Length` is in bytes.
    let units = unsafe { std::slice::from_raw_parts(u.Buffer.0, usize::from(u.Length) / 2) };
    String::from_utf16_lossy(units)
}

#[cfg(test)]
mod cost {
    //! What a sampling pass costs, piece by piece. Ignored by default because it
    //! measures the machine it runs on; run it when changing the probe:
    //!
    //! ```text
    //! cargo test -p ot-probe --release pass_costs -- --ignored --nocapture
    //! ```
    use super::*;

    fn time(label: &str, n: u32, mut f: impl FnMut()) {
        f();
        let mut worst = 0f64;
        let start = Instant::now();
        for _ in 0..n {
            let t = Instant::now();
            f();
            worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
        }
        let mean = start.elapsed().as_secs_f64() * 1e3 / f64::from(n);
        println!("{label:<28} mean {mean:>7.3} ms   worst {worst:>7.3} ms");
    }

    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn pass_costs() {
        let mut net = network::NetProbe::default();
        let mut adapters = Vec::new();
        time("network adapters", 30, || net.sample(&mut adapters));
        let mut c = PerfCounters::open().expect("PDH");
        time("performance counters", 30, || {
            c.collect();
        });
        let mut probe = WindowsProbe::new().expect("probe");
        let mut out = ProbeOutput::default();
        probe.sample(&mut out).expect("first pass");
        std::thread::sleep(Duration::from_millis(300));
        time("whole pass", 20, || {
            probe.sample(&mut out).expect("pass");
        });
        println!(
            "({} processes, {} threads, {} disks, {} adapters)",
            out.processes.len(),
            out.threads.len(),
            out.disks.len(),
            out.adapters.len()
        );
    }
}
