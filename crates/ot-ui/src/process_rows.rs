//! The process table's row source: columns, cells, ordering, the process hierarchy
//! with its subtree rollups, and the rows that live *inside* a process.
//!
//! [`ProcessTree`] is rebuilt once per snapshot. It turns each process's parent
//! identity into an index into the snapshot's process list, breaks any cycle a
//! malformed parent chain could form, and folds every process's usage into its
//! ancestors so a collapsed branch can be shown as one row that still adds up.
//!
//! [`Layout`] is rebuilt with it. Its rows are the processes first, in snapshot
//! order (so a process's row index is its index in the snapshot), followed by the
//! rows beneath each process: one per hosted service, a "Threads" group for the
//! threads no service claims, one per thread, and, after an on-demand CPU sample,
//! the modules and clients that sample found. In list mode only the process rows
//! are visible; in tree mode the inner rows hang under their process, folded by
//! default so the tree reads as it always has until you open one.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use ot_core::Snapshot;
use ot_model::attribution::Attribution;
use ot_model::process::{ProcessSample, ProcessStatic};
use ot_model::service::ServiceInfo;
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState};
use ot_model::{Bytes, ProcessKey};

use crate::format;
use crate::steady::{Band, Steady};
use crate::table::{Column, RowId, RowSource};

/// Process table columns, by index into [`columns`]. The first dozen are shown
/// unless turned off; the rest wait in the column chooser, as on Task Manager's
/// Details page.
pub(crate) mod col {
    pub const NAME: usize = 0;
    pub const PID: usize = 1;
    pub const STATUS: usize = 2;
    pub const USER: usize = 3;
    pub const CPU: usize = 4;
    pub const CYCLES: usize = 5;
    pub const MEMORY: usize = 6;
    pub const WORKING_SET: usize = 7;
    pub const DISK_READ: usize = 8;
    pub const DISK_WRITE: usize = 9;
    pub const GPU: usize = 10;
    pub const THREADS: usize = 11;
    pub const HANDLES: usize = 12;
    pub const DESCRIPTION: usize = 13;
    pub const COMMAND_LINE: usize = 14;
    pub const GPU_ENGINE: usize = 15;
    pub const KIND: usize = 16;
    pub const COMPANY: usize = 17;
    pub const PRIORITY: usize = 18;
    pub const ARCHITECTURE: usize = 19;
    pub const SESSION: usize = 20;
    pub const ELEVATED: usize = 21;
    pub const CPU_TIME: usize = 22;
    pub const STARTED: usize = 23;
    pub const PAGE_FAULTS: usize = 24;
    pub const PEAK_WORKING_SET: usize = 25;
    pub const VIRTUAL_SIZE: usize = 26;
    pub const PAGED_POOL: usize = 27;
    pub const NONPAGED_POOL: usize = 28;
    pub const IO_READS: usize = 29;
    pub const IO_WRITES: usize = 30;
    pub const IO_OTHER: usize = 31;
    pub const IO_READ_BYTES: usize = 32;
    pub const IO_WRITE_BYTES: usize = 33;
    pub const IO_OTHER_BYTES: usize = 34;
    pub const IMAGE_PATH: usize = 35;
    pub const WINDOW: usize = 36;
    pub const PACKAGE: usize = 37;
}

pub(crate) fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 300.0),
        Column::number("PID", 70.0),
        Column::text("Status", 100.0),
        Column::text("User", 110.0),
        Column::number("CPU %", 70.0),
        Column::number("Cycles", 75.0),
        Column::number("Memory", 95.0),
        Column::number("Working set", 95.0),
        Column::number("Disk read", 95.0),
        Column::number("Disk write", 95.0),
        Column::number("GPU %", 70.0),
        Column::number("Threads", 70.0),
        Column::number("Handles", 75.0),
        Column::text("Description", 200.0),
        Column::text("Command line", 480.0),
        Column::text("GPU engine", 110.0).hidden(),
        Column::text("Type", 90.0).hidden(),
        Column::text("Company", 160.0).hidden(),
        Column::text("Priority", 100.0).hidden(),
        Column::text("Architecture", 90.0).hidden(),
        Column::number("Session", 70.0).hidden(),
        Column::text("Elevated", 70.0).hidden(),
        Column::number("CPU time", 95.0).hidden(),
        Column::number("Started", 110.0).hidden(),
        Column::number("Page faults", 95.0).hidden(),
        Column::number("Peak working set", 115.0).hidden(),
        Column::number("Virtual size", 95.0).hidden(),
        Column::number("Paged pool", 90.0).hidden(),
        Column::number("NP pool", 90.0).hidden(),
        Column::number("I/O reads", 90.0).hidden(),
        Column::number("I/O writes", 90.0).hidden(),
        Column::number("I/O other", 90.0).hidden(),
        Column::number("I/O read bytes", 110.0).hidden(),
        Column::number("I/O write bytes", 110.0).hidden(),
        Column::number("I/O other bytes", 110.0).hidden(),
        Column::text("Image path", 320.0).hidden(),
        Column::text("Window title", 220.0).hidden(),
        Column::text("Package name", 260.0).hidden(),
    ]
}

/// The Status cell: what is wrong, or special, about a process right now.
pub(crate) fn status_label(p: &ProcessSample) -> &'static str {
    if p.window.as_ref().is_some_and(|w| w.hung) {
        "Not responding"
    } else if p.suspended {
        "Suspended"
    } else if p.efficiency_mode == Some(true) {
        "Efficiency mode"
    } else {
        ""
    }
}

/// `Yes` for a process running with administrator or system rights.
fn elevated_label(p: &ProcessSample) -> &'static str {
    match p.statics.integrity {
        ot_model::process::Integrity::High | ot_model::process::Integrity::System => "Yes",
        ot_model::process::Integrity::Low | ot_model::process::Integrity::Medium => "No",
        ot_model::process::Integrity::Unknown => "",
    }
}

/// A process's own figure in a numeric column that is not rolled up through the
/// tree: the cumulative counters, sizes and times from the Details page. `None`
/// for the columns handled elsewhere and for the text columns.
fn own_figure(p: &ProcessSample, column: usize) -> Option<f64> {
    Some(match column {
        col::GPU => f64::from(p.gpu?.get()),
        col::SESSION => f64::from(p.statics.session_id),
        col::CPU_TIME => p.cpu_time.as_secs_f64(),
        col::STARTED => p.statics.started_unix_ms? as f64,
        col::PAGE_FAULTS => f64::from(p.page_faults),
        col::PEAK_WORKING_SET => p.peak_working_set.get() as f64,
        col::VIRTUAL_SIZE => p.virtual_size.get() as f64,
        col::PAGED_POOL => p.paged_pool.get() as f64,
        col::NONPAGED_POOL => p.nonpaged_pool.get() as f64,
        col::IO_READS => p.io.reads as f64,
        col::IO_WRITES => p.io.writes as f64,
        col::IO_OTHER => p.io.other as f64,
        col::IO_READ_BYTES => p.io.read_bytes.get() as f64,
        col::IO_WRITE_BYTES => p.io.write_bytes.get() as f64,
        col::IO_OTHER_BYTES => p.io.other_bytes.get() as f64,
        _ => return None,
    })
}

/// ASCII case-insensitive substring test. `needle` must already be lower-case.
/// Non-ASCII letters compare exactly, which is the honest cheap answer; a task
/// manager's haystacks (names, paths, command lines) are overwhelmingly ASCII.
pub(crate) fn contains_ci(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || (h.len() >= n.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n)))
}

/// Whether a process matches a search: by name, PID, user, image path, command
/// line, or the name of a service it hosts. `pid_buf` is caller-owned scratch so
/// the PID needs no allocation.
pub(crate) fn process_matches(p: &ProcessSample, needle: &str, pid_buf: &mut String) -> bool {
    statics_match(&p.statics, &p.services, needle, pid_buf)
}

/// [`process_matches`] on a process's facts alone, for one that is no longer in the
/// snapshot (its services, if any, are passed empty).
pub(crate) fn statics_match(
    s: &ProcessStatic,
    services: &[ServiceInfo],
    needle: &str,
    pid_buf: &mut String,
) -> bool {
    if needle.is_empty() {
        return true;
    }
    format::count(pid_buf, s.key.pid);
    let opt = |v: &Option<String>| v.as_deref().is_some_and(|v| contains_ci(v, needle));
    contains_ci(&s.name, needle)
        || contains_ci(pid_buf, needle)
        || opt(&s.user)
        || opt(&s.image_path)
        || opt(&s.command_line)
        || services
            .iter()
            .any(|svc| contains_ci(&svc.name, needle) || contains_ci(&svc.display_name, needle))
}

/// The `-k <group>` of a `svchost.exe` command line, if present.
pub(crate) fn svchost_group(command_line: &str) -> Option<&str> {
    let mut parts = command_line.split_whitespace();
    while let Some(p) = parts.next() {
        if p.eq_ignore_ascii_case("-k") {
            return parts.next().map(|g| g.trim_matches('"'));
        }
    }
    None
}

