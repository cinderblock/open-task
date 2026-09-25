//! Pure data types for open-task.
//!
//! This crate deliberately has no dependencies, performs no I/O, and contains no
//! platform-specific code. Everything here is a plain value type that both the
//! sampling core and the UI agree on. Keeping it inert is what lets the Flight
//! Recorder serialize a session and replay it into an unmodified UI.

#![forbid(unsafe_code)]

pub mod cpu;
pub mod identity;
pub mod memory;
pub mod process;
pub mod units;

pub use identity::{ProcessKey, ProcessKeyRaw};
pub use units::{Bytes, Hertz, Percent, Watts};

/// Monotonic sample counter. Increments once per full sampling pass.
///
/// This is deliberately not a timestamp: it gives the UI a cheap, exact way to tell
/// whether a value was refreshed this pass, without any float comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub u64);

impl Tick {
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}
