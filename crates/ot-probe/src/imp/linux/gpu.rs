//! Graphics adapters: their load, their memory, and which process is using them.
//!
//! Load comes from the DRM client usage stats (kernel 5.19 and later): every open
//! file on a GPU's device node (`/dev/dri/card0`, `/dev/dri/renderD128`) is a
//! client, and its `/proc/<pid>/fdinfo/<fd>` says how long each engine has spent
//! running that client's work (`drm-engine-render: 9288864723 ns`, or busy and
//! total cycles on Xe), and how much memory it holds in each region. i915, Xe,
//! amdgpu, and most of the open drivers for phones and boards publish them; what
//! `intel_gpu_top`, `nvtop` and `gputop` read. Summing the processes' shares gives
//! each engine's load; the busiest engine is the adapter's utilization, as on
//! Windows, because the engines run independently and do not add up.
//!
//! A process's open files can only be read by its own user, or by root. Without
//! root, work for other users' processes (a display server running as root) is
//! missing from the engines' load, so where the driver measures the whole GPU
//! itself (amdgpu's `gpu_busy_percent`) the utilization is the larger of the two,
//! and the rest is no process's.
//!
//! Finding the clients means listing each process's open files and reading where
//! each points, which costs the kernel an entry per file: about 90 ms for 9000
//! files. So a process's files are listed when it is first seen, when one found on
//! a GPU has closed, and when its count of open files has changed (the process
//! pass counts them anyway), but then no more than every [`CHANGED_PASSES`]
//! passes, since a browser's changes all the time; in between, only the files
//! found on a GPU are read. Opening a GPU's device node changes the count, unless
//! the process closed another file in the same pass and then never opens or
//! closes another; that client is missed. A process whose files cannot be read (another user's)
//! is not tried again. A client open in two files or two processes (a duplicated
//! or passed descriptor) has one `drm-client-id` and is counted once.
//!
//! Adapters are the devices behind `/sys/class/drm/card*` and `renderD*`, named
//! from the PCI ID database where they are PCI devices. Firmware framebuffers,
//! virtual and USB display drivers are listed as software adapters, as Windows
//! lists its Basic Render Driver; NVIDIA's own driver publishes no client stats
//! and is left out (NVML would be the way).

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ot_model::gpu::{EngineSample, GpuInfo, GpuSample};
use ot_model::process::ProcessSample;
use ot_model::units::{Bytes, Percent};
use ot_model::ProcessKey;

use super::fs;

/// Device drivers that are not graphics hardware: firmware framebuffers, virtual
/// devices (`vgem` and `vkms` sit on the faux bus), and USB display adapters.
const SOFTWARE_DRIVERS: &[&str] = &[
    "simple-framebuffer",
    "simpledrm",
    "efidrm",
    "vesadrm",
    "ofdrm",
    "faux_driver",
    "vgem",
    "vkms",
    "evdi",
    "udl",
    "gud",
    "gm12u320",
];
/// Drivers that publish no client stats, whose adapters are left out.
const UNREAD_DRIVERS: &[&str] = &["nvidia"];
/// A process's open files are listed again at least this often, in passes.
/// A process whose count of open files changed is listed again at most this often.
const CHANGED_PASSES: u64 = 3;
/// Unknown device nodes are looked for again at most this often, in passes.
const DISCOVER_PASSES: u64 = 10;
/// Where the PCI ID database is kept, by distribution.
const PCI_IDS: &[&str] = &[
    "/usr/share/hwdata/pci.ids",
    "/usr/share/misc/pci.ids",
    "/usr/share/pci.ids",
];

/// A graphics adapter and this pass's sums for it.
#[derive(Debug)]
struct Adapter {
    info: Arc<GpuInfo>,
    /// Files the driver publishes about the whole device, where it does.
    busy_file: Option<PathBuf>,
    vram_used_file: Option<PathBuf>,
    gtt_used_file: Option<PathBuf>,
    /// Each engine's load this pass, summed over the clients, in percent.
    engines: Vec<(Arc<str>, f64)>,
    /// The clients' memory this pass, in bytes.
    dedicated: u64,
    shared: u64,
}

/// What a GPU device node is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    /// The adapter at this index.
    Gpu(usize),
    /// An adapter left out, whose clients cannot be read.
    LeftOut,
}

/// What tells two clients apart: the driver's client id where it gives one, else
/// the file it is open in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ClientKey {
    Id(usize, u64),
    File(ProcessKey, u32),
}

/// A client's counters at the last pass.
#[derive(Debug)]
struct Client {
    /// Each engine's raw name, busy counter (ns or cycles) and total cycles.
    engines: Vec<(Box<str>, u64, u64)>,
    seen: u64,
}

/// A process's files that are open on a GPU.
#[derive(Debug, Default)]
struct Files {
    fds: Vec<u32>,
    /// Its count of open files when they were last listed.
    handles: u32,
    listed: u64,
    seen: u64,
    /// A file thought to be on a GPU no longer is: list them again.
    stale: bool,
    /// The files cannot be read: another user's process.
    denied: bool,
}

/// One process's load on one engine this pass.
#[derive(Debug)]
struct Use {
    proc: usize,
    adapter: usize,
    engine: Arc<str>,
    share: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CounterKind {
    /// `drm-engine-<e>`: busy time, ns.
    Ns,
    /// `drm-cycles-<e>`: busy cycles.
    Cycles,
    /// `drm-total-cycles-<e>`: every cycle, busy or not.
    TotalCycles,
    /// `drm-maxfreq-<e>`: the clock the cycles count at the most, Hz.
    MaxFreq,
    /// `drm-engine-capacity-<e>`: how many engines of the kind there are.
    Capacity,
}

/// One counter line of an fdinfo; `key` is the engine's name, in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Counter {
    key: Range<usize>,
    kind: CounterKind,
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryKind {
    /// `drm-resident-<r>`: backed in the region. What "in use" means.
    Resident,
    /// `drm-memory-<r>`: amdgpu's older name for resident.
    Memory,
    /// `drm-total-<r>`: asked for, backed or not.
    Total,
}

/// One memory line of an fdinfo; the region's `name` is in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Region {
    name: Range<usize>,
    kind: MemoryKind,
    bytes: u64,
}

