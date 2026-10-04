//! The measuring core.
//!
//! Owns the sampling thread, folds probe output into immutable [`Snapshot`]s, keeps
//! whole-system history, and publishes the latest snapshot through a lock-free slot
//! that any number of readers (the UI thread, a recorder, a remote endpoint) can load
//! without ever blocking the sampler.
//!
//! The same slot serves a replay: a [`Player`] publishes the frames of a recording
//! where the [`Sampler`] would publish live passes, and a [`Feed`] is whichever of
//! the two the UI is reading. [`Sampler::record_to`] writes what the sampler
//! publishes to a file (`record`).
//!
//! The sampling cadence and the UI's frame rate are independent by design. The UI
//! renders whatever the newest snapshot is at each frame; the sampler never waits for
//! the UI and the UI never waits for the sampler.

#![forbid(unsafe_code)]

pub mod feed;
pub mod history;
pub mod player;
pub mod record;
pub mod sampler;
pub mod timeline;
pub mod usage;

pub use feed::Feed;
pub use history::Ring;
/// The snapshot type lives in `ot-model` (so the Flight Recorder can read and write
/// it without depending on the core); this is the same module under its old name.
pub use ot_model::snapshot;
/// The Flight Recorder's file types, for callers that create a recorder or open a
/// recording to hand to [`Sampler::record_to`] or [`Player::new`].
pub use ot_record::{self, Error as RecordError, RecordHeader, Recorder, RecorderStats, Recording};
pub use player::Player;
pub use record::{RecordProgress, RecordStats, RecordingProbe};
pub use sampler::{Sampler, SamplerConfig};
pub use snapshot::Snapshot;
pub use timeline::{
    AdapterSeries, Bucket, DiskSeries, History, Resolution, Retention, Sample, Series, Timeline,
};
pub use usage::{Frame, ProcessUsage, ProgramId, Usage};