/// Case-insensitive ordering of two names without allocating.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Ordering for optional text: present sorts before absent, so a column of
/// unknowns sinks to the bottom in ascending order.
fn cmp_opt(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => cmp_ci(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(crate) fn row_id(key: ProcessKey) -> RowId {
    RowId(u64::from(key.pid) ^ key.birth.0.rotate_left(32))
}

/// Derive a stable id for a row beneath a process from the process's id and the
/// row's own identity. A multiply-xorshift mix; collisions only matter within one
/// process's rows, where the inputs are distinct by construction.
fn sub_id(base: RowId, kind: u64, a: u64, b: u64) -> RowId {
    let mut h = base.0 ^ 0x9E37_79B9_7F4A_7C15;
    for v in [kind, a, b] {
        h ^= v.wrapping_add(0x9E37_79B9_7F4A_7C15);
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 31;
    }
    RowId(h)
}

/// Sums over a process and everything below it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Rollup {
    pub cpu: f32,
    pub private_bytes: u64,
    pub working_set: u64,
    pub disk_read: u64,
    pub disk_write: u64,
    pub threads: u32,
    pub handles: u32,
    /// Processes below this one, at any depth.
    pub descendants: u32,
}

impl Rollup {
    fn own(p: &ProcessSample) -> Self {
        Self {
            cpu: p.cpu.get(),
            private_bytes: p.private_bytes.get(),
            working_set: p.working_set.get(),
            disk_read: p.disk_read.get(),
            disk_write: p.disk_write.get(),
            threads: p.threads,
            handles: p.handles,
            descendants: 0,
        }
    }

    /// Fold a completed child subtree into this one.
    fn add(&mut self, child: &Self) {
        self.cpu += child.cpu;
        self.private_bytes = self.private_bytes.saturating_add(child.private_bytes);
        self.working_set = self.working_set.saturating_add(child.working_set);
        self.disk_read = self.disk_read.saturating_add(child.disk_read);
        self.disk_write = self.disk_write.saturating_add(child.disk_write);
        self.threads = self.threads.saturating_add(child.threads);
        self.handles = self.handles.saturating_add(child.handles);
        self.descendants = self
            .descendants
            .saturating_add(child.descendants)
            .saturating_add(1);
    }
}

const NONE: u32 = u32::MAX;
const UNKNOWN: u32 = u32::MAX;
const VISITING: u32 = u32::MAX - 1;

/// Parent indices and subtree rollups for one snapshot's process list.
#[derive(Debug, Default)]
pub(crate) struct ProcessTree {
    /// Parent's index in the process list, or `NONE` for a root.
    parent: Vec<u32>,
    rollup: Vec<Rollup>,
    /// Each process's fading cycle total with everything below it, see
    /// [`ProcessTree::roll_cycles`].
    cycles: Vec<f64>,
    index: HashMap<ProcessKey, u32>,
    depth: Vec<u32>,
    by_depth: Vec<u32>,
    stack: Vec<u32>,
}

impl ProcessTree {
    /// Recompute for a new process list. Allocation-free once the buffers have grown.
    pub fn rebuild(&mut self, procs: &[ProcessSample]) {
        let n = procs.len();

        self.index.clear();
        for (i, p) in procs.iter().enumerate() {
            self.index.insert(p.key(), i as u32);
        }

        // Parent identity to index. An exited parent is simply absent, which makes
        // the child a root, the same as Process Explorer's orphan handling.
        self.parent.clear();
        self.parent.extend(procs.iter().enumerate().map(|(i, p)| {
            p.statics
                .parent
                .and_then(|k| self.index.get(&k).copied())
                .filter(|&pi| pi as usize != i)
                .unwrap_or(NONE)
        }));

        // Depth of every node, which also breaks any parent cycle.
        self.depth.clear();
        self.depth.resize(n, UNKNOWN);
        for i in 0..n {
            self.resolve_depth(i);
        }

        // Rollups: own values, then fold each node into its parent deepest first, so
        // a node is folded only once all of its own children have been.
        self.rollup.clear();
        self.rollup.extend(procs.iter().map(Rollup::own));
        self.by_depth.clear();
        self.by_depth.extend(0..n as u32);
        self.by_depth
            .sort_unstable_by_key(|&i| std::cmp::Reverse(self.depth[i as usize]));
        for &i in &self.by_depth {
            let p = self.parent[i as usize];
            if p != NONE {
                let child = self.rollup[i as usize];
                self.rollup[p as usize].add(&child);
            }
        }
    }

    /// Walk up from `start` until a node with a known depth or a root, then unwind
    /// assigning depths. Meeting a node already on the way up means the chain is a
    /// cycle; that node becomes a root, which is the least surprising repair.
    fn resolve_depth(&mut self, start: usize) {
        self.stack.clear();
        let mut j = start;
        loop {
            match self.depth[j] {
                UNKNOWN => {
                    self.depth[j] = VISITING;
                    self.stack.push(j as u32);
                }
                VISITING => {
                    self.parent[j] = NONE;
                    self.depth[j] = 0;
                    break;
                }
                _ => break,
            }
            match self.parent[j] {
                NONE => {
                    self.depth[j] = 0;
                    break;
                }
                p => j = p as usize,
            }
        }
        while let Some(k) = self.stack.pop() {
            let k = k as usize;
            if self.depth[k] == VISITING {
                self.depth[k] = match self.parent[k] {
                    NONE => 0,
                    p => self.depth[p as usize] + 1,
                };
            }
        }
    }

    /// Fold the processes' fading cycle totals (`own`, parallel to the process list
    /// of the last [`ProcessTree::rebuild`]) into their ancestors.
    pub fn roll_cycles(&mut self, own: &[f64]) {
        self.cycles.clear();
        self.cycles.extend_from_slice(own);
        self.cycles.resize(self.parent.len(), 0.0);
        for &i in &self.by_depth {
            let p = self.parent[i as usize];
            if p != NONE {
                self.cycles[p as usize] += self.cycles[i as usize];
            }
        }
    }

    /// Fading cycle total of a process and everything below it.
    #[must_use]
    pub fn cycles(&self, row: usize) -> f64 {
        self.cycles.get(row).copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn parent(&self, row: usize) -> Option<usize> {
        match self.parent.get(row) {
            Some(&p) if p != NONE => Some(p as usize),
            _ => None,
        }
    }

    #[must_use]
    pub fn rollup(&self, row: usize) -> Rollup {
        self.rollup.get(row).copied().unwrap_or_default()
    }

    /// Set the flag of every ancestor of a set row, so a filtered tree keeps the
    /// path from each match up to its root. `flags` is parallel to the process list.
    pub fn propagate_up(&self, flags: &mut [bool]) {
        // Deepest first, so a node is visited after every node below it.
        for &i in &self.by_depth {
            let i = i as usize;
            if flags.get(i).copied().unwrap_or(false) {
                let p = self.parent[i];
                if p != NONE {
                    flags[p as usize] = true;
                }
            }
        }
    }

    /// `root` followed by every process below it, shallowest first, so that an
    /// action applied in order reaches a parent before its children.
    pub fn subtree(&self, root: usize, out: &mut Vec<usize>) {
        out.clear();
        if root >= self.parent.len() {
            return;
        }
        out.push(root);
        for &i in self.by_depth.iter().rev() {
            let i = i as usize;
            if i == root {
                continue;
            }
            let mut j = i;
            for _ in 0..self.parent.len() {
                match self.parent(j) {
                    Some(p) if p == root => {
                        out.push(i);
                        break;
                    }
                    Some(p) => j = p,
                    None => break,
                }
            }
        }
    }
}

/// What a row is. Process rows come first in the layout and are indexed like the
/// snapshot's process list; every other kind hangs under a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    Process,
    /// A hosted service; the index is into the process's service list.
    Service(u16),
    /// The group holding the threads no service claims.
    ThreadGroup,
    /// A thread; the index is into the snapshot's thread list.
    Thread(u32),
    /// A CPU sample is running for this process.
    Sampling,
    /// Header of a finished CPU sample.
    Sample,
    /// A module the sample landed in; index into the attribution's module list.
    Module(u16),
    /// Header of the clients a broker service reported during the sample.
    Clients,
    /// One client bucket.
    Client(u16),
    /// A module one thread's samples landed in: (thread index, module index within
    /// that thread's list in the attribution).
    ThreadModule(u32, u16),
}

/// One row of the layout.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
    pub kind: RowKind,
    /// Owning process, as an index into the process list.
    pub proc: u32,
    /// Parent row for rows beneath a process. Process rows use the process tree.
    pub parent: u32,
    pub id: RowId,
    /// The number the row sorts and heats by: CPU % for service, group and thread
    /// rows; share of samples or events, in percent, for sample rows.
    pub value: f32,
    /// Threads under a service or group; samples or events for sample rows.
    pub count: u32,
}

/// Per-process facts about its inner rows.
#[derive(Debug, Clone, Copy, Default)]
struct ProcExtra {
    /// First service row, or `NONE`; the process's services are contiguous.
    svc_start: u32,
    /// Whether any thread of this process carries a known service tag, i.e. the
    /// per-service CPU numbers mean something.
    tags_known: bool,
}

/// Every row the table can show for one snapshot. See the module docs.
#[derive(Debug, Default)]
pub(crate) struct Layout {
    pub rows: Vec<Row>,
    extra: Vec<ProcExtra>,
    /// Scratch: thread row index by TID, per process, for attaching sample rows.
    tid_rows: Vec<(u32, u32)>,
}

