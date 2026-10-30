//! NVIDIA GPUs, through NVML, the library behind `nvidia-smi`.
//!
//! NVIDIA's driver publishes no DRM client stats, on its open kernel modules or its
//! proprietary ones: `/proc/<pid>/fdinfo` has only the generic lines, and compute
//! work does not go through `/dev/dri` at all. NVML is the one source, and it is
//! better in one way: it names other users' processes without root.
//!
//! `libnvidia-ml.so.1` comes with the driver. It is loaded at run time, so the
//! binary links nothing extra, and only when the kernel says an NVIDIA GPU is bound
//! to the driver, because it costs about 20 MB of memory. Only the functions below
//! are bound, each looked up on its own: a driver too old for one, or a device that
//! answers `NOT_SUPPORTED`, loses that reading and keeps the rest.
//!
//! What the readings mean, measured on an RTX 4090 with driver 595:
//!
//! The driver keeps 72 samples per device, one every 200 ms, each naming the process
//! active on each engine when it was taken. `nvmlDeviceGetProcessUtilization`
//! answers with one row per process that had any since a timestamp, averaged over
//! all the samples since then, so the rows add up to the engine's load, as Windows'
//! do. It wants room for all 72 whatever it returns. `NOT_FOUND` means nothing ran.
//! `nvmlDeviceGetUtilizationRates` is a short snapshot, which aliases bursty work
//! at a pass a second, so the 3D/Compute engine takes the mean of
//! `nvmlDeviceGetSamples` since the last pass, and the snapshot only without it. The
//! other engines use their snapshots, which cost 2 us against the 1.5 ms of a
//! sample query; the rows top them up.
//!
//! A pass here is about 3.9 ms and the same every pass: 1.5 ms each for the samples
//! and the rows, about 0.6 ms for the three process lists, microseconds for the
//! rest. Connecting is about 13 ms. A GPU that is doing nothing skips the rows. The
//! library itself adds about 21 MB of resident memory.
//!
//! `nvmlInit_v2` opens the device, whatever its documentation says, so it counts as
//! touching the GPU; see [`super::gpu`] for when that is left alone. Init is
//! reference-counted: each [`Device`] holds one, and dropping it closes the device
//! files once it is the last. NVML leaves one thread behind, blocked in `poll`.

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::time::{SystemTime, UNIX_EPOCH};

/// `nvmlReturn_t`.
type Return = c_uint;
const SUCCESS: Return = 0;
const ERROR_NOT_SUPPORTED: Return = 3;
const ERROR_NO_PERMISSION: Return = 4;
const ERROR_INSUFFICIENT_SIZE: Return = 7;
const ERROR_FUNCTION_NOT_FOUND: Return = 13;
const ERROR_ARGUMENT_VERSION_MISMATCH: Return = 25;
const ERROR_DEPRECATED: Return = 26;

/// `NVML_GPU_UTILIZATION_SAMPLES`.
const GPU_UTILIZATION_SAMPLES: c_uint = 1;
/// `NVML_VALUE_TYPE_UNSIGNED_INT`.
const VALUE_UNSIGNED_INT: c_uint = 1;
/// `NVML_VALUE_NOT_AVAILABLE` in a 64-bit field.
const NOT_AVAILABLE: u64 = u64::MAX;
/// `NVML_DEVICE_NAME_V2_BUFFER_SIZE`.
const NAME_SIZE: usize = 96;
/// Rows for process utilization: the driver wants room for its 72 samples.
const ROWS: usize = 100;
/// Whole-device samples kept for one query. More than the driver holds.
const VALUES: usize = 128;
/// Room for this many processes in the running lists to begin with.
const ENTRIES: usize = 32;

/// The engines of a [`Reading`], by the name [`super::gpu`] shows them under.
pub(super) const ENGINES: [&str; 5] = ["sm", "enc", "dec", "jpeg", "ofa"];
const SM: usize = 0;
const ENCODER: usize = 1;
const DECODER: usize = 2;
const JPEG: usize = 3;
const OFA: usize = 4;

type Handle = *mut c_void;
type Init = unsafe extern "C" fn() -> Return;
type HandleByPci = unsafe extern "C" fn(*const c_char, *mut Handle) -> Return;
type GetName = unsafe extern "C" fn(Handle, *mut c_char, c_uint) -> Return;
type GetMemory = unsafe extern "C" fn(Handle, *mut Memory) -> Return;
type GetMemoryV2 = unsafe extern "C" fn(Handle, *mut MemoryV2) -> Return;
type GetRates = unsafe extern "C" fn(Handle, *mut Rates) -> Return;
type GetSamples =
    unsafe extern "C" fn(Handle, c_uint, u64, *mut c_uint, *mut c_uint, *mut ValueSample) -> Return;
