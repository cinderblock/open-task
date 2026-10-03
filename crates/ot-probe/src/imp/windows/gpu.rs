//! Graphics adapters: their load, their memory, and which process is using them.
//!
//! Load comes from the `GPU Engine` performance counters, the same source Task
//! Manager reads. The graphics kernel (`dxgkrnl`) keeps one instance per process,
//! adapter and engine, named like
//! `pid_1234_luid_0x00000000_0x0000C8A1_phys_0_eng_0_engtype_3D`, with the share of
//! the interval that engine spent running that process's work. Summing over the
//! processes gives each engine's load; the busiest engine is the adapter's
//! "utilization", because the engines run independently and do not add up. Memory
//! comes from `GPU Adapter Memory`, one instance per adapter. There is no public API
//! for any of this short of ETW; the counters are what every tool uses.
//!
//! The counters are added by English name (`PdhAddEnglishCounterW`) in a query of
//! their own, collected once per pass on the sampler thread. Utilization is a rate,
//! so the first collection has no value and the first pass reports nothing. The
//! instance list is long: about 600 engine instances here (a hundred-odd processes
//! times the engines they touched on two adapters), and it is formatted into the
//! reused scratch buffer every pass. Names are decoded into one reused `String` and
//! engine names interned as `Arc<str>`, so a steady-state pass allocates only the
//! per-adapter engine lists it hands out.
//!
//! Cost: one `sample` call took 2.4 to 3.2 ms mean and 3 to 10 ms worst over
//! three runs of 30 calls on this machine (two adapters in the counters, ~580
//! engine instances; `gpu_cost` in the tests). Almost all of it is PDH formatting
//! the engine array. That is under the ~5 ms a pass can afford, so it runs every
//! pass; if a machine with more processes or adapters pushes it past that,
//! sampling every other pass is the lever.
//!
//! Adapter facts are read once per LUID when it first shows up in the counters:
//! DXGI (`IDXGIFactory1::EnumAdapters1`) for the description, memory sizes and the
//! software flag, matched to the counters by `AdapterLuid`; and the display class
//! key in the registry, matched by `DriverDesc`, for the driver version and date.
//! `GPU N` numbers follow DXGI enumeration order, software adapters included; the UI
//! decides what to show. DXGI objects are created, used and released inside the
//! read, so nothing COM is held across passes or threads.
//!
//! Per-process attribution (`process_gpu`) is the process's busiest engine type on
//! any adapter over the last pass, labelled `GPU 0 - 3D`, which is what the
//! Processes table shows.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use ot_model::gpu::{EngineSample, GpuInfo, GpuSample};
use ot_model::{Bytes, Percent};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    RRF_RT_REG_SZ,
};

use super::AlignedBuf;

/// `PDH_FMT_NOCAP100` is missing from the SDK metadata. An engine's summed share can
/// pass 100 by a little; the sum is clamped after the fact instead.
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const ERROR_SUCCESS: u32 = 0;
/// `DXGI_ADAPTER_DESC1.Flags` is a `u32` while the flag constant is an `i32`.
const SOFTWARE_FLAG: u32 = DXGI_ADAPTER_FLAG_SOFTWARE.0.unsigned_abs();

/// One engine instance's name, parsed:
/// `pid_1234_luid_0x00000000_0x0000C8A1_phys_0_eng_0_engtype_3D`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EngineInstance<'a> {
    pid: u32,
    luid: u64,
    /// The engine's index on its adapter.
    engine: u32,
    /// The type as the driver names it: `3D`, `VideoDecode`, `Compute_0`; empty for
    /// an engine the driver does not classify.
    kind: &'a str,
}

/// Facts read from the display class key for one adapter.
#[derive(Debug, Default)]
struct DriverFacts {
    version: Option<String>,
    date: Option<String>,
    location: Option<String>,
}

/// What DXGI says about one adapter.
#[derive(Debug)]
struct DxgiAdapter {
    luid: u64,
    description: String,
    dedicated: u64,
    shared: u64,
    software: bool,
}