impl Layout {
    /// Rebuild for a snapshot, an optional finished sample and an optional sample
    /// in progress.
    // One walk per process that emits its services, threads and sample rows in
    // order; the row indices each step hands to the next are what keep it in one
    // function.
    #[allow(clippy::too_many_lines)]
    pub fn rebuild(
        &mut self,
        snap: &Snapshot,
        attribution: Option<&Attribution>,
        sampling: Option<ProcessKey>,
    ) {
        let procs = &snap.processes;
        let threads = &snap.threads;
        self.rows.clear();
        self.extra.clear();
        self.extra.resize(procs.len(), ProcExtra::default());
        for e in &mut self.extra {
            e.svc_start = NONE;
        }

        for (i, p) in procs.iter().enumerate() {
            self.rows.push(Row {
                kind: RowKind::Process,
                proc: i as u32,
                parent: NONE,
                id: row_id(p.key()),
                value: p.cpu.get(),
                count: 0,
            });
        }

        for (i, p) in procs.iter().enumerate() {
            let key = p.key();
            if key.pid == 0 {
                continue;
            }
            let base = row_id(key);
            let pi = i as u32;
            let push = |rows: &mut Vec<Row>, kind, parent, id, value, count| {
                rows.push(Row {
                    kind,
                    proc: pi,
                    parent,
                    id,
                    value,
                    count,
                });
                (rows.len() - 1) as u32
            };

            // Services, in the probe's (name) order.
            let svc_start = self.rows.len() as u32;
            for (k, s) in p.services.iter().enumerate() {
                let id = sub_id(base, 1, hash_str(&s.name), 0);
                push(&mut self.rows, RowKind::Service(k as u16), pi, id, 0.0, 0);
            }
            if !p.services.is_empty() {
                self.extra[i].svc_start = svc_start;
            }

            // Threads: tagged ones under their service, the rest under a group.
            let range = p.thread_range();
            let range = range.start.min(threads.len())..range.end.min(threads.len());
            let mut group = NONE;
            if !range.is_empty() {
                group = push(
                    &mut self.rows,
                    RowKind::ThreadGroup,
                    pi,
                    sub_id(base, 2, 0, 0),
                    0.0,
                    0,
                );
            }
            self.tid_rows.clear();
            for t_idx in range {
                let t = &threads[t_idx];
                let parent = match t.service {
                    ServiceTag::Service(k) if usize::from(k) < p.services.len() => {
                        self.extra[i].tags_known = true;
                        svc_start + u32::from(k)
                    }
                    ServiceTag::None => {
                        self.extra[i].tags_known = true;
                        group
                    }
                    _ => group,
                };
                let parent_row = &mut self.rows[parent as usize];
                parent_row.value += t.cpu.get();
                parent_row.count += 1;
                let id = sub_id(base, 3, u64::from(t.tid), t.birth);
                let r = push(
                    &mut self.rows,
                    RowKind::Thread(t_idx as u32),
                    parent,
                    id,
                    t.cpu.get(),
                    0,
                );
                self.tid_rows.push((t.tid, r));
            }

            if sampling == Some(key) {
                push(
                    &mut self.rows,
                    RowKind::Sampling,
                    pi,
                    sub_id(base, 9, 0, 0),
                    0.0,
                    0,
                );
            }
            let Some(a) = attribution.filter(|a| a.target == key) else {
                continue;
            };
            let total = a.samples.max(1) as f32;
            let sample = push(
                &mut self.rows,
                RowKind::Sample,
                pi,
                sub_id(base, 4, 0, 0),
                a.samples as f32,
                a.samples,
            );
            for (m, share) in a.modules.iter().enumerate() {
                let pct = share.count as f32 / total * 100.0;
                let id = sub_id(base, 5, m as u64, 0);
                push(
                    &mut self.rows,
                    RowKind::Module(m as u16),
                    sample,
                    id,
                    pct,
                    share.count,
                );
            }
            if let Some(c) = &a.clients {
                let clients = push(
                    &mut self.rows,
                    RowKind::Clients,
                    pi,
                    sub_id(base, 6, 0, 0),
                    c.events as f32,
                    c.events,
                );
                let ev = c.events.max(1) as f32;
                for (b, share) in c.buckets.iter().enumerate() {
                    let pct = share.count as f32 / ev * 100.0;
                    let id = sub_id(base, 7, b as u64, 0);
                    push(
                        &mut self.rows,
                        RowKind::Client(b as u16),
                        clients,
                        id,
                        pct,
                        share.count,
                    );
                }
            }
            for ts in &a.threads {
                let Some(&(_, trow)) = self.tid_rows.iter().find(|(tid, _)| *tid == ts.tid) else {
                    continue;
                };
                let RowKind::Thread(t_idx) = self.rows[trow as usize].kind else {
                    continue;
                };
                let own = ts.samples.max(1) as f32;
                for (m, share) in ts.modules.iter().enumerate() {
                    let pct = share.count as f32 / own * 100.0;
                    let id = sub_id(base, 8, u64::from(ts.tid), m as u64);
                    push(
                        &mut self.rows,
                        RowKind::ThreadModule(t_idx, m as u16),
                        trow,
                        id,
                        pct,
                        share.count,
                    );
                }
            }
        }
    }

    /// Service rows of process `proc`, in service-list order.
    fn service_rows(&self, proc: usize, n: usize) -> Option<std::ops::Range<usize>> {
        let s = self.extra.get(proc)?.svc_start;
        (s != NONE).then(|| s as usize..s as usize + n)
    }
}

pub(crate) fn hash_str(s: &str) -> u64 {
    // FNV-1a; short strings, stability across runs is all that matters.
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Adapts a snapshot's process list, threads and layout to the table.
pub(crate) struct ProcessRows<'a> {
    pub procs: &'a [ProcessSample],
    pub threads: &'a [ThreadSample],
    pub tree: &'a ProcessTree,
    pub layout: &'a Layout,
    pub attribution: Option<&'a Attribution>,
    pub interval_secs: f32,
    pub mem_total: f32,
    /// Each process's fading cycle total ([`ot_core::Usage`]), parallel to `procs`;
    /// empty when there is none to show.
    pub cycles: &'a [f64],
    /// The total a process settles at with one core busy throughout, which a full
    /// heat tint in the Cycles column stands for. Zero when the clock is unknown.
    pub cycles_core: f64,
    /// Search results, parallel to `procs`; empty when there is no search.
    pub matched: &'a [bool],
    /// Rows to list: the matches, plus their ancestors in tree mode. Empty means
    /// everything.
    pub shown: &'a [bool],
    /// Tree mode: the rows beneath a process are listed. In list mode they are not.
    pub tree_mode: bool,
    /// Sticky sort keys; `None` sorts by the exact values.
    pub steady: Option<&'a Steady>,
    /// When the snapshot was taken, for the Started column's "ago".
    pub now_unix_ms: i64,
}

/// The dead band a numeric column's sort keys get, so noise does not reorder rows
/// (see [`Steady`]). `None` for the text columns and PID, which sort exactly.
#[must_use]
pub(crate) fn band(column: usize) -> Option<Band> {
    const MIB: f64 = 1024.0 * 1024.0;
    match column {
        col::CPU | col::GPU => Some(Band {
            abs: 1.0,
            rel: 0.15,
        }),
        // A fading total moves smoothly, so a narrow band is enough: 50 M cycles
        // (a few hundredths of a second of one core) or 10 %.
        col::CYCLES => Some(Band {
            abs: 5.0e7,
            rel: 0.10,
        }),
        // Bytes per interval; I/O is bursty, so a wide relative band.
        col::DISK_READ | col::DISK_WRITE => Some(Band {
            abs: 64.0 * 1024.0,
            rel: 0.25,
        }),
        col::MEMORY | col::WORKING_SET => Some(Band {
            abs: MIB,
            rel: 0.02,
        }),
        col::THREADS | col::HANDLES => Some(Band {
            abs: 2.0,
            rel: 0.02,
        }),
        // Sizes that drift: the same band as memory.
        col::PEAK_WORKING_SET | col::VIRTUAL_SIZE | col::PAGED_POOL | col::NONPAGED_POOL => {
            Some(Band {
                abs: MIB,
                rel: 0.02,
            })
        }
        // Cumulative counters only grow; a small relative band keeps a busy
        // process from leapfrogging every second.
        col::PAGE_FAULTS
        | col::IO_READS
        | col::IO_WRITES
        | col::IO_OTHER
        | col::IO_READ_BYTES
        | col::IO_WRITE_BYTES
        | col::IO_OTHER_BYTES
        | col::CPU_TIME => Some(Band {
            abs: 0.0,
            rel: 0.05,
        }),
        // Exact: a session id or a start time does not move.
        col::SESSION | col::STARTED => Some(Band { abs: 0.0, rel: 0.0 }),
        _ => None,
    }
}

