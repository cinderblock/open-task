//! Who has been using the CPU: each process's clock cycles, as a total that fades.
//!
//! The table's CPU column is the last interval, which says who is busy this instant:
//! power, in watts. The usage views ask what was consumed: energy, in joules. So
//! [`Usage`] adds up the cycles each process uses, and, so that the totals do not
//! grow without bound, lets them fade: every second each total loses a set share
//! ([`Usage::decay`], 5 % unless changed). A process that was busy lately is at the
//! top, one that has been steadily at work for a long time is still in view, and a
//! short burst shows for a while and then sinks.
//!
//! A total that fades this way is an exponentially weighted sum. At a steady rate it
//! settles at the rate times [`Usage::time_constant`] (19.5 s at 5 % a second), so
//! it reads as "what was used recently, the last few time constants counted most".
//! The fading is by elapsed time, not by sample, and cycles are taken as spent
//! evenly across the interval they arrived in, so the numbers do not depend on how
//! often the system is sampled.
//!
//! Edges, handled so the numbers stay honest:
//! - a process that starts while the session runs counts everything it has used; one
//!   that was already running when the session began counts from then;
//! - a process that exits stays, as a ghost fading like the rest, until its total is
//!   too small to matter;
//! - cycles a process uses between its last sample and its exit, and all of a
//!   process that lives and dies between two samples, are never seen.
//!
//! [`Usage`] also keeps the history behind the usage chart: for every interval, the
//! cycles each *program* used, a program being every process of one label (the two
//! hundred `rustc.exe` of a build are one line of the chart). The label is the
//! name, and for the children of a Chromium-based program also their role, so
//! Chrome's renderers, `chrome.exe (Renderer)`, are one line and its GPU process,
//! `chrome.exe (GPU)`, another. As much is kept as the charts reach back ([`Usage::set_history_span`],
//! an hour unless set otherwise): the last ten minutes sample by sample, the rest in
//! steps of ten seconds, or wider when the reach is long, so a day is at most 720
//! steps. Each step lists only the programs that ran in it, so the cost is set by how
//! many programs are busy, not by how many processes exist.
//!
//! Beside the cycles, each step keeps what the chart's other readings need: the CPU
//! time each program used, the private working set each held, its share of the
//! GPUs' time and the dedicated and shared GPU memory it held, and the machine's
//! side (its CPU capacity and how much of it was busy, its RAM and how much of it
//! was in use, its GPUs' time and how much of it was busy, and their dedicated
//! memory in use), so a program's share can be set against the summary charts.
//! Every quantity is kept as an integral over the step (CPU milliseconds,
//! byte-milliseconds), so folding steps together is a sum and the averages come
//! out exact. Memory is listed for every program holding any, which makes it the
//! larger part of a step.
//!
//! And it keeps each process's lineage ([`Seen`]): its parent and when it ran, for
//! every process seen as far back as the history reaches and for every ancestor of
//! those, gone or not, so that a band of the chart can say which processes made it
//! up at a moment and what started each of them, all the way to the root.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ot_model::process::ProcessStatic;
use ot_model::{ProcessKey, Tick};

use crate::snapshot::Snapshot;

/// A ghost below this many cycles (a fraction of a millisecond of work) is dropped.
const GHOST_FLOOR: f64 = 1.0e6;
/// So is one below this share of everything in the totals.
const GHOST_SHARE: f64 = 1.0e-4;
/// History kept sample by sample.
const RAW_FRAMES: usize = 600;
/// The narrowest step older history is kept in, and the most steps a history has,
/// which sets the step width of a long one.
const STEP_MIN_MS: i64 = 10_000;
const STEPS: i64 = 720;
/// How far back the history reaches unless set otherwise: an hour.
pub const DEFAULT_HISTORY_MS: i64 = 3_600_000;

/// Width of the steps a history reaching back `history_ms` folds old frames into:
/// ten seconds, or wider so that there are at most [`STEPS`] of them.
#[must_use]
pub fn step_ms(history_ms: i64) -> i64 {
    (history_ms / STEPS).max(STEP_MIN_MS)
}

/// About how many bytes a history reaching back `span_ms` takes, at `frame_bytes`
/// a frame ([`Usage::frame_bytes`] for a running session). An estimate for the
/// Settings page; the raw frames count whole since their reach depends on the
/// sampling interval.
#[must_use]
pub fn history_bytes(span_ms: i64, frame_bytes: usize) -> usize {
    let span_ms = span_ms.max(STEP_MIN_MS);
    let steps = (span_ms / step_ms(span_ms)) as usize + 1;
    (RAW_FRAMES + steps) * frame_bytes
}

/// A program in the usage history: every process of one label. An index into
/// [`Usage::program_name`], stable for the session.
pub type ProgramId = u32;

/// One process's fading total.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessUsage<'a> {
    pub statics: &'a Arc<ProcessStatic>,
    /// Cycles used, faded.
    pub used: f64,
    /// False once the process has exited; it stays until its total has faded away.
    pub alive: bool,
    /// The program the process's cycles are charted under.
    pub program: ProgramId,
}

/// Which of a stretch's per-program lists: an index into its lists, in the order
/// [`Frame`] has them.
const CYCLES: usize = 0;
const CPU_MS: usize = 1;
const MEMORY: usize = 2;
const GPU_MS: usize = 3;
const GPU_DEDICATED: usize = 4;
const GPU_SHARED: usize = 5;
const KINDS: usize = 6;

/// One stretch of the usage history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame<'a> {
    /// When the stretch ended, and how long it was, in milliseconds (the end since
    /// the Unix epoch).
    pub end_ms: i64,
    pub span_ms: i64,
    /// Cycles each program used in it. Programs that used none are not listed.
    pub cycles: &'a [(ProgramId, f32)],
    /// CPU time each program used in it, in milliseconds. Programs that used none
    /// are not listed.
    pub cpu_ms: &'a [(ProgramId, f32)],
    /// The private working set each program held over it, in byte-milliseconds:
    /// divided by `span_ms`, its average size. Programs that held none are not
    /// listed.
    pub memory: &'a [(ProgramId, f32)],
    /// GPU time each program used in it, in milliseconds: its share of the engine
    /// busiest on each hardware GPU, summed over the GPUs, times the stretch.
    /// Programs that used none are not listed.
    pub gpu_ms: &'a [(ProgramId, f32)],
    /// The dedicated and the shared GPU memory each program held over it, in
    /// byte-milliseconds. Programs that held none are not listed.
    pub gpu_dedicated: &'a [(ProgramId, f32)],
    pub gpu_shared: &'a [(ProgramId, f32)],
    pub machine: Machine,
}

impl<'a> Frame<'a> {
    fn new<L: std::ops::Deref<Target = [(ProgramId, f32)]>>(
        end_ms: i64,
        span_ms: i64,
        lists: &'a Lists<L>,
        machine: Machine,
    ) -> Self {
        Frame {
            end_ms,
            span_ms,
            cycles: &lists[CYCLES],
            cpu_ms: &lists[CPU_MS],
            memory: &lists[MEMORY],
            gpu_ms: &lists[GPU_MS],
            gpu_dedicated: &lists[GPU_DEDICATED],
            gpu_shared: &lists[GPU_SHARED],
            machine,
        }
    }
}

