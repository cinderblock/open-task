//! On-demand CPU sampling of one process, and the clients its services served.
//!
//! Per-thread CPU and service tags say which thread and which service are busy.
//! They do not say what code is running. The only safe way to learn that from a
//! process we must not disturb is sampling: Event Tracing for Windows' kernel
//! profile source delivers, on every timer tick on every core, the instruction
//! pointer and thread id of whatever was running. Nothing is suspended, nothing is
//! attached, nothing is written into the target. This is what `xperf` and Windows
//! Performance Recorder do; we do less of it, for a few seconds, and reduce the
//! result to modules.
//!
//! For a service that acts as a broker, the busy code is the broker's own and the
//! interesting question is *who is asking*. Those services have trace providers of
//! their own; a small table maps service name to provider and to the payload field
//! that names the client. The same window enables the provider and histograms that
//! field.
//!
//! Both need `SeSystemProfilePrivilege` (the kernel's profile source) and the right to
//! control trace sessions (Administrators or Performance Log Users by default); see
//! [`can_sample`]. In practice that means running as administrator, elevated.
//! Two named sessions are used, never the shared "NT Kernel Logger", so another
//! tool's session is neither disturbed nor required.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ot_model::attribution::{Attribution, ClientReport, Share, ThreadShares};
use ot_model::ProcessKey;
use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Wdk::System::SystemInformation::SystemProcessInformation;
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_SUCCESS, ERROR_WMI_INSTANCE_NOT_FOUND, HMODULE, WIN32_ERROR,
};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW,
    TdhGetProperty, TdhGetPropertySize, CONTROLTRACE_HANDLE, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP,
    EVENT_TRACE_FLAG_PROFILE, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, EVENT_TRACE_SYSTEM_LOGGER_MODE, PROCESSTRACE_HANDLE,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME, PROPERTY_DATA_DESCRIPTOR,
    TRACE_LEVEL_VERBOSE, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetModuleBaseNameW, GetModuleInformation, LIST_MODULES_ALL, MODULEINFO,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use super::control::times_of;
use super::nt::{SystemProcessInformation, SystemThreadInformation};
use windows::Win32::Security::{WinBuiltinAdministratorsSid, WinBuiltinPerfLoggingUsersSid};

use super::access::{enable_privilege, holds_privilege, in_group};
use super::tags::OwnedHandle;
use super::{query_growing, AlignedBuf};
use crate::{CpuSampler, SampleError};

/// `PerfInfo` provider, opcode `SampleProf`: one event per profile interrupt.
const PERFINFO_GUID: GUID = GUID::from_u128(0xce1d_bfb4_137e_4da6_87b0_3f59_aa10_2cbc);
const OPCODE_SAMPLE_PROFILE: u8 = 46;

const CPU_SESSION: PCWSTR = windows::core::w!("open-task CPU sample");
const CLIENT_SESSION: PCWSTR = windows::core::w!("open-task service clients");

/// Services whose own trace provider names the client they were working for.
struct ProviderEntry {
    service: &'static str,
    provider: &'static str,
    guid: GUID,
    /// Payload fields that identify the client, joined into one label.
    fields: &'static [&'static str],
}

const PROVIDERS: &[ProviderEntry] = &[
    ProviderEntry {
        service: "BrokerInfrastructure",
        provider: "Microsoft-Windows-BrokerInfrastructure",
        guid: GUID::from_u128(0xe683_5967_e0d2_41fb_bcec_5838_7404_e25a),
        fields: &["PackageFullName", "TaskName"],
    },
    ProviderEntry {
        service: "PlugPlay",
        provider: "Microsoft-Windows-Kernel-PnP",
        guid: GUID::from_u128(0x9c20_5a39_1250_487d_abd7_e831_c629_0539),
        fields: &["DeviceInstanceId"],
    },
    ProviderEntry {
        service: "Schedule",
        provider: "Microsoft-Windows-TaskScheduler",
        guid: GUID::from_u128(0xde7b_24ea_73c8_4a09_985d_5bda_dcfa_9017),
        fields: &["TaskName"],
    },
    ProviderEntry {
        service: "wuauserv",
        provider: "Microsoft-Windows-WindowsUpdateClient",
        guid: GUID::from_u128(0x945a_8954_c147_4acd_923f_40c4_5405_a658),
        fields: &["updateTitle"],
    },
];

