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
//! cycles each *program* used, a program being every process of one name
//! (twelve `chrome.exe`, or the two hundred `rustc.exe` of a build, are one line of
//! the chart). An hour is kept, the last ten minutes sample by sample and the rest
//! in ten-second steps, with only the programs that ran in each step, so the cost is
//! set by how many programs are busy, not by how many processes exist.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ot_model::process::ProcessStatic;
use ot_model::{ProcessKey, Tick};

use crate::snapshot::Snapshot;

/// A ghost below this many cycles (a fraction of a millisecond of work) is dropped.
const GHOST_FLOOR: f64 = 1.0e6;
/// So is one below this share of everything in the totals.
const GHOST_SHARE: f64 = 1.0e-4;
/// History kept sample by sample.
const RAW_FRAMES: usize = 600;
/// Width of the steps older history is kept in, and how many of them.
const COARSE_MS: i64 = 10_000;
const COARSE_FRAMES: usize = 360;

/// A program in the usage history: every process of one name. An index into
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

/// One stretch of the usage history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame<'a> {
    /// When the stretch ended, and how long it was, in milliseconds (the end since
    /// the Unix epoch).
    pub end_ms: i64,
    pub span_ms: i64,
    /// Cycles each program used in it. Programs that used none are not listed.
    pub cycles: &'a [(ProgramId, f32)],
}

#[derive(Debug, Clone)]
struct Entry {
    statics: Arc<ProcessStatic>,
    /// Cumulative cycles at the last sample.
    last: u64,
    total: f64,
    alive: bool,
    program: ProgramId,
}

#[derive(Debug, Clone)]
struct Stored {
    end_ms: i64,
    span_ms: i64,
    cycles: Box<[(ProgramId, f32)]>,
}

impl Stored {
    fn frame(&self) -> Frame<'_> {
        Frame {
            end_ms: self.end_ms,
            span_ms: self.span_ms,
            cycles: &self.cycles,
        }
    }
}

/// A coarse step still being filled from the raw frames leaving the raw history.
#[derive(Debug, Clone, Default)]
struct Open {
    /// Which step: `end_ms` of its frames divided by [`COARSE_MS`], rounded down.
    index: i64,
    end_ms: i64,
    span_ms: i64,
    cycles: Vec<(ProgramId, f32)>,
}