/// The machine's side of a stretch of the history, each an integral over it like
/// the programs' numbers: divided by the stretch's length, the average.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Machine {
    /// CPU time there was to use: the stretch's length times the logical
    /// processors, in milliseconds. What [`Frame::cpu_ms`] is a share of.
    pub cpu_capacity_ms: f64,
    /// Of that, how much the summary CPU chart counts as busy.
    pub cpu_busy_ms: f64,
    /// Physical memory installed, and the part the summary memory chart counts as
    /// in use, in byte-milliseconds.
    pub ram: f64,
    pub ram_in_use: f64,
    /// GPU time there was to use: the stretch's length times the hardware GPUs,
    /// in milliseconds. What [`Frame::gpu_ms`] is a share of.
    pub gpu_capacity_ms: f64,
    /// Of that, how much the GPUs' utilization counts as busy.
    pub gpu_busy_ms: f64,
    /// The hardware GPUs' dedicated memory in use, in byte-milliseconds.
    pub gpu_dedicated_in_use: f64,
}

impl Machine {
    fn add(&mut self, other: Self) {
        self.cpu_capacity_ms += other.cpu_capacity_ms;
        self.cpu_busy_ms += other.cpu_busy_ms;
        self.ram += other.ram;
        self.ram_in_use += other.ram_in_use;
        self.gpu_capacity_ms += other.gpu_capacity_ms;
        self.gpu_busy_ms += other.gpu_busy_ms;
        self.gpu_dedicated_in_use += other.gpu_dedicated_in_use;
    }
}

/// What the history remembers of a process it has seen, after it has gone too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    /// The parent's identity, as the probe resolved it: absent for a root, and for
    /// a process whose parent had exited before it was first seen.
    pub parent: Option<ProcessKey>,
    /// Where `parent` is absent: the PID the system gave for a parent that had
    /// exited before this process was first seen, all that is known of it.
    pub lost_parent: Option<u32>,
    /// The program it is charted under, which [`Usage::program_name`] names.
    pub program: ProgramId,
    /// The first and the last snapshot it was in, in ms since the Unix epoch.
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Debug, Clone)]
struct Entry {
    statics: Arc<ProcessStatic>,
    /// Cumulative cycles and CPU time at the last sample.
    last: u64,
    last_cpu: Duration,
    total: f64,
    alive: bool,
    program: ProgramId,
}

/// Each program's numbers for one stretch, a list for each of [`CYCLES`] to
/// [`GPU_SHARED`], sorted by program, without the zeros.
type Lists<L> = [L; KINDS];

type List = Box<[(ProgramId, f32)]>;

#[derive(Debug, Clone)]
struct Stored {
    end_ms: i64,
    span_ms: i64,
    lists: Lists<List>,
    machine: Machine,
}

impl Stored {
    fn frame(&self) -> Frame<'_> {
        Frame::new(self.end_ms, self.span_ms, &self.lists, self.machine)
    }

    fn entries(&self) -> usize {
        self.lists.iter().map(|l| l.len()).sum()
    }
}

/// A coarse step still being filled from the raw frames leaving the raw history.
#[derive(Debug, Clone, Default)]
struct Open {
    /// Which step: `end_ms` of its frames divided by [`COARSE_MS`], rounded down.
    index: i64,
    end_ms: i64,
    span_ms: i64,
    lists: Lists<Vec<(ProgramId, f32)>>,
    machine: Machine,
}

/// Add `from` into `into`, both sorted by program, keeping it sorted; `scratch` is
/// spare room for the result.
fn merge(
    into: &mut Vec<(ProgramId, f32)>,
    from: &[(ProgramId, f32)],
    scratch: &mut Vec<(ProgramId, f32)>,
) {
    scratch.clear();
    scratch.reserve(into.len() + from.len());
    let (mut a, mut b) = (into.iter().peekable(), from.iter().peekable());
    loop {
        let next = match (a.peek(), b.peek()) {
            (Some(&&x), Some(&&y)) if x.0 == y.0 => {
                a.next();
                b.next();
                (x.0, x.1 + y.1)
            }
            (Some(&&x), Some(&&y)) if x.0 < y.0 => {
                a.next();
                x
            }
            (_, Some(&&y)) => {
                b.next();
                y
            }
            (Some(&&x), None) => {
                a.next();
                x
            }
            (None, None) => break,
        };
        scratch.push(next);
    }
    std::mem::swap(into, scratch);
}

/// Fading per-process cycle totals and the per-program history, fed by snapshots.
#[derive(Debug, Clone)]
pub struct Usage {
    /// Share of every total lost each second.
    decay: f64,
    procs: HashMap<ProcessKey, Entry>,
    /// Every process seen as far back as the history reaches, and their ancestors;
    /// and when that was last trimmed to the reach.
    seen: HashMap<ProcessKey, Seen>,
    trimmed_ms: i64,
    /// Sum of every total, the exited processes' included.
    sum: f64,
    /// Time of the newest snapshot and of the first, in ms since the epoch.
    now_ms: i64,
    first_ms: Option<i64>,
    last_tick: Option<Tick>,

    programs: Vec<String>,
    program_ids: HashMap<String, ProgramId>,
    /// Oldest first; `coarse` is older than `raw`, `open` sits between them.
    raw: VecDeque<Stored>,
    coarse: VecDeque<Stored>,
    open: Option<Open>,
    /// Each program's fading total of the cycles in the stretches that have passed
    /// the reach, as at the end of the newest of them ([`Usage::faded_out`]).
    faded_out: Vec<f64>,
    faded_out_ms: Option<i64>,
    /// Scratch: this interval's numbers by program, one of each kind of list,
    /// then the same as lists; and room for merging lists.
    by_program: Vec<[f64; KINDS]>,
    interval: Lists<Vec<(ProgramId, f32)>>,
    merged: Vec<(ProgramId, f32)>,
    /// How far back the history reaches, in milliseconds.
    history_ms: i64,
    /// Changes whenever anything here does, and no two `Usage`s share one
    /// ([`Usage::revision`]).
    revision: u64,
}

/// A number no `Usage` has had: a view can keep what it worked out from one
/// until its revision changes.
fn next_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One step of a fading total: the share of it left after `secs`, and the share
/// left, at the end, of what arrived evenly over that time. A total `t` that gains
/// `n` over the step becomes `t * kept + n * arrived`.
#[must_use]
pub fn fade(decay: f64, secs: f64) -> (f64, f64) {
    let decay = clamp_decay(decay);
    let rate = -(1.0 - decay).ln();
    let x = rate * secs;
    let keep = (-x).exp();
    // (1 - e^-x) / x, which is 1 at x = 0.
    let arrived = if x < 1e-9 { 1.0 } else { -(-x).exp_m1() / x };
    (keep, arrived)
}

impl Usage {
    /// Totals that lose `decay` of themselves every second (0.05 is 5 %). Clamped
    /// to a range in which the totals neither stand still nor vanish at once.
    #[must_use]
    pub fn new(decay: f64) -> Self {
        Self {
            decay: clamp_decay(decay),
            procs: HashMap::new(),
            seen: HashMap::new(),
            trimmed_ms: 0,
            sum: 0.0,
            now_ms: 0,
            first_ms: None,
            last_tick: None,
            programs: Vec::new(),
            program_ids: HashMap::new(),
            raw: VecDeque::new(),
            coarse: VecDeque::new(),
            open: None,
            faded_out: Vec::new(),
            faded_out_ms: None,
            by_program: Vec::new(),
            interval: Lists::default(),
            merged: Vec::new(),
            history_ms: DEFAULT_HISTORY_MS,
            revision: next_revision(),
        }
    }

    /// Changes whenever the totals, the history, the fade rate or the reach do,
    /// and differs between any two `Usage`s that are not copies of each other.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Change how fast the totals fade. What has accumulated stays, and fades at the
    /// new rate from here on.
    pub fn set_decay(&mut self, decay: f64) {
        self.decay = clamp_decay(decay);
        self.revision = next_revision();
    }

