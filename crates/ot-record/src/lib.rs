//! Flight Recorder: record and replay the open-task sample stream.
//!
//! A recording is the sequence of [`Snapshot`]s the sampler published, in a file
//! the user named. [`Recorder`] writes them as they arrive; [`Recording`] reads
//! them back by index, so a player can scrub. The core's `Player` publishes them
//! where the sampler normally would, and the UI cannot tell the difference.
//!
//! # What makes a frame small
//!
//! A snapshot of a busy machine is a few hundred processes and several thousand
//! threads, most of them unchanged from one pass to the next. Three things keep a
//! frame to a few kilobytes:
//!
//! - **Shared values are written once.** The probe hands out a process's statics,
//!   a disk's facts, a window, a service list behind an `Arc` and keeps the same
//!   `Arc` while the value is unchanged. The recorder numbers each distinct pointer
//!   the first time it sees it, writes the value in a table record, and refers to
//!   it by number from then on (`table.rs`). The reader hands every frame the same
//!   `Arc` for the same number, so the UI's pointer-identity logic works on a replay.
//! - **Frames are deltas.** Every [`DEFAULT_KEYFRAME_INTERVAL`]th frame is written
//!   whole; the others store each process and thread as its change from the frame
//!   before, so an idle process is a few dozen zero bytes and an idle thread four
//!   (`delta.rs`). A seek decodes forward from the nearest keyframe.
//! - **lz4 on top**, which turns the runs of zeros into almost nothing.
//!
//! The encoding is postcard (serde) throughout; `format.rs` has the file layout.
//!
//! [`Snapshot`]: ot_model::snapshot::Snapshot

#![forbid(unsafe_code)]

mod delta;
mod error;
mod format;
mod frame;
mod recorder;
mod recording;
mod table;

pub use error::Error;
pub use format::{RecordHeader, FORMAT_VERSION, MAGIC};
pub use recorder::{Recorder, RecorderStats, DEFAULT_KEYFRAME_INTERVAL};
pub use recording::Recording;