/// Whether this process can take a CPU sample: its token holds
/// `SeSystemProfilePrivilege`, which the kernel's profile source demands, and it
/// may control trace sessions, which by default Administrators and Performance Log
/// Users may. Asks about the privilege and the groups themselves rather than
/// "elevated": a basic-user token (`runas /trustlevel:0x20000` from an elevated
/// prompt) reports elevated and has neither. Changes nothing.
pub(super) fn can_sample() -> bool {
    holds_privilege(windows::core::w!("SeSystemProfilePrivilege"))
        && (in_group(WinBuiltinAdministratorsSid) || in_group(WinBuiltinPerfLoggingUsersSid))
}

/// The Windows [`CpuSampler`]. Stateless; each call is one complete sample.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsSampler;

impl CpuSampler for WindowsSampler {
    fn sample(
        &self,
        target: ProcessKey,
        services: &[String],
        duration: Duration,
    ) -> Result<Attribution, SampleError> {
        if !can_sample() || !enable_privilege(windows::core::w!("SeSystemProfilePrivilege")) {
            return Err(SampleError::NotPermitted);
        }
        // Opening a SYSTEM-owned target for its module list needs this too; without
        // it, sampling still works and modules of such a target read as unknown.
        let _ = enable_privilege(windows::core::w!("SeDebugPrivilege"));

        let process = open_target(target)?;
        let modules = modules_of(process.0);
        let before = thread_ids_of(target.pid);
        let mut notes = Vec::new();

        // The kernel profile session and its consumer.
        let cpu_session = Session::start_system(CPU_SESSION)?;
        let cpu = Box::new(Mutex::new(CpuCollector::default()));
        let cpu_consumer = Consumer::open(
            CPU_SESSION,
            std::ptr::from_ref::<Mutex<CpuCollector>>(&cpu)
                .cast_mut()
                .cast(),
            cpu_callback,
        )?;

        // The client session, if one of the target's services has a provider.
        let entry = services
            .iter()
            .find_map(|s| PROVIDERS.iter().find(|e| e.service.eq_ignore_ascii_case(s)));
        let mut clients: Option<(
            &ProviderEntry,
            Session,
            Box<Mutex<ClientCollector>>,
            Consumer,
        )> = None;
        if let Some(e) = entry {
            match Session::start_plain(CLIENT_SESSION).and_then(|s| {
                s.enable(&e.guid)?;
                Ok(s)
            }) {
                Ok(session) => {
                    let collector = Box::new(Mutex::new(ClientCollector::new(e)));
                    match Consumer::open(
                        CLIENT_SESSION,
                        std::ptr::from_ref::<Mutex<ClientCollector>>(&collector)
                            .cast_mut()
                            .cast(),
                        client_callback,
                    ) {
                        Ok(consumer) => clients = Some((e, session, collector, consumer)),
                        Err(err) => notes.push(format!("{}: {err}", e.provider)),
                    }
                }
                Err(err) => notes.push(format!("{}: {err}", e.provider)),
            }
        }

        let started = Instant::now();
        std::thread::sleep(duration);
        let elapsed = started.elapsed();

        // Stop the sessions; the consumers return once the buffers are drained.
        let lost = cpu_session.stop();
        drop(cpu_consumer);
        let after = thread_ids_of(target.pid);
        let cpu = cpu
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let client_report = clients.map(|(e, session, collector, consumer)| {
            let lost = session.stop();
            drop(consumer);
            let c = collector
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            c.report(e, lost)
        });

        if lost > 0 {
            notes.push(format!(
                "{lost} profile events were dropped; shares are approximate"
            ));
        }
        let mut tids: HashSet<u32> = before;
        tids.extend(after);
        let mut result = attribute(target, elapsed, &cpu, &tids, &modules);
        if result.samples == 0 && cpu.total > 0 {
            notes.push("the process used no CPU during the sample".to_owned());
        } else if cpu.total == 0 {
            notes.push("no profile events arrived; is another profiler running?".to_owned());
        }
        result.clients = client_report;
        result.notes = notes;
        Ok(result)
    }
}

