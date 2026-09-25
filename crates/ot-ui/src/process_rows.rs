//! The process table's row source: columns, cells, ordering, and the process
//! hierarchy with its subtree rollups.
//!
//! [`ProcessTree`] is rebuilt once per snapshot. It turns each process's parent
//! identity into an index into the snapshot's process list, breaks any cycle a
//! malformed parent chain could form, and folds every process's usage into its
//! ancestors so a collapsed branch can be shown as one row that still adds up.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;

use ot_model::process::ProcessSample;
use ot_model::{Bytes, ProcessKey};

use crate::format;
use crate::table::{Column, RowId, RowSource};

/// Process table columns, in display order.
pub(crate) mod col {
    pub const NAME: usize = 0;
    pub const PID: usize = 1;
    pub const CPU: usize = 2;
    pub const MEMORY: usize = 3;
    pub const WORKING_SET: usize = 4;
    pub const DISK_READ: usize = 5;
    pub const DISK_WRITE: usize = 6;
    pub const THREADS: usize = 7;
    pub const HANDLES: usize = 8;
}

pub(crate) fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 240.0),
        Column::number("PID", 70.0),
        Column::number("CPU %", 70.0),
        Column::number("Memory", 95.0),
        Column::number("Working set", 95.0),
        Column::number("Disk read", 95.0),
        Column::number("Disk write", 95.0),
        Column::number("Threads", 70.0),
        Column::number("Handles", 75.0),
    ]
}

pub(crate) fn row_id(key: ProcessKey) -> RowId {
    RowId(u64::from(key.pid) ^ key.birth.0.rotate_left(32))
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
}

/// Adapts a snapshot's process list to the table.
pub(crate) struct ProcessRows<'a> {
    pub procs: &'a [ProcessSample],
    pub tree: &'a ProcessTree,
    pub interval_secs: f32,
    pub mem_total: f32,
}

impl ProcessRows<'_> {
    /// Root-first chain of ancestor names ending in the row itself, e.g.
    /// `wininit.exe › services.exe › svchost.exe`. `chain` is caller-owned scratch.
    pub fn ancestry(&self, row: usize, out: &mut String, chain: &mut Vec<u32>) {
        out.clear();
        chain.clear();
        let n = self.procs.len();
        let mut r = row;
        for _ in 0..n {
            chain.push(r as u32);
            match self.tree.parent(r) {
                Some(p) if p < n && self.visible(p) => r = p,
                _ => break,
            }
        }
        for (i, &r) in chain.iter().rev().enumerate() {
            if i > 0 {
                out.push_str(" › ");
            }
            out.push_str(self.procs[r as usize].name());
        }
    }

    /// Row index of a process by table id, if it is in this snapshot.
    #[must_use]
    pub fn row_of(&self, id: RowId) -> Option<usize> {
        (0..self.procs.len()).find(|&r| self.id(r) == id)
    }

    /// Number of rows the table lists.
    #[must_use]
    pub fn listed(&self) -> usize {
        (0..self.procs.len()).filter(|&r| self.visible(r)).count()
    }

    fn memory_heat(&self, private_bytes: u64) -> Option<f32> {
        // Memory heat is relative to a tenth of RAM: one process holding 10% of the
        // machine is fully hot.
        (self.mem_total > 0.0).then(|| (private_bytes as f32 / (self.mem_total * 0.1)).min(1.0))
    }
}