type GetEngine = unsafe extern "C" fn(Handle, *mut c_uint, *mut c_uint) -> Return;
type GetProcesses = unsafe extern "C" fn(Handle, *mut c_uint, *mut c_void) -> Return;
type GetProcessUse = unsafe extern "C" fn(Handle, *mut Row, *mut c_uint, u64) -> Return;

/// `nvmlMemory_t`.
#[repr(C)]
#[derive(Debug, Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlMemory_v2_t`. Its `used` leaves out what the driver reserves for itself,
/// as `nvidia-smi` does.
#[repr(C)]
#[derive(Debug, Default)]
struct MemoryV2 {
    version: c_uint,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}

/// `nvmlUtilization_t`.
#[repr(C)]
#[derive(Debug, Default)]
struct Rates {
    gpu: c_uint,
    memory: c_uint,
}

/// `nvmlSample_t`, for samples whose value is an `unsigned int`: the first member
/// of the union.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct ValueSample {
    time: u64,
    value: c_uint,
    rest: c_uint,
}

/// The start of `nvmlProcessInfo_t`, which every version of it shares.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ProcessEntry {
    pid: c_uint,
    used: u64,
}

/// `nvmlProcessUtilizationSample_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Row {
    pid: c_uint,
    time: u64,
    sm: c_uint,
    memory: c_uint,
    encoder: c_uint,
    decoder: c_uint,
}

/// NVML's functions. Copied into each device, which clears what it finds it cannot
/// use. Beyond the first three, a driver may lack any of them.
#[derive(Debug, Clone, Copy)]
struct Api {
    init: Init,
    shutdown: Init,
    handle_by_pci: HandleByPci,
    name: Option<GetName>,
    memory_v2: Option<GetMemoryV2>,
    memory: Option<GetMemory>,
    rates: Option<GetRates>,
    samples: Option<GetSamples>,
    encoder: Option<GetEngine>,
    decoder: Option<GetEngine>,
    jpeg: Option<GetEngine>,
    ofa: Option<GetEngine>,
    /// Compute, graphics, MPS; each with the size of its entries.
    processes: [Option<(GetProcesses, usize)>; 3],
    process_use: Option<GetProcessUse>,
}

/// A symbol as the function type `F`.
///
/// # Safety
/// `lib` is a live handle, and `F` is a function pointer of the symbol's signature.
unsafe fn symbol<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    // SAFETY: a live handle and a NUL-terminated name.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    // SAFETY: the caller says `F` is the function's type: a pointer, as `p` is.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

/// The newest of a running-process function's versions. Versions 2 and 3 share
/// their layout, which added two fields to the first's.
///
/// # Safety
/// `lib` is a live handle to NVML.
unsafe fn processes(lib: *mut c_void, names: [&CStr; 3]) -> Option<(GetProcesses, usize)> {
    let [v3, v2, v1] = names;
    // SAFETY: each is NVML's function of that name, of this signature.
    unsafe {
        symbol(lib, v3)
            .or_else(|| symbol(lib, v2))
            .map(|f| (f, 24))
            .or_else(|| symbol(lib, v1).map(|f| (f, 16)))
    }
}

impl Api {
    fn load() -> Option<Self> {
        // SAFETY: dlopen with a NUL-terminated name. The handle is never closed:
        // the functions stay in use for the life of the process.
        let lib = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return None;
        }
        // SAFETY: each symbol is NVML's function of that name, whose C signature
        // the field's type spells out.
        unsafe {
            Some(Self {
                init: symbol(lib, c"nvmlInit_v2")?,
                shutdown: symbol(lib, c"nvmlShutdown")?,
                handle_by_pci: symbol(lib, c"nvmlDeviceGetHandleByPciBusId_v2")?,
                name: symbol(lib, c"nvmlDeviceGetName"),
                memory_v2: symbol(lib, c"nvmlDeviceGetMemoryInfo_v2"),
                memory: symbol(lib, c"nvmlDeviceGetMemoryInfo"),
                rates: symbol(lib, c"nvmlDeviceGetUtilizationRates"),
                samples: symbol(lib, c"nvmlDeviceGetSamples"),
                encoder: symbol(lib, c"nvmlDeviceGetEncoderUtilization"),
                decoder: symbol(lib, c"nvmlDeviceGetDecoderUtilization"),
                jpeg: symbol(lib, c"nvmlDeviceGetJpgUtilization"),
                ofa: symbol(lib, c"nvmlDeviceGetOfaUtilization"),
                processes: [
                    processes(
                        lib,
                        [
                            c"nvmlDeviceGetComputeRunningProcesses_v3",
                            c"nvmlDeviceGetComputeRunningProcesses_v2",
                            c"nvmlDeviceGetComputeRunningProcesses",
                        ],
                    ),
                    processes(
                        lib,
                        [
                            c"nvmlDeviceGetGraphicsRunningProcesses_v3",
                            c"nvmlDeviceGetGraphicsRunningProcesses_v2",
                            c"nvmlDeviceGetGraphicsRunningProcesses",
                        ],
                    ),
                    processes(
                        lib,
                        [
                            c"nvmlDeviceGetMPSComputeRunningProcesses_v3",
                            c"nvmlDeviceGetMPSComputeRunningProcesses_v2",
                            c"nvmlDeviceGetMPSComputeRunningProcesses",
                        ],
                    ),
                ],
                process_use: symbol(lib, c"nvmlDeviceGetProcessUtilization"),
            })
        }
    }
}

