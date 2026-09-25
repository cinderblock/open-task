//! Whole-system time series, fed from snapshots.
//!
//! Every graph is a window over one of these. Each point carries its own timestamp
//! rather than assuming a fixed cadence, because the sampling interval is adjustable
//! at runtime and because the charts are meant to grow a log-scale time axis, which
//! needs real times, not sample indices.
//!
//! Capacity is fixed, so a session that runs for a week has the same memory ceiling
//! as one that runs for a minute. Multi-resolution retention (full rate for the last
//! minutes, decimated for hours) is the planned next step and slots in behind this
//! API without changing callers.

use std::time::{SystemTime, UNIX_EPOCH};

use ot_model::Tick;

use crate::history::Ring;
use crate::snapshot::Snapshot;

/// One point on a series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Wall-clock time of the sample, milliseconds since the Unix epoch.
    pub at_unix_ms: i64,
    pub value: f32,
}

/// A bounded series of timestamped values.
#[derive(Debug, Clone)]
pub struct Series {
    ring: Ring<Sample>,
}

impl Series {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: Ring::new(capacity),
        }
    }

    pub fn push(&mut self, at_unix_ms: i64, value: f32) {
        self.ring.push(Sample { at_unix_ms, value });
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    #[must_use]
    pub fn latest(&self) -> Option<Sample> {
        self.ring.latest().copied()
    }

    /// Oldest to newest.
    #[must_use]
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Sample> + ExactSizeIterator {
        self.ring.iter()
    }

    /// Largest value currently held, or `0.0` when empty. For auto-scaling axes.
    #[must_use]
    pub fn max_value(&self) -> f32 {
        self.ring.iter().map(|s| s.value).fold(0.0, f32::max)
    }
}

/// All system-wide series the UI graphs.
#[derive(Debug, Clone)]
pub struct Timeline {
    /// Aggregate CPU, percent `0..=100`.
    pub cpu_total: Series,
    /// Per logical core, percent `0..=100`, ordered by core index. Empty until the
    /// first snapshot reveals how many cores there are.
    pub cores: Vec<Series>,
    /// Physical memory in use, bytes.
    pub mem_in_use: Series,
    capacity: usize,
    last_tick: Option<Tick>,
}

impl Timeline {
    /// `capacity` points per series. At 1 Hz, 3600 is one hour.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            cpu_total: Series::new(capacity),
            cores: Vec::new(),
            mem_in_use: Series::new(capacity),
            capacity,
            last_tick: None,
        }
    }

    /// Fold a snapshot in. Ignores empty snapshots and repeats of the last tick, so
    /// callers can pass whatever is newest on every wake-up without double-counting.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.is_empty() || self.last_tick == Some(snap.tick) {
            return;
        }
        self.last_tick = Some(snap.tick);

        let at = snap
            .taken_at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);

        self.cpu_total.push(at, snap.cpu.total.get());
        self.mem_in_use.push(at, snap.memory.in_use().get() as f32);

        if self.cores.len() != snap.cpu.cores.len() {
            self.cores = (0..snap.cpu.cores.len())
                .map(|_| Series::new(self.capacity))
                .collect();
        }
        for (series, core) in self.cores.iter_mut().zip(&snap.cpu.cores) {
            series.push(at, core.usage.get());
        }
    }

    /// The tick most recently folded in, if any.
    #[must_use]
    pub fn last_tick(&self) -> Option<Tick> {
        self.last_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
    use ot_model::memory::MemorySample;
    use ot_model::{Bytes, Percent};
    use std::time::Duration;

    fn snap(tick: u64, cpu: f32, cores: usize) -> Snapshot {
        Snapshot {
            tick: Tick(tick),
            taken_at: Some(UNIX_EPOCH + Duration::from_secs(tick)),
            interval: Duration::from_secs(1),
            probe_cost: Duration::ZERO,
            cpu: CpuSample {
                total: Percent(cpu),
                cores: (0..cores)
                    .map(|i| LogicalCore {
                        index: i as u32,
                        physical: i as u32,
                        kind: CoreKind::Unknown,
                        usage: Percent(cpu),
                        frequency: None,
                    })
                    .collect(),
                package_power: None,
                hotspot_celsius: None,
            },
            memory: MemorySample {
                total: Bytes(100),
                available: Bytes(40),
                ..Default::default()
            },
            processes: Vec::new(),
        }
    }

    #[test]
    fn same_tick_is_not_double_counted() {
        let mut t = Timeline::new(10);
        let s = snap(1, 50.0, 2);
        t.observe(&s);
        t.observe(&s);
        assert_eq!(t.cpu_total.len(), 1);
        assert_eq!(t.cores.len(), 2);
        assert_eq!(t.cores[0].len(), 1);
        assert_eq!(t.mem_in_use.latest().map(|s| s.value), Some(60.0));
    }

    #[test]
    fn empty_snapshot_is_ignored() {
        let mut t = Timeline::new(10);
        t.observe(&Snapshot::default());
        assert!(t.cpu_total.is_empty());
        assert!(t.last_tick().is_none());
    }

    #[test]
    fn timestamps_come_from_the_snapshot() {
        let mut t = Timeline::new(10);
        t.observe(&snap(7, 1.0, 1));
        assert_eq!(t.cpu_total.latest().map(|s| s.at_unix_ms), Some(7000));
    }
}