/// The parts of a DRM fdinfo the probe uses. Reused between files.
#[derive(Debug, Default)]
struct FdInfo {
    /// `drm-driver` is there: the file is a DRM client.
    drm: bool,
    client_id: Option<u64>,
    counters: Vec<Counter>,
    regions: Vec<Region>,
}

/// Whether a memory region is the adapter's own memory (`vram`, `vram0`, i915's
/// `local0`), system memory the GPU uses (`gtt`, `system0`, `memory`), or neither
/// (amdgpu's CPU-only `cpu`, firmware-reserved `stolen-*`).
fn region_class(name: &str) -> Option<bool> {
    if name.starts_with("vram") || name.starts_with("local") {
        Some(true)
    } else if name == "gtt" || name == "memory" || name.starts_with("system") {
        Some(false)
    } else {
        None
    }
}

/// Parse a DRM fdinfo into `out`; the names in it are ranges of `buf`.
fn parse_fdinfo(buf: &[u8], out: &mut FdInfo) {
    out.drm = false;
    out.client_id = None;
    out.counters.clear();
    out.regions.clear();
    let mut start = 0;
    for line in buf.split(|&b| b == b'\n') {
        let line_start = start;
        start += line.len() + 1;
        let Some((key, value)) = fs::key_value(line) else {
            continue;
        };
        let Some(key) = key.strip_prefix(b"drm-") else {
            continue;
        };
        // The key's offset in `buf`, for naming what follows a prefix.
        let at = |rest: &[u8]| {
            let s = line_start + 4 + (key.len() - rest.len());
            s..s + rest.len()
        };
        if key == b"driver" {
            out.drm = true;
            continue;
        }
        let Some((number, unit)) = parse_value(value) else {
            continue;
        };
        if key == b"client-id" {
            out.client_id = Some(number);
            continue;
        }
        // Longer prefixes first: `engine-capacity-` before `engine-`, and
        // `total-cycles-` before `total-`.
        let counter = [
            (&b"engine-capacity-"[..], CounterKind::Capacity),
            (b"engine-", CounterKind::Ns),
            (b"total-cycles-", CounterKind::TotalCycles),
            (b"cycles-", CounterKind::Cycles),
            (b"maxfreq-", CounterKind::MaxFreq),
        ]
        .into_iter()
        .find_map(|(p, kind)| key.strip_prefix(p).map(|rest| (rest, kind)));
        if let Some((rest, kind)) = counter {
            let value = match (kind, unit) {
                (CounterKind::MaxFreq, b"MHz") => number.saturating_mul(1_000_000),
                (CounterKind::MaxFreq, b"KHz" | b"kHz") => number.saturating_mul(1000),
                _ => number,
            };
            out.counters.push(Counter {
                key: at(rest),
                kind,
                value,
            });
            continue;
        }
        let region = [
            (&b"resident-"[..], MemoryKind::Resident),
            (b"memory-", MemoryKind::Memory),
            (b"total-", MemoryKind::Total),
        ]
        .into_iter()
        .find_map(|(p, kind)| key.strip_prefix(p).map(|rest| (rest, kind)));
        if let Some((rest, kind)) = region {
            let scale = match unit {
                b"KiB" => 1 << 10,
                b"MiB" => 1 << 20,
                b"GiB" => 1 << 30,
                _ => 1,
            };
            out.regions.push(Region {
                name: at(rest),
                kind,
                bytes: number.saturating_mul(scale),
            });
        }
    }
}

/// `9288864723 ns`, `216 KiB`, `7`: the number and its unit, if any.
fn parse_value(value: &[u8]) -> Option<(u64, &[u8])> {
    let value = value.trim_ascii();
    let end = value
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(value.len());
    if end == 0 {
        return None;
    }
    let number = std::str::from_utf8(&value[..end]).ok()?.parse().ok()?;
    Some((number, value[end..].trim_ascii()))
}

/// What an engine is called on screen, for the names the drivers use. Windows'
/// names where there is one; anything else as the driver has it.
fn display_name(raw: &str) -> &str {
    match raw {
        "render" | "gfx" | "rcs" => "3D",
        "copy" | "dma" | "bcs" => "Copy",
        "video" | "vcs" => "Video",
        "dec" => "Video Decode",
        "enc" => "Video Encode",
        "enc_1" => "Video Encode 1",
        "video-enhance" | "vecs" | "vpe" => "Video Processing",
        "compute" | "ccs" => "Compute",
        "jpeg" => "JPEG",
        _ => raw,
    }
}

/// The Linux GPU probe.
#[derive(Debug)]
pub(super) struct GpuProbe {
    proc_root: PathBuf,
    sys_root: PathBuf,
    adapters: Vec<Adapter>,
    /// What each device node (`renderD128`) is of.
    nodes: HashMap<String, Node>,
    clients: HashMap<ClientKey, Client>,
    files: HashMap<ProcessKey, Files>,
    /// Engine display names by the driver's name, interned.
    engine_names: HashMap<Box<str>, Arc<str>>,
    /// `GPU 0 - 3D` labels, interned.
    labels: HashMap<(usize, Arc<str>), Arc<str>>,
    uses: Vec<Use>,
    pass: u64,
    last: Option<Instant>,
    next_discover: u64,
    fdinfo: FdInfo,
    buf: Vec<u8>,
    path: String,
}