/// Open the target for module enumeration, refusing a recycled PID.
fn open_target(target: ProcessKey) -> Result<OwnedHandle, SampleError> {
    // SAFETY: plain call; the handle is owned by the guard.
    let h = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            target.pid,
        )
    }
    .map_err(|e| SampleError::os("OpenProcess", e))?;
    let h = OwnedHandle(h);
    let (birth, exited) = times_of(h.0).map_err(|_| SampleError::Gone)?;
    if exited || birth != target.birth.0 {
        return Err(SampleError::Gone);
    }
    Ok(h)
}

/// One loaded module: address range and file name.
#[derive(Debug, Clone)]
struct Module {
    base: u64,
    end: u64,
    name: String,
}

fn modules_of(process: windows::Win32::Foundation::HANDLE) -> Vec<Module> {
    let mut handles: Vec<HMODULE> = vec![HMODULE::default(); 1024];
    let mut needed = 0u32;
    loop {
        let cb = (handles.len() * size_of::<HMODULE>()) as u32;
        // SAFETY: the array and `cb` agree; `needed` is a valid out-pointer.
        let r = unsafe {
            EnumProcessModulesEx(
                process,
                handles.as_mut_ptr(),
                cb,
                &raw mut needed,
                LIST_MODULES_ALL,
            )
        };
        if r.is_err() {
            return Vec::new();
        }
        if needed <= cb {
            handles.truncate(needed as usize / size_of::<HMODULE>());
            break;
        }
        handles.resize(
            needed as usize / size_of::<HMODULE>() + 64,
            HMODULE::default(),
        );
    }
    let mut out = Vec::with_capacity(handles.len());
    let mut name = [0u16; 260];
    for h in handles {
        let mut info = MODULEINFO::default();
        // SAFETY: valid out-struct of the size passed.
        if unsafe {
            GetModuleInformation(process, h, &raw mut info, size_of::<MODULEINFO>() as u32)
        }
        .is_err()
        {
            continue;
        }
        // SAFETY: the buffer is the length passed; the API NUL-terminates or truncates.
        let n = unsafe { GetModuleBaseNameW(process, Some(h), &mut name) } as usize;
        if n == 0 {
            continue;
        }
        let base = info.lpBaseOfDll as u64;
        out.push(Module {
            base,
            end: base + u64::from(info.SizeOfImage),
            name: String::from_utf16_lossy(&name[..n.min(name.len())]),
        });
    }
    out.sort_by_key(|m| m.base);
    out
}

/// The thread ids of `pid` right now.
fn thread_ids_of(pid: u32) -> HashSet<u32> {
    let mut buf = AlignedBuf::default();
    let mut out = HashSet::new();
    let Ok(len) = query_growing(
        SystemProcessInformation,
        &mut buf,
        "SystemProcessInformation",
    ) else {
        return out;
    };
    let base = buf.as_ptr();
    let mut offset = 0usize;
    while offset + size_of::<SystemProcessInformation>() <= len {
        // SAFETY: within the bytes the kernel wrote; entries are 8-byte aligned.
        let p = unsafe { &*base.byte_add(offset).cast::<SystemProcessInformation>() };
        if p.UniqueProcessId.0 as usize as u32 == pid {
            let n = p.NumberOfThreads as usize;
            let t_off = offset + size_of::<SystemProcessInformation>();
            if t_off + n * size_of::<SystemThreadInformation>() <= len {
                // SAFETY: `n` thread records follow the process entry, within `len`.
                let ts = unsafe {
                    std::slice::from_raw_parts(
                        base.byte_add(t_off).cast::<SystemThreadInformation>(),
                        n,
                    )
                };
                out.extend(ts.iter().map(|t| t.ClientId.UniqueThread.0 as usize as u32));
            }
            break;
        }
        if p.NextEntryOffset == 0 {
            break;
        }
        offset += p.NextEntryOffset as usize;
    }
    out
}

/// A controller-side trace session, stopped on drop.
struct Session {
    handle: CONTROLTRACE_HANDLE,
    name: PCWSTR,
    /// `EVENT_TRACE_PROPERTIES` followed by room for the logger name, 8-byte aligned.
    props: Vec<u64>,
    stopped: bool,
}

