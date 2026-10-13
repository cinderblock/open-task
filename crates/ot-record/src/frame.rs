//! One frame: a snapshot with its shared values replaced by table ids, and, in a
//! delta frame, its processes and threads as changes from the frame before.
//!
//! [`Body`] borrows the snapshot when writing (`Cow::Borrowed`) and owns its parts
//! when read back, so one definition serves both directions and cannot drift.
//!
//! From format version 2 on, the body is followed by an [`Extra`]: what was added
//! to the samples since version 1, kept apart so that a version 1 body still
//! decodes as it is (postcard writes a struct as its fields one after another, so
//! a version 2 payload is the body's bytes and then the extra's). Version 3 adds a
//! [`GpuExtra`] after the [`Extra`] the same way.
//!
//! A table's values are written whole, so a type that gains a field keeps its old
//! layout for older files: [`AdapterInfoV3`], [`ProcessStaticV4`].

use ot_model::Percent;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ot_model::battery::BatterySample;
use ot_model::cpu::CpuSample;
use ot_model::device::{AdapterInfo, AdapterSample, DiskInfo, DiskSample, LinkKind, VolumeSample};
use ot_model::gpu::{GpuInfo, GpuSample};
use ot_model::hardware::Hardware;
use ot_model::memory::MemorySample;
use ot_model::process::{Architecture, Integrity, ProcessSample, ProcessStatic, WindowInfo};
use ot_model::service::{ServiceEntry, ServiceInfo};
use ot_model::session::SessionInfo;
use ot_model::snapshot::Snapshot;
use ot_model::thread::ThreadSample;
use ot_model::{Capabilities, ProcessKey, Tick};
use serde::{Deserialize, Serialize};

use crate::delta::{self, ProcessDelta, ThreadDelta};
use crate::format::Kind;
use crate::table;
use crate::Error;

/// The writer's tables, one per shared type.
#[derive(Debug, Default)]
pub(crate) struct Tables {
    pub statics: table::Writer<ProcessStatic>,
    pub windows: table::Writer<WindowInfo>,
    pub process_services: table::Writer<[ServiceInfo]>,
    pub disks: table::Writer<DiskInfo>,
    pub adapters: table::Writer<AdapterInfo>,
    pub gpus: table::Writer<GpuInfo>,
    pub services: table::Writer<[ServiceEntry]>,
}

impl Tables {
    /// Drop the values nothing but the tables still hold.
    pub(crate) fn sweep(&mut self) {
        self.statics.sweep();
        self.windows.sweep();
        self.process_services.sweep();
        self.disks.sweep();
        self.adapters.sweep();
        self.gpus.sweep();
        self.services.sweep();
    }
}

/// An adapter's facts as format versions 1 to 3 write them: before
/// [`AdapterInfo::parent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AdapterInfoV3 {
    id: u64,
    name: String,
    adapter: String,
    kind: LinkKind,
    hardware: bool,
    addresses: Vec<std::net::IpAddr>,
    dns_suffix: Option<String>,
    mac: Option<String>,
}

impl From<AdapterInfoV3> for AdapterInfo {
    fn from(a: AdapterInfoV3) -> Self {
        Self {
            id: a.id,
            name: a.name,
            adapter: a.adapter,
            kind: a.kind,
            hardware: a.hardware,
            addresses: a.addresses,
            dns_suffix: a.dns_suffix,
            mac: a.mac,
            parent: None,
        }
    }
}

impl From<&AdapterInfo> for AdapterInfoV3 {
    fn from(a: &AdapterInfo) -> Self {
        Self {
            id: a.id,
            name: a.name.clone(),
            adapter: a.adapter.clone(),
            kind: a.kind,
            hardware: a.hardware,
            addresses: a.addresses.clone(),
            dns_suffix: a.dns_suffix.clone(),
            mac: a.mac.clone(),
        }
    }
}

/// A process's facts as format versions 1 to 4 write them: before
/// [`ProcessStatic::parent_pid`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ProcessStaticV4 {
    key: ProcessKey,
    parent: Option<ProcessKey>,
    name: String,
    image_path: Option<String>,
    command_line: Option<String>,
    user: Option<String>,
    integrity: Integrity,
    started_unix_ms: Option<i64>,
    session_id: u32,
    architecture: Architecture,
    description: Option<String>,
    company: Option<String>,
    package: Option<String>,
}