/// Stop calling a function that has said it will never answer here.
fn settle<F>(call: &mut Option<F>, rc: Return) {
    if matches!(
        rc,
        ERROR_NOT_SUPPORTED
            | ERROR_NO_PERMISSION
            | ERROR_FUNCTION_NOT_FOUND
            | ERROR_ARGUMENT_VERSION_MISMATCH
            | ERROR_DEPRECATED
    ) {
        *call = None;
    }
}

/// Microseconds of wall clock, what NVML stamps its samples with.
fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// A percentage from NVML, whose values can be nonsense: over 100, or "not available".
fn percent(value: c_uint) -> Option<f64> {
    (value <= 100).then(|| f64::from(value))
}

/// A device's facts, as far as NVML gives them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Facts {
    pub name: Option<String>,
    pub memory_total: Option<u64>,
}

/// One process's load on the engines that NVML splits by process: the first three
/// of [`ENGINES`], in percent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessUse {
    pub pid: u32,
    pub shares: [u32; 3],
}

/// One device over one pass. Reused between passes.
#[derive(Debug, Default)]
pub(super) struct Reading {
    /// Each of [`ENGINES`]' load, in percent; `None` where nothing answered.
    pub engines: [Option<f64>; ENGINES.len()],
    pub memory_used: Option<u64>,
    /// Who has memory on the device: pid and bytes.
    pub memory: Vec<(u32, u64)>,
    pub uses: Vec<ProcessUse>,
}

impl Reading {
    fn clear(&mut self) {
        self.engines = [None; ENGINES.len()];
        self.memory_used = None;
        self.memory.clear();
        self.uses.clear();
    }

    /// Nothing is running on the device, as far as anything answered. A device that
    /// would not say is not known to be idle.
    pub(super) fn idle(&self) -> bool {
        self.engines[SM].is_some()
            && self.engines.iter().flatten().all(|&load| load == 0.0)
            && self.memory.is_empty()
            && self.uses.is_empty()
    }
}

/// A connected GPU.
pub(super) trait Device: std::fmt::Debug + Send {
    fn facts(&mut self) -> Facts;
    /// Refill `out` with what has happened since the last read.
    fn read(&mut self, out: &mut Reading);
}

/// What can connect to a GPU; tests put a fake in its place.
pub(super) trait Library: std::fmt::Debug + Send {
    /// Connect to the GPU in this PCI slot (`0000:01:00.0`). Dropping what comes back
    /// lets go of it.
    fn attach(&self, slot: &str) -> Option<Box<dyn Device>>;
}

/// The real library.
#[derive(Debug)]
struct Nvml(Api);

/// Load NVML. `None`, quietly, when the driver's library is not installed.
pub(super) fn load() -> Option<Box<dyn Library>> {
    Api::load().map(|api| Box::new(Nvml(api)) as Box<dyn Library>)
}

impl Library for Nvml {
    fn attach(&self, slot: &str) -> Option<Box<dyn Device>> {
        NvmlDevice::open(self.0, slot).map(|d| Box::new(d) as Box<dyn Device>)
    }
}

#[derive(Debug)]
struct NvmlDevice {
    api: Api,
    handle: Handle,
    /// The newest timestamp seen: samples up to here are counted.
    last_seen: u64,
    values: Vec<ValueSample>,
    rows: Vec<Row>,
    /// Running-list entries; `u64` for their alignment.
    entries: Vec<u64>,
}

// SAFETY: NVML's functions may be called from any thread, and the handle is a
// reference into the library. The device belongs to the probe, which one sampler
// thread owns.
unsafe impl Send for NvmlDevice {}

impl Drop for NvmlDevice {
    fn drop(&mut self) {
        // SAFETY: balances the nvmlInit_v2 in `open`.
        unsafe { (self.api.shutdown)() };
    }
}