/// Per-adapter accumulation for one pass; kept between passes for its capacity.
#[derive(Debug)]
struct Adapter {
    luid: u64,
    /// Each engine type's summed share, in the order first seen.
    engines: Vec<(Arc<str>, f64)>,
    dedicated: u64,
    shared: u64,
    /// Whether any counter named this adapter this pass.
    seen: bool,
}

impl Adapter {
    fn new(luid: u64) -> Self {
        Self {
            luid,
            engines: Vec::new(),
            dedicated: 0,
            shared: 0,
            seen: false,
        }
    }

    fn reset(&mut self) {
        self.engines.clear();
        self.dedicated = 0;
        self.shared = 0;
        self.seen = false;
    }
}

/// The PDH query and its counters. Split from the probe so an instance array
/// borrowed from its scratch buffer can be walked while the probe's other fields
/// are updated.
#[derive(Debug)]
struct Query {
    handle: PDH_HQUERY,
    engine: PDH_HCOUNTER,
    dedicated: Option<PDH_HCOUNTER>,
    shared: Option<PDH_HCOUNTER>,
    /// Scratch for instance arrays, reused every pass.
    items: AlignedBuf,
    /// Whether a failed collection has been logged.
    warned: bool,
}

// SAFETY: PDH query and counter handles may be used from any thread, just not from
// two at once. `Query` belongs to the GPU probe, which one sampler thread owns.
unsafe impl Send for Query {}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the query came from PdhOpenQueryW and is closed exactly once; that
        // also frees every counter in it.
        unsafe {
            let _ = PdhCloseQuery(self.handle);
        }
    }
}

impl Query {
    /// Open the query. `None` if PDH will not open one or the `GPU Engine` counter
    /// does not exist here (no WDDM 2.x driver, or a damaged counter registry).
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: a null data source means live data; `query` is a valid out-pointer.
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) } != ERROR_SUCCESS {
            return None;
        }
        let mut q = Self {
            handle: query,
            engine: PDH_HCOUNTER::default(),
            dedicated: None,
            shared: None,
            items: AlignedBuf::default(),
            warned: false,
        };
        // Dropping `q` on the early return closes the query.
        q.engine = q.add(w!(r"\GPU Engine(*)\Utilization Percentage"))?;
        q.dedicated = q.add(w!(r"\GPU Adapter Memory(*)\Dedicated Usage"));
        q.shared = q.add(w!(r"\GPU Adapter Memory(*)\Shared Usage"));
        Some(q)
    }

    fn add(&self, path: PCWSTR) -> Option<PDH_HCOUNTER> {
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: the query is open; the path is a static wide string.
        let status = unsafe { PdhAddEnglishCounterW(self.handle, path, 0, &raw mut counter) };
        (status == ERROR_SUCCESS).then_some(counter)
    }

    /// Take this pass's readings. Returns false if the collection failed.
    fn collect(&mut self) -> bool {
        // SAFETY: the query is open.
        let ok = unsafe { PdhCollectQueryData(self.handle) == ERROR_SUCCESS };
        if !ok && !self.warned {
            self.warned = true;
            tracing::warn!("PdhCollectQueryData failed for the GPU counters");
        }
        ok
    }

    /// A wildcard counter's instances, in the reused scratch buffer. `None` when
    /// the counter has no value yet (a rate before its second collection) or the
    /// read failed.
    fn array(
        &mut self,
        counter: PDH_HCOUNTER,
        format: PDH_FMT,
    ) -> Option<&[PDH_FMT_COUNTERVALUE_ITEM_W]> {
        loop {
            let mut size = self.items.len_bytes() as u32;
            let mut count = 0u32;
            let buffer = (size > 0).then(|| self.items.as_mut_ptr().cast());
            // SAFETY: `size` is the buffer's length in bytes (zero asks for the size).
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter, format, &raw mut size, &raw mut count, buffer)
            };
            match status {
                ERROR_SUCCESS => {
                    // SAFETY: on success the buffer starts with `count` items; the
                    // strings they point at follow them in the same buffer.
                    return Some(unsafe {
                        std::slice::from_raw_parts(
                            self.items.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                            count as usize,
                        )
                    });
                }
                PDH_MORE_DATA if (size as usize) > self.items.len_bytes() => {
                    self.items.resize_bytes(size as usize);
                }
                _ => return None,
            }
        }
    }
}