impl ProcessRows<'_> {
    /// The exact number `row` sorts by in a numeric column: a process's own figure
    /// in the list, its whole subtree's in the tree; for a service, thread group,
    /// thread or sample module its one number (CPU %, or its share of the samples)
    /// in the CPU column and its thread or sample count in the Threads column. CPU
    /// sample rows are pinned above their siblings with `INFINITY`. `None` where the
    /// row has no figure for the column, and for the text columns and PID.
    #[must_use]
    pub fn raw_key(&self, row: usize, column: usize) -> Option<f64> {
        let r = self.row(row);
        match r.kind {
            RowKind::Process => {
                let v = if self.tree_mode {
                    let ru = self.tree.rollup(row);
                    match column {
                        col::CPU => f64::from(ru.cpu),
                        col::CYCLES => self.tree.cycles(row),
                        col::MEMORY => ru.private_bytes as f64,
                        col::WORKING_SET => ru.working_set as f64,
                        col::DISK_READ => ru.disk_read as f64,
                        col::DISK_WRITE => ru.disk_write as f64,
                        col::THREADS => f64::from(ru.threads),
                        col::HANDLES => f64::from(ru.handles),
                        _ => return own_figure(&self.procs[r.proc as usize], column),
                    }
                } else {
                    let p = &self.procs[r.proc as usize];
                    match column {
                        col::CPU => f64::from(p.cpu.get()),
                        col::CYCLES => self.own_cycles(r.proc as usize),
                        col::MEMORY => p.private_bytes.get() as f64,
                        col::WORKING_SET => p.working_set.get() as f64,
                        col::DISK_READ => p.disk_read.get() as f64,
                        col::DISK_WRITE => p.disk_write.get() as f64,
                        col::THREADS => f64::from(p.threads),
                        col::HANDLES => f64::from(p.handles),
                        _ => return own_figure(p, column),
                    }
                };
                Some(v)
            }
            RowKind::Sampling | RowKind::Sample | RowKind::Clients => {
                band(column).map(|_| f64::INFINITY)
            }
            _ => match column {
                col::CPU => Some(f64::from(r.value)),
                col::THREADS => Some(f64::from(r.count)),
                _ => None,
            },
        }
    }

    /// What `row` sorts by in `column`: its sticky key if the table is steadied,
    /// else [`ProcessRows::raw_key`].
    fn key(&self, row: usize, column: usize) -> Option<f64> {
        let v = self.raw_key(row, column)?;
        Some(
            self.steady
                .map_or(v, |s| s.key(self.id(row), column, self.tree_mode, v)),
        )
    }

    /// Order by [`ProcessRows::key`] in a numeric column: a row with a figure above
    /// one without. `None` for the columns that are not numeric.
    fn compare_keys(&self, a: usize, b: usize, column: usize) -> Option<Ordering> {
        band(column)?;
        Some(match (self.key(a, column), self.key(b, column)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        })
    }

    /// Real processes in the snapshot, whether or not the search shows them.
    #[must_use]
    pub fn population(&self) -> usize {
        self.procs.iter().filter(|p| p.key().pid != 0).count()
    }

    /// Whether a search is narrowing the list.
    #[must_use]
    pub fn filtered(&self) -> bool {
        !self.shown.is_empty()
    }

    /// The process a row belongs to, as an index into the process list.
    #[must_use]
    pub fn process_of(&self, row: usize) -> usize {
        self.layout.rows[row].proc as usize
    }

    #[cfg(test)]
    pub fn kind(&self, row: usize) -> RowKind {
        self.layout.rows[row].kind
    }

    /// Root-first chain of names ending in the row itself, e.g.
    /// `wininit.exe › services.exe › svchost.exe › BrokerInfrastructure`. `chain` is
    /// caller-owned scratch.
    pub fn ancestry(&self, row: usize, out: &mut String, chain: &mut Vec<u32>) {
        out.clear();
        chain.clear();
        let n = self.layout.rows.len();
        let mut r = row;
        for _ in 0..n {
            chain.push(r as u32);
            match self.parent(r) {
                Some(p) if p < n && self.visible(p) => r = p,
                _ => break,
            }
        }
        let mut name = String::new();
        for (i, &r) in chain.iter().rev().enumerate() {
            if i > 0 {
                out.push_str(" › ");
            }
            self.name_of(r as usize, &mut name);
            out.push_str(&name);
        }
    }

    /// Row index by table id, if it is in this snapshot.
    #[must_use]
    pub fn row_of(&self, id: RowId) -> Option<usize> {
        self.layout.rows.iter().position(|r| r.id == id)
    }

    /// Number of processes the table lists.
    #[must_use]
    pub fn listed(&self) -> usize {
        (0..self.procs.len()).filter(|&r| self.visible(r)).count()
    }

    /// A process's own fading cycle total.
    fn own_cycles(&self, proc: usize) -> f64 {
        self.cycles.get(proc).copied().unwrap_or(0.0)
    }

    /// Cycle heat is relative to one core busy throughout.
    fn cycles_heat(&self, cycles: f64) -> Option<f32> {
        (self.cycles_core > 0.0).then(|| (cycles / self.cycles_core).min(1.0) as f32)
    }

    fn memory_heat(&self, private_bytes: u64) -> Option<f32> {
        // Memory heat is relative to a tenth of RAM: one process holding 10% of the
        // machine is fully hot.
        (self.mem_total > 0.0).then(|| (private_bytes as f32 / (self.mem_total * 0.1)).min(1.0))
    }

    fn row(&self, row: usize) -> Row {
        self.layout.rows[row]
    }

    /// Plain name of any row, for the Name column and the ancestry strip.
    fn name_of(&self, row: usize, out: &mut String) {
        out.clear();
        let r = self.row(row);
        let p = &self.procs[r.proc as usize];
        match r.kind {
            RowKind::Process => out.push_str(p.name()),
            RowKind::Service(k) => {
                out.push_str(p.services.get(usize::from(k)).map_or("", |s| &s.name));
            }
            RowKind::ThreadGroup => out.push_str("Threads"),
            RowKind::Thread(t) => {
                let _ = write!(out, "Thread {}", self.threads[t as usize].tid);
            }
            RowKind::Sampling => out.push_str("Sampling CPU\u{2026}"),
            RowKind::Sample => match self.attribution {
                Some(a) => {
                    let _ = write!(
                        out,
                        "CPU sample \u{b7} {} samples in {:.1} s",
                        a.samples,
                        a.duration.as_secs_f32()
                    );
                }
                None => out.push_str("CPU sample"),
            },
            RowKind::Module(m) => {
                if let Some(s) = self.attribution.and_then(|a| a.modules.get(usize::from(m))) {
                    out.push_str(&s.label);
                }
            }
            RowKind::Clients => {
                if let Some(c) = self.attribution.and_then(|a| a.clients.as_ref()) {
                    let _ = write!(
                        out,
                        "Clients of {} \u{b7} {} events by {}",
                        c.service, c.events, c.field
                    );
                    if c.lost > 0 {
                        let _ = write!(out, " ({} lost)", c.lost);
                    }
                }
            }
            RowKind::Client(b) => {
                if let Some(s) = self
                    .attribution
                    .and_then(|a| a.clients.as_ref())
                    .and_then(|c| c.buckets.get(usize::from(b)))
                {
                    out.push_str(&s.label);
                }
            }
            RowKind::ThreadModule(t, m) => {
                let tid = self.threads[t as usize].tid;
                if let Some(s) = self
                    .attribution
                    .and_then(|a| a.threads.iter().find(|ts| ts.tid == tid))
                    .and_then(|ts| ts.modules.get(usize::from(m)))
                {
                    out.push_str(&s.label);
                }
            }
        }
    }

    /// The Name cell of a process: its name, and for a service host the group and
    /// the services it runs, busiest first when per-service CPU is known.
    fn process_name_cell(&self, proc: usize, out: &mut String) {
        let p = &self.procs[proc];
        out.clear();
        out.push_str(p.name());
        if p.services.is_empty() {
            return;
        }
        if let Some(group) = p.statics.command_line.as_deref().and_then(svchost_group) {
            let _ = write!(out, " ({group})");
        }
        out.push_str(" \u{b7} ");
        let known = self.layout.extra.get(proc).is_some_and(|e| e.tags_known);
        let rows = self.layout.service_rows(proc, p.services.len());
        let mut order: Vec<usize> = (0..p.services.len()).collect();
        if let (true, Some(rows)) = (known, rows.clone()) {
            let cpu = |k: usize| self.layout.rows[rows.start + k].value;
            order.sort_by(|&a, &b| cpu(b).total_cmp(&cpu(a)));
        }
        for (n, k) in order.into_iter().enumerate() {
            if n > 0 {
                out.push_str(", ");
            }
            out.push_str(&p.services[k].name);
            if let (true, Some(rows)) = (known, rows.clone()) {
                let cpu = self.layout.rows[rows.start + k].value;
                if cpu >= 0.05 {
                    let _ = write!(out, " {cpu:.0}%");
                }
            }
        }
    }

    fn thread_state_cell(t: &ThreadSample, out: &mut String) {
        out.clear();
        out.push_str(t.state.label());
        if t.state == ThreadState::Waiting {
            match t.wait_reason.label() {
                Some(w) => {
                    let _ = write!(out, " \u{b7} {w}");
                }
                None => {
                    let _ = write!(out, " \u{b7} {}", t.wait_reason.0);
                }
            }
        }
    }
}