impl NvmlDevice {
    fn open(api: Api, slot: &str) -> Option<Self> {
        let bus_id = CString::new(slot).ok()?;
        // SAFETY: no preconditions; a success is balanced by shutdown.
        let rc = unsafe { (api.init)() };
        if rc != SUCCESS {
            tracing::debug!(rc, "nvmlInit failed");
            return None;
        }
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: a NUL-terminated id and a writable out-pointer.
        let rc = unsafe { (api.handle_by_pci)(bus_id.as_ptr(), &raw mut handle) };
        if rc != SUCCESS {
            tracing::debug!(rc, slot, "NVML has no such device");
            // SAFETY: balances the init above; no device was made to do it.
            unsafe { (api.shutdown)() };
            return None;
        }
        Some(Self {
            api,
            handle,
            last_seen: 0,
            values: vec![ValueSample::default(); VALUES],
            rows: vec![Row::default(); ROWS],
            entries: vec![0; ENTRIES * 3],
        })
    }

    /// The memory on the device and how much is in use.
    fn memory(&mut self) -> Option<(u64, u64)> {
        if let Some(call) = self.api.memory_v2 {
            let mut m = MemoryV2 {
                version: size_of::<MemoryV2>() as c_uint | (2 << 24),
                ..MemoryV2::default()
            };
            // SAFETY: a live handle and a struct of the version it says it is.
            let rc = unsafe { call(self.handle, &raw mut m) };
            settle(&mut self.api.memory_v2, rc);
            if rc == SUCCESS {
                return Some((m.total, m.used));
            }
        }
        let call = self.api.memory?;
        let mut m = Memory::default();
        // SAFETY: a live handle and a writable struct.
        let rc = unsafe { call(self.handle, &raw mut m) };
        settle(&mut self.api.memory, rc);
        (rc == SUCCESS).then_some((m.total, m.used))
    }