/// Samples every adapter's engines and memory, and remembers which process was
/// busiest on which engine.
#[derive(Debug)]
pub(super) struct GpuProbe {
    query: Query,
    /// Adapter facts by LUID, shared with every sample that names the adapter.
    infos: HashMap<u64, Arc<GpuInfo>>,
    /// Engine display names by the driver's raw type (or `Engine N` for an
    /// unclassified engine), so the same name is one allocation for good.
    engine_names: HashMap<Box<str>, Arc<str>>,
    /// `GPU 0 - 3D` labels by adapter and engine name.
    labels: HashMap<(u64, Arc<str>), Arc<str>>,
    /// This pass's per-adapter sums, kept for their capacity.
    adapters: Vec<Adapter>,
    /// This pass's per-process, per-adapter, per-engine-type sums.
    by_process: HashMap<(u32, u64, Arc<str>), f64>,
    /// Each process's busiest engine over the last pass, with its label.
    busiest: HashMap<u32, (f32, Arc<str>)>,
    /// Scratch for decoding an instance name.
    name: String,
    /// Scratch for an unclassified engine's `Engine N` key.
    key: String,
}

impl GpuProbe {
    /// Open the counters. `None` when this machine has no GPU counters, in which
    /// case there is simply nothing to report.
    pub fn new() -> Option<Self> {
        Some(Self {
            query: Query::open()?,
            infos: HashMap::new(),
            engine_names: HashMap::new(),
            labels: HashMap::new(),
            adapters: Vec::new(),
            by_process: HashMap::new(),
            busiest: HashMap::new(),
            name: String::new(),
            key: String::new(),
        })
    }

    /// Refill `out` with every adapter the counters name, `GPU 0` first. Empty on
    /// the first pass, before the rate counters have an interval.
    pub fn sample(&mut self, out: &mut Vec<GpuSample>) {
        out.clear();
        self.busiest.clear();
        self.by_process.clear();
        for a in &mut self.adapters {
            a.reset();
        }
        if !self.query.collect() || !self.read_engines() {
            return;
        }
        self.read_memory();
        self.publish(out);
        self.attribute();
    }

    /// The process's busiest engine over the last pass and its label, such as
    /// `GPU 0 - 3D`; `None` if it used no GPU.
    pub fn process_gpu(&self, pid: u32) -> Option<(Percent, Arc<str>)> {
        self.busiest
            .get(&pid)
            .map(|(share, label)| (Percent(*share), Arc::clone(label)))
    }

    /// Sum every engine instance into its adapter's engine type and into its
    /// process. False when the counter has no interval yet.
    fn read_engines(&mut self) -> bool {
        let engine = self.query.engine;
        let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let Some(items) = self.query.array(engine, format) else {
            return false;
        };
        for item in items {
            if !valid(item.FmtValue.CStatus) {
                continue;
            }
            decode(item.szName, &mut self.name);
            let Some(inst) = parse_engine(&self.name) else {
                continue;
            };
            // SAFETY: a DOUBLE format fills the `doubleValue` member.
            let share = unsafe { item.FmtValue.Anonymous.doubleValue };
            let name = engine_name(&mut self.engine_names, &mut self.key, &inst);
            let adapter = adapter_mut(&mut self.adapters, inst.luid);
            match adapter
                .engines
                .iter_mut()
                .find(|(n, _)| Arc::ptr_eq(n, &name))
            {
                Some((_, total)) => *total += share,
                None => adapter.engines.push((Arc::clone(&name), share)),
            }
            if share > 0.0 {
                *self
                    .by_process
                    .entry((inst.pid, inst.luid, name))
                    .or_insert(0.0) += share;
            }
        }
        true
    }

