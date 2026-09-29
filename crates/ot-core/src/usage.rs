//! Who has been using the CPU: each process's CPU time over a recent window.
//!
//! The table's CPU column is the last interval, which says who is busy this instant.
//! The usage views ask a different question, who has been busy, so they size each
//! process by the CPU time it used over the last minute. A process's cumulative CPU
//! time only grows, so its use over a window is the difference between now and the
//! window's start. [`Usage`] keeps, per process, a short run of (time, cumulative)
//! samples, enough to find the value at the start of the window.
//!
//! Three edges are handled so the numbers stay honest:
//! - a process that started inside the window counts from zero, not from the first
//!   time it was seen;
//! - while the session is younger than the window, the window is only as long as
//!   the session ([`Usage::span`]), and says so;
//! - a process that exits stays, as a ghost with its last numbers, until its last
//!   sample falls out of the window, so its share does not vanish the moment it ends.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ot_model::process::ProcessStatic;
use ot_model::{ProcessKey, Tick};

use crate::snapshot::Snapshot;

/// One process's use of the CPU over the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessUsage<'a> {
    pub statics: &'a Arc<ProcessStatic>,
    /// CPU time used inside the window.
    pub used: Duration,
    /// False once the process has exited; it stays until the window passes it.
    pub alive: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    statics: Arc<ProcessStatic>,
    /// Oldest first: (milliseconds since the Unix epoch, cumulative CPU time).
    samples: VecDeque<(i64, Duration)>,
    alive: bool,
}

impl Entry {
    fn latest(&self) -> Duration {
        self.samples.back().map_or(Duration::ZERO, |s| s.1)
    }

    /// Cumulative CPU time at `t`, the start of the window: interpolated between
    /// the samples around it; zero if the process started after it; the oldest
    /// sample if the process was already running when it was first seen.
    fn at(&self, t: i64) -> Duration {
        if self.statics.started_unix_ms.is_some_and(|s| s >= t) {
            return Duration::ZERO;
        }
        let Some(&(first_t, first)) = self.samples.front() else {
            return Duration::ZERO;
        };
        if t <= first_t {
            return first;
        }
        let later = self.samples.partition_point(|s| s.0 <= t);
        let (t0, c0) = self.samples[later - 1];
        let Some(&(t1, c1)) = self.samples.get(later) else {
            return c0;
        };
        let f = (t - t0) as f64 / (t1 - t0).max(1) as f64;
        c0 + (c1.saturating_sub(c0)).mul_f64(f)
    }
}

/// CPU time used per process over a sliding window, fed by snapshots.
#[derive(Debug, Clone)]
pub struct Usage {
    window_ms: i64,
    procs: HashMap<ProcessKey, Entry>,
    /// Time of the newest snapshot and of the first, in ms since the epoch.
    now_ms: i64,
    first_ms: Option<i64>,
    logical_processors: usize,
    last_tick: Option<Tick>,
}