impl RowSource for ProcessRows<'_> {
    fn len(&self) -> usize {
        self.layout.rows.len()
    }

    fn id(&self, row: usize) -> RowId {
        self.layout.rows[row].id
    }

    fn images(&self) -> bool {
        true
    }

    /// A process row shows its program's icon; the rows under a process (its
    /// services, threads and samples) show none.
    fn image(&self, row: usize) -> Option<&str> {
        let r = self.row(row);
        match r.kind {
            RowKind::Process => self.procs[r.proc as usize].statics.image_path.as_deref(),
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let r = self.row(row);
        let p = &self.procs[r.proc as usize];
        match r.kind {
            RowKind::Process => match col {
                col::NAME => self.process_name_cell(r.proc as usize, out),
                col::USER => {
                    out.clear();
                    out.push_str(p.statics.user.as_deref().unwrap_or(""));
                }
                col::COMMAND_LINE => {
                    out.clear();
                    out.push_str(p.statics.command_line.as_deref().unwrap_or(""));
                }
                col::PID => format::count(out, p.key().pid),
                col::STATUS => {
                    out.clear();
                    out.push_str(status_label(p));
                }
                col::CPU => format::percent(out, p.cpu.get()),
                col::CYCLES => format::cycles_cell(out, self.own_cycles(r.proc as usize)),
                col::MEMORY => format::bytes(out, p.private_bytes),
                col::WORKING_SET => format::bytes(out, p.working_set),
                col::DISK_READ => format::rate(out, p.disk_read, self.interval_secs),
                col::DISK_WRITE => format::rate(out, p.disk_write, self.interval_secs),
                col::GPU => {
                    out.clear();
                    if let Some(g) = p.gpu.filter(|g| g.get() > 0.0) {
                        format::percent(out, g.get());
                    }
                }
                col::GPU_ENGINE => {
                    out.clear();
                    out.push_str(p.gpu_engine.as_deref().unwrap_or(""));
                }
                col::THREADS => format::count(out, p.threads),
                col::HANDLES => format::count(out, p.handles),
                col::DESCRIPTION => {
                    out.clear();
                    out.push_str(p.statics.description.as_deref().unwrap_or(""));
                }
                col::COMPANY => {
                    out.clear();
                    out.push_str(p.statics.company.as_deref().unwrap_or(""));
                }
                col::PACKAGE => {
                    out.clear();
                    out.push_str(p.statics.package.as_deref().unwrap_or(""));
                }
                col::KIND => {
                    out.clear();
                    out.push_str(p.kind.label());
                }
                col::PRIORITY => {
                    out.clear();
                    out.push_str(p.priority.label());
                }
                col::ARCHITECTURE => {
                    out.clear();
                    out.push_str(p.statics.architecture.label());
                }
                col::SESSION => format::count(out, p.statics.session_id),
                col::ELEVATED => {
                    out.clear();
                    out.push_str(elevated_label(p));
                }
                col::CPU_TIME => format::hms(out, p.cpu_time.as_secs()),
                col::STARTED => {
                    out.clear();
                    if let Some(started) = p.statics.started_unix_ms {
                        let ms = (self.now_unix_ms - started).max(0) as f32;
                        format::ago(out, ms, format::AgoFields::of(ms));
                    }
                }
                col::PAGE_FAULTS => format::count(out, p.page_faults),
                col::PEAK_WORKING_SET => format::bytes(out, p.peak_working_set),
                col::VIRTUAL_SIZE => format::bytes(out, p.virtual_size),
                col::PAGED_POOL => format::bytes(out, p.paged_pool),
                col::NONPAGED_POOL => format::bytes(out, p.nonpaged_pool),
                col::IO_READS => format::count64(out, p.io.reads),
                col::IO_WRITES => format::count64(out, p.io.writes),
                col::IO_OTHER => format::count64(out, p.io.other),
                col::IO_READ_BYTES => format::bytes(out, p.io.read_bytes),
                col::IO_WRITE_BYTES => format::bytes(out, p.io.write_bytes),
                col::IO_OTHER_BYTES => format::bytes(out, p.io.other_bytes),
                col::IMAGE_PATH => {
                    out.clear();
                    out.push_str(p.statics.image_path.as_deref().unwrap_or(""));
                }
                col::WINDOW => {
                    out.clear();
                    out.push_str(p.window.as_ref().map_or("", |w| w.title.as_str()));
                }
                _ => out.clear(),
            },
            RowKind::Service(k) => {
                let s = p.services.get(usize::from(k));
                let known = self
                    .layout
                    .extra
                    .get(r.proc as usize)
                    .is_some_and(|e| e.tags_known);
                match col {
                    col::NAME => self.name_of(row, out),
                    col::USER => {
                        out.clear();
                        out.push_str(s.map_or("", |s| s.state.label()));
                    }
                    col::CPU if known => format::percent(out, r.value),
                    col::THREADS if known => format::count(out, r.count),
                    col::COMMAND_LINE => {
                        out.clear();
                        if let Some(s) = s {
                            out.push_str(&s.display_name);
                            if let Some(dll) = &s.dll {
                                let _ = write!(out, " \u{b7} {dll}");
                            }
                        }
                    }
                    _ => out.clear(),
                }
            }
            RowKind::Thread(t) => {
                let t = &self.threads[t as usize];
                match col {
                    col::NAME => self.name_of(row, out),
                    col::PID => format::count(out, t.tid),
                    col::USER => Self::thread_state_cell(t, out),
                    col::CPU => format::percent(out, t.cpu.get()),
                    col::COMMAND_LINE => {
                        out.clear();
                        if let ServiceTag::Service(k) = t.service {
                            out.push_str(p.services.get(usize::from(k)).map_or("", |s| &s.name));
                        }
                    }
                    _ => out.clear(),
                }
            }
            RowKind::Sampling | RowKind::Sample | RowKind::Clients => match col {
                col::NAME => self.name_of(row, out),
                _ => out.clear(),
            },
            RowKind::ThreadGroup
            | RowKind::Module(_)
            | RowKind::Client(_)
            | RowKind::ThreadModule(..) => match col {
                col::NAME => self.name_of(row, out),
                col::CPU => format::percent(out, r.value),
                col::THREADS => format::count(out, r.count),
                _ => out.clear(),
            },
        }
    }

    fn heat(&self, row: usize, col: usize) -> Option<f32> {
        let r = self.row(row);
        match (r.kind, col) {
            (RowKind::Process, col::CPU) => Some((r.value / 100.0).min(1.0)),
            (RowKind::Process, col::CYCLES) => self.cycles_heat(self.own_cycles(r.proc as usize)),
            (RowKind::Process, col::MEMORY) => {
                self.memory_heat(self.procs[r.proc as usize].private_bytes.get())
            }
            (RowKind::Process, col::GPU) => self.procs[r.proc as usize]
                .gpu
                .filter(|g| g.get() > 0.0)
                .map(|g| (g.get() / 100.0).min(1.0)),
            (RowKind::Process | RowKind::Sampling | RowKind::Sample | RowKind::Clients, _) => None,
            (RowKind::Service(_), col::CPU) => {
                let known = self
                    .layout
                    .extra
                    .get(r.proc as usize)
                    .is_some_and(|e| e.tags_known);
                known.then(|| (r.value / 100.0).min(1.0))
            }
            (_, col::CPU) => Some((r.value / 100.0).min(1.0)),
            _ => None,
        }
    }

    fn visible(&self, row: usize) -> bool {
        let r = self.row(row);
        let proc = r.proc as usize;
        // PID 0 is the kernel's idle accounting, not a process. Its "CPU" is the
        // machine's idle time, which would otherwise pin it to the top of every sort.
        if self.procs[proc].key().pid == 0 {
            return false;
        }
        let owner_shown = self.shown.get(proc).is_none_or(|&s| s);
        match r.kind {
            RowKind::Process => owner_shown,
            _ => self.tree_mode && owner_shown,
        }
    }

    fn muted(&self, row: usize) -> bool {
        let proc = self.process_of(row);
        self.matched.get(proc).is_some_and(|&m| !m)
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        if let Some(o) = self.compare_keys(a, b, col) {
            return o;
        }
        let (ra, rb) = (self.row(a), self.row(b));
        if ra.kind == RowKind::Process && rb.kind == RowKind::Process {
            let (a, b) = (&self.procs[ra.proc as usize], &self.procs[rb.proc as usize]);
            return match col {
                col::NAME => cmp_ci(a.name(), b.name()),
                col::USER => cmp_opt(a.statics.user.as_deref(), b.statics.user.as_deref()),
                col::COMMAND_LINE => cmp_opt(
                    a.statics.command_line.as_deref(),
                    b.statics.command_line.as_deref(),
                ),
                col::PID => a.key().pid.cmp(&b.key().pid),
                col::STATUS => cmp_ci(status_label(a), status_label(b)),
                col::GPU_ENGINE => cmp_opt(a.gpu_engine.as_deref(), b.gpu_engine.as_deref()),
                col::DESCRIPTION => cmp_opt(
                    a.statics.description.as_deref(),
                    b.statics.description.as_deref(),
                ),
                col::COMPANY => cmp_opt(a.statics.company.as_deref(), b.statics.company.as_deref()),
                col::PACKAGE => cmp_opt(a.statics.package.as_deref(), b.statics.package.as_deref()),
                col::KIND => a.kind.label().cmp(b.kind.label()),
                col::PRIORITY => a.priority.cmp(&b.priority),
                col::ARCHITECTURE => cmp_ci(
                    a.statics.architecture.label(),
                    b.statics.architecture.label(),
                ),
                col::ELEVATED => cmp_ci(elevated_label(a), elevated_label(b)),
                col::IMAGE_PATH => cmp_opt(
                    a.statics.image_path.as_deref(),
                    b.statics.image_path.as_deref(),
                ),
                col::WINDOW => cmp_opt(
                    a.window.as_ref().map(|w| w.title.as_str()),
                    b.window.as_ref().map(|w| w.title.as_str()),
                ),
                _ => Ordering::Equal,
            };
        }
        // Rows beneath a process, or a mix: by name. Threads sort by id in the PID
        // column.
        match col {
            col::NAME => {
                let (mut na, mut nb) = (String::new(), String::new());
                self.name_of(a, &mut na);
                self.name_of(b, &mut nb);
                cmp_ci(&na, &nb)
            }
            col::PID => match (ra.kind, rb.kind) {
                (RowKind::Thread(x), RowKind::Thread(y)) => self.threads[x as usize]
                    .tid
                    .cmp(&self.threads[y as usize].tid),
                _ => Ordering::Equal,
            },
            _ => Ordering::Equal,
        }
    }

    fn parent(&self, row: usize) -> Option<usize> {
        let r = self.row(row);
        match r.kind {
            RowKind::Process => self.tree.parent(row),
            _ => (r.parent != NONE).then_some(r.parent as usize),
        }
    }

    fn cell_collapsed(&self, row: usize, col: usize, out: &mut String) {
        let r = self.row(row);
        match r.kind {
            RowKind::Process => {
                let ru = self.tree.rollup(row);
                match col {
                    col::NAME => {
                        self.process_name_cell(r.proc as usize, out);
                        if ru.descendants > 0 {
                            let _ = write!(out, " ({})", ru.descendants);
                        }
                    }
                    col::CPU => format::percent(out, ru.cpu),
                    col::CYCLES => format::cycles_cell(out, self.tree.cycles(row)),
                    col::MEMORY => format::bytes(out, Bytes(ru.private_bytes)),
                    col::WORKING_SET => format::bytes(out, Bytes(ru.working_set)),
                    col::DISK_READ => format::rate(out, Bytes(ru.disk_read), self.interval_secs),
                    col::DISK_WRITE => format::rate(out, Bytes(ru.disk_write), self.interval_secs),
                    col::THREADS => format::count(out, ru.threads),
                    col::HANDLES => format::count(out, ru.handles),
                    _ => self.cell(row, col, out),
                }
            }
            RowKind::ThreadGroup if col == col::NAME => {
                out.clear();
                let _ = write!(out, "Threads ({})", r.count);
            }
            RowKind::Service(_) if col == col::NAME => {
                self.name_of(row, out);
                if r.count > 0 {
                    let _ = write!(out, " ({})", r.count);
                }
            }
            _ => self.cell(row, col, out),
        }
    }

    fn heat_collapsed(&self, row: usize, col: usize) -> Option<f32> {
        if self.row(row).kind != RowKind::Process {
            return self.heat(row, col);
        }
        let r = self.tree.rollup(row);
        match col {
            col::CPU => Some((r.cpu / 100.0).min(1.0)),
            col::CYCLES => self.cycles_heat(self.tree.cycles(row)),
            col::MEMORY => self.memory_heat(r.private_bytes),
            _ => None,
        }
    }

    fn compare_subtree(&self, a: usize, b: usize, col: usize) -> Ordering {
        // `key` already gives processes their subtree figure in the tree. Every
        // numeric comparison goes through it, so the order is transitive even
        // among a mix of child processes, services and threads.
        let Some(o) = self.compare_keys(a, b, col) else {
            return self.compare(a, b, col);
        };
        let procs = self.row(a).kind == RowKind::Process && self.row(b).kind == RowKind::Process;
        if procs {
            o
        } else {
            o.then_with(|| self.tie_break(a, b))
        }
    }

    fn collapsed_by_default(&self, row: usize) -> bool {
        matches!(
            self.row(row).kind,
            RowKind::Service(_) | RowKind::ThreadGroup | RowKind::Thread(_)
        )
    }
}