impl From<ProcessStaticV4> for ProcessStatic {
    fn from(p: ProcessStaticV4) -> Self {
        Self {
            key: p.key,
            parent: p.parent,
            name: p.name,
            image_path: p.image_path,
            command_line: p.command_line,
            user: p.user,
            integrity: p.integrity,
            started_unix_ms: p.started_unix_ms,
            session_id: p.session_id,
            architecture: p.architecture,
            description: p.description,
            company: p.company,
            package: p.package,
            parent_pid: None,
        }
    }
}

impl From<&ProcessStatic> for ProcessStaticV4 {
    fn from(p: &ProcessStatic) -> Self {
        Self {
            key: p.key,
            parent: p.parent,
            name: p.name.clone(),
            image_path: p.image_path.clone(),
            command_line: p.command_line.clone(),
            user: p.user.clone(),
            integrity: p.integrity,
            started_unix_ms: p.started_unix_ms,
            session_id: p.session_id,
            architecture: p.architecture,
            description: p.description.clone(),
            company: p.company.clone(),
            package: p.package.clone(),
        }
    }
}

/// The reader's tables.
#[derive(Debug)]
pub(crate) struct Readers {
    pub statics: table::Reader<ProcessStatic>,
    pub windows: table::Reader<WindowInfo>,
    pub process_services: table::Reader<[ServiceInfo]>,
    pub disks: table::Reader<DiskInfo>,
    pub adapters: table::Reader<AdapterInfo>,
    pub gpus: table::Reader<GpuInfo>,
    pub services: table::Reader<[ServiceEntry]>,
    /// The file's format version, which decides how some tables decode.
    version: u16,
}

impl Readers {
    /// Empty tables for a file of format `version`.
    pub(crate) fn new(version: u16) -> Self {
        Self {
            statics: table::Reader::default(),
            windows: table::Reader::default(),
            process_services: table::Reader::with_empty(),
            disks: table::Reader::default(),
            adapters: table::Reader::default(),
            gpus: table::Reader::default(),
            services: table::Reader::with_empty(),
            version,
        }
    }

    /// Add one decompressed table record.
    pub(crate) fn load(&mut self, kind: Kind, raw: &[u8]) -> Result<(), Error> {
        match kind {
            Kind::Statics if self.version >= 5 => self.statics.insert(postcard::from_bytes(raw)?),
            Kind::Statics => {
                let old: Vec<(u32, ProcessStaticV4)> = postcard::from_bytes(raw)?;
                self.statics.insert(
                    old.into_iter()
                        .map(|(id, p)| (id, Arc::new(ProcessStatic::from(p))))
                        .collect(),
                );
            }
            Kind::Windows => self.windows.insert(postcard::from_bytes(raw)?),
            Kind::ProcessServices => self.process_services.insert(postcard::from_bytes(raw)?),
            Kind::Disks => self.disks.insert(postcard::from_bytes(raw)?),
            Kind::Adapters if self.version >= 4 => {
                self.adapters.insert(postcard::from_bytes(raw)?);
            }
            Kind::Adapters => {
                let old: Vec<(u32, AdapterInfoV3)> = postcard::from_bytes(raw)?;
                self.adapters.insert(
                    old.into_iter()
                        .map(|(id, a)| (id, Arc::new(AdapterInfo::from(a))))
                        .collect(),
                );
            }
            Kind::Gpus => self.gpus.insert(postcard::from_bytes(raw)?),
            Kind::Services => self.services.insert(postcard::from_bytes(raw)?),
            _ => return Err(Error::corrupt(format!("{kind:?} is not a table record"))),
        }
        Ok(())
    }

    /// Distinct process statics held: how many processes the recording has seen.
    pub(crate) fn statics_len(&self) -> usize {
        self.statics.len()
    }
}

/// A frame's payload after the tick-and-time prefix, before compression.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Body<'a> {
    tick: Tick,
    taken_at: Option<SystemTime>,
    interval: Duration,
    probe_cost: Duration,
    cpu: Cow<'a, CpuSample>,
    memory: MemorySample,
    disks: Cow<'a, [DiskSample]>,
    disk_infos: Vec<u32>,
    adapters: Cow<'a, [AdapterSample]>,
    adapter_infos: Vec<u32>,
    gpus: Cow<'a, [GpuSample]>,
    gpu_infos: Vec<u32>,
    battery: Cow<'a, Option<BatterySample>>,
    volumes: Cow<'a, [VolumeSample]>,
    sessions: Cow<'a, [SessionInfo]>,
    services: u32,
    capabilities: Capabilities,
    processes: Vec<Row<'a>>,
}