    /// The 3D/Compute engine's mean load over the samples since `last`.
    fn samples(&mut self, last: u64, newest: &mut u64) -> Option<f64> {
        let call = self.api.samples?;
        let (mut kind, mut count) = (0, self.values.len() as c_uint);
        // SAFETY: a live handle, writable out-pointers, and room for `count` samples.
        let rc = unsafe {
            call(
                self.handle,
                GPU_UTILIZATION_SAMPLES,
                last,
                &raw mut kind,
                &raw mut count,
                self.values.as_mut_ptr(),
            )
        };
        settle(&mut self.api.samples, rc);
        if rc != SUCCESS || kind != VALUE_UNSIGNED_INT {
            return None;
        }
        let (mut sum, mut n) = (0.0, 0u32);
        for s in &self.values[..self.values.len().min(count as usize)] {
            if s.time > last {
                *newest = (*newest).max(s.time);
                if let Some(load) = percent(s.value) {
                    sum += load;
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / f64::from(n))
    }

    /// The 3D/Compute engine's load right now.
    fn snapshot(&mut self) -> Option<f64> {
        let call = self.api.rates?;
        let mut rates = Rates::default();
        // SAFETY: a live handle and a writable struct.
        let rc = unsafe { call(self.handle, &raw mut rates) };
        settle(&mut self.api.rates, rc);
        if rc == SUCCESS {
            percent(rates.gpu)
        } else {
            None
        }
    }

    /// Another engine's load right now.
    fn engine(&mut self, pick: fn(&mut Api) -> &mut Option<GetEngine>) -> Option<f64> {
        let call = pick(&mut self.api);
        let f = (*call)?;
        let (mut load, mut period) = (0, 0);
        // SAFETY: a live handle and writable out-pointers.
        let rc = unsafe { f(self.handle, &raw mut load, &raw mut period) };
        settle(call, rc);
        if rc == SUCCESS {
            percent(load)
        } else {
            None
        }
    }

    /// Who has memory on the device. A process in two lists has all of its memory
    /// in each.
    fn running(&mut self, out: &mut Reading) {
        for list in 0..self.api.processes.len() {
            let Some((call, size)) = self.api.processes[list] else {
                continue;
            };
            let room = self.entries.len() * 8 / size;
            let mut count = room as c_uint;
            let buffer = self.entries.as_mut_ptr().cast::<c_void>();
            // SAFETY: a live handle, and room for `count` entries of `size` bytes.
            let mut rc = unsafe { call(self.handle, &raw mut count, buffer) };
            if rc == ERROR_INSUFFICIENT_SIZE {
                // A few over, for what starts between the two calls.
                let want = count as usize + 8;
                self.entries.resize(want * size / 8, 0);
                count = want as c_uint;
                let buffer = self.entries.as_mut_ptr().cast::<c_void>();
                // SAFETY: as above, with the larger buffer.
                rc = unsafe { call(self.handle, &raw mut count, buffer) };
            }
            settle(&mut self.api.processes[list], rc);
            if rc != SUCCESS {
                continue;
            }
            let room = self.entries.len() * 8 / size;
            for i in 0..room.min(count as usize) {
                // SAFETY: entry `i` is in the buffer, `size` is a multiple of 8, and
                // every version starts this way.
                let e = unsafe {
                    self.entries
                        .as_ptr()
                        .add(i * size / 8)
                        .cast::<ProcessEntry>()
                        .read()
                };
                let used = if e.used == NOT_AVAILABLE { 0 } else { e.used };
                match out.memory.iter_mut().find(|m| m.0 == e.pid) {
                    Some(m) => m.1 = m.1.max(used),
                    None => out.memory.push((e.pid, used)),
                }
            }
        }
    }

    /// Who used the engines since `last`.
    fn process_use(&mut self, last: u64, newest: &mut u64, out: &mut Reading) {
        let Some(call) = self.api.process_use else {
            return;
        };
        let mut count = self.rows.len() as c_uint;
        // SAFETY: a live handle, and room for `count` rows.
        let mut rc = unsafe { call(self.handle, self.rows.as_mut_ptr(), &raw mut count, last) };
        if rc == ERROR_INSUFFICIENT_SIZE {
            self.rows
                .resize((count as usize).max(self.rows.len()), Row::default());
            count = self.rows.len() as c_uint;
            // SAFETY: as above, with the larger buffer.
            rc = unsafe { call(self.handle, self.rows.as_mut_ptr(), &raw mut count, last) };
        }
        settle(&mut self.api.process_use, rc);
        if rc != SUCCESS {
            // Mostly NOT_FOUND: nothing ran since `last`.
            return;
        }
        let share = |v| percent(v).map_or(0, |_| v);
        for r in &self.rows[..self.rows.len().min(count as usize)] {
            // The driver has been seen to hand back older samples than it was asked
            // for.
            if r.time > last {
                *newest = (*newest).max(r.time);
                out.uses.push(ProcessUse {
                    pid: r.pid,
                    shares: [share(r.sm), share(r.encoder), share(r.decoder)],
                });
            }
        }
    }
}

impl Device for NvmlDevice {
    fn facts(&mut self) -> Facts {
        let mut buf = [0 as c_char; NAME_SIZE];
        let name = self.api.name.and_then(|call| {
            // SAFETY: a live handle and a buffer of the length given.
            let rc = unsafe { call(self.handle, buf.as_mut_ptr(), NAME_SIZE as c_uint) };
            (rc == SUCCESS).then(|| {
                // SAFETY: on success NVML leaves a NUL-terminated string in `buf`,
                // which is zeroed to begin with and one char longer than it writes.
                let name = unsafe { CStr::from_ptr(buf.as_ptr()) };
                name.to_string_lossy().into_owned()
            })
        });
        Facts {
            name: name.filter(|n| !n.is_empty()),
            memory_total: self.memory().map(|m| m.0).filter(|&t| t > 0),
        }
    }

    fn read(&mut self, out: &mut Reading) {
        out.clear();
        // A clock set back would otherwise hide every sample until it caught up.
        let last = self.last_seen.min(unix_micros());
        let mut newest = last;
        out.memory_used = self.memory().map(|m| m.1);
        out.engines[SM] = self.samples(last, &mut newest).or_else(|| self.snapshot());
        out.engines[ENCODER] = self.engine(|a| &mut a.encoder);
        out.engines[DECODER] = self.engine(|a| &mut a.decoder);
        out.engines[JPEG] = self.engine(|a| &mut a.jpeg);
        out.engines[OFA] = self.engine(|a| &mut a.ofa);
        self.running(out);
        // A GPU doing nothing has no rows to give, and they are half of a pass.
        if out.engines.iter().any(|e| e.is_none_or(|load| load > 0.0)) {
            self.process_use(last, &mut newest, out);
        }
        self.last_seen = newest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// What the fake library answers, and what it was asked.
    #[derive(Default)]
    struct Fake {
        init: Return,
        handle: Return,
        shutdowns: u32,
        memory_v2: Return,
        memory_used: u64,
        rates: Return,
        snapshot: c_uint,
        samples: Return,
        values: Vec<ValueSample>,
        sample_lasts: Vec<u64>,
        encoder: Return,
        encoder_calls: u32,
        decoder: Return,
        /// Each list's answer and entries (pid, bytes).
        lists: [(Return, Vec<(u32, u64)>); 3],
        list_calls: u32,
        process_use: Return,
        rows: Vec<Row>,
        row_room: usize,
        row_calls: Vec<u64>,
    }

    thread_local! {
        static FAKE: RefCell<Fake> = RefCell::new(Fake {
            row_room: 72,
            ..Fake::default()
        });
    }

    fn fake<R>(f: impl FnOnce(&mut Fake) -> R) -> R {
        FAKE.with_borrow_mut(f)
    }

    unsafe extern "C" fn init() -> Return {
        fake(|f| f.init)
    }
    unsafe extern "C" fn shutdown() -> Return {
        fake(|f| f.shutdowns += 1);
        SUCCESS
    }
    unsafe extern "C" fn handle_by_pci(_: *const c_char, out: *mut Handle) -> Return {
        // SAFETY: the caller's out-pointer.
        unsafe { *out = std::ptr::dangling_mut() };
        fake(|f| f.handle)
    }
    unsafe extern "C" fn name(_: Handle, out: *mut c_char, _: c_uint) -> Return {
        let text = c"NVIDIA GeForce RTX 4090";
        // SAFETY: the caller's buffer is longer than this.
        unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.count_bytes() + 1) };
        SUCCESS
    }
    unsafe extern "C" fn memory_v2(_: Handle, out: *mut MemoryV2) -> Return {
        fake(|f| {
            // SAFETY: the caller's struct.
            let m = unsafe { &mut *out };
            assert_eq!(m.version, 0x0200_0028);
            m.total = 24 << 30;
            m.used = f.memory_used;
            f.memory_v2
        })
    }
    unsafe extern "C" fn memory_v1(_: Handle, out: *mut Memory) -> Return {
        // SAFETY: the caller's struct.
        let m = unsafe { &mut *out };
        m.total = 24 << 30;
        m.used = 7 << 30;
        SUCCESS
    }
    unsafe extern "C" fn rates(_: Handle, out: *mut Rates) -> Return {
        fake(|f| {
            // SAFETY: the caller's struct.
            unsafe { (*out).gpu = f.snapshot };
            f.rates
        })
    }
    unsafe extern "C" fn samples(
        _: Handle,
        _: c_uint,
        last: u64,
        kind: *mut c_uint,
        count: *mut c_uint,
        out: *mut ValueSample,
    ) -> Return {
        fake(|f| {
            f.sample_lasts.push(last);
            // SAFETY: the caller's out-pointers and buffer, which has room.
            unsafe {
                *kind = VALUE_UNSIGNED_INT;
                *count = f.values.len() as c_uint;
                std::ptr::copy_nonoverlapping(f.values.as_ptr(), out, f.values.len());
            }
            f.samples
        })
    }
    unsafe extern "C" fn encoder(_: Handle, load: *mut c_uint, _: *mut c_uint) -> Return {
        fake(|f| {
            f.encoder_calls += 1;
            // SAFETY: the caller's out-pointer.
            unsafe { *load = 60 };
            f.encoder
        })
    }
    unsafe extern "C" fn decoder(_: Handle, load: *mut c_uint, _: *mut c_uint) -> Return {
        // SAFETY: the caller's out-pointer.
        unsafe { *load = 50 };
        fake(|f| f.decoder)
    }
    unsafe extern "C" fn idle_engine(_: Handle, load: *mut c_uint, _: *mut c_uint) -> Return {
        // SAFETY: the caller's out-pointer.
        unsafe { *load = 0 };
        SUCCESS
    }

    /// A running list, in the layout of versions 2 and 3.
    fn list(which: usize, count: *mut c_uint, out: *mut c_void, size: usize) -> Return {
        fake(|f| {
            f.list_calls += 1;
            let (rc, entries) = &f.lists[which];
            // SAFETY: the caller's count, and room for it in the buffer.
            unsafe {
                let room = *count as usize;
                *count = entries.len() as c_uint;
                if room < entries.len() {
                    return ERROR_INSUFFICIENT_SIZE;
                }
                for (i, &(pid, used)) in entries.iter().enumerate() {
                    out.cast::<u64>()
                        .add(i * size / 8)
                        .cast::<ProcessEntry>()
                        .write(ProcessEntry { pid, used });
                }
            }
            *rc
        })
    }
    unsafe extern "C" fn compute(_: Handle, count: *mut c_uint, out: *mut c_void) -> Return {
        list(0, count, out, 24)
    }
    unsafe extern "C" fn graphics(_: Handle, count: *mut c_uint, out: *mut c_void) -> Return {
        list(1, count, out, 24)
    }
    unsafe extern "C" fn mps(_: Handle, count: *mut c_uint, out: *mut c_void) -> Return {
        list(2, count, out, 24)
    }
    unsafe extern "C" fn old_compute(_: Handle, count: *mut c_uint, out: *mut c_void) -> Return {
        list(0, count, out, 16)
    }
    unsafe extern "C" fn process_use(
        _: Handle,
        out: *mut Row,
        count: *mut c_uint,
        last: u64,
    ) -> Return {
        fake(|f| {
            f.row_calls.push(last);
            // SAFETY: the caller's count, and room for it in the buffer.
            unsafe {
                if (*count as usize) < f.row_room {
                    *count = f.row_room as c_uint;
                    return ERROR_INSUFFICIENT_SIZE;
                }
                *count = f.rows.len() as c_uint;
                std::ptr::copy_nonoverlapping(f.rows.as_ptr(), out, f.rows.len());
            }
            f.process_use
        })
    }

    fn api() -> Api {
        Api {
            init,
            shutdown,
            handle_by_pci,
            name: Some(name),
            memory_v2: Some(memory_v2),
            memory: Some(memory_v1),
            rates: Some(rates),
            samples: Some(samples),
            encoder: Some(encoder),
            decoder: Some(decoder),
            jpeg: Some(idle_engine),
            ofa: Some(idle_engine),
            processes: [Some((compute, 24)), Some((graphics, 24)), Some((mps, 24))],
            process_use: Some(process_use),
        }
    }

    fn device() -> NvmlDevice {
        NvmlDevice::open(api(), "0000:01:00.0").expect("a device")
    }

    fn value(time: u64, value: c_uint) -> ValueSample {
        ValueSample {
            time,
            value,
            rest: 0,
        }
    }

    fn row(pid: u32, time: u64, sm: c_uint, encoder: c_uint, decoder: c_uint) -> Row {
        Row {
            pid,
            time,
            sm,
            memory: 0,
            encoder,
            decoder,
        }
    }

    #[test]
    fn the_layouts_are_nvmls() {
        assert_eq!(size_of::<MemoryV2>(), 40);
        assert_eq!(size_of::<Memory>(), 24);
        assert_eq!(size_of::<ValueSample>(), 16);
        assert_eq!(size_of::<Row>(), 32);
        assert_eq!(std::mem::offset_of!(Row, time), 8);
        assert_eq!(std::mem::offset_of!(ProcessEntry, used), 8);
    }

    #[test]
    fn a_reading_takes_what_each_call_answers() {
        fake(|f| {
            f.memory_used = 8 << 30;
            f.values = vec![value(1000, 40), value(1200, 44)];
            f.lists[0].1 = vec![(10, 3 << 30), (11, 1 << 30)];
            f.lists[1].1 = vec![(11, 1 << 30), (12, 2 << 20)];
            f.rows = vec![row(10, 1100, 30, 60, 40), row(11, 1200, 12, 0, 10)];
        });
        let mut d = device();
        assert_eq!(
            d.facts(),
            Facts {
                name: Some("NVIDIA GeForce RTX 4090".into()),
                memory_total: Some(24 << 30),
            }
        );
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(
            r.engines,
            [Some(42.0), Some(60.0), Some(50.0), Some(0.0), Some(0.0)]
        );
        assert_eq!(r.memory_used, Some(8 << 30));
        // In two lists, counted once.
        assert_eq!(r.memory, [(10, 3 << 30), (11, 1 << 30), (12, 2 << 20)]);
        assert_eq!(
            r.uses,
            [
                ProcessUse {
                    pid: 10,
                    shares: [30, 60, 40],
                },
                ProcessUse {
                    pid: 11,
                    shares: [12, 0, 10],
                },
            ]
        );
        assert!(!r.idle());
    }

    #[test]
    fn each_read_counts_from_the_newest_sample_seen() {
        fake(|f| {
            f.values = vec![value(1000, 40), value(1200, 44)];
            f.rows = vec![row(10, 1400, 30, 0, 0)];
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        fake(|f| {
            f.values = vec![value(1200, 99), value(1600, 10)];
            f.rows = vec![row(10, 1400, 99, 0, 0), row(11, 1600, 8, 0, 0)];
        });
        d.read(&mut r);
        fake(|f| {
            assert_eq!(f.sample_lasts, [0, 1400]);
            assert_eq!(f.row_calls, [0, 1400]);
        });
        // What was counted last time is not counted again.
        assert_eq!(r.engines[SM], Some(10.0));
        assert_eq!(
            r.uses,
            [ProcessUse {
                pid: 11,
                shares: [8, 0, 0],
            }]
        );
    }

    #[test]
    fn one_call_failing_loses_only_its_own_reading() {
        fake(|f| {
            f.memory_used = 8 << 30;
            f.encoder = ERROR_NOT_SUPPORTED;
            f.decoder = 999;
            f.memory_v2 = ERROR_NOT_SUPPORTED;
            f.lists[0] = (SUCCESS, vec![(10, 1 << 30)]);
            f.lists[1].0 = ERROR_NO_PERMISSION;
            f.values = vec![value(1000, 40)];
            f.rows = vec![row(10, 1000, 40, 0, 0)];
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(r.engines[SM], Some(40.0));
        assert_eq!(r.engines[ENCODER], None);
        assert_eq!(r.engines[DECODER], None);
        assert_eq!(r.memory_used, Some(7 << 30), "version 1 stands in");
        assert_eq!(r.memory, [(10, 1 << 30)]);
        assert_eq!(r.uses.len(), 1);

        // What will never answer is not asked again; what only failed is.
        d.read(&mut r);
        assert!(d.api.encoder.is_none());
        assert!(d.api.memory_v2.is_none());
        assert!(d.api.processes[1].is_none());
        assert!(d.api.decoder.is_some());
        fake(|f| {
            assert_eq!(f.encoder_calls, 1);
            assert_eq!(f.list_calls, 2 + 3);
        });
    }

    #[test]
    fn the_snapshot_stands_in_for_the_samples() {
        fake(|f| {
            f.samples = ERROR_NOT_SUPPORTED;
            f.snapshot = 33;
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(r.engines[SM], Some(33.0));
        // Not supported, then nothing new: the snapshot either way.
        d.read(&mut r);
        assert_eq!(r.engines[SM], Some(33.0));
        assert_eq!(d.api.samples.map(|_| ()), None);

        fake(|f| {
            f.samples = SUCCESS;
            f.rates = ERROR_NOT_SUPPORTED;
        });
        let mut d = device();
        d.read(&mut r);
        assert_eq!(r.engines[SM], None, "no samples yet and no snapshot");
        assert!(!r.idle());
    }

    #[test]
    fn a_full_buffer_grows() {
        let many: Vec<(u32, u64)> = (0..50).map(|i| (i, 1 << 20)).collect();
        fake(|f| {
            f.lists[0].1 = many;
            f.row_room = 500;
            f.values = vec![value(1000, 40)];
            f.rows = vec![row(10, 1000, 40, 0, 0)];
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(r.memory.len(), 50);
        assert_eq!(r.uses.len(), 1);
    }

    #[test]
    fn the_first_version_of_a_process_entry_is_smaller() {
        fake(|f| f.lists[0].1 = vec![(10, 1 << 30), (11, 2 << 30)]);
        let mut a = api();
        a.processes = [Some((old_compute, 16)), None, None];
        let mut d = NvmlDevice::open(a, "0000:01:00.0").unwrap();
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(r.memory, [(10, 1 << 30), (11, 2 << 30)]);
    }

    #[test]
    fn nonsense_is_not_believed() {
        fake(|f| {
            f.values = vec![value(1000, u32::MAX), value(1200, 200), value(1400, 20)];
            f.lists[0].1 = vec![(10, NOT_AVAILABLE)];
            f.rows = vec![row(10, 1000, u32::MAX, 300, 7)];
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        assert_eq!(r.engines[SM], Some(20.0));
        assert_eq!(r.memory, [(10, 0)]);
        assert_eq!(r.uses[0].shares, [0, 0, 7]);
    }

    #[test]
    fn an_idle_gpu_is_not_asked_for_its_rows() {
        fake(|f| {
            f.values = vec![value(1000, 0)];
            f.encoder = 999;
            f.decoder = 999;
            f.process_use = 6;
        });
        let mut d = device();
        let mut r = Reading::default();
        d.read(&mut r);
        // Two engines would not say, so the rows are asked for: not there either.
        assert_eq!(fake(|f| f.row_calls.len()), 1);
        assert!(r.idle());

        fake(|f| {
            f.encoder = SUCCESS;
            f.decoder = SUCCESS;
        });
        d.api.encoder = Some(idle_engine);
        d.api.decoder = Some(idle_engine);
        d.read(&mut r);
        assert_eq!(fake(|f| f.row_calls.len()), 1, "not asked again");
        assert!(r.idle());

        // With memory on the device, something is there.
        fake(|f| f.lists[0].1 = vec![(10, 1 << 20)]);
        d.read(&mut r);
        assert!(!r.idle());
    }

    #[test]
    fn opening_is_balanced_by_a_shutdown() {
        fake(|f| f.init = 9);
        assert!(NvmlDevice::open(api(), "0000:01:00.0").is_none());
        assert_eq!(fake(|f| f.shutdowns), 0, "nothing to balance");

        fake(|f| {
            f.init = SUCCESS;
            f.handle = 6;
        });
        assert!(NvmlDevice::open(api(), "0000:01:00.0").is_none());
        assert_eq!(fake(|f| f.shutdowns), 1);

        fake(|f| f.handle = SUCCESS);
        drop(device());
        assert_eq!(fake(|f| f.shutdowns), 2);
    }
}