impl ProcessRows<'_> {
    /// Order among siblings whose numbers tie, so a host's idle services do not
    /// shuffle: sample rows, child processes, services, the thread group, threads,
    /// each group by name. Expressed *reversed*, because the numeric columns sort
    /// descending by default and the table reverses the whole ordering; this way
    /// names still read A to Z in the usual direction.
    fn tie_break(&self, a: usize, b: usize) -> Ordering {
        fn rank(k: RowKind) -> u8 {
            match k {
                RowKind::Sampling | RowKind::Sample => 0,
                RowKind::Clients => 1,
                RowKind::Process => 2,
                RowKind::Service(_) => 3,
                RowKind::ThreadGroup => 4,
                RowKind::Thread(_) => 5,
                RowKind::Module(_) | RowKind::Client(_) | RowKind::ThreadModule(..) => 6,
            }
        }
        let (ra, rb) = (self.row(a), self.row(b));
        rank(rb.kind).cmp(&rank(ra.kind)).then_with(|| {
            let (mut na, mut nb) = (String::new(), String::new());
            self.name_of(a, &mut na);
            self.name_of(b, &mut nb);
            cmp_ci(&nb, &na)
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ot_model::attribution::{ClientReport, Share, ThreadShares};
    use ot_model::process::{Integrity, ProcessStatic};
    use ot_model::service::{ServiceInfo, ServiceState};
    use ot_model::thread::WaitReason;
    use ot_model::Percent;
    use std::sync::Arc;
    use std::time::Duration;

    /// A process with a fixed birth stamp of 1; `parent` is a PID with the same
    /// convention, so `Some(0)` is the idle pseudo-process.
    pub fn proc(pid: u32, parent: Option<u32>, cpu: f32) -> ProcessSample {
        ProcessSample {
            statics: Arc::new(ProcessStatic {
                key: ProcessKey::new(pid, 1),
                parent: parent.map(|p| ProcessKey::new(p, 1)),
                name: format!("p{pid}.exe"),
                image_path: None,
                command_line: None,
                user: None,
                integrity: Integrity::Unknown,
                started_unix_ms: None,
                ..ProcessStatic::default()
            }),
            cpu: Percent(cpu),
            cpu_time: Duration::ZERO,
            cycles: 0,
            working_set: Bytes(1),
            private_bytes: Bytes(1),
            disk_read: Bytes(0),
            disk_write: Bytes(0),
            net_rx: Bytes(0),
            net_tx: Bytes(0),
            threads: 1,
            handles: 1,
            ..ProcessSample::default()
        }
    }

    pub fn service(name: &str) -> ServiceInfo {
        ServiceInfo {
            name: Arc::from(name),
            display_name: Arc::from(format!("{name} Service").as_str()),
            state: ServiceState::Running,
            dll: Some(Arc::from("x.dll")),
        }
    }

    pub fn thread(tid: u32, cpu: f32, service: ServiceTag) -> ThreadSample {
        ThreadSample {
            tid,
            birth: 5,
            cpu: Percent(cpu),
            state: ThreadState::Running,
            wait_reason: WaitReason(0),
            service,
            started_unix_ms: None,
        }
    }

    /// A service host: PID 7 hosting `Alpha` and `Beta`, with three threads: one
    /// tagged Alpha (30%), one tagged Beta (5%), one untagged (1%).
    pub fn host_snapshot() -> Snapshot {
        let mut host = proc(7, None, 36.0);
        host.statics = Arc::new(ProcessStatic {
            command_line: Some("C:\\Windows\\system32\\svchost.exe -k DcomLaunch -p".to_owned()),
            ..(*host.statics).clone()
        });
        host.services = vec![service("Alpha"), service("Beta")].into();
        host.thread_first = 0;
        host.thread_rows = 3;
        host.threads = 3;
        Snapshot {
            processes: vec![host, proc(8, Some(7), 2.0)],
            threads: vec![
                thread(70, 30.0, ServiceTag::Service(0)),
                thread(71, 5.0, ServiceTag::Service(1)),
                thread(72, 1.0, ServiceTag::None),
            ],
            ..Default::default()
        }
    }

    fn tree_of(procs: &[ProcessSample]) -> ProcessTree {
        let mut t = ProcessTree::default();
        t.rebuild(procs);
        t
    }

    fn layout_of(snap: &Snapshot, a: Option<&Attribution>) -> Layout {
        let mut l = Layout::default();
        l.rebuild(snap, a, None);
        l
    }

    fn rows<'a>(
        snap: &'a Snapshot,
        tree: &'a ProcessTree,
        layout: &'a Layout,
        a: Option<&'a Attribution>,
        tree_mode: bool,
    ) -> ProcessRows<'a> {
        ProcessRows {
            procs: &snap.processes,
            threads: &snap.threads,
            tree,
            layout,
            attribution: a,
            interval_secs: 1.0,
            mem_total: 0.0,
            cycles: &[],
            cycles_core: 0.0,
            matched: &[],
            shown: &[],
            tree_mode,
            steady: None,
            now_unix_ms: 0,
        }
    }

    fn cell(r: &ProcessRows<'_>, row: usize, col: usize) -> String {
        let mut s = String::new();
        r.cell(row, col, &mut s);
        s
    }

    #[test]
    fn rollups_fold_descendants_into_ancestors() {
        // 1 ─ 2 ─ 3, and 4 alone.
        let procs = vec![
            proc(1, None, 1.0),
            proc(2, Some(1), 2.0),
            proc(3, Some(2), 4.0),
            proc(4, None, 8.0),
        ];
        let t = tree_of(&procs);
        assert_eq!(t.parent(0), None);
        assert_eq!(t.parent(1), Some(0));
        assert_eq!(t.parent(2), Some(1));
        assert_eq!(t.parent(3), None);
        assert!((t.rollup(0).cpu - 7.0).abs() < 1e-6);
        assert_eq!(t.rollup(0).descendants, 2);
        assert!((t.rollup(1).cpu - 6.0).abs() < 1e-6);
        assert_eq!(t.rollup(1).descendants, 1);
        assert_eq!(t.rollup(2).descendants, 0);
        assert_eq!(t.rollup(0).threads, 3);
        assert!((t.rollup(3).cpu - 8.0).abs() < 1e-6);
    }

    #[test]
    fn exited_parent_leaves_an_orphan_root() {
        let procs = vec![proc(1, Some(99), 1.0), proc(2, Some(1), 1.0)];
        let t = tree_of(&procs);
        assert_eq!(t.parent(0), None);
        assert_eq!(t.parent(1), Some(0));
    }

    #[test]
    fn parent_with_a_different_birth_is_a_stranger() {
        // PID 1 was recycled: the child names (1, birth 7), the live one is (1, 1).
        let mut child = proc(2, None, 1.0);
        child.statics = Arc::new(ProcessStatic {
            parent: Some(ProcessKey::new(1, 7)),
            ..(*child.statics).clone()
        });
        let procs = vec![proc(1, None, 1.0), child];
        let t = tree_of(&procs);
        assert_eq!(t.parent(1), None);
    }

    #[test]
    fn a_cycle_is_broken_without_hanging() {
        let procs = vec![
            proc(1, Some(2), 1.0),
            proc(2, Some(1), 2.0),
            proc(3, Some(1), 4.0),
        ];
        let t = tree_of(&procs);
        // Exactly one of the two became a root; the third hangs off PID 1 either way.
        let roots = (0..2).filter(|&i| t.parent(i).is_none()).count();
        assert_eq!(roots, 1);
        assert_eq!(t.parent(2), Some(0));
        // Whichever root it is, its rollup covers all three.
        let root = (0..2).find(|&i| t.parent(i).is_none()).unwrap();
        assert!((t.rollup(root).cpu - 7.0).abs() < 1e-6);
        assert_eq!(t.rollup(root).descendants, 2);
    }

    #[test]
    fn ancestry_reads_root_first_and_skips_the_idle_process() {
        let snap = Snapshot {
            processes: vec![
                proc(0, None, 0.0),
                proc(4, Some(0), 0.0),
                proc(9, Some(4), 0.0),
            ],
            ..Default::default()
        };
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let r = rows(&snap, &t, &l, None, false);
        let mut out = String::new();
        let mut chain = Vec::new();
        r.ancestry(2, &mut out, &mut chain);
        assert_eq!(out, "p4.exe › p9.exe");
        assert_eq!(r.listed(), 2);
        assert_eq!(r.population(), 2);
        assert!(!r.filtered());
    }

    #[test]
    fn search_matches_any_detail_field_and_hosted_services() {
        let mut p = proc(42, None, 0.0);
        p.statics = Arc::new(ProcessStatic {
            user: Some("CORP\\Cameron".to_owned()),
            image_path: Some("C:\\Tools\\Thing.exe".to_owned()),
            command_line: Some("\"C:\\Tools\\Thing.exe\" --serve".to_owned()),
            ..(*p.statics).clone()
        });
        p.services = vec![service("BrokerInfrastructure")].into();
        let mut buf = String::new();
        for needle in [
            "p42",
            "42",
            "cameron",
            "tools",
            "--serve",
            "",
            "brokerinfra",
            "infrastructure service",
        ] {
            assert!(process_matches(&p, needle, &mut buf), "{needle}");
        }
        for needle in ["p43", "root", "--run"] {
            assert!(!process_matches(&p, needle, &mut buf), "{needle}");
        }
        assert!(contains_ci("Svchost.EXE", "host.e"));
        assert!(!contains_ci("ab", "abc"));
        assert_eq!(
            svchost_group("C:\\W\\svchost.exe -k DcomLaunch -p"),
            Some("DcomLaunch")
        );
        assert_eq!(
            svchost_group("C:\\W\\svchost.exe -K \"netsvcs\""),
            Some("netsvcs")
        );
        assert_eq!(svchost_group("C:\\W\\svchost.exe"), None);
    }

    #[test]
    fn shown_and_matched_drive_visibility_and_muting() {
        let snap = Snapshot {
            processes: vec![proc(1, None, 0.0), proc(2, Some(1), 0.0)],
            ..Default::default()
        };
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let r = ProcessRows {
            matched: &[false, true],
            shown: &[true, true],
            ..rows(&snap, &t, &l, None, false)
        };
        assert!(r.visible(0) && r.visible(1));
        assert!(r.muted(0) && !r.muted(1));
        assert!(r.filtered());
        assert_eq!(cell(&r, 0, col::USER), "");
        assert_eq!(r.compare(0, 1, col::USER), Ordering::Equal);
    }

    #[test]
    fn propagate_up_reaches_every_ancestor_and_subtree_lists_parents_first() {
        // 1 ─ 2 ─ 3
        //   └─ 4
        // 5
        let procs = vec![
            proc(1, None, 0.0),
            proc(2, Some(1), 0.0),
            proc(3, Some(2), 0.0),
            proc(4, Some(1), 0.0),
            proc(5, None, 0.0),
        ];
        let t = tree_of(&procs);
        let mut flags = vec![false, false, true, false, false];
        t.propagate_up(&mut flags);
        assert_eq!(flags, [true, true, true, false, false]);

        let mut out = Vec::new();
        t.subtree(0, &mut out);
        assert_eq!(out[0], 0);
        assert_eq!(out.len(), 4);
        assert!(out.iter().position(|&i| i == 1) < out.iter().position(|&i| i == 2));
        t.subtree(2, &mut out);
        assert_eq!(out, [2]);
        t.subtree(99, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn collapsed_cells_show_subtree_totals_and_descendant_count() {
        let snap = Snapshot {
            processes: vec![
                proc(1, None, 1.0),
                proc(2, Some(1), 2.0),
                proc(3, Some(2), 4.0),
            ],
            ..Default::default()
        };
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let r = rows(&snap, &t, &l, None, true);
        let mut s = String::new();
        r.cell(0, col::CPU, &mut s);
        assert_eq!(s, "1.0");
        r.cell_collapsed(0, col::CPU, &mut s);
        assert_eq!(s, "7.0");
        r.cell_collapsed(0, col::NAME, &mut s);
        assert_eq!(s, "p1.exe (2)");
        r.cell_collapsed(2, col::NAME, &mut s);
        assert_eq!(s, "p3.exe", "a leaf process shows no count");
        r.cell_collapsed(0, col::PID, &mut s);
        assert_eq!(s, "1", "identity columns never aggregate");
        // The tree orders by subtree totals (7 > 4); the list by own figures (1 < 4).
        assert_eq!(r.compare_subtree(0, 2, col::CPU), Ordering::Greater);
        let list = rows(&snap, &t, &l, None, false);
        assert_eq!(list.compare(0, 2, col::CPU), Ordering::Less);
    }

    #[test]
    fn mixed_siblings_sort_transitively_by_memory() {
        // Under the host: child 8 (5 MB, 40 % CPU), child 9 (10 MB, 1 %), and the
        // service Alpha (30 % CPU, no memory figure). Comparing processes by
        // memory but a process against a service by CPU made a cycle:
        // 9 > 8 (memory), 8 > Alpha (40 > 30), Alpha > 9 (30 > 1).
        let mut snap = host_snapshot();
        let mut nine = proc(9, Some(7), 1.0);
        nine.private_bytes = Bytes(10 << 20);
        snap.processes[1].cpu = Percent(40.0);
        snap.processes[1].private_bytes = Bytes(5 << 20);
        snap.processes.push(nine);
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let r = rows(&snap, &t, &l, None, true);
        let alpha = (0..l.rows.len())
            .find(|&i| r.kind(i) == RowKind::Service(0))
            .unwrap();
        let (eight, nine) = (1, 2);
        let m = col::MEMORY;
        assert_eq!(r.compare_subtree(nine, eight, m), Ordering::Greater);
        assert_eq!(r.compare_subtree(eight, alpha, m), Ordering::Greater);
        assert_eq!(r.compare_subtree(nine, alpha, m), Ordering::Greater);
        assert_eq!(r.raw_key(alpha, m), None, "a service has no memory figure");
        assert_eq!(r.raw_key(alpha, col::CPU), Some(30.0));
        assert_eq!(r.raw_key(nine, col::NAME), None);
    }

    #[test]
    fn steady_keys_decide_the_order_within_the_dead_band() {
        let mut snap = Snapshot {
            processes: vec![proc(1, None, 5.0), proc(2, None, 5.5)],
            ..Default::default()
        };
        let mut steady = Steady::default();
        let fold = |snap: &Snapshot, steady: &mut Steady| {
            let t = tree_of(&snap.processes);
            let l = layout_of(snap, None);
            let r = rows(snap, &t, &l, None, false);
            steady.update(
                col::CPU,
                false,
                band(col::CPU),
                (0..2).map(|i| (r.id(i), r.raw_key(i, col::CPU).unwrap())),
            );
        };
        fold(&snap, &mut steady);
        // The numbers cross, but by less than a point: the order stays.
        snap.processes[0].cpu = Percent(5.8);
        snap.processes[1].cpu = Percent(5.2);
        fold(&snap, &mut steady);
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let exact = rows(&snap, &t, &l, None, false);
        assert_eq!(exact.compare(0, 1, col::CPU), Ordering::Greater);
        let steadied = ProcessRows {
            steady: Some(&steady),
            ..rows(&snap, &t, &l, None, false)
        };
        assert_eq!(steadied.compare(0, 1, col::CPU), Ordering::Less);
        // A real move gets through.
        snap.processes[0].cpu = Percent(9.0);
        fold(&snap, &mut steady);
        let steadied = ProcessRows {
            steady: Some(&steady),
            ..rows(&snap, &t, &l, None, false)
        };
        assert_eq!(steadied.compare(0, 1, col::CPU), Ordering::Greater);
    }

    #[test]
    fn a_service_host_lays_out_services_a_thread_group_and_threads() {
        let snap = host_snapshot();
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let r = rows(&snap, &t, &l, None, true);
        // 2 processes, 2 services, 1 group, 3 threads.
        assert_eq!(r.len(), 8);
        let kinds: Vec<RowKind> = (0..r.len()).map(|i| r.kind(i)).collect();
        assert_eq!(kinds[0], RowKind::Process);
        assert_eq!(kinds[2], RowKind::Service(0));
        assert_eq!(kinds[3], RowKind::Service(1));
        assert_eq!(kinds[4], RowKind::ThreadGroup);
        assert!(matches!(kinds[5], RowKind::Thread(_)));

        // Threads hang under their service; the untagged one under the group.
        assert_eq!(r.parent(5), Some(2));
        assert_eq!(r.parent(6), Some(3));
        assert_eq!(r.parent(7), Some(4));
        assert_eq!(r.parent(2), Some(0));
        assert_eq!(r.parent(4), Some(0));
        // The child process is a tree child of the host too.
        assert_eq!(r.parent(1), Some(0));

        // Per-service CPU is the sum of tagged threads.
        assert_eq!(cell(&r, 2, col::CPU), "30");
        assert_eq!(cell(&r, 3, col::CPU), "5.0");
        assert_eq!(cell(&r, 4, col::CPU), "1.0");
        assert_eq!(cell(&r, 2, col::THREADS), "1");
        assert_eq!(cell(&r, 2, col::NAME), "Alpha");
        assert_eq!(cell(&r, 2, col::USER), "Running");
        assert_eq!(cell(&r, 2, col::COMMAND_LINE), "Alpha Service \u{b7} x.dll");
        assert_eq!(cell(&r, 5, col::NAME), "Thread 70");
        assert_eq!(cell(&r, 5, col::PID), "70");
        assert_eq!(cell(&r, 5, col::COMMAND_LINE), "Alpha");
        assert_eq!(cell(&r, 7, col::COMMAND_LINE), "");

        // The host's Name cell names the group and the services, busiest first.
        assert_eq!(
            cell(&r, 0, col::NAME),
            "p7.exe (DcomLaunch) \u{b7} Alpha 30%, Beta 5%"
        );
        let mut s = String::new();
        r.cell_collapsed(4, col::NAME, &mut s);
        assert_eq!(s, "Threads (1)");
        r.cell_collapsed(2, col::NAME, &mut s);
        assert_eq!(s, "Alpha (1)");

        // Inner rows fold by default; processes do not.
        assert!(
            r.collapsed_by_default(2) && r.collapsed_by_default(4) && r.collapsed_by_default(5)
        );
        assert!(!r.collapsed_by_default(0));

        // Siblings under the host order by their one number.
        assert_eq!(r.compare_subtree(2, 3, col::CPU), Ordering::Greater);
        assert_eq!(
            r.compare_subtree(2, 1, col::CPU),
            Ordering::Greater,
            "Alpha 30 > child 2"
        );
        // Ties: in the descending direction the table reverses, so a "greater"
        // answer here lists first there. Services before the group, A before B.
        let mut idle = host_snapshot();
        for th in &mut idle.threads {
            th.cpu = Percent(0.0);
        }
        let lt = layout_of(&idle, None);
        let ri = rows(&idle, &t, &lt, None, true);
        assert_eq!(
            ri.compare_subtree(2, 3, col::CPU),
            Ordering::Greater,
            "Alpha before Beta"
        );
        assert_eq!(
            ri.compare_subtree(3, 4, col::CPU),
            Ordering::Greater,
            "Beta before Threads"
        );
        assert_eq!(
            ri.compare_subtree(1, 2, col::CPU),
            Ordering::Greater,
            "child before services"
        );

        // Ancestry walks through inner rows.
        let mut out = String::new();
        let mut chain = Vec::new();
        r.ancestry(5, &mut out, &mut chain);
        assert_eq!(out, "p7.exe › Alpha › Thread 70");

        // Ids are stable across rebuilds and distinct.
        let l2 = layout_of(&snap, None);
        assert!(l.rows.iter().zip(&l2.rows).all(|(a, b)| a.id == b.id));
        let mut ids: Vec<u64> = l.rows.iter().map(|r| r.id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), l.rows.len());
    }

    #[test]
    fn inner_rows_are_hidden_in_list_mode_and_without_tags_cpu_is_blank() {
        let mut snap = host_snapshot();
        let t = tree_of(&snap.processes);
        let l = layout_of(&snap, None);
        let list = rows(&snap, &t, &l, None, false);
        assert!(list.visible(0) && list.visible(1));
        assert!((2..8).all(|i| !list.visible(i)));
        assert_eq!(list.listed(), 2);

        // Unelevated: no tags known. Services still list, without numbers, and all
        // threads sit in the group.
        for th in &mut snap.threads {
            th.service = ServiceTag::Unknown;
        }
        let l = layout_of(&snap, None);
        let r = rows(&snap, &t, &l, None, true);
        assert_eq!(cell(&r, 2, col::CPU), "");
        assert_eq!(cell(&r, 2, col::THREADS), "");
        assert_eq!(r.heat(2, col::CPU), None);
        assert_eq!(cell(&r, 4, col::CPU), "36");
        assert!((5..8).all(|i| r.parent(i) == Some(4)));
        assert_eq!(
            cell(&r, 0, col::NAME),
            "p7.exe (DcomLaunch) \u{b7} Alpha, Beta"
        );
    }

    #[test]
    fn a_finished_sample_adds_modules_clients_and_per_thread_modules() {
        let snap = host_snapshot();
        let t = tree_of(&snap.processes);
        let a = Attribution {
            target: ProcessKey::new(7, 1),
            duration: Duration::from_secs(5),
            samples: 200,
            modules: vec![
                Share {
                    label: "bisrv.dll".into(),
                    count: 150,
                },
                Share {
                    label: "ntdll.dll".into(),
                    count: 50,
                },
            ],
            threads: vec![ThreadShares {
                tid: 70,
                samples: 100,
                modules: vec![Share {
                    label: "bisrv.dll".into(),
                    count: 100,
                }],
            }],
            clients: Some(ClientReport {
                service: "Alpha".into(),
                provider: "Microsoft-Windows-Alpha".into(),
                field: "PackageFullName".into(),
                events: 1000,
                lost: 7,
                buckets: vec![Share {
                    label: "Xerox.PrintExperience".into(),
                    count: 1000,
                }],
            }),
            notes: vec![],
        };
        let l = layout_of(&snap, Some(&a));
        let r = rows(&snap, &t, &l, Some(&a), true);
        let kinds: Vec<RowKind> = (0..r.len()).map(|i| r.kind(i)).collect();
        let sample = kinds.iter().position(|k| *k == RowKind::Sample).unwrap();
        assert_eq!(r.parent(sample), Some(0));
        assert!(!r.collapsed_by_default(sample));
        // The sample the user asked for leads its process, whatever the CPU column says.
        assert_eq!(r.compare_subtree(sample, 1, col::CPU), Ordering::Greater);
        assert_eq!(r.compare_subtree(sample, 2, col::CPU), Ordering::Greater);
        assert_eq!(
            cell(&r, sample, col::NAME),
            "CPU sample \u{b7} 200 samples in 5.0 s"
        );
        let m0 = kinds.iter().position(|k| *k == RowKind::Module(0)).unwrap();
        assert_eq!(r.parent(m0), Some(sample));
        assert_eq!(cell(&r, m0, col::NAME), "bisrv.dll");
        assert_eq!(cell(&r, m0, col::CPU), "75");
        assert_eq!(cell(&r, m0, col::THREADS), "150");
        let clients = kinds.iter().position(|k| *k == RowKind::Clients).unwrap();
        assert_eq!(r.parent(clients), Some(0));
        assert_eq!(
            cell(&r, clients, col::NAME),
            "Clients of Alpha \u{b7} 1000 events by PackageFullName (7 lost)"
        );
        let c0 = kinds.iter().position(|k| *k == RowKind::Client(0)).unwrap();
        assert_eq!(r.parent(c0), Some(clients));
        assert_eq!(cell(&r, c0, col::CPU), "100");
        let tm = kinds
            .iter()
            .position(|k| matches!(k, RowKind::ThreadModule(..)))
            .unwrap();
        assert_eq!(r.parent(tm), Some(5), "under Thread 70");
        assert_eq!(cell(&r, tm, col::NAME), "bisrv.dll");
        assert_eq!(cell(&r, tm, col::CPU), "100");

        // A sample for a process that is gone adds nothing; a sample in progress
        // adds one marker row.
        let mut l2 = Layout::default();
        let other = Attribution {
            target: ProcessKey::new(99, 1),
            ..a.clone()
        };
        l2.rebuild(&snap, Some(&other), Some(ProcessKey::new(7, 1)));
        let kinds: Vec<RowKind> = l2.rows.iter().map(|r| r.kind).collect();
        assert!(!kinds.contains(&RowKind::Sample));
        assert_eq!(kinds.iter().filter(|k| **k == RowKind::Sampling).count(), 1);
    }
}