impl GpuProbe {
    /// Find the adapters. `None` when there is no graphics hardware to watch.
    pub(super) fn new() -> Option<Self> {
        Self::at(Path::new("/proc"), Path::new("/sys"))
    }

    fn at(proc_root: &Path, sys_root: &Path) -> Option<Self> {
        let mut probe = Self {
            proc_root: proc_root.to_owned(),
            sys_root: sys_root.to_owned(),
            adapters: Vec::new(),
            nodes: HashMap::new(),
            clients: HashMap::new(),
            files: HashMap::new(),
            engine_names: HashMap::new(),
            labels: HashMap::new(),
            uses: Vec::new(),
            pass: 0,
            last: None,
            next_discover: 0,
            fdinfo: FdInfo::default(),
            buf: Vec::with_capacity(1024),
            path: String::with_capacity(64),
        };
        probe.discover();
        probe
            .adapters
            .iter()
            .any(|a| !a.info.software)
            .then_some(probe)
    }

    /// Refill `out` with every adapter, `GPU 0` first, and fill in each process's
    /// use of them.
    pub(super) fn sample(&mut self, processes: &mut [ProcessSample], out: &mut Vec<GpuSample>) {
        let now = Instant::now();
        let wall = self
            .last
            .map_or(0.0, |prev| now.duration_since(prev).as_secs_f64());
        self.last = Some(now);
        self.sample_over(processes, out, wall);
    }

    /// One pass, `wall` seconds after the last.
    fn sample_over(
        &mut self,
        processes: &mut [ProcessSample],
        out: &mut Vec<GpuSample>,
        wall: f64,
    ) {
        self.pass += 1;
        out.clear();
        self.uses.clear();
        for a in &mut self.adapters {
            a.engines.clear();
            a.dedicated = 0;
            a.shared = 0;
        }
        for (i, p) in processes.iter_mut().enumerate() {
            // No open files, or not this user's to read.
            if p.handles > 0 {
                self.read_process(i, p, wall);
            }
        }
        let pass = self.pass;
        self.files.retain(|_, f| f.seen == pass);
        self.clients.retain(|_, c| c.seen == pass);
        self.publish(out);
        self.attribute(processes);
    }

    /// Read a process's GPU clients: their memory into the sample, their engine
    /// load into [`Self::uses`] and the adapters' sums.
    fn read_process(&mut self, index: usize, sample: &mut ProcessSample, wall: f64) {
        let key = sample.key();
        let pass = self.pass;
        let mut files = self.files.remove(&key).unwrap_or_default();
        files.seen = pass;
        let since = pass - files.listed;
        let relist = files.listed == 0
            || files.stale
            || (files.handles != sample.handles && since >= CHANGED_PASSES);
        if relist && !files.denied {
            files.denied = !self.list_files(key.pid, &mut files.fds);
            files.handles = sample.handles;
            files.listed = pass;
            files.stale = false;
        }
        let (mut dedicated, mut shared) = (0u64, 0u64);
        let first_use = self.uses.len();
        for &fd in &files.fds {
            let adapter = match self.node_of(key.pid, fd) {
                Some(Node::Gpu(adapter)) => adapter,
                Some(Node::LeftOut) => continue,
                None => {
                    files.stale = true;
                    continue;
                }
            };
            self.set_path(key.pid, "fdinfo/", fd);
            if fs::read_into(&self.path, &mut self.buf).is_err() {
                files.stale = true;
                continue;
            }
            parse_fdinfo(&self.buf, &mut self.fdinfo);
            if !self.fdinfo.drm {
                files.stale = true;
                continue;
            }
            let client_key = self
                .fdinfo
                .client_id
                .map_or(ClientKey::File(key, fd), |id| ClientKey::Id(adapter, id));
            let (mut client, fresh) = match self.clients.remove(&client_key) {
                Some(c) => (c, false),
                None => (
                    Client {
                        engines: Vec::new(),
                        seen: 0,
                    },
                    true,
                ),
            };
            // Counted already when it is open in another file too.
            if client.seen != pass {
                client.seen = pass;
                let (own, system) = client_memory(&self.buf, &self.fdinfo.regions);
                dedicated += own;
                shared += system;
                let gpu = &mut self.adapters[adapter];
                gpu.dedicated += own;
                gpu.shared += system;
                self.count_engines(&mut client, fresh, adapter, index, first_use, wall);
            }
            self.clients.insert(client_key, client);
        }
        sample.gpu_dedicated = Bytes(dedicated);
        sample.gpu_shared = Bytes(shared);
        self.files.insert(key, files);
    }