/// One process in a frame. Its thread rows travel with it, so the snapshot's
/// thread list is rebuilt in process order on decode.
#[derive(Debug, Serialize, Deserialize)]
enum Row<'a> {
    /// The whole sample: a keyframe, or a process not in the previous frame.
    Full {
        statics: u32,
        window: Option<u32>,
        services: u32,
        sample: Cow<'a, ProcessSample>,
        threads: Cow<'a, [ThreadSample]>,
    },
    /// Changes from the same process (by [`ProcessKey`]) in the previous frame.
    Delta {
        statics: u32,
        window: Option<u32>,
        services: u32,
        delta: ProcessDelta,
        threads: Threads<'a>,
    },
}

/// What a frame holds beyond its [`Body`], from format version 2 on.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Extra {
    /// Each process's private working set, in the body's order: for a delta row
    /// its change from the frame before, for a full row the whole of it.
    private_working_set: Vec<i64>,
}

/// What a frame holds beyond its [`Extra`], from format version 3 on: each
/// process's use of the GPUs, in the body's order, for a delta row as its change
/// from the frame before and for a full row the whole of it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct GpuExtra {
    /// `gpu_share`'s bits, exclusive-ORed with the frame before's.
    share: Vec<u32>,
    dedicated: Vec<i64>,
    shared: Vec<i64>,
}

impl GpuExtra {
    fn len(&self) -> usize {
        self.share.len()
    }

    fn consistent(&self) -> bool {
        self.dedicated.len() == self.share.len() && self.shared.len() == self.share.len()
    }
}

/// What the [`Extra`] and [`GpuExtra`] say of one process, where the frame has
/// them: changes for a delta row, the whole for a full one.
#[derive(Debug, Clone, Copy, Default)]
struct Added {
    private: Option<i64>,
    gpu: Option<(u32, i64, i64)>,
}

impl Added {
    /// Set what these say on `sample`, as changes from `before` (a default sample
    /// for a full row). What the frame does not have stays as `sample` has it.
    fn apply(self, sample: &mut ProcessSample, before: &ProcessSample) {
        if let Some(d) = self.private {
            sample.private_working_set = delta::a_bytes(before.private_working_set, d);
        }
        if let Some((share, dedicated, shared)) = self.gpu {
            sample.gpu_share = Percent(f32::from_bits(before.gpu_share.get().to_bits() ^ share));
            sample.gpu_dedicated = delta::a_bytes(before.gpu_dedicated, dedicated);
            sample.gpu_shared = delta::a_bytes(before.gpu_shared, shared);
        }
    }
}

/// A delta row's threads.
#[derive(Debug, Serialize, Deserialize)]
enum Threads<'a> {
    /// The same threads as before, each as a change.
    Same(Vec<ThreadDelta>),
    /// A thread started or ended: the whole list.
    Changed(Cow<'a, [ThreadSample]>),
}

/// The thread rows a process names, or none if the range is out of bounds.
fn thread_rows<'a>(snap: &'a Snapshot, p: &ProcessSample) -> &'a [ThreadSample] {
    snap.threads.get(p.thread_range()).unwrap_or(&[])
}

/// Index of each process in `snap` by identity.
fn by_key(snap: &Snapshot) -> HashMap<ProcessKey, usize> {
    snap.processes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.key(), i))
        .collect()
}