impl Usage {
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            window_ms: i64::try_from(window.as_millis()).unwrap_or(i64::MAX),
            procs: HashMap::new(),
            now_ms: 0,
            first_ms: None,
            logical_processors: 0,
            last_tick: None,
        }
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
        self.now_ms = now;
        self.first_ms.get_or_insert(now);
        if !snap.cpu.cores.is_empty() {
            self.logical_processors = snap.cpu.cores.len();
        }

        for e in self.procs.values_mut() {
            e.alive = false;
        }
        for p in &snap.processes {
            let e = self.procs.entry(p.key()).or_insert_with(|| Entry {
                statics: Arc::clone(&p.statics),
                samples: VecDeque::new(),
                alive: true,
            });
            e.alive = true;
            if !Arc::ptr_eq(&e.statics, &p.statics) {
                e.statics = Arc::clone(&p.statics);
            }
            e.samples.push_back((now, p.cpu_time));
        }

        // Keep one sample at or before the window's start, to interpolate from;
        // drop ghosts whose last sample has left the window.
        let start = now - self.window_ms;
        self.procs.retain(|_, e| {
            while e.samples.len() > 1 && e.samples[1].0 <= start {
                e.samples.pop_front();
            }
            e.alive || e.samples.back().is_some_and(|s| s.0 > start)
        });
    }

    /// Where the window begins: a window's length before the newest snapshot, or
    /// the first snapshot while the session is younger than the window. Clamping to
    /// the first snapshot keeps the numbers and [`Usage::span`] agreeing: a process
    /// that started shortly before the session must not count CPU it used before
    /// anyone was watching, which the span does not cover.
    fn start(&self) -> i64 {
        (self.now_ms - self.window_ms).max(self.first_ms.unwrap_or(i64::MIN))
    }

    /// The window actually covered: the configured one, or less while the session
    /// is younger than that.
    #[must_use]
    pub fn span(&self) -> Duration {
        let observed = self.first_ms.map_or(0, |f| self.now_ms - f);
        Duration::from_millis(observed.clamp(0, self.window_ms).cast_unsigned())
    }

    /// The configured window.
    #[must_use]
    pub fn window(&self) -> Duration {
        Duration::from_millis(self.window_ms.cast_unsigned())
    }

    /// Logical processors on the machine, per the latest snapshot that said.
    #[must_use]
    pub fn logical_processors(&self) -> usize {
        self.logical_processors
    }

    /// Every process with its use over the window, the exited ones included, in no
    /// particular order.
    pub fn iter(&self) -> impl Iterator<Item = (ProcessKey, ProcessUsage<'_>)> + '_ {
        let start = self.start();
        self.procs.iter().map(move |(&k, e)| {
            (
                k,
                ProcessUsage {
                    statics: &e.statics,
                    used: e.latest().saturating_sub(e.at(start)),
                    alive: e.alive,
                },
            )
        })
    }

    /// One process's use over the window.
    #[must_use]
    pub fn used(&self, key: ProcessKey) -> Option<Duration> {
        let start = self.start();
        self.procs
            .get(&key)
            .map(|e| e.latest().saturating_sub(e.at(start)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
    use ot_model::process::{Integrity, ProcessSample};
    use ot_model::{Bytes, Percent};

    fn proc(pid: u32, started_ms: Option<i64>, cpu_ms: u64) -> ProcessSample {
        ProcessSample {
            statics: Arc::new(ProcessStatic {
                key: ProcessKey::new(pid, 1),
                parent: None,
                name: format!("p{pid}.exe"),
                image_path: None,
                command_line: None,
                user: None,
                integrity: Integrity::Unknown,
                started_unix_ms: started_ms,
            }),
            cpu: Percent(0.0),
            cpu_time: Duration::from_millis(cpu_ms),
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

    /// A snapshot at `secs` seconds after the epoch.
    fn snap(secs: u64, procs: Vec<ProcessSample>) -> Snapshot {
        Snapshot {
            tick: Tick(secs),
            taken_at: Some(UNIX_EPOCH + Duration::from_secs(secs)),
            interval: Duration::from_secs(1),
            cpu: CpuSample {
                cores: (0..4)
                    .map(|i| LogicalCore {
                        index: i,
                        physical: i,
                        kind: CoreKind::Unknown,
                        usage: Percent(0.0),
                        frequency: None,
                    })
                    .collect(),
                ..Default::default()
            },
            processes: procs,
            ..Default::default()
        }
    }

    fn used(u: &Usage, pid: u32) -> u128 {
        u.used(ProcessKey::new(pid, 1)).unwrap().as_millis()
    }

    #[test]
    fn use_over_the_window_is_the_difference_of_cumulative_times() {
        let mut u = Usage::new(Duration::from_secs(60));
        // Process 1 was running long before; it uses 100 ms of CPU a second.
        for s in 0..=120u64 {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), 50_000 + s * 100)]));
        }
        assert_eq!(used(&u, 1), 6000, "60 s at 100 ms/s");
        assert_eq!(u.span(), Duration::from_secs(60));
        assert_eq!(u.logical_processors(), 4);
    }

    #[test]
    fn a_young_session_covers_only_what_it_has_seen() {
        let mut u = Usage::new(Duration::from_secs(60));
        for s in 0..=20u64 {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), 50_000 + s * 100)]));
        }
        assert_eq!(u.span(), Duration::from_secs(20));
        assert_eq!(used(&u, 1), 2000, "not the 50 s it used before we looked");
    }

    #[test]
    fn a_process_started_just_before_the_session_counts_from_when_it_was_seen() {
        let mut u = Usage::new(Duration::from_secs(60));
        // Started at 990 s, ten seconds before the session began, having used 5 s
        // by then; inside a minute of now, but not inside what the session saw.
        for s in 0..=20u64 {
            u.observe(&snap(
                1000 + s,
                vec![proc(1, Some(990_000), 5000 + s * 100)],
            ));
        }
        assert_eq!(used(&u, 1), 2000, "the 20 s seen, not the 5 s before");
    }

    #[test]
    fn a_process_started_inside_the_window_counts_from_zero() {
        let mut u = Usage::new(Duration::from_secs(60));
        u.observe(&snap(1000, vec![proc(1, Some(0), 0)]));
        // Process 2 started at 1010 s and had used 700 ms by the time we saw it.
        u.observe(&snap(
            1011,
            vec![proc(1, Some(0), 0), proc(2, Some(1_010_000), 700)],
        ));
        u.observe(&snap(
            1012,
            vec![proc(1, Some(0), 0), proc(2, Some(1_010_000), 900)],
        ));
        assert_eq!(used(&u, 2), 900);
    }

    #[test]
    fn the_window_start_is_interpolated_between_samples() {
        let mut u = Usage::new(Duration::from_secs(10));
        // Samples every 4 s: at 0, 4, 8, 12, 16 s, 1 s of CPU per 4 s.
        for (i, s) in [0u64, 4, 8, 12, 16].into_iter().enumerate() {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), i as u64 * 1000)]));
        }
        // Window [6 s, 16 s]: cumulative at 6 s is halfway between 1 s and 2 s.
        assert_eq!(used(&u, 1), 4000 - 1500);
    }

    #[test]
    fn an_exited_process_stays_until_the_window_passes_it() {
        let mut u = Usage::new(Duration::from_secs(10));
        for s in 0..=5u64 {
            u.observe(&snap(
                1000 + s,
                vec![proc(1, Some(0), 0), proc(2, Some(0), s * 200)],
            ));
        }
        // Process 2 exits after 5 s.
        for s in 6..=14u64 {
            u.observe(&snap(1000 + s, vec![proc(1, Some(0), 0)]));
        }
        let ghost = u
            .iter()
            .find(|(k, _)| k.pid == 2)
            .map(|(_, p)| (p.alive, p.used.as_millis()));
        // Window [4 s, 14 s]: it used 200 ms between 4 s and 5 s.
        assert_eq!(ghost, Some((false, 200)));
        u.observe(&snap(1016, vec![proc(1, Some(0), 0)]));
        assert!(
            u.used(ProcessKey::new(2, 1)).is_none(),
            "gone once out of the window"
        );
        assert!(u.iter().all(|(_, p)| p.alive));
    }

    #[test]
    fn repeated_and_empty_snapshots_change_nothing() {
        let mut u = Usage::new(Duration::from_secs(60));
        let s = snap(1000, vec![proc(1, Some(0), 100)]);
        u.observe(&s);
        u.observe(&s);
        u.observe(&Snapshot::default());
        assert_eq!(u.procs[&ProcessKey::new(1, 1)].samples.len(), 1);
    }
}