    /// Each adapter's dedicated and shared memory in use.
    fn read_memory(&mut self) {
        for (which, counter) in [self.query.dedicated, self.query.shared]
            .into_iter()
            .enumerate()
        {
            let Some(counter) = counter else {
                continue;
            };
            let Some(items) = self.query.array(counter, PDH_FMT_LARGE) else {
                continue;
            };
            for item in items {
                if !valid(item.FmtValue.CStatus) {
                    continue;
                }
                decode(item.szName, &mut self.name);
                let Some(luid) = parse_adapter(&self.name) else {
                    continue;
                };
                // SAFETY: a LARGE format fills the `largeValue` member.
                let bytes =
                    u64::try_from(unsafe { item.FmtValue.Anonymous.largeValue }).unwrap_or(0);
                let adapter = adapter_mut(&mut self.adapters, luid);
                if which == 0 {
                    adapter.dedicated = bytes;
                } else {
                    adapter.shared = bytes;
                }
            }
        }
    }

    /// Turn this pass's sums into samples, `GPU 0` first.
    fn publish(&mut self, out: &mut Vec<GpuSample>) {
        self.adapters.retain(|a| a.seen);
        for i in 0..self.adapters.len() {
            let info = self.info(self.adapters[i].luid);
            let a = &mut self.adapters[i];
            a.engines
                .sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
            let utilization = a.engines.first().map_or(Percent::ZERO, |(_, share)| {
                Percent(*share as f32).clamped(100.0)
            });
            out.push(GpuSample {
                info,
                utilization,
                engines: a
                    .engines
                    .iter()
                    .map(|(name, share)| EngineSample {
                        name: Arc::clone(name),
                        usage: Percent(*share as f32).clamped(100.0),
                    })
                    .collect(),
                dedicated_used: Bytes(a.dedicated),
                shared_used: Bytes(a.shared),
            });
        }
        out.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    }

    /// Each process's busiest engine type, with its `GPU 0 - 3D` label.
    fn attribute(&mut self) {
        for ((pid, luid, name), share) in &self.by_process {
            let share = (*share as f32).clamp(0.0, 100.0);
            let busier = self.busiest.get(pid).is_none_or(|(s, _)| share > *s);
            if !busier {
                continue;
            }
            let label = if let Some(l) = self.labels.get(&(*luid, Arc::clone(name))) {
                Arc::clone(l)
            } else {
                let gpu = self.infos.get(luid).map_or("GPU", |i| i.name.as_str());
                let label: Arc<str> = format!("{gpu} - {name}").into();
                self.labels
                    .insert((*luid, Arc::clone(name)), Arc::clone(&label));
                label
            };
            self.busiest.insert(*pid, (share, label));
        }
    }

    /// The adapter's facts, read on first sight. An adapter the counters name but
    /// DXGI does not is listed by its LUID.
    fn info(&mut self, luid: u64) -> Arc<GpuInfo> {
        if let Some(info) = self.infos.get(&luid) {
            return Arc::clone(info);
        }
        // Either the first pass or an adapter that arrived since: enumerate and
        // take every adapter not yet known. `GPU N` is the DXGI index, so an
        // adapter keeps its number for as long as it is present.
        for (index, a) in dxgi_adapters().into_iter().enumerate() {
            if self.infos.contains_key(&a.luid) {
                continue;
            }
            let driver = driver_facts(&a.description);
            self.infos.insert(
                a.luid,
                Arc::new(GpuInfo {
                    id: a.luid,
                    name: format!("GPU {index}"),
                    adapter: a.description,
                    dedicated_total: Some(Bytes(a.dedicated)),
                    shared_total: Some(Bytes(a.shared)),
                    driver_version: driver.version,
                    driver_date: driver.date,
                    location: driver.location,
                    software: a.software,
                }),
            );
        }
        let info = self.infos.entry(luid).or_insert_with(|| {
            tracing::warn!(
                luid = format_args!("{luid:#x}"),
                "GPU in counters but not in DXGI"
            );
            Arc::new(GpuInfo {
                id: luid,
                name: format!("GPU {luid:#x}"),
                adapter: "GPU".to_owned(),
                ..GpuInfo::default()
            })
        });
        Arc::clone(info)
    }
}