    /// Add what a client's engines did since the last pass, from the fdinfo just
    /// parsed, to its adapter's engines and to the process's uses, which start at
    /// `first_use`. A `fresh` client has nothing to compare with yet.
    fn count_engines(
        &mut self,
        client: &mut Client,
        fresh: bool,
        adapter: usize,
        proc: usize,
        first_use: usize,
        wall: f64,
    ) {
        let info = &self.fdinfo;
        for counter in &info.counters {
            if !matches!(counter.kind, CounterKind::Ns | CounterKind::Cycles) {
                continue;
            }
            let name = &self.buf[counter.key.clone()];
            let raw = std::str::from_utf8(name).unwrap_or("?");
            let other = |kind| {
                info.counters
                    .iter()
                    .find(|o| o.kind == kind && self.buf[o.key.clone()] == *name)
                    .map(|o| o.value)
            };
            let capacity = other(CounterKind::Capacity).unwrap_or(1).max(1) as f64;
            let total = other(CounterKind::TotalCycles).unwrap_or(0);
            let max_freq = other(CounterKind::MaxFreq);
            let Some(at) = client.engines.iter().position(|e| &*e.0 == raw) else {
                client.engines.push((raw.into(), counter.value, total));
                continue;
            };
            let (_, last, last_total) = &mut client.engines[at];
            // A counter may step back for a moment; the larger value holds until
            // it catches up.
            if counter.value < *last {
                continue;
            }
            let busy = (counter.value - *last) as f64;
            let fraction = match counter.kind {
                CounterKind::Ns if wall > 0.0 => busy / (wall * 1e9),
                CounterKind::Cycles if total > *last_total => busy / (total - *last_total) as f64,
                CounterKind::Cycles => match max_freq {
                    Some(hz) if hz > 0 && wall > 0.0 => busy / (hz as f64 * wall),
                    _ => 0.0,
                },
                _ => 0.0,
            };
            let share = fraction / capacity * 100.0;
            *last = counter.value;
            *last_total = total;
            if fresh || !share.is_finite() {
                continue;
            }
            let engine = intern(&mut self.engine_names, raw);
            let gpu = &mut self.adapters[adapter];
            match gpu
                .engines
                .iter_mut()
                .find(|(n, _)| Arc::ptr_eq(n, &engine))
            {
                Some((_, sum)) => *sum += share,
                None => gpu.engines.push((Arc::clone(&engine), share)),
            }
            if share <= 0.0 {
                continue;
            }
            let mine = self.uses[first_use..]
                .iter_mut()
                .find(|u| u.adapter == adapter && Arc::ptr_eq(&u.engine, &engine));
            match mine {
                Some(u) => u.share += share,
                None => self.uses.push(Use {
                    proc,
                    adapter,
                    engine,
                    share,
                }),
            }
        }
    }

    /// Which of a process's open files are on a GPU's device node. False when
    /// this user may not list them.
    fn list_files(&mut self, pid: u32, fds: &mut Vec<u32>) -> bool {
        fds.clear();
        self.set_path(pid, "fd", u32::MAX);
        let dir = match std::fs::read_dir(&self.path) {
            Ok(dir) => dir,
            Err(e) => return e.kind() != std::io::ErrorKind::PermissionDenied,
        };
        for entry in dir.flatten() {
            let Some(fd) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            if self.node_of(pid, fd).is_some() {
                fds.push(fd);
            }
        }
        true
    }

    /// The GPU device node a process's open file is on, if it is on one.
    fn node_of(&mut self, pid: u32, fd: u32) -> Option<Node> {
        self.set_path(pid, "fd/", fd);
        let target = std::fs::read_link(&self.path).ok()?;
        let node = target.to_str()?.strip_prefix("/dev/dri/")?;
        if let Some(&n) = self.nodes.get(node) {
            return Some(n);
        }
        // A device added since the adapters were found.
        if self.pass >= self.next_discover {
            self.discover();
        }
        self.nodes.get(node).copied()
    }

    /// `<proc>/<pid>/<file><fd>`, or without the fd when it is `u32::MAX`.
    fn set_path(&mut self, pid: u32, file: &str, fd: u32) {
        use std::fmt::Write as _;
        self.path.clear();
        // Writing to a String cannot fail.
        let _ = write!(self.path, "{}/{pid}/{file}", self.proc_root.display());
        if fd != u32::MAX {
            let _ = write!(self.path, "{fd}");
        }
    }

    /// Find the adapters behind the device nodes. Adapters already known keep
    /// their facts and their place.
    fn discover(&mut self) {
        self.next_discover = self.pass + DISCOVER_PASSES;
        let drm = self.sys_root.join("class/drm");
        let Ok(dir) = std::fs::read_dir(&drm) else {
            return;
        };
        // Each device, with the lowest card number among its nodes and its nodes.
        let mut devices: Vec<(PathBuf, u32, Vec<String>)> = Vec::new();
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let number = name
                .strip_prefix("card")
                .or_else(|| name.strip_prefix("renderD"))
                .and_then(|n| n.parse::<u32>().ok());
            let Some(number) = number else {
                continue;
            };
            let Ok(device) = std::fs::canonicalize(drm.join(&name).join("device")) else {
                continue;
            };
            // Render nodes count from 128; they sort after the cards.
            let order = if name.starts_with("card") {
                number
            } else {
                number + 1000
            };
            match devices.iter_mut().find(|d| d.0 == device) {
                Some(d) => {
                    d.1 = d.1.min(order);
                    d.2.push(name);
                }
                None => devices.push((device, order, vec![name])),
            }
        }
        devices.sort_by_key(|d| d.1);

        let old = std::mem::take(&mut self.adapters);
        self.nodes.clear();
        for (device, _, names) in devices {
            let driver = std::fs::read_link(device.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            if UNREAD_DRIVERS.contains(&driver.as_str()) {
                for n in names {
                    self.nodes.insert(n, Node::LeftOut);
                }
                continue;
            }
            let index = self.adapters.len();
            let id = fnv1a(device.as_os_str().as_encoded_bytes());
            let info = match old.iter().find(|a| a.info.id == id) {
                Some(a) if a.info.name == format!("GPU {index}") => Arc::clone(&a.info),
                _ => Arc::new(self.info(id, index, &device, &driver)),
            };
            let file = |name: &str| {
                let p = device.join(name);
                p.exists().then_some(p)
            };
            self.adapters.push(Adapter {
                info,
                busy_file: file("gpu_busy_percent"),
                vram_used_file: file("mem_info_vram_used"),
                gtt_used_file: file("mem_info_gtt_used"),
                engines: Vec::new(),
                dedicated: 0,
                shared: 0,
            });
            for n in names {
                self.nodes.insert(n, Node::Gpu(index));
            }
        }
    }

