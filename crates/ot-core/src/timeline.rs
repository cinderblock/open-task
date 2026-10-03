//! Whole-system time series, fed from snapshots.
//!
//! Every graph is a window over one of these. Each point carries its own timestamp
//! rather than assuming a fixed cadence, because the sampling interval is adjustable
//! at runtime and because the charts draw on a log-scale time axis, which needs real
//! times, not sample indices.
//!
//! History is kept at several resolutions, the way the charts read it: every raw
//! sample for the last few minutes, then fixed-width buckets (min, max, mean) for
//! the hours behind that. A log axis spends most of its width on the recent past and
//! compresses the rest, so the old end never needs raw samples, only enough
//! resolution for the pixels it gets. Every tier has a fixed capacity, so a session
//! that runs for a week has the same memory ceiling as one that runs for a minute.

use std::collections::vec_deque;
use std::iter::Rev;
use std::slice;
use std::time::{SystemTime, UNIX_EPOCH};

use ot_model::Tick;

use crate::history::Ring;
use crate::snapshot::Snapshot;

/// One raw sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Wall-clock time of the sample, milliseconds since the Unix epoch.
    pub at_unix_ms: i64,
    pub value: f32,
}

/// A summary of one or more consecutive samples. A raw sample reads as a bucket of
/// one, so readers handle every resolution the same way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    /// Time of the first and last sample in the bucket, milliseconds since the Unix
    /// epoch. Equal for a single sample.
    pub first_ms: i64,
    pub last_ms: i64,
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    /// Samples summarized.
    pub count: u32,
}

impl Bucket {
    fn of(s: Sample) -> Self {
        Self {
            first_ms: s.at_unix_ms,
            last_ms: s.at_unix_ms,
            min: s.value,
            max: s.value,
            mean: s.value,
            count: 1,
        }
    }

    /// Midpoint in time, where a chart places the bucket.
    #[must_use]
    pub fn mid_ms(&self) -> i64 {
        self.first_ms + (self.last_ms - self.first_ms) / 2
    }
}

/// One coarse tier of a series' history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// Bucket width in milliseconds. Buckets are aligned to multiples of this since
    /// the Unix epoch, so two series (or two sessions) bucket the same instants alike.
    pub bucket_ms: i64,
    /// Buckets kept.
    pub capacity: usize,
}

/// How much history a series keeps, at which resolutions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    /// Raw samples kept, each at its own timestamp.
    pub raw: usize,
    /// Coarser tiers, finest first. Each should cover more time than the one before
    /// it, or it adds nothing.
    pub tiers: Vec<Resolution>,
}

/// Raw samples [`Retention::covering`] keeps: ten minutes at one a second.
pub const RAW_SAMPLES: usize = 600;
/// The finer coarse tier's bucket width and how much of the span it covers.
const FINE_MS: i64 = 10_000;
const FINE_SPAN_MS: i64 = 2 * 3_600_000;
/// The coarser tier's bucket width, for spans past the finer tier's reach.
const COARSE_MS: i64 = 60_000;

impl Retention {
    /// Raw samples only.
    #[must_use]
    pub const fn raw(capacity: usize) -> Self {
        Self {
            raw: capacity,
            tiers: Vec::new(),
        }
    }

    /// Enough history to chart the last `span_ms`: [`RAW_SAMPLES`] raw samples,
    /// then 10 s buckets for up to two hours, then 60 s buckets for the rest. The
    /// charts' log time axis gives the old end a few pixels an hour, so the coarse
    /// tiers lose nothing a chart could show.
    #[must_use]
    pub fn covering(span_ms: i64) -> Self {
        let span_ms = span_ms.max(1);
        // Buckets are aligned to the clock, so covering a span takes one more than
        // its width divides into, plus the one being filled.
        let buckets = |span: i64, width: i64| (span / width) as usize + 2;
        let mut tiers = vec![Resolution {
            bucket_ms: FINE_MS,
            capacity: buckets(span_ms.min(FINE_SPAN_MS), FINE_MS),
        }];
        if span_ms > FINE_SPAN_MS {
            tiers.push(Resolution {
                bucket_ms: COARSE_MS,
                capacity: buckets(span_ms, COARSE_MS),
            });
        }
        Self {
            raw: RAW_SAMPLES,
            tiers,
        }
    }

