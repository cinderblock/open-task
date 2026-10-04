//! The sampling thread and its lock-free publication slot.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use arc_swap::{ArcSwap, ArcSwapOption};
use ot_model::Tick;
use ot_probe::{ProbeError, ProbeOutput, SystemProbe};
use ot_record::{Error as RecordError, Recorder};

use crate::record::{self, RecordProgress, RecordStats, Sink};
use crate::snapshot::Snapshot;

/// Tunables for the sampling loop.
#[derive(Debug, Clone, Copy)]
pub struct SamplerConfig {
    /// Time between the start of one pass and the start of the next.
    pub interval: Duration,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
        }
    }
}

/// Shared state between the sampler thread and its readers.
#[derive(Debug)]
struct Shared {
    /// The newest snapshot. Readers `load()` it; the sampler `store()`s the next one.
    current: ArcSwap<Snapshot>,
    /// Interval in microseconds, adjustable at runtime without restarting the thread.
    interval_us: AtomicU64,
    stop: AtomicBool,
    /// Consecutive failed passes, so the UI can show "probe failing" rather than a
    /// frozen table.
    consecutive_errors: AtomicU64,
    /// Where published snapshots also go while recording. Loaded once per pass.
    sink: ArcSwapOption<Sink>,
}

/// A running sampler. Dropping it stops the thread and joins it.
#[derive(Debug)]
pub struct Sampler {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The writer thread of the recording in progress, if any.
    recording: Mutex<Option<record::Writer>>,
}

impl Sampler {
    /// Start sampling `probe` on a dedicated thread.
    ///
    /// See [`Sampler::start_with_notify`] to be woken on each publish.
    ///
    /// # Panics
    /// If the OS refuses to spawn a thread. There is nothing useful the app can do
    /// without its sampler, so this is treated as unrecoverable.
    #[must_use]
    pub fn start(probe: Box<dyn SystemProbe>, config: SamplerConfig) -> Self {
        Self::spawn(probe, config, None)
    }

    /// Like [`Sampler::start`], but `notify` is called on the sampler thread right
    /// after each snapshot is published. It must be cheap and non-blocking; on
    /// Windows it posts a message to the UI thread, nothing more.
    ///
    /// # Panics
    /// If the OS refuses to spawn a thread.
    #[must_use]
    pub fn start_with_notify(
        probe: Box<dyn SystemProbe>,
        config: SamplerConfig,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        Self::spawn(probe, config, Some(Box::new(notify)))
    }

    fn spawn(probe: Box<dyn SystemProbe>, config: SamplerConfig, notify: Option<Notify>) -> Self {
        let shared = Arc::new(Shared {
            current: ArcSwap::from_pointee(Snapshot::default()),
            interval_us: AtomicU64::new(config.interval.as_micros() as u64),
            stop: AtomicBool::new(false),
            consecutive_errors: AtomicU64::new(0),
            sink: ArcSwapOption::empty(),
        });

        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("ot-sampler".into())
                .spawn(move || run(probe, &shared, notify.as_deref()))
                .expect("spawn sampler thread")
        };

        Self {
            shared,
            thread: Some(thread),
            recording: Mutex::new(None),
        }
    }

    /// The most recent snapshot. Never blocks. Cheap enough to call every frame.
    #[must_use]
    pub fn latest(&self) -> Arc<Snapshot> {
        self.shared.current.load_full()
    }

    /// Change the cadence. Takes effect after the current sleep.
    pub fn set_interval(&self, interval: Duration) {
        self.shared
            .interval_us
            .store(interval.as_micros().max(1) as u64, Ordering::Relaxed);
    }

    #[must_use]
    pub fn interval(&self) -> Duration {
        Duration::from_micros(self.shared.interval_us.load(Ordering::Relaxed))
    }

    /// How many passes in a row have failed. Zero means healthy.
    #[must_use]
    pub fn consecutive_errors(&self) -> u64 {
        self.shared.consecutive_errors.load(Ordering::Relaxed)
    }

    /// Write every snapshot published from now on to `recorder`, on a writer
    /// thread (see [`record`](crate::record) for how a slow disk is handled). A
    /// recording already in progress is finished first and its result returned.
    pub fn record_to(&self, recorder: Recorder) -> Option<Result<RecordStats, RecordError>> {
        let previous = self.stop_recording();
        let writer = record::Writer::spawn(recorder);
        self.shared.sink.store(Some(Arc::clone(writer.sink())));
        *self
            .recording
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(writer);
        previous
    }

    /// Finish the recording in progress: the file gets its index and the writer
    /// thread ends. `None` if nothing was being recorded.
    pub fn stop_recording(&self) -> Option<Result<RecordStats, RecordError>> {
        let writer = self
            .recording
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        // No more frames reach the writer; then the channel closes and it finishes.
        self.shared.sink.store(None);
        Some(writer.finish())
    }

    /// The recording in progress, if any.
    #[must_use]
    pub fn recording(&self) -> Option<RecordProgress> {
        self.recording
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(record::Writer::progress)
    }

    /// Ask the thread to stop and wait for it. Also happens on drop. A recording
    /// in progress is finished first.
    pub fn stop(&mut self) {
        if let Some(Err(e)) = self.stop_recording() {
            tracing::error!(error = %e, "recording failed");
        }
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop();
    }
}