    /// An adapter's facts.
    fn info(&self, id: u64, index: usize, device: &Path, driver: &str) -> GpuInfo {
        let hex = |file: &str| {
            fs::read_trimmed(device.join(file))
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        };
        let pci = hex("vendor").zip(hex("device"));
        let adapter = pci
            .and_then(|(v, d)| {
                PCI_IDS
                    .iter()
                    .find_map(|p| std::fs::read_to_string(p).ok())
                    .and_then(|ids| pci_name(&ids, v, d))
                    .or_else(|| Some(format!("{} GPU ({v:04x}:{d:04x})", vendor_short(v)?)))
            })
            .unwrap_or_else(|| driver.to_owned());
        let bytes = |file: &str| {
            fs::read_u64(device.join(file))
                .filter(|&b| b > 0)
                .map(Bytes)
        };
        let driver_version =
            fs::read_trimmed(self.sys_root.join("module").join(driver).join("version"))
                // A driver built into the kernel is the kernel's version.
                .or_else(|| fs::read_trimmed(self.proc_root.join("sys/kernel/osrelease")));
        GpuInfo {
            id,
            name: format!("GPU {index}"),
            adapter,
            dedicated_total: bytes("mem_info_vram_total"),
            shared_total: bytes("mem_info_gtt_total"),
            driver_version: (!driver.is_empty()).then(|| match driver_version {
                Some(v) => format!("{driver} {v}"),
                None => driver.to_owned(),
            }),
            driver_date: None,
            location: pci.and(
                device
                    .file_name()
                    .and_then(|n| pci_location(&n.to_string_lossy())),
            ),
            software: SOFTWARE_DRIVERS.contains(&driver),
        }
    }

    /// Turn this pass's sums into samples, `GPU 0` first.
    fn publish(&mut self, out: &mut Vec<GpuSample>) {
        for a in &mut self.adapters {
            a.engines
                .sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
            let busiest = a.engines.first().map_or(0.0, |e| e.1);
            let measured = a
                .busy_file
                .as_ref()
                .and_then(fs::read_u64)
                .map_or(0.0, |b| b as f64);
            let read = |f: &Option<PathBuf>| f.as_ref().and_then(fs::read_u64);
            out.push(GpuSample {
                info: Arc::clone(&a.info),
                utilization: Percent(busiest.max(measured) as f32).clamped(100.0),
                engines: a
                    .engines
                    .iter()
                    .map(|(name, share)| EngineSample {
                        name: Arc::clone(name),
                        usage: Percent(*share as f32).clamped(100.0),
                    })
                    .collect(),
                dedicated_used: Bytes(read(&a.vram_used_file).unwrap_or(a.dedicated)),
                shared_used: Bytes(read(&a.gtt_used_file).unwrap_or(a.shared)),
            });
        }
    }

    /// Each process's busiest engine, with its `GPU 0 - 3D` label, and its share of
    /// each hardware adapter's busiest engine. Runs after [`Self::publish`], which
    /// leaves each adapter's engines busiest first.
    fn attribute(&mut self, processes: &mut [ProcessSample]) {
        for u in &self.uses {
            let a = &self.adapters[u.adapter];
            let p = &mut processes[u.proc];
            let share = (u.share as f32).clamp(0.0, 100.0);
            let on_busiest = !a.info.software
                && a.engines
                    .first()
                    .is_some_and(|(n, _)| Arc::ptr_eq(n, &u.engine));
            if on_busiest {
                p.gpu_share = Percent(p.gpu_share.get() + share);
            }
            if p.gpu.is_some_and(|s| share <= s.get()) {
                continue;
            }
            let label = self
                .labels
                .entry((u.adapter, Arc::clone(&u.engine)))
                .or_insert_with(|| format!("{} - {}", a.info.name, u.engine).into());
            p.gpu = Some(Percent(share));
            p.gpu_engine = Some(Arc::clone(label));
        }
    }
}

/// A client's dedicated and shared memory, in bytes: in each region the best
/// measure the driver gives, resident before the older name for it before total.
fn client_memory(buf: &[u8], regions: &[Region]) -> (u64, u64) {
    let (mut dedicated, mut shared) = (0, 0);
    for r in regions {
        let name = &buf[r.name.clone()];
        // A better measure of the same region is there: this one is not used.
        if regions
            .iter()
            .any(|o| o.kind < r.kind && buf[o.name.clone()] == *name)
        {
            continue;
        }
        match region_class(std::str::from_utf8(name).unwrap_or("")) {
            Some(true) => dedicated += r.bytes,
            Some(false) => shared += r.bytes,
            None => {}
        }
    }
    (dedicated, shared)
}

fn intern(names: &mut HashMap<Box<str>, Arc<str>>, raw: &str) -> Arc<str> {
    if let Some(n) = names.get(raw) {
        return Arc::clone(n);
    }
    let name: Arc<str> = display_name(raw).into();
    names.insert(raw.into(), Arc::clone(&name));
    name
}

/// What a vendor is called before its products' names.
fn vendor_short(vendor: u16) -> Option<&'static str> {
    Some(match vendor {
        0x1002 | 0x1022 => "AMD",
        0x8086 => "Intel",
        0x10de => "NVIDIA",
        0x1414 => "Microsoft",
        0x15ad => "VMware",
        0x1af4 => "Virtio",
        0x5143 => "Qualcomm",
        _ => return None,
    })
}

