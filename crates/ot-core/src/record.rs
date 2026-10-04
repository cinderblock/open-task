//! Recording the snapshots a sampler publishes.
//!
//! Writing goes to its own thread (`ot-recorder`), fed through a bounded channel
//! of `Arc<Snapshot>`: the sampler already holds each snapshot behind an `Arc`, so
//! handing one over is a pointer copy, and the sampler thread never waits for the
//! disk. When the channel is full, because the disk is slow or the file is on a
//! network share, the frame is dropped and counted rather than stalling sampling;
//! the count is in [`RecordStats::dropped`] and [`RecordProgress::dropped`]. Stopping
//! closes the channel and joins the writer, which finishes the file.
//!
//! [`RecordingProbe`] wraps a probe so a consumer that builds its own `Sampler`
//! (the Windows shell, today) can record without being changed: it clones each
//! pass's output into a snapshot and feeds the same writer. Once the shell owns a
//! [`Feed`](crate::Feed), it should call [`Sampler::record_to`](crate::Sampler::record_to)
//! instead, which costs no copy.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use ot_model::hardware::Hardware;
use ot_model::{Capabilities, Tick};
use ot_probe::{ProbeError, ProbeOutput, SystemProbe};
use ot_record::{Error, Recorder, RecorderStats};

use crate::snapshot::Snapshot;

/// Frames the writer may fall behind by before frames are dropped. At one frame
/// a second this is several seconds of a stalled disk.
const QUEUE: usize = 4;

/// What a finished recording amounted to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordStats {
    /// What reached the file.
    pub written: RecorderStats,
    /// Frames the sampler published that were dropped because the writer was
    /// behind. Zero on a healthy disk.
    pub dropped: u64,
}

/// A recording in flight, for a status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordProgress {
    /// Frames in the file so far.
    pub frames: u64,
    /// Bytes in the file so far.
    pub bytes: u64,
    /// Frames dropped so far.
    pub dropped: u64,
    /// The writer hit an error and has stopped; the file is finished as far as it
    /// got. [`Sampler::stop_recording`](crate::Sampler::stop_recording) returns the
    /// error.
    pub failed: bool,
}

/// The sampler's end of the channel, plus counters both ends update.
#[derive(Debug)]
pub(crate) struct Sink {
    /// `None` once closed: the writer should finish.
    tx: Mutex<Option<SyncSender<Arc<Snapshot>>>>,
    dropped: AtomicU64,
    frames: AtomicU64,
    bytes: AtomicU64,
    failed: AtomicBool,
}