/// Bytes after the properties structure for the session name.
const NAME_ROOM: usize = 1024;

impl Session {
    fn properties(mode: u32, flags: u32) -> Vec<u64> {
        let total = size_of::<EVENT_TRACE_PROPERTIES>() + NAME_ROOM;
        let mut props = vec![0u64; total.div_ceil(8)];
        let p = props.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        // SAFETY: the buffer is at least one structure long and 8-byte aligned; the
        // remaining fields are meant to be zero.
        unsafe {
            (*p).Wnode.BufferSize = total as u32;
            (*p).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*p).Wnode.ClientContext = 1; // QPC timestamps
            (*p).BufferSize = 128; // KB per buffer
            (*p).MinimumBuffers = 32;
            (*p).MaximumBuffers = 128;
            (*p).LogFileMode = mode;
            (*p).EnableFlags = windows::Win32::System::Diagnostics::Etw::EVENT_TRACE_FLAG(flags);
            (*p).LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        }
        props
    }

    fn start(name: PCWSTR, mode: u32, flags: u32) -> Result<Self, WIN32_ERROR> {
        let mut props = Self::properties(mode, flags);
        let mut handle = CONTROLTRACE_HANDLE::default();
        // SAFETY: `props` is a valid properties buffer with room for the name.
        let mut r = unsafe { StartTraceW(&raw mut handle, name, props.as_mut_ptr().cast()) };
        if r == ERROR_ALREADY_EXISTS {
            // A session of ours left behind by a crash. Stop it and try once more.
            let mut stale = Self::properties(mode, flags);
            // SAFETY: as above; a null handle with a name addresses the session by name.
            unsafe {
                let _ = ControlTraceW(
                    CONTROLTRACE_HANDLE::default(),
                    name,
                    stale.as_mut_ptr().cast(),
                    EVENT_TRACE_CONTROL_STOP,
                );
            }
            props = Self::properties(mode, flags);
            // SAFETY: as above.
            r = unsafe { StartTraceW(&raw mut handle, name, props.as_mut_ptr().cast()) };
        }
        if r != ERROR_SUCCESS {
            return Err(r);
        }
        Ok(Self {
            handle,
            name,
            props,
            stopped: false,
        })
    }

    /// A private system logger with the profile source on.
    fn start_system(name: PCWSTR) -> Result<Self, SampleError> {
        Self::start(
            name,
            EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE,
            EVENT_TRACE_FLAG_PROFILE.0,
        )
        .map_err(|e| SampleError::os_code("StartTrace (profile)", e))
    }

    /// An ordinary real-time session for user-mode providers.
    fn start_plain(name: PCWSTR) -> Result<Self, SampleError> {
        Self::start(name, EVENT_TRACE_REAL_TIME_MODE, 0)
            .map_err(|e| SampleError::os_code("StartTrace (clients)", e))
    }

    fn enable(&self, provider: &GUID) -> Result<(), SampleError> {
        // SAFETY: the handle is a live session; the GUID outlives the call.
        let r = unsafe {
            EnableTraceEx2(
                self.handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                TRACE_LEVEL_VERBOSE as u8,
                u64::MAX,
                0,
                0,
                None,
            )
        };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(SampleError::os_code("EnableTraceEx2", r))
        }
    }

    /// Stop the session and return how many events it lost.
    fn stop(mut self) -> u32 {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> u32 {
        if self.stopped {
            return 0;
        }
        self.stopped = true;
        // SAFETY: the properties buffer is valid; STOP fills it with final counters.
        let r = unsafe {
            ControlTraceW(
                self.handle,
                self.name,
                self.props.as_mut_ptr().cast(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        if r != ERROR_SUCCESS && r != ERROR_WMI_INSTANCE_NOT_FOUND {
            tracing::warn!(code = r.0, "ControlTrace stop failed");
        }
        // SAFETY: the buffer still holds a properties structure.
        let p = unsafe { &*self.props.as_ptr().cast::<EVENT_TRACE_PROPERTIES>() };
        p.EventsLost + p.RealTimeBuffersLost
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// A real-time consumer thread. Dropping it waits for the session to end, so the
/// controller must stop the session first.
struct Consumer {
    handle: PROCESSTRACE_HANDLE,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: a trace handle is a process-wide token; it is closed on the thread that
// owns this value after the processing thread has finished with it.
unsafe impl Send for Consumer {}

impl Consumer {
    fn open(
        name: PCWSTR,
        context: *mut c_void,
        callback: unsafe extern "system" fn(*mut EVENT_RECORD),
    ) -> Result<Self, SampleError> {
        // SAFETY: an all-zero logfile structure is the documented starting point.
        let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        logfile.LoggerName = PWSTR(name.as_ptr().cast_mut());
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(callback);
        logfile.Context = context;
        // SAFETY: `logfile` is fully initialized for a real-time session by name.
        let handle = unsafe { OpenTraceW(&raw mut logfile) };
        if handle.Value == u64::MAX {
            return Err(SampleError::os_code("OpenTrace", unsafe {
                windows::Win32::Foundation::GetLastError()
            }));
        }
        let h = handle.Value;
        let thread = std::thread::Builder::new()
            .name("ot-etw-consumer".into())
            .spawn(move || {
                // SAFETY: the handle is open; ProcessTrace blocks until the session
                // is stopped by the controller, then returns.
                let r = unsafe { ProcessTrace(&[PROCESSTRACE_HANDLE { Value: h }], None, None) };
                if r != ERROR_SUCCESS {
                    tracing::debug!(code = r.0, "ProcessTrace returned");
                }
            })
            .map_err(|e| SampleError::Os {
                context: "spawn consumer",
                source: e,
            })?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // SAFETY: the processing thread has returned; the handle is closed once.
        unsafe {
            let _ = CloseTrace(self.handle);
        }
    }
}

/// Profile samples: `(thread id, instruction pointer)`.
#[derive(Debug, Default)]
struct CpuCollector {
    samples: Vec<(u32, u64)>,
    total: u64,
}

unsafe extern "system" fn cpu_callback(rec: *mut EVENT_RECORD) {
    // SAFETY: ETW hands a valid record; the context is the collector we registered,
    // alive until the consumer is dropped, which happens after the session stops.
    unsafe {
        let r = &*rec;
        if r.EventHeader.ProviderId != PERFINFO_GUID
            || r.EventHeader.EventDescriptor.Opcode != OPCODE_SAMPLE_PROFILE
        {
            return;
        }
        let data = r.UserData.cast::<u8>();
        let len = usize::from(r.UserDataLength);
        let is_32 = u32::from(r.EventHeader.Flags) & EVENT_HEADER_FLAG_32_BIT_HEADER != 0;
        let ptr_size = if is_32 { 4 } else { 8 };
        if data.is_null() || len < ptr_size + 4 {
            return;
        }
        let ip = if is_32 {
            u64::from(data.cast::<u32>().read_unaligned())
        } else {
            data.cast::<u64>().read_unaligned()
        };
        let tid = data.add(ptr_size).cast::<u32>().read_unaligned();
        let collector = &*r.UserContext.cast::<Mutex<CpuCollector>>();
        if let Ok(mut c) = collector.lock() {
            c.total += 1;
            c.samples.push((tid, ip));
        }
    }
}

/// Events from a broker's provider, bucketed by the client fields.
#[derive(Debug)]
struct ClientCollector {
    guid: GUID,
    /// Field names as NUL-terminated UTF-16, for TDH.
    fields: Vec<Vec<u16>>,
    total: u32,
    by_event: HashMap<u16, u32>,
    buckets: HashMap<String, u32>,
    buf: Vec<u8>,
    label: String,
}

impl ClientCollector {
    fn new(e: &ProviderEntry) -> Self {
        Self {
            guid: e.guid,
            fields: e
                .fields
                .iter()
                .map(|f| f.encode_utf16().chain(std::iter::once(0)).collect())
                .collect(),
            total: 0,
            by_event: HashMap::new(),
            buckets: HashMap::new(),
            buf: Vec::with_capacity(1024),
            label: String::with_capacity(256),
        }
    }

    fn report(mut self, e: &ProviderEntry, lost: u32) -> ClientReport {
        let mut buckets: Vec<Share> = self
            .buckets
            .drain()
            .map(|(label, count)| Share { label, count })
            .collect();
        // Without a decodable field, show what there is: counts per event id.
        if buckets.is_empty() {
            buckets = self
                .by_event
                .drain()
                .map(|(id, count)| Share {
                    label: format!("event {id}"),
                    count,
                })
                .collect();
        }
        buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        buckets.truncate(20);
        ClientReport {
            service: e.service.to_owned(),
            provider: e.provider.to_owned(),
            field: e.fields.join(" \u{b7} "),
            events: self.total,
            lost,
            buckets,
        }
    }

    /// One string property of an event, by name.
    fn property(&mut self, rec: *const EVENT_RECORD, field: &[u16]) -> Option<usize> {
        let desc = [PROPERTY_DATA_DESCRIPTOR {
            PropertyName: field.as_ptr() as u64,
            ArrayIndex: u32::MAX,
            Reserved: 0,
        }];
        let mut size = 0u32;
        // SAFETY: the record is valid for the callback's duration; the descriptor
        // names a NUL-terminated field.
        if unsafe { TdhGetPropertySize(rec, None, &desc, &raw mut size) } != 0 || size == 0 {
            return None;
        }
        self.buf.resize(size as usize, 0);
        // SAFETY: the buffer is `size` bytes, as TDH asked for.
        if unsafe { TdhGetProperty(rec, None, &desc, &mut self.buf) } != 0 {
            return None;
        }
        Some(size as usize)
    }
}

unsafe extern "system" fn client_callback(rec: *mut EVENT_RECORD) {
    // SAFETY: as for `cpu_callback`.
    unsafe {
        let r = &*rec;
        let collector = &*r.UserContext.cast::<Mutex<ClientCollector>>();
        let Ok(mut c) = collector.lock() else {
            return;
        };
        if r.EventHeader.ProviderId != c.guid {
            return;
        }
        c.total += 1;
        *c.by_event
            .entry(r.EventHeader.EventDescriptor.Id)
            .or_insert(0) += 1;
        // Build the label from every field that decodes as a string.
        let fields = std::mem::take(&mut c.fields);
        c.label.clear();
        for f in &fields {
            if let Some(n) = c.property(rec, f) {
                let units: Vec<u16> = c.buf[..n]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .take_while(|&u| u != 0)
                    .collect();
                if !units.is_empty() {
                    if !c.label.is_empty() {
                        c.label.push_str(" \u{b7} ");
                    }
                    c.label.push_str(&String::from_utf16_lossy(&units));
                }
            }
        }
        c.fields = fields;
        if !c.label.is_empty() && c.buckets.len() < 10_000 {
            let label = c.label.clone();
            *c.buckets.entry(label).or_insert(0) += 1;
        }
    }
}

/// Reduce samples to modules, overall and per thread.
fn attribute(
    target: ProcessKey,
    duration: Duration,
    cpu: &CpuCollector,
    tids: &HashSet<u32>,
    modules: &[Module],
) -> Attribution {
    let name_of = |ip: u64| -> &str {
        if ip >= 0x8000_0000_0000_0000 {
            return "kernel";
        }
        match modules.binary_search_by(|m| {
            if ip < m.base {
                std::cmp::Ordering::Greater
            } else if ip >= m.end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(i) => &modules[i].name,
            Err(_) => "unknown",
        }
    };

    let mut by_module: HashMap<&str, u32> = HashMap::new();
    let mut by_thread: HashMap<u32, HashMap<&str, u32>> = HashMap::new();
    let mut samples = 0u32;
    for &(tid, ip) in &cpu.samples {
        if !tids.contains(&tid) {
            continue;
        }
        samples += 1;
        let m = name_of(ip);
        *by_module.entry(m).or_insert(0) += 1;
        *by_thread.entry(tid).or_default().entry(m).or_insert(0) += 1;
    }

    let shares = |map: HashMap<&str, u32>| -> Vec<Share> {
        let mut v: Vec<Share> = map
            .into_iter()
            .map(|(label, count)| Share {
                label: label.to_owned(),
                count,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        v
    };
    let mut threads: Vec<ThreadShares> = by_thread
        .into_iter()
        .map(|(tid, map)| ThreadShares {
            tid,
            samples: map.values().sum(),
            modules: shares(map),
        })
        .collect();
    threads.sort_by(|a, b| b.samples.cmp(&a.samples).then_with(|| a.tid.cmp(&b.tid)));

    Attribution {
        target,
        duration,
        samples,
        modules: shares(by_module),
        threads,
        clients: None,
        notes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_reduce_to_modules_per_thread_and_overall() {
        let modules = vec![
            Module {
                base: 0x1000,
                end: 0x2000,
                name: "a.dll".into(),
            },
            Module {
                base: 0x2000,
                end: 0x3000,
                name: "b.dll".into(),
            },
        ];
        let cpu = CpuCollector {
            samples: vec![
                (1, 0x1500),
                (1, 0x1600),
                (1, 0x2500),
                (2, 0x2500),
                (3, 0x2500), // not our thread
                (1, 0xFFFF_8000_0000_0000),
                (2, 0x9999),
            ],
            total: 7,
        };
        let tids: HashSet<u32> = [1, 2].into_iter().collect();
        let a = attribute(
            ProcessKey::new(7, 1),
            Duration::from_secs(1),
            &cpu,
            &tids,
            &modules,
        );
        assert_eq!(a.samples, 6);
        assert_eq!(a.modules[0].label, "a.dll");
        assert_eq!(a.modules[0].count, 2);
        assert_eq!(a.modules[1].label, "b.dll");
        assert_eq!(a.modules[1].count, 2);
        assert!(a
            .modules
            .iter()
            .any(|m| m.label == "kernel" && m.count == 1));
        assert!(a
            .modules
            .iter()
            .any(|m| m.label == "unknown" && m.count == 1));
        assert_eq!(a.threads[0].tid, 1);
        assert_eq!(a.threads[0].samples, 4);
        assert_eq!(a.threads[0].modules[0].label, "a.dll");
        assert_eq!(a.threads[1].tid, 2);
    }

    #[test]
    fn our_own_modules_and_threads_enumerate() {
        let me = ProcessKey::new(std::process::id(), 0);
        // `open_target` refuses birth 0, so open directly for this check.
        // SAFETY: opening ourselves always succeeds.
        let h = OwnedHandle(
            unsafe {
                OpenProcess(
                    PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
                    false,
                    std::process::id(),
                )
            }
            .unwrap(),
        );
        let mods = modules_of(h.0);
        assert!(mods
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case("ntdll.dll")));
        assert!(mods.windows(2).all(|w| w[0].base <= w[1].base));
        let tids = thread_ids_of(me.pid);
        // SAFETY: plain call.
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        assert!(tids.contains(&tid));
        assert!(WindowsSampler.sample(me, &[], Duration::ZERO).is_err());
    }

    #[test]
    fn without_permission_a_sample_is_refused_before_anything_starts() {
        if can_sample() {
            return;
        }
        let me = ProcessKey::new(std::process::id(), 1);
        assert!(matches!(
            WindowsSampler.sample(me, &[], Duration::from_millis(10)),
            Err(SampleError::NotPermitted)
        ));
    }

    #[test]
    fn a_short_sample_of_ourselves_runs_when_permitted() {
        if !can_sample() {
            return;
        }
        // Burn a little CPU on a helper thread so there is something to attribute.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let burner = std::thread::spawn(move || {
            let mut x = 0u64;
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            }
            x
        });
        let (birth, _) = times_of(
            // SAFETY: pseudo-handle for ourselves.
            unsafe { windows::Win32::System::Threading::GetCurrentProcess() },
        )
        .unwrap();
        let me = ProcessKey::new(std::process::id(), birth);
        let r = WindowsSampler.sample(
            me,
            &["BrokerInfrastructure".into()],
            Duration::from_millis(600),
        );
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = burner.join();
        let a = r.expect("sample");
        assert_eq!(a.target, me);
        assert!(a.duration >= Duration::from_millis(600));
        assert!(a.samples > 0, "no samples attributed: {a:?}");
        assert!(!a.modules.is_empty(), "no modules: {a:?}");
        assert!(!a.threads.is_empty(), "no threads: {a:?}");
        let c = a.clients.expect("client report for a known service");
        assert_eq!(c.service, "BrokerInfrastructure");
    }
}