/// An adapter's name from the PCI ID database: the marketing name in brackets
/// where there is one (`Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]` becomes
/// `AMD Radeon RX 7900 XT/7900 XTX/7900M`), after the vendor's short name.
fn pci_name(ids: &str, vendor: u16, device: u16) -> Option<String> {
    let vendor_hex = format!("{vendor:04x}  ");
    let device_hex = format!("\t{device:04x}  ");
    let mut lines = ids.lines();
    let vendor_name = lines.find_map(|l| l.strip_prefix(&vendor_hex))?;
    let name = lines
        .take_while(|l| l.starts_with('\t') || l.starts_with('#') || l.is_empty())
        .find_map(|l| l.strip_prefix(&device_hex))?
        .trim();
    let product = match (name.rfind('['), name.ends_with(']')) {
        (Some(open), true) => &name[open + 1..name.len() - 1],
        _ => name,
    };
    let short = vendor_short(vendor).unwrap_or(vendor_name.trim());
    Some(if product.starts_with(short) {
        product.to_owned()
    } else {
        format!("{short} {product}")
    })
}

/// `0000:03:00.0` as `PCI bus 3, device 0, function 0`.
fn pci_location(slot: &str) -> Option<String> {
    let mut parts = slot.rsplitn(3, ':');
    let dev_fn = parts.next()?;
    let bus = u8::from_str_radix(parts.next()?, 16).ok()?;
    let (dev, func) = dev_fn.split_once('.')?;
    let dev = u8::from_str_radix(dev, 16).ok()?;
    let func = u8::from_str_radix(func, 16).ok()?;
    Some(format!("PCI bus {bus}, device {dev}, function {func}"))
}

