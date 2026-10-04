//! Replaying a recording where the sampler normally goes.
//!
//! A [`Player`] is the replay-side counterpart of [`Sampler`](crate::Sampler): it
//! publishes the current frame through the same kind of lock-free slot, wakes the
//! UI the same way, and while playing advances on wall time by the spacing of the
//! recorded frames (divided by the speed). Frames are decoded on the player's own
//! thread (`ot-player`), so a seek into a long run of delta frames never stalls
//! the UI; the UI reads [`Player::latest`] whenever it paints.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use ot_record::{Error, Recording};

use crate::snapshot::Snapshot;

/// What the UI asks of the player thread.
#[derive(Debug)]
struct Control {
    playing: bool,
    speed: f32,
    /// A frame to show next, whatever is playing.
    seek: Option<usize>,
    stop: bool,
    /// When play started or was last re-anchored (after a seek or a speed
    /// change): the wall time, and the recording offset shown then. Frame `i` is
    /// due at `anchor + (offset(i) - anchor_offset) / speed`.
    anchor: Option<(Instant, i64)>,
}

#[derive(Debug)]
struct Shared {
    current: ArcSwap<Snapshot>,
    position: AtomicUsize,
    control: Mutex<Control>,
    wake: Condvar,
    /// Why playback stopped on its own, if it did.
    error: Mutex<Option<String>>,
}

type Notify = Box<dyn Fn() + Send>;

/// Plays a [`Recording`]. Dropping it stops the thread and joins it.
#[derive(Debug)]
pub struct Player {
    recording: Arc<Recording>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Open `recording` at its first frame, paused, at normal speed.
    ///
    /// # Errors
    /// If the first frame cannot be decoded.
    ///
    /// # Panics
    /// If the OS refuses to spawn a thread.
    pub fn new(recording: Recording) -> Result<Self, Error> {
        Self::spawn(recording, None)
    }

    /// Like [`Player::new`], with `notify` called on the player thread after each
    /// frame is published, as the sampler's notify is. It must be cheap and
    /// non-blocking.
    ///
    /// # Errors
    /// If the first frame cannot be decoded.
    ///
    /// # Panics
    /// If the OS refuses to spawn a thread.
    pub fn with_notify(
        recording: Recording,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, Error> {
        Self::spawn(recording, Some(Box::new(notify)))
    }

    fn spawn(recording: Recording, notify: Option<Notify>) -> Result<Self, Error> {
        let first = if recording.is_empty() {
            Arc::new(Snapshot::default())
        } else {
            recording.frame(0)?
        };
        let recording = Arc::new(recording);
        let shared = Arc::new(Shared {
            current: ArcSwap::new(first),
            position: AtomicUsize::new(0),
            control: Mutex::new(Control {
                playing: false,
                speed: 1.0,
                seek: None,
                stop: false,
                anchor: None,
            }),
            wake: Condvar::new(),
            error: Mutex::new(None),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            let recording = Arc::clone(&recording);
            std::thread::Builder::new()
                .name("ot-player".into())
                .spawn(move || run(&recording, &shared, notify.as_deref()))
                .expect("spawn player thread")
        };
        Ok(Self {
            recording,
            shared,
            thread: Some(thread),
        })
    }

    /// The frame on show. Never blocks. Cheap enough to call every paint.
    #[must_use]
    pub fn latest(&self) -> Arc<Snapshot> {
        self.shared.current.load_full()
    }

    /// The recording being played.
    #[must_use]
    pub fn recording(&self) -> &Arc<Recording> {
        &self.recording
    }

    /// Frames in the recording.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recording.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recording.is_empty()
    }

    /// Index of the frame on show.
    #[must_use]
    pub fn position(&self) -> usize {
        self.shared.position.load(Ordering::Acquire)
    }

    /// The configured sampling interval of the recording.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.recording.header().interval
    }