/// Fading per-process cycle totals and the per-program history, fed by snapshots.
#[derive(Debug, Clone)]
pub struct Usage {
    /// Share of every total lost each second.
    decay: f64,
    procs: HashMap<ProcessKey, Entry>,
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
    /// Scratch: this interval's cycles per program.
    interval: Vec<(ProgramId, f32)>,
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
            sum: 0.0,
            now_ms: 0,
            first_ms: None,
            last_tick: None,
            programs: Vec::new(),
            program_ids: HashMap::new(),
            raw: VecDeque::new(),
            coarse: VecDeque::new(),
            open: None,
            interval: Vec::new(),
        }
    }

    /// Change how fast the totals fade. What has accumulated stays, and fades at the
    /// new rate from here on.
    pub fn set_decay(&mut self, decay: f64) {
        self.decay = clamp_decay(decay);
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
        self.interval.clear();
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
                    total: 0.0,
                    alive: true,
                    program: program_id(programs, ids, p.name()),
                }
            });
            e.alive = true;
            if !Arc::ptr_eq(&e.statics, &p.statics) {
                e.statics = Arc::clone(&p.statics);
            }
            let used = if seen_before {
                p.cycles.saturating_sub(e.last)
            } else if !first
                && p.statics
                    .started_unix_ms
                    .is_some_and(|s| s >= session_start)
            {
                // Started since the last snapshot: everything it has used is new.
                p.cycles
            } else {
                // Already running when first seen: what it used before then was
                // used before anyone was watching.
                0
            };
            e.last = p.cycles;
            if used > 0 {
                e.total += used as f64 * arrived;
                let program = e.program;
                match self.interval.iter_mut().find(|(g, _)| *g == program) {
                    Some((_, c)) => *c += used as f32,
                    None => self.interval.push((program, used as f32)),
                }
            }
        }

        self.sum = self.procs.values().map(|e| e.total).sum();
        let floor = GHOST_FLOOR.max(self.sum * GHOST_SHARE);
        self.procs.retain(|_, e| e.alive || e.total >= floor);

        if span_ms > 0 {
            self.push_frame(Stored {
                end_ms: now,
                span_ms,
                cycles: self.interval.as_slice().into(),
            });
        }
    }

    /// Append a frame to the history, moving what leaves the raw part into the
    /// coarse steps.
    fn push_frame(&mut self, frame: Stored) {
        self.raw.push_back(frame);
        while self.raw.len() > RAW_FRAMES {
            let Some(old) = self.raw.pop_front() else {
                break;
            };
            let index = old.end_ms.div_euclid(COARSE_MS);
            if self.open.as_ref().is_some_and(|o| o.index != index) {
                self.close_open();
            }
            let open = self.open.get_or_insert_with(|| Open {
                index,
                ..Open::default()
            });
            open.end_ms = old.end_ms;
            open.span_ms += old.span_ms;
            for &(program, cycles) in &*old.cycles {
                match open.cycles.iter_mut().find(|(g, _)| *g == program) {
                    Some((_, c)) => *c += cycles,
                    None => open.cycles.push((program, cycles)),
                }
            }
        }
    }

    fn close_open(&mut self) {
        if let Some(o) = self.open.take() {
            if self.coarse.len() == COARSE_FRAMES {
                self.coarse.pop_front();
            }
            self.coarse.push_back(Stored {
                end_ms: o.end_ms,
                span_ms: o.span_ms,
                cycles: o.cycles.into(),
            });
        }
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

    /// The name every process of `program` runs under.
    #[must_use]
    pub fn program_name(&self, program: ProgramId) -> &str {
        self.programs
            .get(program as usize)
            .map_or("", String::as_str)
    }

    /// The program processes named `name` are charted under, if any has been seen.
    #[must_use]
    pub fn program(&self, name: &str) -> Option<ProgramId> {
        self.program_ids.get(name).copied()
    }

    /// Programs seen this session; ids run from zero to this.
    #[must_use]
    pub fn programs(&self) -> usize {
        self.programs.len()
    }

    /// The history, newest first: every sample of the last ten minutes, then
    /// ten-second steps for the rest of the hour. No stretch is covered twice.
    pub fn frames(&self) -> impl Iterator<Item = Frame<'_>> + '_ {
        let open = self.open.as_ref().map(|o| Frame {
            end_ms: o.end_ms,
            span_ms: o.span_ms,
            cycles: &o.cycles,
        });
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
    name: &str,
) -> ProgramId {
    if let Some(&id) = ids.get(name) {
        return id;
    }
    let id = ProgramId::try_from(programs.len()).unwrap_or(ProgramId::MAX);
    programs.push(name.to_owned());
    ids.insert(name.to_owned(), id);
    id
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
            power: None,
            gpu: None,
            suspended: false,
            services: Vec::new().into(),
            thread_first: 0,
            thread_rows: 0,
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
    fn old_history_is_kept_in_ten_second_steps_and_bounded() {
        let mut u = Usage::new(0.05);
        // Three hours at one sample a second, 1 G a second.
        let n = 3 * 3600u64;
        for s in 0..=n {
            u.observe(&snap(100_000 + s, vec![proc(1, Some(0), s * G)]));
        }
        let frames: Vec<Frame<'_>> = u.frames().collect();
        assert!(frames.len() <= RAW_FRAMES + 1 + COARSE_FRAMES);
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
        assert_eq!(step.span_ms, COARSE_MS);
        assert!((f64::from(step.cycles[0].1) / (10.0 * G as f64) - 1.0).abs() < 1e-6);
        // An hour and ten minutes, give or take the step being filled.
        let covered: i64 = frames.iter().map(|f| f.span_ms).sum();
        assert!((4_190_000..=4_210_000).contains(&covered), "{covered}");
    }
}