    /// Points a series holds when full: raw samples and buckets together.
    #[must_use]
    pub fn points(&self) -> usize {
        self.raw + self.tiers.iter().map(|t| t.capacity + 1).sum::<usize>()
    }

    /// Memory one series commits to at this retention. The rings allocate their
    /// capacity up front, so this is what a series costs from the start, not what
    /// it grows to.
    #[must_use]
    pub fn bytes_per_series(&self) -> usize {
        let buckets: usize = self.tiers.iter().map(|t| t.capacity + 1).sum();
        self.raw * size_of::<Sample>() + buckets * size_of::<Bucket>()
    }
}

/// A bucket being filled: the sum is kept wide so a long bucket's mean does not
/// drift.
#[derive(Debug, Clone, Copy)]
struct Open {
    index: i64,
    bucket: Bucket,
    sum: f64,
}

#[derive(Debug, Clone)]
struct Tier {
    bucket_ms: i64,
    closed: Ring<Bucket>,
    open: Option<Open>,
}

impl Tier {
    fn push(&mut self, s: Sample) {
        let index = s.at_unix_ms.div_euclid(self.bucket_ms);
        match &mut self.open {
            Some(o) if o.index == index => {
                let b = &mut o.bucket;
                b.last_ms = b.last_ms.max(s.at_unix_ms);
                b.min = b.min.min(s.value);
                b.max = b.max.max(s.value);
                b.count += 1;
                o.sum += f64::from(s.value);
                b.mean = (o.sum / f64::from(b.count)) as f32;
            }
            open => {
                if let Some(done) = open.take() {
                    self.closed.push(done.bucket);
                }
                *open = Some(Open {
                    index,
                    bucket: Bucket::of(s),
                    sum: f64::from(s.value),
                });
            }
        }
    }
}

/// A bounded, multi-resolution series of timestamped values.
#[derive(Debug, Clone)]
pub struct Series {
    raw: Ring<Sample>,
    tiers: Vec<Tier>,
    retention: Retention,
}

impl Series {
    #[must_use]
    pub fn new(retention: Retention) -> Self {
        Self {
            raw: Ring::new(retention.raw),
            tiers: retention
                .tiers
                .iter()
                .map(|r| Tier {
                    bucket_ms: r.bucket_ms.max(1),
                    closed: Ring::new(r.capacity),
                    open: None,
                })
                .collect(),
            retention,
        }
    }

    /// Keep history to a new retention. Rings resize in place, so nothing already
    /// held is lost but what no longer fits; a tier whose bucket width is kept
    /// keeps its buckets, a new one starts empty and fills from here on.
    pub fn set_retention(&mut self, retention: Retention) {
        if self.retention == retention {
            return;
        }
        self.raw.set_capacity(retention.raw);
        let mut old: Vec<Option<Tier>> = std::mem::take(&mut self.tiers)
            .into_iter()
            .map(Some)
            .collect();
        self.tiers = retention
            .tiers
            .iter()
            .map(|r| {
                let bucket_ms = r.bucket_ms.max(1);
                let kept = old
                    .iter_mut()
                    .find(|t| t.as_ref().is_some_and(|t| t.bucket_ms == bucket_ms))
                    .and_then(Option::take);
                match kept {
                    Some(mut t) => {
                        t.closed.set_capacity(r.capacity);
                        t
                    }
                    None => Tier {
                        bucket_ms,
                        closed: Ring::new(r.capacity),
                        open: None,
                    },
                }
            })
            .collect();
        self.retention = retention;
    }

    /// Time of the oldest sample still held, raw or in a bucket.
    #[must_use]
    pub fn oldest_ms(&self) -> Option<i64> {
        let raw = self.raw.oldest().map(|s| s.at_unix_ms);
        let tiers = self.tiers.iter().flat_map(|t| {
            t.closed
                .oldest()
                .map(|b| b.first_ms)
                .into_iter()
                .chain(t.open.map(|o| o.bucket.first_ms))
        });
        raw.into_iter().chain(tiers).min()
    }