impl Sink {
    /// Hand a snapshot to the writer, or drop it if the writer is behind. Never
    /// blocks.
    pub(crate) fn offer(&self, snap: Arc<Snapshot>) {
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = tx.as_ref() else { return };
        // Disconnected means the writer stopped on an error, which
        // `stop_recording` reports; nothing to do here.
        if let Err(TrySendError::Full(_)) = tx.try_send(snap) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn close(&self) {
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    fn progress(&self) -> RecordProgress {
        RecordProgress {
            frames: self.frames.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

/// The writer thread and its sink.
#[derive(Debug)]
pub(crate) struct Writer {
    sink: Arc<Sink>,
    thread: Option<JoinHandle<Result<RecorderStats, Error>>>,
}

impl Writer {
    /// Start writing to `recorder` on a new thread.
    ///
    /// # Panics
    /// If the OS refuses to spawn a thread.
    pub(crate) fn spawn(recorder: Recorder) -> Self {
        let (tx, rx) = sync_channel(QUEUE);
        let sink = Arc::new(Sink {
            tx: Mutex::new(Some(tx)),
            dropped: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        let thread = {
            let sink = Arc::clone(&sink);
            std::thread::Builder::new()
                .name("ot-recorder".into())
                .spawn(move || run(recorder, &rx, &sink))
                .expect("spawn recorder thread")
        };
        Self {
            sink,
            thread: Some(thread),
        }
    }

    pub(crate) fn sink(&self) -> &Arc<Sink> {
        &self.sink
    }

    pub(crate) fn progress(&self) -> RecordProgress {
        self.sink.progress()
    }

    /// Close the channel, wait for the writer to finish the file, and report.
    pub(crate) fn finish(mut self) -> Result<RecordStats, Error> {
        self.finish_inner().unwrap_or_else(|| Err(panicked()))
    }

    fn finish_inner(&mut self) -> Option<Result<RecordStats, Error>> {
        let thread = self.thread.take()?;
        self.sink.close();
        let dropped = self.sink.dropped.load(Ordering::Relaxed);
        Some(match thread.join() {
            Ok(Ok(written)) => Ok(RecordStats { written, dropped }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(panicked()),
        })
    }
}

fn panicked() -> Error {
    Error::Corrupt("the writer thread panicked".to_owned())
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.finish_inner();
    }
}

fn run(
    mut recorder: Recorder,
    rx: &Receiver<Arc<Snapshot>>,
    sink: &Sink,
) -> Result<RecorderStats, Error> {
    for snap in rx {
        if let Err(e) = recorder.write_shared(&snap) {
            sink.failed.store(true, Ordering::Relaxed);
            tracing::error!(error = %e, "recording stopped");
            // Finish what there is; the error is what the caller hears.
            let _ = recorder.finish();
            return Err(e);
        }
        let stats = recorder.stats();
        sink.frames.store(stats.frames, Ordering::Relaxed);
        sink.bytes.store(stats.bytes, Ordering::Relaxed);
    }
    recorder.finish()
}

/// A probe that records every pass it takes.
///
/// For a consumer that builds its own `Sampler` from a probe and cannot be handed
/// a recorder. Each pass is cloned into a [`Snapshot`] (the `Vec`s are copied; the
/// shared `Arc`s are pointer copies) and queued for the writer thread, so the
/// recording is what the sampler publishes, numbered the same way. Dropping the
/// probe, which happens when its sampler stops, finishes the file.
#[derive(Debug)]
pub struct RecordingProbe {
    inner: Box<dyn SystemProbe>,
    writer: Option<Writer>,
    hardware: Arc<Hardware>,
    capabilities: Capabilities,
    tick: Tick,
    last_start: Option<Instant>,
}

impl RecordingProbe {
    /// Wrap `inner`, recording to `recorder`.
    #[must_use]
    pub fn new(inner: Box<dyn SystemProbe>, recorder: Recorder) -> Self {
        Self {
            hardware: Arc::new(inner.hardware()),
            capabilities: inner.capabilities(),
            inner,
            writer: Some(Writer::spawn(recorder)),
            tick: Tick::default(),
            last_start: None,
        }
    }

    /// The recording so far.
    #[must_use]
    pub fn progress(&self) -> Option<RecordProgress> {
        self.writer.as_ref().map(Writer::progress)
    }

    /// Finish the file now rather than when the probe is dropped. `None` if it
    /// was already finished.
    ///
    /// # Errors
    /// If the writer failed.
    pub fn stop(&mut self) -> Option<Result<RecordStats, Error>> {
        self.writer.take().map(Writer::finish)
    }
}

impl SystemProbe for RecordingProbe {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn hardware(&self) -> Hardware {
        (*self.hardware).clone()
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        let start = Instant::now();
        let interval = self
            .last_start
            .map_or(Duration::ZERO, |p| start.duration_since(p));
        self.last_start = Some(start);
        self.inner.sample(out)?;
        self.tick = self.tick.next();
        if let Some(writer) = &self.writer {
            writer.sink().offer(Arc::new(Snapshot {
                tick: self.tick,
                taken_at: Some(SystemTime::now()),
                interval,
                probe_cost: start.elapsed(),
                cpu: out.cpu.clone(),
                memory: out.memory,
                processes: out.processes.clone(),
                threads: out.threads.clone(),
                disks: out.disks.clone(),
                adapters: out.adapters.clone(),
                gpus: out.gpus.clone(),
                battery: out.battery.clone(),
                volumes: out.volumes.clone(),
                sessions: out.sessions.clone(),
                services: Arc::clone(&out.services),
                capabilities: self.capabilities,
                hardware: Arc::clone(&self.hardware),
            }));
        }
        Ok(())
    }
}

impl Drop for RecordingProbe {
    fn drop(&mut self) {
        match self.stop() {
            Some(Ok(stats)) => tracing::info!(
                frames = stats.written.frames,
                bytes = stats.written.bytes,
                dropped = stats.dropped,
                "recording finished"
            ),
            Some(Err(e)) => tracing::error!(error = %e, "recording failed"),
            None => {}
        }
    }
}