    /// Keep history reaching back `ms` milliseconds (at least ten seconds). What is
    /// older goes as the next snapshot arrives; steps already made keep their width,
    /// new ones take the width the new reach calls for ([`step_ms`]).
    pub fn set_history_span(&mut self, ms: i64) {
        self.history_ms = ms.max(STEP_MIN_MS);
        self.revision = next_revision();
    }

    /// How far back the history reaches, in milliseconds.
    #[must_use]
    pub fn history_span(&self) -> i64 {
        self.history_ms
    }

    /// Bytes the history holds now.
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        let entry = size_of::<(ProgramId, f32)>();
        let stored = |s: &Stored| size_of::<Stored>() + s.entries() * entry;
        let open = self.open.as_ref().map_or(0, |o| {
            size_of::<Open>() + o.lists.iter().map(Vec::capacity).sum::<usize>() * entry
        });
        self.raw
            .iter()
            .chain(&self.coarse)
            .map(stored)
            .sum::<usize>()
            + open
    }

    /// Bytes a frame of this history takes on average, for projecting the cost of
    /// another reach with [`history_bytes`]. A guess, until there are frames to go
    /// by, of sixteen busy programs, a hundred and fifty holding memory, ten using
    /// the GPU and forty holding GPU memory of each kind.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        let frames = self.raw.len() + self.coarse.len();
        self.history_bytes().checked_div(frames).unwrap_or(
            size_of::<Stored>() + (2 * 16 + 150 + 10 + 2 * 40) * size_of::<(ProgramId, f32)>(),
        )
    }

    /// Share of every total lost each second.
    #[must_use]
    pub fn decay(&self) -> f64 {
        self.decay
    }

    /// Seconds in which a total falls to 1/e of itself. A process at a steady rate
    /// settles at that rate times this.
    #[must_use]
    pub fn time_constant(&self) -> f64 {
        time_constant(self.decay)
    }

    /// Fold a snapshot in. Repeats of the last tick and empty snapshots are
    /// ignored, like the timeline's.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.is_empty() || self.last_tick == Some(snap.tick) {
            return;
        }
        self.last_tick = Some(snap.tick);
        self.revision = next_revision();
        let now = snap
            .taken_at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let first = self.first_ms.is_none();
        let session_start = *self.first_ms.get_or_insert(now);
        let span_ms = if first { 0 } else { (now - self.now_ms).max(0) };
        self.now_ms = now;
        let (keep, arrived) = fade(self.decay, span_ms as f64 / 1000.0);

        for e in self.procs.values_mut() {
            e.alive = false;
            e.total *= keep;
        }
        self.by_program.clear();
        self.by_program.resize(self.programs.len(), [0.0; KINDS]);
        for p in &snap.processes {
            // PID 0 is the kernel's idle accounting, not a process.
            if p.key().pid == 0 {
                continue;
            }
            let (programs, ids) = (&mut self.programs, &mut self.program_ids);
            let mut seen_before = true;
            let e = self.procs.entry(p.key()).or_insert_with(|| {
                seen_before = false;
                Entry {
                    statics: Arc::clone(&p.statics),
                    last: p.cycles,
                    last_cpu: p.cpu_time,
                    total: 0.0,
                    alive: true,
                    program: program_id(programs, ids, &p.statics),
                }
            });
            e.alive = true;
            if !Arc::ptr_eq(&e.statics, &p.statics) {
                e.statics = Arc::clone(&p.statics);
            }
            let started_since = !first
                && p.statics
                    .started_unix_ms
                    .is_some_and(|s| s >= session_start);
            let (used, cpu) = if seen_before {
                (
                    p.cycles.saturating_sub(e.last),
                    p.cpu_time.saturating_sub(e.last_cpu),
                )
            } else if started_since {
                // Started since the last snapshot: everything it has used is new.
                (p.cycles, p.cpu_time)
            } else {
                // Already running when first seen: what it used before then was
                // used before anyone was watching.
                (0, Duration::ZERO)
            };
            e.last = p.cycles;
            e.last_cpu = p.cpu_time;
            if used > 0 {
                e.total += used as f64 * arrived;
            }
            self.seen
                .entry(p.key())
                .and_modify(|s| {
                    s.last_ms = now;
                    // The probe can learn the parent after first sight.
                    s.parent = p.statics.parent;
                    s.lost_parent = lost_parent(&p.statics);
                })
                .or_insert(Seen {
                    parent: p.statics.parent,
                    lost_parent: lost_parent(&p.statics),
                    program: e.program,
                    first_ms: now,
                    last_ms: now,
                });
            let program = e.program as usize;
            if program >= self.by_program.len() {
                self.by_program.resize(program + 1, [0.0; KINDS]);
            }
            let sums = &mut self.by_program[program];
            let span = span_ms as f64;
            sums[CYCLES] += used as f64;
            sums[CPU_MS] += cpu.as_secs_f64() * 1000.0;
            sums[MEMORY] += p.private_working_set.get() as f64 * span;
            sums[GPU_MS] += f64::from(p.gpu_share.get()) / 100.0 * span;
            sums[GPU_DEDICATED] += p.gpu_dedicated.get() as f64 * span;
            sums[GPU_SHARED] += p.gpu_shared.get() as f64 * span;
        }
        self.sum = self.procs.values().map(|e| e.total).sum();
        let floor = GHOST_FLOOR.max(self.sum * GHOST_SHARE);
        self.procs.retain(|_, e| e.alive || e.total >= floor);
        if now - self.trimmed_ms >= STEP_MIN_MS {
            self.trim_lineage();
        }

        if span_ms > 0 {
            self.record(snap, now, span_ms);
        }
    }

    /// Add the interval that ended at `now`, `span_ms` long, to the history: what
    /// each program used and held, from the sums [`Self::observe`] made, and the
    /// machine's side from `snap`.
    fn record(&mut self, snap: &Snapshot, now: i64, span_ms: i64) {
        let lists = &mut self.interval;
        for list in lists.iter_mut() {
            list.clear();
        }
        for (program, sums) in self.by_program.iter().enumerate() {
            let program = program as ProgramId;
            for (list, &v) in lists.iter_mut().zip(sums) {
                if v > 0.0 {
                    list.push((program, v as f32));
                }
            }
        }
        let span = span_ms as f64;
        let capacity = span * snap.cpu.cores.len() as f64;
        // Software adapters (the basic render driver) are left out, as the
        // Performance page leaves them out, and as the probe leaves them out of
        // each process's share.
        let gpus = || snap.gpus.iter().filter(|g| !g.info.software);
        let machine = Machine {
            cpu_capacity_ms: capacity,
            cpu_busy_ms: capacity * f64::from(snap.cpu.total.get()).clamp(0.0, 100.0) / 100.0,
            ram: snap.memory.total.get() as f64 * span,
            ram_in_use: snap.memory.in_use().get() as f64 * span,
            gpu_capacity_ms: span * gpus().count() as f64,
            gpu_busy_ms: gpus()
                .map(|g| span * f64::from(g.utilization.get()).clamp(0.0, 100.0) / 100.0)
                .sum(),
            gpu_dedicated_in_use: gpus().map(|g| g.dedicated_used.get() as f64 * span).sum(),
        };
        let lists = self.interval.each_ref().map(|l| l.as_slice().into());
        self.push_frame(Stored {
            end_ms: now,
            span_ms,
            lists,
            machine,
        });
    }

    /// Append a frame to the history, moving what leaves the raw part into the
    /// coarse steps and dropping what has passed the reach.
    fn push_frame(&mut self, frame: Stored) {
        let now = frame.end_ms;
        let reach = self.history_ms;
        let step = step_ms(reach);
        self.raw.push_back(frame);
        while self.raw.len() > RAW_FRAMES
            || self.raw.front().is_some_and(|f| now - f.end_ms > reach)
        {
            let Some(old) = self.raw.pop_front() else {
                break;
            };
            let index = old.end_ms.div_euclid(step);
            if self.open.as_ref().is_some_and(|o| o.index != index) {
                self.close_open();
            }
            let open = self.open.get_or_insert_with(|| Open {
                index,
                ..Open::default()
            });
            open.end_ms = old.end_ms;
            open.span_ms += old.span_ms;
            open.machine.add(old.machine);
            for (into, from) in open.lists.iter_mut().zip(&old.lists) {
                merge(into, from, &mut self.merged);
            }
        }
        // Steps that have passed the reach go, oldest first, the one being filled
        // included; their cycles stay on in the faded-out totals.
        while self.coarse.front().is_some_and(|f| now - f.end_ms > reach) {
            if let Some(old) = self.coarse.pop_front() {
                self.fade_out(old.end_ms, old.span_ms, &old.lists[CYCLES]);
            }
        }
        if self.open.as_ref().is_some_and(|o| now - o.end_ms > reach) {
            if let Some(o) = self.open.take() {
                self.fade_out(o.end_ms, o.span_ms, &o.lists[CYCLES]);
            }
        }
    }

    /// Fold the cycles of a stretch leaving the history, ending at `end_ms` and
    /// `span_ms` long, into the faded-out totals.
    fn fade_out(&mut self, end_ms: i64, span_ms: i64, cycles: &[(ProgramId, f32)]) {
        // Stretches follow one another, so the totals are as at this one's start.
        let since_ms = self.faded_out_ms.map_or(span_ms, |t| end_ms - t);
        let (kept, _) = fade(self.decay, since_ms.max(0) as f64 / 1000.0);
        let (_, arrived) = fade(self.decay, span_ms.max(0) as f64 / 1000.0);
        for t in &mut self.faded_out {
            *t *= kept;
        }
        for &(g, x) in cycles {
            let g = g as usize;
            if g >= self.faded_out.len() {
                self.faded_out.resize(g + 1, 0.0);
            }
            self.faded_out[g] += f64::from(x) * arrived;
        }
        self.faded_out_ms = Some(end_ms);
    }

    /// Forget the processes last seen before the history's reach, except those that
    /// are ancestors of one still remembered, so no chain is cut short.
    fn trim_lineage(&mut self) {
        self.trimmed_ms = self.now_ms;
        let horizon = self.now_ms - self.history_ms;
        let mut keep = HashSet::with_capacity(self.seen.len());
        for (&key, s) in &self.seen {
            if s.last_ms < horizon {
                continue;
            }
            let mut at = Some(key);
            // Stops at a key already kept: its ancestors are kept too.
            while let Some(k) = at.filter(|&k| keep.insert(k)) {
                at = self.seen.get(&k).and_then(|s| s.parent);
            }
        }
        self.seen.retain(|k, _| keep.contains(k));
    }

    fn close_open(&mut self) {
        if let Some(o) = self.open.take() {
            self.coarse.push_back(Stored {
                end_ms: o.end_ms,
                span_ms: o.span_ms,
                lists: o.lists.map(Into::into),
                machine: o.machine,
            });
        }
    }

    /// The lineage of a process seen as far back as the history reaches, or of an
    /// ancestor of one.
    #[must_use]
    pub fn seen(&self, key: ProcessKey) -> Option<&Seen> {
        self.seen.get(&key)
    }

    /// The processes of `program` in any snapshot taken after `after_ms` and no
    /// later than `until_ms`: those that make up its share of the stretches
    /// between. In no particular order.
    pub fn seen_in(
        &self,
        program: ProgramId,
        after_ms: i64,
        until_ms: i64,
    ) -> impl Iterator<Item = (ProcessKey, &Seen)> + '_ {
        self.seen.iter().filter_map(move |(&k, s)| {
            (s.program == program && s.first_ms <= until_ms && s.last_ms > after_ms)
                .then_some((k, s))
        })
    }

    /// Whether a process was in the newest snapshot.
    #[must_use]
    pub fn running(&self, key: ProcessKey) -> bool {
        self.seen
            .get(&key)
            .is_some_and(|s| s.last_ms == self.now_ms)
    }

    /// Every process with its fading total, the exited ones included, in no
    /// particular order.
    pub fn iter(&self) -> impl Iterator<Item = (ProcessKey, ProcessUsage<'_>)> + '_ {
        self.procs.iter().map(|(&k, e)| {
            (
                k,
                ProcessUsage {
                    statics: &e.statics,
                    used: e.total,
                    alive: e.alive,
                    program: e.program,
                },
            )
        })
    }

    /// One process's fading total, in cycles.
    #[must_use]
    pub fn used(&self, key: ProcessKey) -> Option<f64> {
        self.procs.get(&key).map(|e| e.total)
    }

    /// Every total together, the exited processes' included.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.sum
    }

    /// Time of the newest snapshot, in ms since the Unix epoch.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// The label every process of `program` runs under: `chrome.exe`, or with a
    /// role `chrome.exe (GPU)`.
    #[must_use]
    pub fn program_name(&self, program: ProgramId) -> &str {
        self.programs
            .get(program as usize)
            .map_or("", String::as_str)
    }

    /// The program processes labelled `label` are charted under, if any has been
    /// seen.
    #[must_use]
    pub fn program(&self, label: &str) -> Option<ProgramId> {
        self.program_ids.get(label).copied()
    }

    /// The program a process is charted under, if one like it has been seen.
    #[must_use]
    pub fn program_of(&self, statics: &ProcessStatic) -> Option<ProgramId> {
        self.program(&label(statics))
    }

    /// Programs seen this session; ids run from zero to this.
    #[must_use]
    pub fn programs(&self) -> usize {
        self.programs.len()
    }

    /// Each program's fading total of the cycles used before the history's oldest
    /// stretch, as at that stretch's start, indexed by [`ProgramId`] (programs past
    /// the end have none). A fading sum run over [`Self::frames`], oldest first,
    /// starts from here, so that it comes to the totals even once the history no
    /// longer reaches back to the session's start.
    #[must_use]
    pub fn faded_out(&self) -> &[f64] {
        &self.faded_out
    }

    /// The history, newest first: every sample of the last ten minutes, then steps
    /// for the rest of the reach. No stretch is covered twice.
    pub fn frames(&self) -> impl Iterator<Item = Frame<'_>> + '_ {
        let open = self
            .open
            .as_ref()
            .map(|o| Frame::new(o.end_ms, o.span_ms, &o.lists, o.machine));
        self.raw
            .iter()
            .rev()
            .map(Stored::frame)
            .chain(open)
            .chain(self.coarse.iter().rev().map(Stored::frame))
    }
}