    /// Append a sample. Samples are expected in time order; one that is older than
    /// the newest is still kept raw, but a coarse tier folds it into whichever
    /// bucket is open.
    pub fn push(&mut self, at_unix_ms: i64, value: f32) {
        let s = Sample { at_unix_ms, value };
        self.raw.push(s);
        for t in &mut self.tiers {
            t.push(s);
        }
    }

    #[must_use]
    pub fn retention(&self) -> &Retention {
        &self.retention
    }

    /// Raw samples held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    #[must_use]
    pub fn latest(&self) -> Option<Sample> {
        self.raw.latest().copied()
    }

    /// Raw samples, oldest to newest.
    pub fn raw(&self) -> impl DoubleEndedIterator<Item = &Sample> + ExactSizeIterator {
        self.raw.iter()
    }

    /// The whole history, newest to oldest, each stretch of time at the finest
    /// resolution that still covers it: raw samples first, then each coarser tier's
    /// buckets from where the finer one runs out. A bucket is only used once it lies
    /// wholly before everything already yielded, so no instant is counted twice; at
    /// each seam that leaves a gap of at most one bucket, which a line simply spans.
    /// Does not allocate.
    #[must_use]
    pub fn history(&self) -> History<'_> {
        History {
            raw: self.raw.into_iter().rev(),
            tiers: self.tiers.iter(),
            open: None,
            closed: None,
            cut: i64::MAX,
        }
    }
}

/// Iterator returned by [`Series::history`].
#[derive(Debug)]
pub struct History<'a> {
    raw: Rev<vec_deque::Iter<'a, Sample>>,
    tiers: slice::Iter<'a, Tier>,
    /// The current tier's bucket still filling, then its closed ones.
    open: Option<Bucket>,
    closed: Option<Rev<vec_deque::Iter<'a, Bucket>>>,
    /// Start of the oldest thing yielded so far; anything yielded next must end
    /// before it.
    cut: i64,
}

impl Iterator for History<'_> {
    type Item = Bucket;

    fn next(&mut self) -> Option<Bucket> {
        if let Some(s) = self.raw.next() {
            self.cut = s.at_unix_ms;
            return Some(Bucket::of(*s));
        }
        loop {
            let next = match self.open.take() {
                Some(b) => Some(b),
                None => self.closed.as_mut().and_then(Iterator::next).copied(),
            };
            match next {
                Some(b) if b.last_ms < self.cut => {
                    self.cut = b.first_ms;
                    return Some(b);
                }
                // Overlaps something finer that was already yielded.
                Some(_) => {}
                None => {
                    let t = self.tiers.next()?;
                    self.open = t.open.map(|o| o.bucket);
                    self.closed = Some(t.closed.into_iter().rev());
                }
            }
        }
    }
}

/// History for one disk.
#[derive(Debug, Clone)]
pub struct DiskSeries {
    /// The disk's number, as in [`ot_model::device::DiskInfo::number`].
    pub number: u32,
    /// Active time, percent `0..=100`.
    pub active: Series,
    /// Bytes read per second.
    pub read: Series,
    /// Bytes written per second.
    pub write: Series,
}

/// History for one network adapter.
#[derive(Debug, Clone)]
pub struct AdapterSeries {
    /// As in [`ot_model::device::AdapterInfo::id`].
    pub id: u64,
    /// Bytes received per second.
    pub rx: Series,
    /// Bytes sent per second.
    pub tx: Series,
}