/// A stable id for an adapter, from its device path.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::process::ProcessStatic;
    use std::os::unix::fs::symlink;

    fn names(buf: &[u8], info: &FdInfo) -> Vec<(String, CounterKind, u64)> {
        info.counters
            .iter()
            .map(|c| {
                (
                    String::from_utf8_lossy(&buf[c.key.clone()]).into_owned(),
                    c.kind,
                    c.value,
                )
            })
            .collect()
    }

    #[test]
    fn an_i915_fdinfo_parses() {
        let buf = b"pos:\t0\nflags:\t02100002\nmnt_id:\t26\ndrm-driver:\ti915\n\
            drm-client-id:\t7\ndrm-pdev:\t0000:00:02.0\n\
            drm-engine-render:\t9288864723 ns\ndrm-engine-copy:\t2035071108 ns\n\
            drm-engine-video:\t0 ns\ndrm-engine-capacity-video:\t2\n\
            drm-total-system0:\t216 KiB\ndrm-resident-system0:\t200 KiB\n\
            drm-total-stolen-system0:\t0\n";
        let mut info = FdInfo::default();
        parse_fdinfo(buf, &mut info);
        assert!(info.drm);
        assert_eq!(info.client_id, Some(7));
        assert_eq!(
            names(buf, &info),
            [
                ("render".into(), CounterKind::Ns, 9_288_864_723),
                ("copy".into(), CounterKind::Ns, 2_035_071_108),
                ("video".into(), CounterKind::Ns, 0),
                ("video".into(), CounterKind::Capacity, 2),
            ]
        );
        // Resident wins over total; stolen memory is neither kind.
        assert_eq!(client_memory(buf, &info.regions), (0, 200 << 10));
    }

    #[test]
    fn an_xe_and_an_amdgpu_fdinfo_parse() {
        let buf = b"drm-driver:\txe\ndrm-client-id:\t3\n\
            drm-cycles-rcs:\t28257900\ndrm-total-cycles-rcs:\t7655183225\n\
            drm-engine-capacity-ccs:\t4\ndrm-resident-vram0:\t2 MiB\ndrm-total-gtt:\t4 KiB\n";
        let mut info = FdInfo::default();
        parse_fdinfo(buf, &mut info);
        assert_eq!(
            names(buf, &info),
            [
                ("rcs".into(), CounterKind::Cycles, 28_257_900),
                ("rcs".into(), CounterKind::TotalCycles, 7_655_183_225),
                ("ccs".into(), CounterKind::Capacity, 4),
            ]
        );
        assert_eq!(client_memory(buf, &info.regions), (2 << 20, 4 << 10));

        let buf = b"drm-driver:\tamdgpu\ndrm-client-id:\t9\ndrm-memory-vram:\t1024 KiB\n\
            drm-memory-gtt:\t8 KiB\ndrm-memory-cpu:\t4 KiB\ndrm-total-vram:\t4096 KiB\n\
            drm-engine-gfx:\t1000 ns\ndrm-maxfreq-gfx:\t800 MHz\n";
        parse_fdinfo(buf, &mut info);
        assert_eq!(client_memory(buf, &info.regions), (1 << 20, 8 << 10));
        assert!(info
            .counters
            .iter()
            .any(|c| c.kind == CounterKind::MaxFreq && c.value == 800_000_000));
        // Not a DRM client at all.
        parse_fdinfo(b"pos:\t0\nflags:\t0100002\n", &mut info);
        assert!(!info.drm);
    }

    #[test]
    fn pci_names_read_as_products() {
        let ids = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\
            \t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]\n\
            \t\t1002 0e3b  Radeon RX 7900 GRE\n\
            8086  Intel Corporation\n\t3e92  CoffeeLake-S GT2 [UHD Graphics 630]\n\
            \t7d55  Meteor Lake-P [Intel Arc Graphics]\n\t1234  Plain Device\n\
            1234  Someone Else\n\t744c  Wrong One\n";
        assert_eq!(
            pci_name(ids, 0x1002, 0x744c).as_deref(),
            Some("AMD Radeon RX 7900 XT/7900 XTX/7900M")
        );
        assert_eq!(
            pci_name(ids, 0x8086, 0x3e92).as_deref(),
            Some("Intel UHD Graphics 630")
        );
        assert_eq!(
            pci_name(ids, 0x8086, 0x7d55).as_deref(),
            Some("Intel Arc Graphics")
        );
        assert_eq!(
            pci_name(ids, 0x8086, 0x1234).as_deref(),
            Some("Intel Plain Device")
        );
        assert_eq!(pci_name(ids, 0x8086, 0x744c), None);
        assert_eq!(
            pci_location("0000:0a:00.1").as_deref(),
            Some("PCI bus 10, device 0, function 1")
        );
    }

    /// A fake `/proc` and `/sys` with two adapters: an amdgpu card (PCI, with its
    /// own busy and memory files) and a firmware framebuffer.
    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ot-probe-gpu-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let pci = root.join("sys/devices/pci0000:00/0000:03:00.0");
            let fb = root.join("sys/devices/platform/simple-framebuffer.0");
            for (dir, driver) in [(&pci, "amdgpu"), (&fb, "simple-framebuffer")] {
                std::fs::create_dir_all(dir).unwrap();
                let drivers = root.join("sys/bus/drivers").join(driver);
                std::fs::create_dir_all(&drivers).unwrap();
                symlink(&drivers, dir.join("driver")).unwrap();
            }
            for (f, v) in [
                ("vendor", "0x1002"),
                ("device", "0x744c"),
                ("gpu_busy_percent", "0"),
                ("mem_info_vram_total", "25753026560"),
                ("mem_info_vram_used", "1073741824"),
            ] {
                std::fs::write(pci.join(f), format!("{v}\n")).unwrap();
            }
            let drm = root.join("sys/class/drm");
            for (node, dev) in [("card1", &pci), ("renderD128", &pci), ("card0", &fb)] {
                std::fs::create_dir_all(drm.join(node)).unwrap();
                symlink(dev, drm.join(node).join("device")).unwrap();
            }
            std::fs::create_dir_all(drm.join("card1-DP-1")).unwrap();
            std::fs::create_dir_all(root.join("proc/sys/kernel")).unwrap();
            std::fs::write(root.join("proc/sys/kernel/osrelease"), "6.18.0\n").unwrap();
            Self { root }
        }

        /// Give `pid` an open file `fd` pointing at `target`, with `fdinfo`.
        fn file(&self, pid: u32, fd: u32, target: &str, fdinfo: &str) {
            let dir = self.root.join(format!("proc/{pid}"));
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            std::fs::create_dir_all(dir.join("fdinfo")).unwrap();
            let link = dir.join(format!("fd/{fd}"));
            let _ = std::fs::remove_file(&link);
            symlink(target, link).unwrap();
            std::fs::write(dir.join(format!("fdinfo/{fd}")), fdinfo).unwrap();
        }

        fn probe(&self) -> GpuProbe {
            GpuProbe::at(&self.root.join("proc"), &self.root.join("sys")).expect("an adapter")
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn process(pid: u32, handles: u32) -> ProcessSample {
        ProcessSample {
            statics: Arc::new(ProcessStatic {
                key: ProcessKey::new(pid, 1),
                ..ProcessStatic::default()
            }),
            handles,
            ..ProcessSample::default()
        }
    }

    fn client(id: u32, render_ns: u64, copy_ns: u64, vram_kib: u64) -> String {
        format!(
            "pos:\t0\ndrm-driver:\tamdgpu\ndrm-client-id:\t{id}\n\
             drm-engine-gfx:\t{render_ns} ns\ndrm-engine-dma:\t{copy_ns} ns\n\
             drm-memory-vram:\t{vram_kib} KiB\ndrm-memory-gtt:\t64 KiB\n"
        )
    }

    #[test]
    fn adapters_are_found_named_and_ordered() {
        let t = Tree::new("adapters");
        let p = t.probe();
        let infos: Vec<_> = p.adapters.iter().map(|a| Arc::clone(&a.info)).collect();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].name, "GPU 0");
        assert!(infos[0].software, "the framebuffer is card0");
        assert_eq!(infos[1].name, "GPU 1");
        assert!(!infos[1].software);
        assert_eq!(
            infos[1].location.as_deref(),
            Some("PCI bus 3, device 0, function 0")
        );
        assert_eq!(infos[1].dedicated_total, Some(Bytes(25_753_026_560)));
        assert_eq!(infos[1].driver_version.as_deref(), Some("amdgpu 6.18.0"));
        assert_eq!(p.nodes.get("renderD128"), Some(&Node::Gpu(1)));
        assert_eq!(p.nodes.get("card1"), Some(&Node::Gpu(1)));
        assert!(!p.nodes.contains_key("card1-DP-1"));
    }

    #[test]
    fn clients_add_up_to_the_engines_and_processes_share_the_busiest() {
        let t = Tree::new("clients");
        // A game on the render node, a compositor on the card node, and the
        // compositor's client passed to the game too: counted once.
        t.file(10, 5, "/dev/dri/renderD128", &client(1, 0, 0, 1024));
        t.file(10, 6, "/dev/dri/card1", &client(2, 0, 0, 0));
        t.file(10, 7, "/tmp/not-a-gpu", "pos:\t0\n");
        t.file(20, 3, "/dev/dri/card1", &client(2, 0, 0, 0));
        let mut p = t.probe();
        let mut procs = vec![process(10, 3), process(20, 1), process(30, 0)];
        let mut out = Vec::new();
        p.sample_over(&mut procs, &mut out, 0.0);
        assert_eq!(out.len(), 2);
        // The game's resident VRAM and GTT, the compositor's GTT counted once.
        assert_eq!(procs[0].gpu_dedicated, Bytes(1 << 20));
        assert_eq!(procs[0].gpu_shared, Bytes(128 << 10));
        assert_eq!(procs[1].gpu_shared, Bytes::ZERO);
        assert_eq!(procs[0].gpu, None, "no rate on the first pass");

        // One second on: the game used 0.5 s of 3D and 0.1 s of copy, the
        // compositor 0.2 s of 3D.
        t.file(
            10,
            5,
            "/dev/dri/renderD128",
            &client(1, 500_000_000, 100_000_000, 1024),
        );
        t.file(10, 6, "/dev/dri/card1", &client(2, 200_000_000, 0, 0));
        t.file(20, 3, "/dev/dri/card1", &client(2, 200_000_000, 0, 0));
        procs = vec![process(10, 3), process(20, 1), process(30, 0)];
        p.sample_over(&mut procs, &mut out, 1.0);
        let gpu = &out[1];
        assert!(
            (gpu.utilization.get() - 70.0).abs() < 1e-3,
            "{:?}",
            gpu.utilization
        );
        assert_eq!(&*gpu.engines[0].name, "3D");
        assert!((gpu.engines[1].usage.get() - 10.0).abs() < 1e-3);
        // The driver's own reading of memory in use wins over the clients' sum.
        assert_eq!(gpu.dedicated_used, Bytes(1 << 30));
        assert_eq!(out[0].engines, Vec::<EngineSample>::new());
        let game = &procs[0];
        assert!(
            (game.gpu_share.get() - 70.0).abs() < 1e-3,
            "both clients are the game's"
        );
        assert!((game.gpu.unwrap().get() - 70.0).abs() < 1e-3);
        assert_eq!(game.gpu_engine.as_deref(), Some("GPU 1 - 3D"));
        assert_eq!(
            procs[1].gpu, None,
            "the shared client went to its first owner"
        );
    }

    #[test]
    fn the_driver_reading_tops_up_what_no_visible_process_used() {
        let t = Tree::new("busy");
        t.file(10, 5, "/dev/dri/renderD128", &client(1, 0, 0, 0));
        let mut p = t.probe();
        let mut out = Vec::new();
        p.sample_over(&mut [process(10, 1)], &mut out, 0.0);
        std::fs::write(
            t.root
                .join("sys/devices/pci0000:00/0000:03:00.0/gpu_busy_percent"),
            "40\n",
        )
        .unwrap();
        t.file(10, 5, "/dev/dri/renderD128", &client(1, 100_000_000, 0, 0));
        let mut procs = [process(10, 1)];
        p.sample_over(&mut procs, &mut out, 1.0);
        assert!((out[1].utilization.get() - 40.0).abs() < 1e-3);
        assert!((procs[0].gpu_share.get() - 10.0).abs() < 1e-3);
    }

    #[test]
    fn engines_count_cycles_and_capacity() {
        let t = Tree::new("cycles");
        let xe = |busy: u64, total: u64| {
            format!(
                "drm-driver:\txe\ndrm-client-id:\t4\ndrm-cycles-ccs:\t{busy}\n\
                 drm-total-cycles-ccs:\t{total}\ndrm-engine-capacity-ccs:\t4\n"
            )
        };
        t.file(10, 5, "/dev/dri/renderD128", &xe(0, 1000));
        let mut p = t.probe();
        let mut out = Vec::new();
        p.sample_over(&mut [process(10, 1)], &mut out, 0.0);
        // Busy for 2000 of 1000 total cycles: two of the four compute engines.
        t.file(10, 5, "/dev/dri/renderD128", &xe(2000, 2000));
        let mut procs = [process(10, 1)];
        p.sample_over(&mut procs, &mut out, 1.0);
        assert_eq!(&*out[1].engines[0].name, "Compute");
        assert!((out[1].utilization.get() - 50.0).abs() < 1e-3);
        // A counter that steps back is held, not read as a huge jump.
        t.file(10, 5, "/dev/dri/renderD128", &xe(1500, 3000));
        p.sample_over(&mut procs, &mut out, 1.0);
        assert_eq!(out[1].utilization, Percent::ZERO);
    }

    /// What a pass costs against this machine's real processes, with a fake GPU
    /// so the probe runs. Run under load: `cargo test -p ot-probe --release
    /// gpu_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn gpu_cost() {
        let t = Tree::new("cost");
        let mut gpu = GpuProbe::at(Path::new("/proc"), &t.root.join("sys")).unwrap();
        let mut procs = super::super::process::ProcessProbe::new(1, None, None);
        let (mut out, mut threads, mut gpus) = (Vec::new(), Vec::new(), Vec::new());
        let mut times = Vec::new();
        for _ in 0..30 {
            out.clear();
            threads.clear();
            procs.sample(&mut out, &mut threads);
            let start = Instant::now();
            gpu.sample(&mut out, &mut gpus);
            times.push(start.elapsed());
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let first = times.remove(0);
        times.sort();
        let mean = times.iter().sum::<std::time::Duration>() / 29;
        println!(
            "{} processes: first pass {first:?}; then mean {mean:?}, median {:?}, worst {:?}",
            out.len(),
            times[14],
            times[28],
        );
    }

    #[test]
    fn this_machine_does_not_crash() {
        if let Some(mut p) = GpuProbe::new() {
            let mut out = Vec::new();
            let mut procs = vec![process(std::process::id(), 1)];
            p.sample(&mut procs, &mut out);
            p.sample(&mut procs, &mut out);
            assert_ne!(out.len(), 0);
        }
    }
}