/// Seconds in which a total losing `decay` of itself each second falls to 1/e.
#[must_use]
pub fn time_constant(decay: f64) -> f64 {
    -1.0 / (1.0 - clamp_decay(decay)).ln()
}

/// Seconds in which a total losing `decay` of itself each second halves.
#[must_use]
pub fn half_life(decay: f64) -> f64 {
    time_constant(decay) * std::f64::consts::LN_2
}

fn clamp_decay(decay: f64) -> f64 {
    if decay.is_finite() {
        decay.clamp(0.001, 0.9)
    } else {
        0.05
    }
}

fn program_id(
    programs: &mut Vec<String>,
    ids: &mut HashMap<String, ProgramId>,
    statics: &ProcessStatic,
) -> ProgramId {
    let label = label(statics);
    if let Some(&id) = ids.get(&*label) {
        return id;
    }
    let id = ProgramId::try_from(programs.len()).unwrap_or(ProgramId::MAX);
    programs.push(label.clone().into_owned());
    ids.insert(label.into_owned(), id);
    id
}

/// The PID of a parent the probe could not find: one the system reported (not the
/// idle process's 0) that did not resolve.
fn lost_parent(statics: &ProcessStatic) -> Option<u32> {
    match statics.parent {
        Some(_) => None,
        None => statics.parent_pid.filter(|&pid| pid != 0),
    }
}