/// History for one graphics adapter.
#[derive(Debug, Clone)]
pub struct GpuSeries {
    /// As in [`ot_model::gpu::GpuInfo::id`].
    pub id: u64,
    /// The busiest engine's share of the interval, percent `0..=100`.
    pub utilization: Series,
    /// Dedicated memory in use, bytes.
    pub dedicated: Series,
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
    /// One entry per disk the latest snapshot listed, in its order.
    pub disks: Vec<DiskSeries>,
    /// One entry per adapter the latest snapshot listed, in its order.
    pub adapters: Vec<AdapterSeries>,
    /// One entry per graphics adapter the latest snapshot listed, in its order.
    pub gpus: Vec<GpuSeries>,
    /// Battery charge, percent `0..=100`; empty on a machine without a battery.
    pub battery_charge: Series,
    /// Power into or out of the battery, watts.
    pub battery_rate: Series,
    retention: Retention,
    last_tick: Option<Tick>,
}

impl Timeline {
    #[must_use]
    pub fn new(retention: Retention) -> Self {
        Self {
            cpu_total: Series::new(retention.clone()),
            cores: Vec::new(),
            mem_in_use: Series::new(retention.clone()),
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            battery_charge: Series::new(retention.clone()),
            battery_rate: Series::new(retention.clone()),
            retention,
            last_tick: None,
        }
    }

    #[must_use]
    pub fn retention(&self) -> &Retention {
        &self.retention
    }

    /// Keep every series to a new retention, present and future ones alike. See
    /// [`Series::set_retention`] for what is kept.
    pub fn set_retention(&mut self, retention: Retention) {
        if self.retention == retention {
            return;
        }
        for s in self.series_mut() {
            s.set_retention(retention.clone());
        }
        self.retention = retention;
    }

    /// Every series, for the ones that apply to all alike.
    fn series_mut(&mut self) -> impl Iterator<Item = &mut Series> {
        let disks = self
            .disks
            .iter_mut()
            .flat_map(|d| [&mut d.active, &mut d.read, &mut d.write]);
        let adapters = self
            .adapters
            .iter_mut()
            .flat_map(|a| [&mut a.rx, &mut a.tx]);
        let gpus = self
            .gpus
            .iter_mut()
            .flat_map(|g| [&mut g.utilization, &mut g.dedicated]);
        [&mut self.cpu_total, &mut self.mem_in_use]
            .into_iter()
            .chain(self.cores.iter_mut())
            .chain(disks)
            .chain(adapters)
            .chain(gpus)
            .chain([&mut self.battery_charge, &mut self.battery_rate])
    }

    /// Series held, each committed to [`Retention::bytes_per_series`].
    #[must_use]
    pub fn series_count(&self) -> usize {
        4 + self.cores.len() + 3 * self.disks.len() + 2 * self.adapters.len() + 2 * self.gpus.len()
    }

    /// How far back the history reaches: the age of the oldest sample still held,
    /// in milliseconds before the newest. `None` before the first sample. The charts
    /// size their time axis to this, so a session fills its width from the start.
    #[must_use]
    pub fn span_ms(&self) -> Option<i64> {
        let newest = self.cpu_total.latest()?.at_unix_ms;
        let oldest = self.cpu_total.oldest_ms()?;
        Some((newest - oldest).max(0))
    }

    /// Fold a snapshot in. Ignores empty snapshots and repeats of the last tick, so
    /// callers can pass whatever is newest on every wake-up without double-counting.
    pub fn observe(&mut self, snap: &Snapshot) {
        if snap.is_empty() || self.last_tick == Some(snap.tick) {
            return;
        }
        self.last_tick = Some(snap.tick);
        // The first pass of a session has no previous pass to measure against, so
        // its rates (CPU above all) read as zero. Charting that draws a cliff from
        // 0% into the first real sample; leave it out.
        if snap.interval.is_zero() {
            return;
        }

        let at = snap
            .taken_at
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);

        self.cpu_total.push(at, snap.cpu.total.get());
        self.mem_in_use.push(at, snap.memory.in_use().get() as f32);

        if self.cores.len() != snap.cpu.cores.len() {
            self.cores = (0..snap.cpu.cores.len())
                .map(|_| Series::new(self.retention.clone()))
                .collect();
        }
        for (series, core) in self.cores.iter_mut().zip(&snap.cpu.cores) {
            series.push(at, core.usage.get());
        }