type Notify = Box<dyn Fn() + Send>;

fn run(mut probe: Box<dyn SystemProbe>, shared: &Shared, notify: Option<&(dyn Fn() + Send)>) {
    let mut out = ProbeOutput::default();
    let capabilities = probe.capabilities();
    let hardware = Arc::new(probe.hardware());
    let mut tick = Tick::default();
    let mut last_start: Option<Instant> = None;

    while !shared.stop.load(Ordering::Relaxed) {
        let start = Instant::now();
        let interval = last_start.map_or(Duration::ZERO, |p| start.duration_since(p));
        last_start = Some(start);

        match probe.sample(&mut out) {
            Ok(()) => {
                tick = tick.next();
                shared.consecutive_errors.store(0, Ordering::Relaxed);
                let probe_cost = start.elapsed();

                // Move the buffers out of `out` into the snapshot, then give `out`
                // fresh ones with the same capacity so the next pass does not grow
                // from zero. The old snapshot's Vecs are freed when its last reader
                // drops it.
                let proc_cap = out.processes.capacity();
                let processes = std::mem::replace(&mut out.processes, Vec::with_capacity(proc_cap));
                let thread_cap = out.threads.capacity();
                let threads = std::mem::replace(&mut out.threads, Vec::with_capacity(thread_cap));
                let core_cap = out.cpu.cores.capacity();
                let cores = std::mem::replace(&mut out.cpu.cores, Vec::with_capacity(core_cap));
                let mut cpu = out.cpu.clone();
                cpu.cores = cores;
                // Few entries; taking them is cheaper than keeping capacity around.
                let disks = std::mem::take(&mut out.disks);
                let adapters = std::mem::take(&mut out.adapters);
                let gpus = std::mem::take(&mut out.gpus);
                let battery = out.battery.take();
                let volumes = std::mem::take(&mut out.volumes);
                let sessions = std::mem::take(&mut out.sessions);
                let services = Arc::clone(&out.services);

                let snap = Arc::new(Snapshot {
                    tick,
                    taken_at: Some(SystemTime::now()),
                    interval,
                    probe_cost,
                    cpu,
                    memory: out.memory,
                    processes,
                    threads,
                    disks,
                    adapters,
                    gpus,
                    battery,
                    volumes,
                    sessions,
                    services,
                    capabilities,
                    hardware: Arc::clone(&hardware),
                });
                if let Some(sink) = shared.sink.load().as_ref() {
                    sink.offer(Arc::clone(&snap));
                }
                shared.current.store(snap);
                if let Some(n) = notify {
                    n();
                }
            }
            Err(ProbeError::Unsupported(platform)) => {
                // Nothing will ever succeed here; log once and idle rather than spin.
                tracing::warn!(platform, "no probe implementation; sampler idle");
                shared.consecutive_errors.store(u64::MAX, Ordering::Relaxed);
                idle_until_stopped(shared);
                return;
            }
            Err(e) => {
                let n = shared.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::error!(error = %e, consecutive = n, "sampling pass failed");
            }
        }

        // Sleep for the remainder of the interval, re-reading it so `set_interval`
        // applies promptly. Sleep in short slices so `stop` is responsive.
        let target = Duration::from_micros(shared.interval_us.load(Ordering::Relaxed));
        let deadline = start + target;
        while !shared.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
        }
    }
}

fn idle_until_stopped(shared: &Shared) {
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}