/// A process's label, borrowed when it is just the name, as most are.
fn label(statics: &ProcessStatic) -> Cow<'_, str> {
    let label = statics.label();
    if label.is_name() {
        Cow::Borrowed(&statics.name)
    } else {
        Cow::Owned(label.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::process::{Integrity, ProcessSample};
    use ot_model::{Bytes, Percent};
    use std::time::Duration;

    /// A billion cycles: about a third of a second of one core.
    const G: u64 = 1_000_000_000;

    fn named(pid: u32, name: &str, started_ms: Option<i64>, cycles: u64) -> ProcessSample {
        ProcessSample {
            statics: Arc::new(ProcessStatic {
                key: ProcessKey::new(pid, 1),
                parent: None,
                name: name.to_owned(),
                image_path: None,
                command_line: None,
                user: None,
                integrity: Integrity::Unknown,
                started_unix_ms: started_ms,
                ..ProcessStatic::default()
            }),
            cpu: Percent(0.0),
            cpu_time: Duration::ZERO,
            cycles,
            working_set: Bytes(0),
            private_bytes: Bytes(0),
            disk_read: Bytes(0),
            disk_write: Bytes(0),
            net_rx: Bytes(0),
            net_tx: Bytes(0),
            threads: 1,
            handles: 1,
            ..ProcessSample::default()
        }
    }

    fn proc(pid: u32, started_ms: Option<i64>, cycles: u64) -> ProcessSample {
        named(pid, &format!("p{pid}.exe"), started_ms, cycles)
    }

    /// A snapshot at `ms` milliseconds after the epoch.
    fn snap_ms(ms: u64, procs: Vec<ProcessSample>) -> Snapshot {
        Snapshot {
            tick: Tick(ms),
            taken_at: Some(UNIX_EPOCH + Duration::from_millis(ms)),
            interval: Duration::from_secs(1),
            processes: procs,
            ..Default::default()
        }
    }

    fn snap(secs: u64, procs: Vec<ProcessSample>) -> Snapshot {
        snap_ms(secs * 1000, procs)
    }

    fn used(u: &Usage, pid: u32) -> f64 {
        u.used(ProcessKey::new(pid, 1)).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn the_revision_changes_with_every_change_and_is_never_shared() {
        let mut u = Usage::new(0.05);
        let other = Usage::new(0.05);
        assert_ne!(
            u.revision(),
            other.revision(),
            "two of them, even both empty"
        );
        let r = u.revision();
        u.observe(&snap(1000, vec![proc(1, Some(0), 5 * G)]));
        assert_ne!(u.revision(), r, "a snapshot");
        let r = u.revision();
        u.observe(&snap(1000, vec![proc(1, Some(0), 6 * G)]));
        assert_eq!(u.revision(), r, "a repeat of the last tick is ignored");
        u.set_decay(0.1);
        assert_ne!(u.revision(), r, "the fade rate");
        let r = u.revision();
        u.set_history_span(60_000);
        assert_ne!(u.revision(), r, "the reach");
        assert_eq!(u.clone().revision(), u.revision(), "a copy is the same");
    }

    #[test]
    fn a_steady_rate_settles_at_the_rate_times_the_time_constant() {
        let mut u = Usage::new(0.05);
        assert!((u.time_constant() - 19.4957).abs() < 1e-3);
        assert!((half_life(0.05) - 13.5134).abs() < 1e-3);
        // One G of cycles a second, for five minutes.
        for s in 0..=300u64 {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), 50 * G + s * G)]));
        }
        let settled = G as f64 * u.time_constant();
        assert!(
            (used(&u, 1) / settled - 1.0).abs() < 1e-5,
            "{}",
            used(&u, 1)
        );
        assert!(close(u.total(), used(&u, 1)));
    }

    #[test]
    fn a_sum_over_the_history_from_what_faded_out_of_it_comes_to_the_totals() {
        let mut u = Usage::new(0.05);
        u.set_history_span(60_000);
        assert!(u.faded_out().is_empty(), "nothing has left yet");
        // p1 steady at a G a second; p2 busy for its first minute only, which
        // has left the history by the end.
        for s in 0..=300u64 {
            let busy = s.min(60);
            u.observe(&snap(
                1000 + s,
                vec![proc(1, Some(0), s * G), proc(2, Some(0), 3 * busy * G)],
            ));
        }
        let oldest = u.frames().last().unwrap();
        assert!(u.now_ms() - oldest.end_ms <= 60_000, "the history was cut");
        let mut sum = u.faded_out().to_vec();
        sum.resize(u.programs(), 0.0);
        let frames: Vec<_> = u.frames().collect();
        for f in frames.iter().rev() {
            let (kept, arrived) = fade(u.decay(), f.span_ms as f64 / 1000.0);
            for t in &mut sum {
                *t *= kept;
            }
            for &(g, x) in f.cycles {
                sum[g as usize] += f64::from(x) * arrived;
            }
        }
        let of = |pid| u.program(&format!("p{pid}.exe")).unwrap() as usize;
        for pid in [1, 2] {
            assert!(
                (sum[of(pid)] / used(&u, pid) - 1.0).abs() < 1e-5,
                "p{pid}: {} {}",
                sum[of(pid)],
                used(&u, pid)
            );
        }
        // The steady one had long settled by the history's start.
        let settled = G as f64 * u.time_constant();
        assert!((u.faded_out()[of(1)] / settled - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_totals_do_not_depend_on_how_often_the_system_is_sampled() {
        // The same steady rate, sampled every 250 ms and every 4 s.
        let run = |step_ms: u64| {
            let mut u = Usage::new(0.05);
            let mut t = 0;
            while t <= 120_000 {
                u.observe(&snap_ms(
                    1_000_000 + t,
                    vec![proc(1, Some(0), t * (G / 1000))],
                ));
                t += step_ms;
            }
            used(&u, 1)
        };
        let (fast, slow) = (run(250), run(4000));
        assert!((fast / slow - 1.0).abs() < 1e-3, "{fast} {slow}");
    }

    #[test]
    fn a_total_loses_the_set_share_every_second() {
        let mut u = Usage::new(0.05);
        u.observe(&snap(1000, vec![proc(1, Some(0), 0)]));
        u.observe(&snap(1001, vec![proc(1, Some(0), 10 * G)]));
        let after_burst = used(&u, 1);
        // Idle from here: 5 % less each second, whatever the sampling.
        u.observe(&snap(1002, vec![proc(1, Some(0), 10 * G)]));
        assert!(close(used(&u, 1), after_burst * 0.95));
        u.observe(&snap(1012, vec![proc(1, Some(0), 10 * G)]));
        assert!(close(used(&u, 1), after_burst * 0.95f64.powi(11)));
        // A faster fade applies from the change on.
        u.set_decay(0.5);
        u.observe(&snap(1013, vec![proc(1, Some(0), 10 * G)]));
        assert!(close(used(&u, 1), after_burst * 0.95f64.powi(11) * 0.5));
    }

    #[test]
    fn a_burst_and_a_steady_worker_meet_and_part() {
        // p1 uses 1 G a second throughout; p2 uses 20 G in one second, once.
        let mut u = Usage::new(0.05);
        let burst_at = 100;
        for s in 0..=200u64 {
            let two = if s >= burst_at { 20 * G } else { 0 };
            u.observe(&snap(
                1000 + s,
                vec![proc(1, Some(0), s * G), proc(2, Some(0), two)],
            ));
            if s == burst_at {
                assert!(used(&u, 2) > used(&u, 1) * 0.99, "the burst is at the top");
            }
        }
        assert!(used(&u, 2) < used(&u, 1) * 0.01, "and has sunk since");
    }

    #[test]
    fn what_was_used_before_the_session_does_not_count() {
        let mut u = Usage::new(0.05);
        // Running since long before, 50 G behind it; and one started ten seconds
        // before the session began.
        let procs = |extra: u64| {
            vec![
                proc(1, Some(0), 50 * G + extra),
                proc(2, Some(990_000), 5 * G + extra),
            ]
        };
        u.observe(&snap(1000, procs(0)));
        assert!(close(used(&u, 1), 0.0) && close(used(&u, 2), 0.0));
        u.observe(&snap(1001, procs(G)));
        assert!(used(&u, 1) > 0.0 && used(&u, 1) < G as f64);
        assert!(close(used(&u, 1), used(&u, 2)));
    }

    #[test]
    fn a_process_started_during_the_session_counts_from_zero() {
        let mut u = Usage::new(0.05);
        u.observe(&snap(1000, vec![proc(1, Some(0), 0)]));
        u.observe(&snap(1010, vec![proc(1, Some(0), 0)]));
        // p2 started at 1010.5 s and had used 3 G when first seen.
        u.observe(&snap(
            1011,
            vec![proc(1, Some(0), 0), proc(2, Some(1_010_500), 3 * G)],
        ));
        let (_, arrived) = fade(0.05, 1.0);
        assert!(close(used(&u, 2), 3.0 * G as f64 * arrived));
        // One with no known start time is taken as already running.
        u.observe(&snap(
            1012,
            vec![
                proc(1, Some(0), 0),
                proc(2, Some(1_010_500), 3 * G),
                proc(3, None, 9 * G),
            ],
        ));
        assert!(close(used(&u, 3), 0.0));
    }

    #[test]
    fn an_exited_process_fades_and_then_goes() {
        let mut u = Usage::new(0.2);
        u.observe(&snap(1000, vec![proc(1, Some(0), 0), proc(2, Some(0), 0)]));
        u.observe(&snap(
            1001,
            vec![proc(1, Some(0), G), proc(2, Some(0), 10 * G)],
        ));
        let at_exit = used(&u, 2);
        // p2 exits; p1 keeps working.
        u.observe(&snap(1002, vec![proc(1, Some(0), 2 * G)]));
        let ghost = u.iter().find(|(k, _)| k.pid == 2).map(|(_, p)| p).unwrap();
        assert!(!ghost.alive);
        assert!(close(ghost.used, at_exit * 0.8));
        assert!(close(u.total(), used(&u, 1) + ghost.used));
        for s in 3..=120u64 {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), s * G)]));
        }
        assert!(u.used(ProcessKey::new(2, 1)).is_none(), "faded away");
        assert!(u.iter().all(|(_, p)| p.alive));
        // A live process stays however little it has used.
        u.observe(&snap(
            1121,
            vec![proc(1, Some(0), 120 * G), proc(4, Some(0), 7)],
        ));
        assert!(u.used(ProcessKey::new(4, 1)).is_some());
    }

    #[test]
    fn the_idle_process_is_not_counted() {
        let mut u = Usage::new(0.05);
        u.observe(&snap(1000, vec![proc(0, Some(0), 0), proc(1, Some(0), 0)]));
        u.observe(&snap(
            1001,
            vec![proc(0, Some(0), 90 * G), proc(1, Some(0), G)],
        ));
        assert!(u.used(ProcessKey::new(0, 1)).is_none());
        assert!(close(u.total(), used(&u, 1)));
        assert_eq!(u.frames().next().unwrap().cycles.len(), 1);
    }

    #[test]
    fn repeated_and_empty_snapshots_change_nothing() {
        let mut u = Usage::new(0.05);
        u.observe(&snap(1000, vec![proc(1, Some(0), 0)]));
        let s = snap(1001, vec![proc(1, Some(0), G)]);
        u.observe(&s);
        let before = used(&u, 1);
        u.observe(&s);
        u.observe(&Snapshot::default());
        assert!(close(used(&u, 1), before));
        assert_eq!(u.frames().count(), 1);
    }

    #[test]
    fn the_history_charts_processes_of_one_name_together() {
        let mut u = Usage::new(0.05);
        let procs = |a: u64, b: u64, c: u64| {
            vec![
                named(1, "chrome.exe", Some(0), a),
                named(2, "chrome.exe", Some(0), b),
                named(3, "idle.exe", Some(0), 0),
                named(4, "code.exe", Some(0), c),
            ]
        };
        u.observe(&snap(1000, procs(0, 0, 0)));
        u.observe(&snap(1001, procs(2 * G, 3 * G, G)));
        u.observe(&snap(1003, procs(2 * G, 4 * G, G)));
        assert_eq!(u.programs(), 3);
        let chrome = u.program("chrome.exe").unwrap();
        assert_eq!(u.program_name(chrome), "chrome.exe");
        assert_eq!(
            u.iter().filter(|(_, p)| p.program == chrome).count(),
            2,
            "both processes"
        );
        let frames: Vec<Frame<'_>> = u.frames().collect();
        assert_eq!(
            frames.len(),
            2,
            "the first snapshot has no interval behind it"
        );
        // Newest first: two seconds in which only one chrome worked.
        assert_eq!((frames[0].end_ms, frames[0].span_ms), (1_003_000, 2000));
        assert_eq!(frames[0].cycles, [(chrome, G as f32)]);
        let of = |f: &Frame<'_>, name: &str| {
            let id = u.program(name).unwrap();
            f.cycles.iter().find(|c| c.0 == id).map(|c| c.1)
        };
        assert_eq!(of(&frames[1], "chrome.exe"), Some(5.0 * G as f32));
        assert_eq!(of(&frames[1], "code.exe"), Some(G as f32));
        assert_eq!(of(&frames[1], "idle.exe"), None, "nothing used, not listed");
    }

    #[test]
    fn chromium_children_are_charted_by_role() {
        let mut u = Usage::new(0.05);
        let child = |pid: u32, cycles: u64, args: &str| {
            let mut p = named(pid, "chrome.exe", Some(0), cycles);
            let s = Arc::make_mut(&mut p.statics);
            s.command_line = Some(format!(
                "chrome.exe {args} --mojo-platform-channel-handle=1"
            ));
            p
        };
        let procs = |c: u64| {
            vec![
                named(1, "chrome.exe", Some(0), c),
                child(2, c, "--type=renderer"),
                child(3, c, "--type=renderer"),
                child(4, c, "--type=gpu-process"),
            ]
        };
        u.observe(&snap(1000, procs(0)));
        u.observe(&snap(1001, procs(G)));
        assert_eq!(u.programs(), 3);
        let renderers = u.program("chrome.exe (Renderer)").unwrap();
        assert_eq!(u.program_name(renderers), "chrome.exe (Renderer)");
        assert_eq!(u.program_of(&procs(0)[2].statics), Some(renderers));
        let frame = u.frames().next().unwrap();
        let of = |g| frame.cycles.iter().find(|c| c.0 == g).map(|c| c.1);
        assert_eq!(of(renderers), Some(2.0 * G as f32));
        assert_eq!(of(u.program("chrome.exe (GPU)").unwrap()), Some(G as f32));
        assert_eq!(of(u.program("chrome.exe").unwrap()), Some(G as f32));
    }

    #[test]
    fn electron_apps_run_from_source_are_charted_apart() {
        let mut u = Usage::new(0.05);
        let app = |pid: u32, project: &str, cycles: u64| {
            let mut p = named(pid, "electron.exe", Some(0), cycles);
            Arc::make_mut(&mut p.statics).image_path = Some(format!(
                r"C:\src\{project}\node_modules\electron\dist\electron.exe"
            ));
            p
        };
        let procs = |c: u64| vec![app(1, "notes", c), app(2, "chat", c)];
        u.observe(&snap(1000, procs(0)));
        u.observe(&snap(1001, procs(G)));
        assert_eq!(u.programs(), 2);
        assert!(u.program("electron.exe (notes)").is_some());
        assert!(u.program("electron.exe (chat)").is_some());
    }

    #[test]
    fn old_history_is_kept_in_ten_second_steps_and_dropped_past_the_reach() {
        let mut u = Usage::new(0.05);
        assert_eq!(u.history_span(), DEFAULT_HISTORY_MS);
        // Three hours at one sample a second, 1 G a second.
        let n = 3 * 3600u64;
        for s in 0..=n {
            u.observe(&snap(100_000 + s, vec![proc(1, Some(0), s * G)]));
        }
        let frames: Vec<Frame<'_>> = u.frames().collect();
        assert!(frames.len() <= RAW_FRAMES + 1 + 362, "{}", frames.len());
        // The raw samples, and perhaps a step that has only its first one yet.
        let raw = frames.iter().filter(|f| f.span_ms == 1000).count();
        assert!((RAW_FRAMES..=RAW_FRAMES + 1).contains(&raw), "{raw}");
        // Newest first, each ending where the one after it began: no gap, no overlap.
        assert_eq!(frames[0].end_ms, i64::try_from(100_000 + n).unwrap() * 1000);
        assert!(frames
            .windows(2)
            .all(|w| w[0].end_ms - w[0].span_ms == w[1].end_ms));
        // A full step holds ten seconds' cycles.
        let step = frames.last().unwrap();
        assert_eq!(step.span_ms, STEP_MIN_MS);
        assert!((f64::from(step.cycles[0].1) / (10.0 * G as f64) - 1.0).abs() < 1e-6);
        // An hour, give or take the step that straddles its edge.
        let covered: i64 = frames.iter().map(|f| f.span_ms).sum();
        assert!((3_600_000..=3_620_000).contains(&covered), "{covered}");
        assert!(u.history_bytes() > 0);
        assert!(u.frame_bytes() >= size_of::<Stored>());
    }

    #[test]
    fn a_longer_reach_takes_wider_steps_and_a_shorter_one_drops_the_old() {
        assert_eq!(step_ms(3_600_000), 10_000);
        assert_eq!(step_ms(2 * 3_600_000), 10_000);
        assert_eq!(step_ms(6 * 3_600_000), 30_000);
        assert_eq!(step_ms(24 * 3_600_000), 120_000);
        assert!(history_bytes(24 * 3_600_000, 100) > history_bytes(3_600_000, 100));
        assert!(history_bytes(24 * 3_600_000, 100) <= (RAW_FRAMES + 721) * 100);

        let mut u = Usage::new(0.05);
        u.set_history_span(24 * 3_600_000);
        // Two hours at one sample a second: nothing is old enough to go, and the
        // steps are two minutes wide.
        let n = 2 * 3600u64;
        for s in 0..=n {
            u.observe(&snap(100_000 + s, vec![proc(1, Some(0), s * G)]));
        }
        let frames: Vec<Frame<'_>> = u.frames().collect();
        let covered: i64 = frames.iter().map(|f| f.span_ms).sum();
        assert_eq!(covered, i64::try_from(n).unwrap() * 1000);
        // The oldest step is the partial one the session began in; the next is full.
        assert_eq!(frames[frames.len() - 2].span_ms, 120_000);

        // Ten minutes: the next snapshot drops everything older, but the step that
        // straddles the edge.
        u.set_history_span(600_000);
        u.observe(&snap(100_000 + n + 1, vec![proc(1, Some(0), (n + 1) * G)]));
        let frames: Vec<Frame<'_>> = u.frames().collect();
        let covered: i64 = frames.iter().map(|f| f.span_ms).sum();
        assert!((600_000..=721_000).contains(&covered), "{covered}");
        assert!(frames
            .windows(2)
            .all(|w| w[0].end_ms - w[0].span_ms == w[1].end_ms));
    }

    #[test]
    fn each_frame_keeps_cpu_time_memory_and_the_machine() {
        use ot_model::cpu::{CoreKind, LogicalCore};
        // Two processes of one program and one of another, on four logical
        // processors with 16 GB, a quarter of it in use.
        let cores: Vec<LogicalCore> = (0..4)
            .map(|index| LogicalCore {
                index,
                physical: index / 2,
                kind: CoreKind::default(),
                usage: Percent(40.0),
                frequency: None,
            })
            .collect();
        let at = |s: u64, cpu_ms: [u64; 3]| {
            let mut procs = Vec::new();
            for (i, (name, ms)) in ["a.exe", "a.exe", "b.exe"]
                .into_iter()
                .zip(cpu_ms)
                .enumerate()
            {
                let mut p = named(i as u32 + 1, name, Some(0), 0);
                p.cpu_time = Duration::from_millis(ms);
                p.private_working_set = Bytes(100 << 20);
                procs.push(p);
            }
            let mut snap = snap(s, procs);
            snap.cpu.cores.clone_from(&cores);
            snap.cpu.total = Percent(40.0);
            snap.memory.total = Bytes(16 << 30);
            snap.memory.available = Bytes(12 << 30);
            snap
        };
        let mut u = Usage::new(0.05);
        u.observe(&at(10, [1000, 2000, 3000]));
        u.observe(&at(12, [1500, 2500, 3100]));
        let (a, b) = (u.program("a.exe").unwrap(), u.program("b.exe").unwrap());
        let frame = u.frames().next().unwrap();
        assert_eq!(frame.span_ms, 2000);
        // CPU time used in the interval, not since the processes started.
        assert_eq!(frame.cpu_ms, [(a, 1000.0), (b, 100.0)]);
        // The private working sets held, times the interval.
        assert_eq!(
            frame.memory,
            [
                (a, 2.0 * 100.0 * 1_048_576.0 * 2000.0),
                (b, 100.0 * 1_048_576.0 * 2000.0)
            ]
        );
        let machine = frame.machine;
        assert!(close(machine.cpu_capacity_ms, 8000.0));
        assert!(close(machine.cpu_busy_ms, 3200.0));
        assert!(close(machine.ram / 2000.0, (16u64 << 30) as f64));
        assert!(close(machine.ram_in_use / 2000.0, (4u64 << 30) as f64));
    }

    #[test]
    fn each_frame_keeps_gpu_use_and_the_hardware_gpus() {
        use ot_model::gpu::{GpuInfo, GpuSample};
        let gpu = |id: u64, software: bool, utilization: f32, dedicated_mb: u64| GpuSample {
            info: Arc::new(GpuInfo {
                id,
                software,
                ..GpuInfo::default()
            }),
            utilization: Percent(utilization),
            engines: Vec::new(),
            dedicated_used: Bytes(dedicated_mb << 20),
            shared_used: Bytes::ZERO,
        };
        // Two programs on two GPUs, one at 60 % and one at 20 %, and the basic
        // render driver, which counts for nothing.
        let at = |s: u64| {
            let mut procs = Vec::new();
            for (i, (name, share, dedicated, shared)) in [
                ("game.exe", 50.0, 900u64, 30u64),
                ("dwm.exe", 25.0, 100, 70),
                ("idle.exe", 0.0, 0, 0),
            ]
            .into_iter()
            .enumerate()
            {
                let mut p = named(i as u32 + 1, name, Some(0), 0);
                p.gpu_share = Percent(share);
                p.gpu_dedicated = Bytes(dedicated << 20);
                p.gpu_shared = Bytes(shared << 20);
                procs.push(p);
            }
            let mut snap = snap(s, procs);
            snap.gpus = vec![
                gpu(1, false, 60.0, 1200),
                gpu(2, false, 20.0, 0),
                gpu(3, true, 90.0, 0),
            ];
            snap
        };
        let mut u = Usage::new(0.05);
        u.observe(&at(10));
        u.observe(&at(12));
        let (game, dwm) = (
            u.program("game.exe").unwrap(),
            u.program("dwm.exe").unwrap(),
        );
        let frame = u.frames().next().unwrap();
        let mb_ms = |mb: f32| mb * 1_048_576.0 * 2000.0;
        assert_eq!(frame.gpu_ms, [(game, 1000.0), (dwm, 500.0)]);
        assert_eq!(
            frame.gpu_dedicated,
            [(game, mb_ms(900.0)), (dwm, mb_ms(100.0))]
        );
        assert_eq!(frame.gpu_shared, [(game, mb_ms(30.0)), (dwm, mb_ms(70.0))]);
        let m = frame.machine;
        // Two hardware GPUs' worth of time, 80 % of one of them busy.
        assert!(close(m.gpu_capacity_ms, 4000.0));
        assert!(close(m.gpu_busy_ms, 1600.0));
        assert!(close(
            m.gpu_dedicated_in_use / 2000.0,
            (1200u64 << 20) as f64
        ));
    }

    #[test]
    fn a_step_sums_its_frames_so_its_averages_are_exact() {
        // One program whose working set grows by a megabyte a second while it uses
        // a tenth of a second of CPU a second, for twenty minutes.
        let mut u = Usage::new(0.05);
        for s in 0..=1200u64 {
            let mut p = proc(1, Some(0), s * G);
            p.cpu_time = Duration::from_millis(100 * s);
            p.private_working_set = Bytes(s << 20);
            let mut snap = snap(100_000 + s, vec![p]);
            snap.memory.total = Bytes(8 << 30);
            u.observe(&snap);
        }
        let step = u
            .frames()
            .filter(|f| f.span_ms == STEP_MIN_MS)
            .last()
            .unwrap();
        let secs = step.span_ms as f64 / 1000.0;
        assert!(close(f64::from(step.cpu_ms[0].1), 100.0 * secs));
        // The average of ten samples a megabyte apart: the middle of the step.
        let end_s = (step.end_ms / 1000 - 100_000) as f64;
        let mean = f64::from(step.memory[0].1) / step.span_ms as f64;
        assert!(
            ((mean / 1_048_576.0) - (end_s - 4.5)).abs() < 1e-3,
            "{mean}"
        );
        assert!(close(
            step.machine.ram / step.span_ms as f64,
            (8u64 << 30) as f64
        ));
    }

    #[test]
    fn merging_keeps_lists_sorted_and_adds_what_they_share() {
        let mut into = vec![(1, 1.0), (4, 4.0), (9, 9.0)];
        let mut scratch = Vec::new();
        merge(
            &mut into,
            &[(0, 0.5), (4, 1.0), (7, 7.0), (10, 1.0)],
            &mut scratch,
        );
        assert_eq!(
            into,
            [(0, 0.5), (1, 1.0), (4, 5.0), (7, 7.0), (9, 9.0), (10, 1.0)]
        );
        merge(&mut into, &[], &mut scratch);
        assert_eq!(into.len(), 6);
    }

    /// Process `pid` named `name`, a child of `parent`.
    fn child(pid: u32, parent: Option<u32>, name: &str) -> ProcessSample {
        let mut p = named(pid, name, Some(0), 0);
        Arc::make_mut(&mut p.statics).parent = parent.map(|pp| ProcessKey::new(pp, 1));
        p
    }

    fn keys<'a>(it: impl Iterator<Item = (ProcessKey, &'a Seen)>) -> Vec<u32> {
        let mut pids: Vec<u32> = it.map(|(k, _)| k.pid).collect();
        pids.sort_unstable();
        pids
    }

    #[test]
    fn lineage_outlives_the_processes_and_says_who_was_in_a_stretch() {
        let mut u = Usage::new(0.05);
        let shell = || child(1, None, "shell.exe");
        let runner = || child(2, Some(1), "runner.exe");
        u.observe(&snap(
            1000,
            vec![shell(), runner(), child(3, Some(2), "tool.exe")],
        ));
        u.observe(&snap(1001, vec![shell(), child(3, Some(2), "tool.exe")]));
        u.observe(&snap(1002, vec![shell(), child(4, Some(1), "tool.exe")]));
        let tool = u.program("tool.exe").unwrap();
        let s = 1000_i64;
        assert_eq!(keys(u.seen_in(tool, 999 * s, 1000 * s)), [3]);
        assert_eq!(
            keys(u.seen_in(tool, 1000 * s, 1001 * s)),
            [3],
            "the stretch ending at 1001"
        );
        assert_eq!(keys(u.seen_in(tool, 1001 * s, 1002 * s)), [4]);
        assert_eq!(keys(u.seen_in(tool, 999 * s, 1002 * s)), [3, 4]);
        // p3's parent has gone, and is still named, and its parent still known.
        let p3 = u.seen(ProcessKey::new(3, 1)).unwrap();
        assert_eq!(p3.parent, Some(ProcessKey::new(2, 1)));
        let p2 = u.seen(ProcessKey::new(2, 1)).unwrap();
        assert_eq!(u.program_name(p2.program), "runner.exe");
        assert_eq!(p2.parent, Some(ProcessKey::new(1, 1)));
        assert_eq!((p2.first_ms, p2.last_ms), (1000 * s, 1000 * s));
        assert!(!u.running(ProcessKey::new(2, 1)));
        assert!(u.running(ProcessKey::new(1, 1)));
        assert_eq!(p2.lost_parent, None, "its parent was found");
    }

    #[test]
    fn a_parent_gone_before_first_sight_is_remembered_by_its_pid() {
        let mut u = Usage::new(0.05);
        let mut orphan = child(7, None, "orphan.exe");
        Arc::make_mut(&mut orphan.statics).parent_pid = Some(4242);
        let mut root = child(4, None, "System");
        Arc::make_mut(&mut root.statics).parent_pid = Some(0);
        u.observe(&snap(1000, vec![orphan, root]));
        let seen = |pid| u.seen(ProcessKey::new(pid, 1)).unwrap().lost_parent;
        assert_eq!(seen(7), Some(4242));
        assert_eq!(seen(4), None, "the idle process is no parent");
    }

    #[test]
    fn lineage_is_kept_as_far_back_as_the_history_and_for_every_ancestor() {
        let mut u = Usage::new(0.05);
        u.set_history_span(60_000);
        let shell = || child(1, None, "shell.exe");
        let worker = || child(3, Some(2), "worker.exe");
        u.observe(&snap(
            1000,
            vec![
                shell(),
                child(2, Some(1), "launcher.exe"),
                worker(),
                child(5, None, "brief.exe"),
            ],
        ));
        for t in 1001..=1200 {
            u.observe(&snap(t, vec![shell(), worker()]));
        }
        assert!(
            u.seen(ProcessKey::new(5, 1)).is_none(),
            "gone past the reach"
        );
        let launcher = u
            .seen(ProcessKey::new(2, 1))
            .expect("an ancestor of a live process");
        assert_eq!(launcher.last_ms, 1_000_000);
        assert!(u.seen(ProcessKey::new(1, 1)).is_some());
        assert!(u.seen(ProcessKey::new(3, 1)).is_some());
    }
}