impl RowSource for ProcessRows<'_> {
    fn len(&self) -> usize {
        self.procs.len()
    }

    fn id(&self, row: usize) -> RowId {
        row_id(self.procs[row].key())
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let p = &self.procs[row];
        match col {
            col::NAME => {
                out.clear();
                out.push_str(p.name());
            }
            col::PID => format::count(out, p.key().pid),
            col::CPU => format::percent(out, p.cpu.get()),
            col::MEMORY => format::bytes(out, p.private_bytes),
            col::WORKING_SET => format::bytes(out, p.working_set),
            col::DISK_READ => format::rate(out, p.disk_read, self.interval_secs),
            col::DISK_WRITE => format::rate(out, p.disk_write, self.interval_secs),
            col::THREADS => format::count(out, p.threads),
            col::HANDLES => format::count(out, p.handles),
            _ => out.clear(),
        }
    }

    fn heat(&self, row: usize, col: usize) -> Option<f32> {
        let p = &self.procs[row];
        match col {
            col::CPU => Some((p.cpu.get() / 100.0).min(1.0)),
            col::MEMORY => self.memory_heat(p.private_bytes.get()),
            _ => None,
        }
    }

    fn visible(&self, row: usize) -> bool {
        // PID 0 is the kernel's idle accounting, not a process. Its "CPU" is the
        // machine's idle time, which would otherwise pin it to the top of every sort.
        self.procs[row].key().pid != 0
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (a, b) = (&self.procs[a], &self.procs[b]);
        match col {
            col::NAME => a
                .name()
                .to_ascii_lowercase()
                .cmp(&b.name().to_ascii_lowercase()),
            col::PID => a.key().pid.cmp(&b.key().pid),
            col::CPU => a.cpu.get().total_cmp(&b.cpu.get()),
            col::MEMORY => a.private_bytes.cmp(&b.private_bytes),
            col::WORKING_SET => a.working_set.cmp(&b.working_set),
            col::DISK_READ => a.disk_read.cmp(&b.disk_read),
            col::DISK_WRITE => a.disk_write.cmp(&b.disk_write),
            col::THREADS => a.threads.cmp(&b.threads),
            col::HANDLES => a.handles.cmp(&b.handles),
            _ => Ordering::Equal,
        }
    }

    fn parent(&self, row: usize) -> Option<usize> {
        self.tree.parent(row)
    }

    fn cell_collapsed(&self, row: usize, col: usize, out: &mut String) {
        let r = self.tree.rollup(row);
        match col {
            col::NAME => {
                out.clear();
                out.push_str(self.procs[row].name());
                let _ = write!(out, " ({})", r.descendants);
            }
            col::CPU => format::percent(out, r.cpu),
            col::MEMORY => format::bytes(out, Bytes(r.private_bytes)),
            col::WORKING_SET => format::bytes(out, Bytes(r.working_set)),
            col::DISK_READ => format::rate(out, Bytes(r.disk_read), self.interval_secs),
            col::DISK_WRITE => format::rate(out, Bytes(r.disk_write), self.interval_secs),
            col::THREADS => format::count(out, r.threads),
            col::HANDLES => format::count(out, r.handles),
            _ => self.cell(row, col, out),
        }
    }

    fn heat_collapsed(&self, row: usize, col: usize) -> Option<f32> {
        let r = self.tree.rollup(row);
        match col {
            col::CPU => Some((r.cpu / 100.0).min(1.0)),
            col::MEMORY => self.memory_heat(r.private_bytes),
            _ => None,
        }
    }

    fn compare_subtree(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (ra, rb) = (self.tree.rollup(a), self.tree.rollup(b));
        match col {
            col::CPU => ra.cpu.total_cmp(&rb.cpu),
            col::MEMORY => ra.private_bytes.cmp(&rb.private_bytes),
            col::WORKING_SET => ra.working_set.cmp(&rb.working_set),
            col::DISK_READ => ra.disk_read.cmp(&rb.disk_read),
            col::DISK_WRITE => ra.disk_write.cmp(&rb.disk_write),
            col::THREADS => ra.threads.cmp(&rb.threads),
            col::HANDLES => ra.handles.cmp(&rb.handles),
            _ => self.compare(a, b, col),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ot_model::process::{Integrity, ProcessStatic};
    use ot_model::Percent;
    use std::sync::Arc;

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
            }),
            cpu: Percent(cpu),
            working_set: Bytes(1),
            private_bytes: Bytes(1),
            disk_read: Bytes(0),
            disk_write: Bytes(0),
            net_rx: Bytes(0),
            net_tx: Bytes(0),
            threads: 1,
            handles: 1,
            power: None,
            gpu: None,
            suspended: false,
        }
    }

    fn tree_of(procs: &[ProcessSample]) -> ProcessTree {
        let mut t = ProcessTree::default();
        t.rebuild(procs);
        t
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
        let procs = vec![
            proc(0, None, 0.0),
            proc(4, Some(0), 0.0),
            proc(9, Some(4), 0.0),
        ];
        let t = tree_of(&procs);
        let rows = ProcessRows {
            procs: &procs,
            tree: &t,
            interval_secs: 1.0,
            mem_total: 0.0,
        };
        let mut out = String::new();
        let mut chain = Vec::new();
        rows.ancestry(2, &mut out, &mut chain);
        assert_eq!(out, "p4.exe › p9.exe");
        assert_eq!(rows.listed(), 2);
    }

    #[test]
    fn collapsed_cells_show_subtree_totals_and_descendant_count() {
        let procs = vec![
            proc(1, None, 1.0),
            proc(2, Some(1), 2.0),
            proc(3, Some(2), 4.0),
        ];
        let t = tree_of(&procs);
        let rows = ProcessRows {
            procs: &procs,
            tree: &t,
            interval_secs: 1.0,
            mem_total: 0.0,
        };
        let mut s = String::new();
        rows.cell(0, col::CPU, &mut s);
        assert_eq!(s, "1.0");
        rows.cell_collapsed(0, col::CPU, &mut s);
        assert_eq!(s, "7.0");
        rows.cell_collapsed(0, col::NAME, &mut s);
        assert_eq!(s, "p1.exe (2)");
        rows.cell_collapsed(0, col::PID, &mut s);
        assert_eq!(s, "1", "identity columns never aggregate");
        assert_eq!(rows.compare_subtree(0, 2, col::CPU), Ordering::Greater);
        assert_eq!(rows.compare(0, 2, col::CPU), Ordering::Less);
    }
}