/// This pass's accumulator for `luid`, marked seen.
fn adapter_mut(adapters: &mut Vec<Adapter>, luid: u64) -> &mut Adapter {
    let i = adapters
        .iter()
        .position(|a| a.luid == luid)
        .unwrap_or_else(|| {
            adapters.push(Adapter::new(luid));
            adapters.len() - 1
        });
    let a = &mut adapters[i];
    a.seen = true;
    a
}

/// The interned display name for an engine instance's type.
fn engine_name(
    names: &mut HashMap<Box<str>, Arc<str>>,
    key: &mut String,
    inst: &EngineInstance<'_>,
) -> Arc<str> {
    let raw: &str = if inst.kind.is_empty() {
        key.clear();
        // Writing to a String cannot fail.
        let _ = write!(key, "Engine {}", inst.engine);
        key
    } else {
        inst.kind
    };
    if let Some(name) = names.get(raw) {
        return Arc::clone(name);
    }
    let name: Arc<str> = display_name(raw).into();
    names.insert(raw.into(), Arc::clone(&name));
    name
}

/// Decode a PDH instance name into the reused buffer.
fn decode(name: PWSTR, into: &mut String) {
    into.clear();
    if name.is_null() {
        return;
    }
    // SAFETY: PDH points `szName` at a NUL-terminated string inside the same buffer
    // the item came from, which outlives this call.
    let units = unsafe { name.as_wide() };
    into.extend(
        char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
    );
}

fn valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

/// `pid_1234_luid_0x00000000_0x0000C8A1_phys_0_eng_0_engtype_3D`. The type is
/// everything after `engtype_`; it may hold spaces (`GDI Render`) and underscores
/// (`Compute_0`) and may be empty.
fn parse_engine(name: &str) -> Option<EngineInstance<'_>> {
    let rest = name.strip_prefix("pid_")?;
    let (pid, rest) = rest.split_once('_')?;
    let rest = rest.strip_prefix("luid_")?;
    let (high, rest) = rest.split_once('_')?;
    let (low, rest) = rest.split_once('_')?;
    let rest = rest.strip_prefix("phys_")?;
    let (_phys, rest) = rest.split_once('_')?;
    let rest = rest.strip_prefix("eng_")?;
    let (engine, rest) = rest.split_once('_')?;
    let kind = rest.strip_prefix("engtype_")?;
    Some(EngineInstance {
        pid: pid.parse().ok()?,
        luid: parse_luid(high, low)?,
        engine: engine.parse().ok()?,
        kind,
    })
}

/// `luid_0x00000000_0x0000C8A1_phys_0`, as `GPU Adapter Memory` names its
/// instances.
fn parse_adapter(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("luid_")?;
    let (high, rest) = rest.split_once('_')?;
    let low = rest.split_once('_').map_or(rest, |(low, _)| low);
    parse_luid(high, low)
}

/// The two hex halves of an instance name, high part first, as one `u64`: the
/// same packing DXGI's `AdapterLuid` gets in [`luid`].
fn parse_luid(high: &str, low: &str) -> Option<u64> {
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    Some((hex(high)? << 32) | hex(low)?)
}

/// A `LUID` packed as the counters name it: `high << 32 | low`.
fn luid(high: i32, low: u32) -> u64 {
    (u64::from(high.cast_unsigned()) << 32) | u64::from(low)
}

/// `VideoDecode` to `Video Decode`, `Compute_0` to `Compute 0`; `3D` and
/// `GDI Render` stay as they are.
fn display_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    let mut after_lower = false;
    for c in raw.chars() {
        if c == '_' {
            out.push(' ');
            after_lower = false;
            continue;
        }
        if after_lower && c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
        after_lower = c.is_ascii_lowercase();
    }
    out
}