/// Encode `snap`, as a keyframe when `prev` is `None` and as changes from `prev`
/// otherwise. New shared values are numbered in `tables` and left pending there.
pub(crate) fn encode<'a>(
    snap: &'a Snapshot,
    prev: Option<&'a Snapshot>,
    tables: &mut Tables,
) -> (Body<'a>, Extra, GpuExtra) {
    let prev_index = prev.map(by_key);
    let mut extra = Extra::default();
    let mut gpu = GpuExtra::default();
    let zero = ProcessSample::default();
    let processes = snap
        .processes
        .iter()
        .map(|p| {
            let statics = tables.statics.id(&p.statics);
            let window = p.window.as_ref().map(|w| tables.windows.id(w));
            let services = tables.process_services.id_or_empty(&p.services);
            let threads = thread_rows(snap, p);
            let base = prev
                .zip(prev_index.as_ref())
                .and_then(|(prev, index)| index.get(&p.key()).map(|&i| (&prev.processes[i], prev)));
            let was = base.map_or(&zero, |(before, _)| before);
            extra.private_working_set.push(delta::d_bytes(
                p.private_working_set,
                was.private_working_set,
            ));
            gpu.share
                .push(p.gpu_share.get().to_bits() ^ was.gpu_share.get().to_bits());
            gpu.dedicated
                .push(delta::d_bytes(p.gpu_dedicated, was.gpu_dedicated));
            gpu.shared
                .push(delta::d_bytes(p.gpu_shared, was.gpu_shared));
            match base {
                Some((before, prev)) => {
                    let prev_threads = thread_rows(prev, before);
                    let threads = if delta::same_threads(threads, prev_threads) {
                        Threads::Same(
                            threads
                                .iter()
                                .zip(prev_threads)
                                .map(|(c, p)| delta::thread(c, p))
                                .collect(),
                        )
                    } else {
                        Threads::Changed(Cow::Borrowed(threads))
                    };
                    Row::Delta {
                        statics,
                        window,
                        services,
                        delta: delta::process(p, before),
                        threads,
                    }
                }
                None => Row::Full {
                    statics,
                    window,
                    services,
                    sample: Cow::Borrowed(p),
                    threads: Cow::Borrowed(threads),
                },
            }
        })
        .collect();
    let body = Body {
        tick: snap.tick,
        taken_at: snap.taken_at,
        interval: snap.interval,
        probe_cost: snap.probe_cost,
        cpu: Cow::Borrowed(&snap.cpu),
        memory: snap.memory,
        disks: Cow::Borrowed(&snap.disks),
        disk_infos: snap
            .disks
            .iter()
            .map(|d| tables.disks.id(&d.info))
            .collect(),
        adapters: Cow::Borrowed(&snap.adapters),
        adapter_infos: snap
            .adapters
            .iter()
            .map(|a| tables.adapters.id(&a.info))
            .collect(),
        gpus: Cow::Borrowed(&snap.gpus),
        gpu_infos: snap.gpus.iter().map(|g| tables.gpus.id(&g.info)).collect(),
        battery: Cow::Borrowed(&snap.battery),
        volumes: Cow::Borrowed(&snap.volumes),
        sessions: Cow::Borrowed(&snap.sessions),
        services: tables.services.id_or_empty(&snap.services),
        capabilities: snap.capabilities,
        processes,
    };
    (body, extra, gpu)
}

/// Rebuild a snapshot from `body` and, from format version 2 on, its `extra`, and
/// from version 3 its `gpu`, with `prev` the frame before it (required for a delta
/// frame) and the shared values looked up in `readers`.
pub(crate) fn decode(
    body: Body<'_>,
    extra: Option<Extra>,
    gpu: Option<&GpuExtra>,
    prev: Option<&Snapshot>,
    readers: &Readers,
    hardware: &Arc<Hardware>,
) -> Result<Snapshot, Error> {
    let base = prev.map(|prev| (prev, by_key(prev)));
    let mut processes = Vec::with_capacity(body.processes.len());
    let mut threads = Vec::new();
    let private = extra.map(|e| e.private_working_set);
    if private
        .as_ref()
        .is_some_and(|p| p.len() != body.processes.len())
    {
        return Err(Error::corrupt(format!(
            "{} processes with {} private working sets",
            body.processes.len(),
            private.as_ref().map_or(0, Vec::len)
        )));
    }
    if gpu.is_some_and(|g| !g.consistent() || g.len() != body.processes.len()) {
        return Err(Error::corrupt(format!(
            "{} processes with {} GPU rows",
            body.processes.len(),
            gpu.map_or(0, GpuExtra::len)
        )));
    }
    for (i, row) in body.processes.into_iter().enumerate() {
        let added = Added {
            private: private.as_ref().map(|p| p[i]),
            gpu: gpu.map(|g| (g.share[i], g.dedicated[i], g.shared[i])),
        };
        let (mut sample, rows) = decode_row(row, added, base.as_ref(), readers)?;
        sample.thread_first = u32::try_from(threads.len())
            .map_err(|_| Error::corrupt("more than four billion thread rows"))?;
        sample.thread_rows = u32::try_from(rows.len())
            .map_err(|_| Error::corrupt("more than four billion thread rows"))?;
        threads.extend(rows);
        processes.push(sample);
    }

    let mut disks = body.disks.into_owned();
    attach(
        &mut disks,
        &body.disk_infos,
        "disk",
        |d, info| d.info = info,
        &readers.disks,
    )?;
    let mut adapters = body.adapters.into_owned();
    attach(
        &mut adapters,
        &body.adapter_infos,
        "adapter",
        |a, info| a.info = info,
        &readers.adapters,
    )?;
    let mut gpus = body.gpus.into_owned();
    attach(
        &mut gpus,
        &body.gpu_infos,
        "GPU",
        |g, info| g.info = info,
        &readers.gpus,
    )?;

    Ok(Snapshot {
        tick: body.tick,
        taken_at: body.taken_at,
        interval: body.interval,
        probe_cost: body.probe_cost,
        cpu: body.cpu.into_owned(),
        memory: body.memory,
        processes,
        threads,
        disks,
        adapters,
        gpus,
        battery: body.battery.into_owned(),
        volumes: body.volumes.into_owned(),
        sessions: body.sessions.into_owned(),
        services: readers.services.get(body.services, "service list")?,
        capabilities: body.capabilities,
        hardware: Arc::clone(hardware),
    })
}

