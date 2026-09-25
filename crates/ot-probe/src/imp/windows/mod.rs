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
//! # Known gaps (tracked in the plan)
//! - Only the first processor group (up to 64 logical processors) is sampled.
//! - Image path, command line, user and integrity are not yet collected; they need
//!   `OpenProcess` and are deferred to a lazy on-demand path.
//! - Per-core frequency is not reported. `CallNtPowerInformation` is known to be
//!   stale on modern Windows; the correct source is the PDH counter
//!   `\Processor Information(*)\% Processor Performance` scaled by base frequency.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;
use std::time::Instant;

use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::memory::MemorySample;
use ot_model::process::{Integrity, ProcessSample, ProcessStatic};
use ot_model::{Bytes, Percent, ProcessKey};

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

mod nt;
use nt::SystemProcessInformation;

/// Difference between the Windows FILETIME epoch (1601-01-01) and the Unix epoch, in
/// 100 ns units.
const FILETIME_UNIX_OFFSET_100NS: i64 = 116_444_736_000_000_000;

/// Raw monotonic counters from the previous pass, per process.
#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    /// Kernel + user time in 100 ns units.
    cpu_100ns: u64,
    read_bytes: u64,
    write_bytes: u64,
}

/// Everything we remember about a process between passes.
#[derive(Debug)]
struct Tracked {
    statics: Arc<ProcessStatic>,
    prev: Counters,
    /// Pass number this entry was last observed in, for reaping exited processes.
    last_seen: u64,
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
struct AlignedBuf(Vec<u64>);

impl AlignedBuf {
    fn len_bytes(&self) -> usize {
        self.0.len() * 8
    }

    fn resize_bytes(&mut self, bytes: usize) {
        self.0.resize(bytes.div_ceil(8), 0);
    }

    /// 8-byte-aligned base pointer. Callers use `byte_add` for byte offsets.
    fn as_mut_ptr(&mut self) -> *mut u64 {
        self.0.as_mut_ptr()
    }

    /// 8-byte-aligned base pointer. Callers use `byte_add` for byte offsets.
    fn as_ptr(&self) -> *const u64 {
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
    prev_cores: Vec<CoreTimes>,
    /// Physical core index and class for each logical processor, computed once.
    topology: Vec<(u32, CoreKind)>,
    logical_count: u32,
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
            prev_cores: vec![CoreTimes::default(); logical_count as usize],
            topology,
            logical_count,
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
            out.cores.push(LogicalCore {
                index: i as u32,
                physical,
                kind,
                usage,
                frequency: None,
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

    fn sample_processes(
        &mut self,
        out: &mut Vec<ProcessSample>,
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

        out.clear();

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

            let entry = self.tracked.entry(key).or_insert_with(|| Tracked {
                statics: Arc::new(build_statics(p, key)),
                prev: now,
                last_seen: pass,
            });
            let was_new = entry.last_seen != pass;
            let prev = if was_new { entry.prev } else { now };
            entry.prev = now;
            entry.last_seen = pass;

            let (cpu, disk_read, disk_write) = if first_pass || !was_new || wall_100ns == 0 {
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
            });

            if p.NextEntryOffset == 0 {
                break;
            }
            offset += p.NextEntryOffset as usize;
        }

        // Reap processes that were not in this pass. Their statics Arcs may still be
        // held by history buffers upstream; that is fine and intended.
        self.tracked.retain(|_, t| t.last_seen == pass);
        Ok(())
    }
}

impl SystemProbe for WindowsProbe {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            per_process_cpu: true,
            per_process_disk: true,
            per_process_network: false,
            per_process_gpu: false,
            per_process_power: false,
            core_frequency: false,
            package_power: false,
            thermals: false,
            hybrid_core_kinds: self.topology.iter().any(|(_, k)| *k != CoreKind::Unknown),
        }
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        let now = Instant::now();
        let wall_100ns = self
            .last_pass
            .map_or(0, |prev| (now.duration_since(prev).as_nanos() / 100) as u64);

        self.sample_cores(&mut out.cpu)?;
        self.sample_processes(&mut out.processes, wall_100ns)?;
        out.memory = sample_memory()?;

        self.last_pass = Some(now);
        Ok(())
    }
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
    // We only know the parent's PID here, not its birth stamp. The core resolves this
    // against the live table; a PID-only parent is a hint, not an identity.
    let parent = (parent_pid != key.pid).then(|| ProcessKey::new(parent_pid, 0));

    let started_unix_ms =
        (p.CreateTime > 0).then(|| (p.CreateTime - FILETIME_UNIX_OFFSET_100NS) / 10_000);

    ProcessStatic {
        key,
        parent,
        name,
        image_path: None,
        command_line: None,
        user: None,
        integrity: Integrity::Unknown,
        started_unix_ms,
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
fn query_growing(
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

fn unicode_to_string(u: &UNICODE_STRING) -> String {
    if u.Buffer.is_null() || u.Length == 0 {
        return String::new();
    }
    // SAFETY: the kernel places the string inside the same buffer as the structure that
    // references it, and `Length` is in bytes.
    let units = unsafe { std::slice::from_raw_parts(u.Buffer.0, usize::from(u.Length) / 2) };
    String::from_utf16_lossy(units)
}