/// Every adapter DXGI enumerates, in its order. Empty, with a warning, if DXGI
/// will not start.
fn dxgi_adapters() -> Vec<DxgiAdapter> {
    let mut out = Vec::new();
    // SAFETY: plain object creation; the factory lives only until this returns.
    let factory = match unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "CreateDXGIFactory1 failed; GPU facts unavailable");
            return out;
        }
    };
    for index in 0.. {
        // SAFETY: the factory is live; DXGI_ERROR_NOT_FOUND ends the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: the adapter is live; the description is returned by value.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        out.push(DxgiAdapter {
            luid: luid(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart),
            description: wide(&desc.Description).trim().to_owned(),
            dedicated: desc.DedicatedVideoMemory as u64,
            shared: desc.SharedSystemMemory as u64,
            software: desc.Flags & SOFTWARE_FLAG != 0,
        });
    }
    out
}

/// The driver's version, date and location for the adapter whose `DriverDesc` is
/// `description`, from the display class key. Anything missing is `None`.
fn driver_facts(description: &str) -> DriverFacts {
    let mut facts = DriverFacts::default();
    let mut class = HKEY::default();
    // SAFETY: `class` is a valid out-pointer; the key is closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}"),
            None,
            KEY_READ,
            &raw mut class,
        )
    };
    if opened.is_err() {
        return facts;
    }
    let mut index = 0;
    loop {
        // Subkeys are `0000`, `0001`, ...; the buffer stays NUL-terminated.
        let mut name = [0u16; 64];
        // SAFETY: the key is open; the buffer's length is passed with it.
        if unsafe { RegEnumKeyW(class, index, Some(&mut name[..63])) }.is_err() {
            break;
        }
        index += 1;
        let sub = PCWSTR(name.as_ptr());
        let matches = reg_string(class, sub, w!("DriverDesc"))
            .is_some_and(|d| d.eq_ignore_ascii_case(description));
        if !matches {
            continue;
        }
        facts.version = reg_string(class, sub, w!("DriverVersion"));
        facts.date = reg_string(class, sub, w!("DriverDate"));
        facts.location = reg_string(class, sub, w!("LocationInformation"));
        break;
    }
    // SAFETY: opened above, closed exactly once.
    unsafe {
        let _ = RegCloseKey(class);
    }
    facts
}