    fn control(&self) -> std::sync::MutexGuard<'_, Control> {
        self.shared
            .control
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Advance on wall time from the current frame. At the last frame, start over.
    pub fn play(&self) {
        if self.len() < 2 {
            return;
        }
        let mut c = self.control();
        if self.position() + 1 >= self.len() && c.seek.is_none() {
            c.seek = Some(0);
        }
        c.playing = true;
        c.anchor = None;
        self.shared.wake.notify_all();
    }

    /// Stay on the current frame.
    pub fn pause(&self) {
        let mut c = self.control();
        c.playing = false;
        c.anchor = None;
    }

    /// Pause if playing, play if paused.
    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.control().playing
    }

    /// Show frame `i` (clamped to the recording). Playback, if on, continues from
    /// there.
    pub fn seek(&self, i: usize) {
        let Some(last) = self.len().checked_sub(1) else {
            return;
        };
        let mut c = self.control();
        c.seek = Some(i.min(last));
        c.anchor = None;
        self.shared.wake.notify_all();
    }

    /// Show the frame `by` frames away: `-1` for the one before, `1` for the one
    /// after. Pauses.
    pub fn step(&self, by: i64) {
        let Some(last) = self.len().checked_sub(1) else {
            return;
        };
        let target = (i64::try_from(self.position())
            .unwrap_or(i64::MAX)
            .saturating_add(by))
        .clamp(0, i64::try_from(last).unwrap_or(i64::MAX));
        let mut c = self.control();
        c.playing = false;
        c.seek = Some(usize::try_from(target).unwrap_or(0));
        c.anchor = None;
        self.shared.wake.notify_all();
    }

    /// Playback rate: 1 is as recorded, 2 twice as fast. Clamped to a sane range.
    pub fn set_speed(&self, speed: f32) {
        let mut c = self.control();
        c.speed = if speed.is_finite() {
            speed.clamp(0.01, 1000.0)
        } else {
            1.0
        };
        c.anchor = None;
        self.shared.wake.notify_all();
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.control().speed
    }

    /// Why playback stopped on its own: a frame that could not be read. Cleared
    /// by the next frame that could.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stop the thread and wait for it. Also happens on drop.
    pub fn stop(&mut self) {
        self.control().stop = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the thread decided to do after looking at the controls.
enum Next {
    Load(usize),
    Stop,
}

fn run(recording: &Recording, shared: &Shared, notify: Option<&(dyn Fn() + Send)>) {
    let offset = |i: usize| recording.offset_ms(i).unwrap_or(0);
    loop {
        let next = {
            let mut c = shared
                .control
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            loop {
                if c.stop {
                    break Next::Stop;
                }
                if let Some(target) = c.seek.take() {
                    break Next::Load(target);
                }
                if !c.playing {
                    c = shared.wake.wait(c).unwrap_or_else(PoisonError::into_inner);
                    continue;
                }
                let pos = shared.position.load(Ordering::Acquire);
                let next = pos + 1;
                if next >= recording.len() {
                    c.playing = false;
                    c.anchor = None;
                    continue;
                }
                let (anchor_at, anchor_ms) = *c
                    .anchor
                    .get_or_insert_with(|| (Instant::now(), offset(pos)));
                let ahead_ms = (offset(next) - anchor_ms).max(0) as f64 / f64::from(c.speed);
                let due = anchor_at + Duration::from_secs_f64(ahead_ms / 1000.0);
                let now = Instant::now();
                if now < due {
                    let (guard, _) = shared
                        .wake
                        .wait_timeout(c, due - now)
                        .unwrap_or_else(PoisonError::into_inner);
                    c = guard;
                    continue;
                }
                break Next::Load(next);
            }
        };
        let Next::Load(i) = next else { return };
        match recording.frame(i) {
            Ok(snap) => {
                shared.current.store(snap);
                shared.position.store(i, Ordering::Release);
                shared
                    .error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(n) = notify {
                    n();
                }
            }
            Err(e) => {
                tracing::error!(frame = i, error = %e, "replay stopped");
                *shared.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
                let mut c = shared
                    .control
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                c.playing = false;
                c.anchor = None;
            }
        }
    }
}