/// One row back into a sample with its shared values attached, plus its thread
/// rows. `added` is what the frame's [`Extra`] and [`GpuExtra`] say of it, where
/// the frame's version has them; `base` is the previous frame and its processes by
/// key, for a delta row.
fn decode_row(
    row: Row<'_>,
    added: Added,
    base: Option<&(&Snapshot, HashMap<ProcessKey, usize>)>,
    readers: &Readers,
) -> Result<(ProcessSample, Vec<ThreadSample>), Error> {
    let (statics, window, services, mut sample, rows) = match row {
        Row::Full {
            statics,
            window,
            services,
            sample,
            threads,
        } => {
            let mut sample = sample.into_owned();
            added.apply(&mut sample, &ProcessSample::default());
            (statics, window, services, sample, threads.into_owned())
        }
        Row::Delta {
            statics,
            window,
            services,
            delta,
            threads,
        } => {
            let key = readers.statics.get(statics, "process statics")?.key;
            let (before, prev) = base
                .and_then(|(prev, index)| index.get(&key).map(|&i| (&prev.processes[i], *prev)))
                .ok_or_else(|| {
                    Error::corrupt(format!(
                        "delta for PID {} has no base in the previous frame",
                        key.pid
                    ))
                })?;
            let rows = match threads {
                Threads::Same(deltas) => {
                    let prev_rows = thread_rows(prev, before);
                    if prev_rows.len() != deltas.len() {
                        return Err(Error::corrupt(format!(
                            "PID {}: {} thread deltas for {} threads",
                            key.pid,
                            deltas.len(),
                            prev_rows.len()
                        )));
                    }
                    deltas
                        .iter()
                        .zip(prev_rows)
                        .map(|(d, p)| delta::apply_thread(p, d))
                        .collect()
                }
                Threads::Changed(list) => list.into_owned(),
            };
            let mut sample = delta::apply_process(before, &delta);
            added.apply(&mut sample, before);
            (statics, window, services, sample, rows)
        }
    };
    sample.statics = readers.statics.get(statics, "process statics")?;
    sample.window = match window {
        Some(id) => Some(readers.windows.get(id, "window")?),
        None => None,
    };
    sample.services = readers.process_services.get(services, "service list")?;
    Ok((sample, rows))
}

/// Give each device sample its facts back.
fn attach<S, I: ?Sized>(
    samples: &mut [S],
    ids: &[u32],
    what: &'static str,
    set: impl Fn(&mut S, Arc<I>),
    table: &table::Reader<I>,
) -> Result<(), Error> {
    if samples.len() != ids.len() {
        return Err(Error::corrupt(format!(
            "{} {what} samples with {} ids",
            samples.len(),
            ids.len()
        )));
    }
    for (s, &id) in samples.iter_mut().zip(ids) {
        set(s, table.get(id, what)?);
    }
    Ok(())
}