        let retention = self.retention.clone();
        for d in &snap.disks {
            let n = d.info.number;
            let i = self
                .disks
                .iter()
                .position(|s| s.number == n)
                .unwrap_or_else(|| {
                    self.disks.push(DiskSeries {
                        number: n,
                        active: Series::new(retention.clone()),
                        read: Series::new(retention.clone()),
                        write: Series::new(retention.clone()),
                    });
                    self.disks.len() - 1
                });
            let s = &mut self.disks[i];
            s.active.push(at, d.active.get());
            s.read.push(at, d.read_per_sec.get() as f32);
            s.write.push(at, d.write_per_sec.get() as f32);
        }
        for a in &snap.adapters {
            let id = a.info.id;
            let i = self
                .adapters
                .iter()
                .position(|s| s.id == id)
                .unwrap_or_else(|| {
                    self.adapters.push(AdapterSeries {
                        id,
                        rx: Series::new(retention.clone()),
                        tx: Series::new(retention.clone()),
                    });
                    self.adapters.len() - 1
                });
            let s = &mut self.adapters[i];
            s.rx.push(at, a.rx_per_sec.get() as f32);
            s.tx.push(at, a.tx_per_sec.get() as f32);
        }
        for g in &snap.gpus {
            let id = g.info.id;
            let i = self
                .gpus
                .iter()
                .position(|s| s.id == id)
                .unwrap_or_else(|| {
                    self.gpus.push(GpuSeries {
                        id,
                        utilization: Series::new(retention.clone()),
                        dedicated: Series::new(retention.clone()),
                    });
                    self.gpus.len() - 1
                });
            let s = &mut self.gpus[i];
            s.utilization.push(at, g.utilization.get());
            s.dedicated.push(at, g.dedicated_used.get() as f32);
        }
        if let Some(b) = &snap.battery {
            if let Some(charge) = b.charge {
                self.battery_charge.push(at, charge);
            }
            if let Some(rate) = b.rate {
                self.battery_rate.push(at, rate.0);
            }
        }
        // A device that is gone takes its history with it. An empty list is more
        // likely a failed read than every disk (or every adapter) vanishing at once,
        // so it prunes nothing.
        if !snap.disks.is_empty() {
            self.disks
                .retain(|s| snap.disks.iter().any(|d| d.info.number == s.number));
        }
        if !snap.adapters.is_empty() {
            self.adapters
                .retain(|s| snap.adapters.iter().any(|a| a.info.id == s.id));
        }
        if !snap.gpus.is_empty() {
            self.gpus
                .retain(|s| snap.gpus.iter().any(|g| g.info.id == s.id));
        }
    }

    #[must_use]
    pub fn disk(&self, number: u32) -> Option<&DiskSeries> {
        self.disks.iter().find(|s| s.number == number)
    }

    #[must_use]
    pub fn adapter(&self, id: u64) -> Option<&AdapterSeries> {
        self.adapters.iter().find(|s| s.id == id)
    }

    #[must_use]
    pub fn gpu(&self, id: u64) -> Option<&GpuSeries> {
        self.gpus.iter().find(|s| s.id == id)
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
            threads: Vec::new(),
            disks: Vec::new(),
            adapters: Vec::new(),
            capabilities: ot_model::Capabilities::default(),
            hardware: std::sync::Arc::default(),
            ..Snapshot::default()
        }
    }

    #[test]
    fn same_tick_is_not_double_counted() {
        let mut t = Timeline::new(Retention::raw(10));
        let s = snap(1, 50.0, 2);
        t.observe(&s);
        t.observe(&s);
        assert_eq!(t.cpu_total.len(), 1);
        assert_eq!(t.cores.len(), 2);
        assert_eq!(t.cores[0].len(), 1);
        assert_eq!(t.mem_in_use.latest().map(|s| s.value), Some(60.0));
    }

    #[test]
    fn the_first_pass_has_no_rates_and_is_not_charted() {
        let mut t = Timeline::new(Retention::raw(10));
        let mut first = snap(1, 0.0, 2);
        first.interval = Duration::ZERO;
        t.observe(&first);
        assert!(t.cpu_total.is_empty());
        assert_eq!(t.last_tick(), Some(Tick(1)));
        t.observe(&snap(2, 30.0, 2));
        assert_eq!(t.cpu_total.len(), 1);
    }

    #[test]
    fn disks_and_adapters_get_series_that_follow_the_devices() {
        use ot_model::device::{AdapterInfo, AdapterSample, DiskInfo, DiskSample};
        use std::sync::Arc;
        let disk = |n: u32, active: f32| DiskSample {
            info: Arc::new(DiskInfo {
                number: n,
                ..DiskInfo::default()
            }),
            active: Percent(active),
            read_per_sec: Bytes(100),
            write_per_sec: Bytes(200),
            response_ms: None,
        };
        let nic = |id: u64| AdapterSample {
            info: Arc::new(AdapterInfo {
                id,
                ..AdapterInfo::default()
            }),
            rx_per_sec: Bytes(5),
            tx_per_sec: Bytes(6),
            link_bps: None,
        };
        let mut t = Timeline::new(Retention::raw(10));
        let mut s = snap(1, 1.0, 1);
        s.disks = vec![disk(0, 10.0), disk(1, 20.0)];
        s.adapters = vec![nic(7)];
        t.observe(&s);
        assert_eq!(
            t.disk(1).map(|d| d.active.latest().unwrap().value),
            Some(20.0)
        );
        assert_eq!(
            t.disk(0).map(|d| d.write.latest().unwrap().value),
            Some(200.0)
        );
        assert_eq!(t.adapter(7).map(|a| a.tx.len()), Some(1));

        // Disk 1 is unplugged; the adapter read failed this pass.
        let mut s = snap(2, 1.0, 1);
        s.disks = vec![disk(0, 30.0)];
        t.observe(&s);
        assert!(t.disk(1).is_none());
        assert_eq!(t.disk(0).map(|d| d.active.len()), Some(2));
        assert!(t.adapter(7).is_some(), "an empty list prunes nothing");
    }

    #[test]
    fn gpus_and_the_battery_get_series_too() {
        use ot_model::battery::BatterySample;
        use ot_model::gpu::{GpuInfo, GpuSample};
        use ot_model::Watts;
        use std::sync::Arc;
        let gpu = |id: u64, load: f32| GpuSample {
            info: Arc::new(GpuInfo {
                id,
                ..GpuInfo::default()
            }),
            utilization: Percent(load),
            engines: Vec::new(),
            dedicated_used: Bytes(1 << 30),
            shared_used: Bytes(0),
        };
        let mut t = Timeline::new(Retention::raw(10));
        let mut s = snap(1, 1.0, 1);
        s.gpus = vec![gpu(3, 40.0), gpu(4, 5.0)];
        s.battery = Some(BatterySample {
            charge: Some(80.0),
            rate: Some(Watts(12.0)),
            ..BatterySample::default()
        });
        t.observe(&s);
        assert_eq!(
            t.gpu(3).map(|g| g.utilization.latest().unwrap().value),
            Some(40.0)
        );
        assert_eq!(
            t.gpu(4).map(|g| g.dedicated.latest().unwrap().value),
            Some(1_073_741_824.0)
        );
        assert_eq!(t.battery_charge.latest().map(|s| s.value), Some(80.0));
        assert_eq!(t.battery_rate.latest().map(|s| s.value), Some(12.0));

        // GPU 4 is gone; the battery sample is missing this pass.
        let mut s = snap(2, 1.0, 1);
        s.gpus = vec![gpu(3, 50.0)];
        t.observe(&s);
        assert!(t.gpu(4).is_none());
        assert_eq!(t.gpu(3).map(|g| g.utilization.len()), Some(2));
        assert_eq!(t.battery_charge.len(), 1, "nothing to push, nothing lost");
    }

    #[test]
    fn empty_snapshot_is_ignored() {
        let mut t = Timeline::new(Retention::raw(10));
        t.observe(&Snapshot::default());
        assert!(t.cpu_total.is_empty());
        assert!(t.last_tick().is_none());
    }

    #[test]
    fn timestamps_come_from_the_snapshot() {
        let mut t = Timeline::new(Retention::raw(10));
        t.observe(&snap(7, 1.0, 1));
        assert_eq!(t.cpu_total.latest().map(|s| s.at_unix_ms), Some(7000));
    }

    fn tiered() -> Retention {
        Retention {
            raw: 30,
            tiers: vec![Resolution {
                bucket_ms: 10_000,
                capacity: 6,
            }],
        }
    }

    /// One sample a second from t = 0 s, value = the second.
    fn seconds(n: i64) -> Series {
        let mut s = Series::new(tiered());
        for i in 0..n {
            s.push(i * 1000, i as f32);
        }
        s
    }

    #[test]
    fn buckets_summarize_aligned_windows() {
        let s = seconds(25);
        // Closed: [0, 10) and [10, 20). Open: [20, 25).
        let t = &s.tiers[0];
        let closed: Vec<Bucket> = t.closed.iter().copied().collect();
        assert_eq!(closed.len(), 2);
        assert_eq!(
            closed[1],
            Bucket {
                first_ms: 10_000,
                last_ms: 19_000,
                min: 10.0,
                max: 19.0,
                mean: 14.5,
                count: 10,
            }
        );
        let open = t.open.expect("filling").bucket;
        assert_eq!((open.first_ms, open.count, open.mean), (20_000, 5, 22.0));
    }

    #[test]
    fn history_is_raw_while_raw_covers_everything() {
        let s = seconds(25);
        let h: Vec<Bucket> = s.history().collect();
        assert_eq!(h.len(), 25, "no bucket repeats what raw already covers");
        assert!(h.iter().all(|b| b.count == 1));
        assert_eq!(h[0].first_ms, 24_000, "newest first");
        assert_eq!(h[24].first_ms, 0);
    }

    #[test]
    fn history_continues_in_buckets_where_raw_runs_out() {
        // 100 s of samples; raw keeps the last 30 (70..=99 s). The tier keeps six
        // closed buckets, [30, 40) through [80, 90), plus [90, 100) filling. Of
        // those, the ones wholly before 70 s continue the history.
        let s = seconds(100);
        let h: Vec<Bucket> = s.history().collect();
        let raw: Vec<&Bucket> = h.iter().take_while(|b| b.count == 1).collect();
        assert_eq!(raw.len(), 30);
        assert_eq!(raw.last().unwrap().first_ms, 70_000);
        let coarse: Vec<i64> = h[30..].iter().map(|b| b.first_ms).collect();
        assert_eq!(coarse, [60_000, 50_000, 40_000, 30_000]);
        assert!(h[30..].iter().all(|b| b.count == 10));
        // Strictly older all the way down: nothing overlaps, nothing is reordered.
        assert!(h.windows(2).all(|w| w[1].last_ms < w[0].first_ms));
    }

    #[test]
    fn a_seam_that_splits_a_bucket_skips_it() {
        // Raw keeps 30 samples; with 105 s pushed, raw starts at 75 s, inside the
        // [70, 80) bucket. That bucket is not wholly older, so it is skipped and the
        // coarse part starts at [60, 70): a gap of 5 s at the seam, not an overlap.
        let s = seconds(105);
        let h: Vec<Bucket> = s.history().collect();
        let first_coarse = h.iter().find(|b| b.count > 1).unwrap();
        assert_eq!(first_coarse.first_ms, 60_000);
        assert!(h.windows(2).all(|w| w[1].last_ms < w[0].first_ms));
    }

    #[test]
    fn memory_is_bounded_by_the_retention() {
        let s = seconds(10_000);
        assert_eq!(s.len(), 30);
        assert_eq!(s.tiers[0].closed.len(), 6);
        // Raw covers the last 30 s, which the filling bucket and the two newest
        // closed ones overlap; the other four closed buckets extend it.
        assert_eq!(s.history().count(), 34);
    }

    #[test]
    fn the_oldest_sample_held_is_the_span_of_the_history() {
        let s = seconds(25);
        assert_eq!(s.oldest_ms(), Some(0), "all raw");
        let s = seconds(10_000);
        // Raw holds 9970..=9999 s; [9990, 10000) is filling, and the six closed 10 s
        // buckets before it reach back to 9930 s.
        assert_eq!(s.oldest_ms(), Some(9_930_000));
        assert_eq!(Series::new(tiered()).oldest_ms(), None);
    }

    #[test]
    fn covering_a_span_scales_the_tiers_with_it() {
        let hour = Retention::covering(3_600_000);
        assert_eq!(hour.raw, RAW_SAMPLES);
        assert_eq!(hour.tiers.len(), 1);
        assert_eq!(hour.tiers[0].bucket_ms, 10_000);
        assert!(hour.tiers[0].capacity >= 360);

        let day = Retention::covering(24 * 3_600_000);
        assert_eq!(day.tiers.len(), 2);
        assert_eq!(day.tiers[0].capacity, hour.tiers[0].capacity.max(722));
        assert_eq!(day.tiers[1].bucket_ms, 60_000);
        assert!(day.tiers[1].capacity >= 1440);
        assert!(day.bytes_per_series() > hour.bytes_per_series());
        assert!(
            day.bytes_per_series() < 100_000,
            "{}",
            day.bytes_per_series()
        );
        assert_eq!(
            day.points(),
            day.raw + day.tiers[0].capacity + 1 + day.tiers[1].capacity + 1
        );

        // A day of samples fits and reaches back a day.
        let mut s = Series::new(day);
        for i in 0..(25 * 3600) {
            s.push(i64::from(i) * 1000, 1.0);
        }
        let span = s.latest().unwrap().at_unix_ms - s.oldest_ms().unwrap();
        assert!(span >= 24 * 3_600_000, "{span}");
    }

    #[test]
    fn changing_the_retention_keeps_what_still_fits() {
        let mut s = seconds(100);
        // Shrink: raw drops to the newest 10, the tier to 3 closed buckets.
        s.set_retention(Retention {
            raw: 10,
            tiers: vec![Resolution {
                bucket_ms: 10_000,
                capacity: 3,
            }],
        });
        assert_eq!(s.len(), 10);
        assert_eq!(s.raw().next().unwrap().at_unix_ms, 90_000);
        assert_eq!(s.tiers[0].closed.len(), 3);
        assert_eq!(s.oldest_ms(), Some(60_000));
        // Grow, with a coarser tier added: the 10 s tier keeps its buckets, the new
        // one starts empty and fills from the next sample.
        s.set_retention(Retention {
            raw: 50,
            tiers: vec![
                Resolution {
                    bucket_ms: 10_000,
                    capacity: 8,
                },
                Resolution {
                    bucket_ms: 60_000,
                    capacity: 4,
                },
            ],
        });
        assert_eq!(s.tiers[0].closed.len(), 3, "kept");
        assert_eq!(s.tiers.len(), 2);
        assert!(s.tiers[1].open.is_none());
        for i in 100..250 {
            s.push(i * 1000, i as f32);
        }
        assert_eq!(s.len(), 50);
        assert_eq!(s.tiers[0].closed.len(), 8);
        assert_eq!(
            s.tiers[1].closed.len(),
            3,
            "[60, 120) from 100 s on, then [120, 180) and [180, 240)"
        );
        assert!(s.history().count() > 50);
    }

    #[test]
    fn the_timeline_resizes_every_series_and_reports_its_span() {
        let mut t = Timeline::new(Retention::raw(10));
        assert_eq!(t.span_ms(), None);
        for tick in 1..=20 {
            t.observe(&snap(tick, 50.0, 2));
        }
        assert_eq!(t.span_ms(), Some(9000), "ten samples, 11 s to 20 s");
        assert_eq!(t.series_count(), 6);
        t.set_retention(Retention::raw(3));
        assert_eq!(t.span_ms(), Some(2000));
        assert_eq!(t.cores[1].len(), 3);
        assert_eq!(t.mem_in_use.len(), 3);
        assert_eq!(t.retention().raw, 3);
    }
}
