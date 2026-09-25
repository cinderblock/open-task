//! The measuring core.
//!
//! Owns the sampling thread, folds probe output into immutable [`Snapshot`]s, keeps
//! whole-system history, and publishes the latest snapshot through a lock-free slot
//! that any number of readers (the UI thread, a recorder, a remote endpoint) can load
//! without ever blocking the sampler.
//!
//! The sampling cadence and the UI's frame rate are independent by design. The UI
//! renders whatever the newest snapshot is at each frame; the sampler never waits for
//! the UI and the UI never waits for the sampler.

#![forbid(unsafe_code)]

pub mod history;
pub mod sampler;
pub mod snapshot;

pub use history::Ring;
pub use sampler::{Sampler, SamplerConfig};
pub use snapshot::Snapshot;