/// A string value under `key\subkey`, trimmed; `None` if absent, empty or not a
/// string.
fn reg_string(key: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte size agree; `subkey` is NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            key,
            subkey,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let s = wide(&buf[..units]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn wide(units: &[u16]) -> String {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_instances_parse() {
        let e = parse_engine("pid_1234_luid_0x00000000_0x0000C8A1_phys_0_eng_0_engtype_3D")
            .expect("parses");
        assert_eq!(
            e,
            EngineInstance {
                pid: 1234,
                luid: 0xC8A1,
                engine: 0,
                kind: "3D"
            }
        );
        let e =
            parse_engine("pid_10916_luid_0x00000001_0x00012C5A_phys_0_eng_10_engtype_GDI Render")
                .expect("parses");
        assert_eq!(e.pid, 10916);
        assert_eq!(e.luid, 0x1_0001_2C5A);
        assert_eq!(e.engine, 10);
        assert_eq!(e.kind, "GDI Render");
        let e = parse_engine("pid_8_luid_0x00000000_0x00012C5A_phys_0_eng_12_engtype_")
            .expect("an unclassified engine still parses");
        assert_eq!(e.kind, "");
        assert_eq!(e.engine, 12);
        assert_eq!(parse_engine("_Total"), None);
        assert_eq!(parse_engine("luid_0x00000000_0x0000C8A1_phys_0"), None);
    }

    #[test]
    fn adapter_instances_parse() {
        assert_eq!(
            parse_adapter("luid_0x00000000_0x0000C8A1_phys_0"),
            Some(0xC8A1)
        );
        assert_eq!(parse_adapter("luid_0x00000000_0x0000C8A1"), Some(0xC8A1));
        assert_eq!(parse_adapter("_Total"), None);
        assert_eq!(parse_adapter("luid_0xZZ_0x1_phys_0"), None);
    }

    #[test]
    fn luid_round_trips() {
        for (high, low) in [
            (0, 0xC8A1u32),
            (1, 0),
            (-1, u32::MAX),
            (0x7FFF_FFFF, 0x1234_5678),
        ] {
            let packed = luid(high, low);
            let text = format!("luid_0x{:08X}_0x{low:08X}_phys_0", high.cast_unsigned());
            assert_eq!(parse_adapter(&text), Some(packed), "{text}");
            assert_eq!(packed >> 32, u64::from(high.cast_unsigned()));
            assert_eq!(packed & 0xFFFF_FFFF, u64::from(low));
        }
    }

    #[test]
    fn engine_types_display_with_spaces() {
        assert_eq!(display_name("3D"), "3D");
        assert_eq!(display_name("Copy"), "Copy");
        assert_eq!(display_name("VideoDecode"), "Video Decode");
        assert_eq!(display_name("VideoProcessing"), "Video Processing");
        assert_eq!(display_name("GDI Render"), "GDI Render");
        assert_eq!(display_name("Compute_0"), "Compute 0");
        assert_eq!(display_name("LegacyOverlay"), "Legacy Overlay");
        assert_eq!(display_name("Engine 12"), "Engine 12");
    }

    #[test]
    fn this_machine_has_a_gpu_and_measures_it() {
        let mut probe = GpuProbe::new().expect("GPU counters exist here");
        let mut out = Vec::new();
        probe.sample(&mut out);
        std::thread::sleep(std::time::Duration::from_millis(300));
        probe.sample(&mut out);
        assert!(!out.is_empty(), "a desktop with a GPU reports it");
        for g in &out {
            assert!(!g.info.adapter.is_empty(), "{g:?}");
            assert!(g.info.name.starts_with("GPU "), "{g:?}");
            let u = g.utilization.get();
            assert!((0.0..=100.0).contains(&u), "{g:?}");
            assert!(!g.engines.is_empty(), "{g:?}");
            assert!(
                g.engines.windows(2).all(|w| w[0].usage >= w[1].usage),
                "busiest first: {g:?}"
            );
            assert!(
                g.engines.iter().all(|e| !e.name.contains('_')),
                "display names: {g:?}"
            );
            assert_eq!(
                g.utilization, g.engines[0].usage,
                "utilization is the busiest engine"
            );
        }
        // Facts are shared, not rebuilt, and the engine names are interned.
        let first = Arc::clone(&out[0].info);
        let name = Arc::clone(&out[0].engines[0].name);
        std::thread::sleep(std::time::Duration::from_millis(100));
        probe.sample(&mut out);
        assert!(Arc::ptr_eq(&first, &out[0].info));
        assert!(out[0].engines.iter().any(|e| Arc::ptr_eq(&e.name, &name)));
        // Every attributed process names a listed adapter.
        for (pid, (share, label)) in &probe.busiest {
            assert!(*share > 0.0 && *share <= 100.0, "{pid}: {share} {label}");
            assert!(label.contains(" - "), "{label}");
            assert!(probe.process_gpu(*pid).is_some());
        }
        assert!(probe.process_gpu(u32::MAX).is_none());
    }

    /// What a pass costs here. Ignored because it measures the machine it runs on:
    ///
    /// ```text
    /// cargo test -p ot-probe --release gpu_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn gpu_cost() {
        use std::time::Instant;
        let mut probe = GpuProbe::new().expect("GPU counters");
        let mut out = Vec::new();
        probe.sample(&mut out);
        std::thread::sleep(std::time::Duration::from_millis(300));
        probe.sample(&mut out);
        let n = 30;
        let mut worst = 0f64;
        let mut total = 0f64;
        for _ in 0..n {
            let t = Instant::now();
            probe.sample(&mut out);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            worst = worst.max(ms);
            total += ms;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mean = total / f64::from(n);
        let instances: usize = out.iter().map(|g| g.engines.len()).sum();
        println!(
            "gpu sample  mean {mean:>7.3} ms   worst {worst:>7.3} ms  ({} adapters, {} engine types, {} processes attributed)",
            out.len(),
            instances,
            probe.busiest.len()
        );
        for g in &out {
            println!("{g:#?}");
        }
    }
}
