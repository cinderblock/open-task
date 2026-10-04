//! Pure data types for open-task.
//!
//! This crate deliberately depends on nothing but serde, performs no I/O, and
//! contains no platform-specific code. Everything here is a plain value type that
//! both the sampling core and the UI agree on, and every type derives `Serialize`
//! and `Deserialize`. Keeping it inert is what lets the Flight Recorder (`ot-record`)
//! write a session to a file and replay it into an unmodified UI.
//!
//! Fields shared by pointer between passes (`Arc<ProcessStatic>` and its kin) are
//! skipped by serde: the recorder writes each distinct value once and restores the
//! sharing on read, so a `Snapshot` serialized on its own is not complete. Use the
//! recorder for a full copy.

#![forbid(unsafe_code)]

pub mod apps;
pub mod attribution;
pub mod battery;
pub mod connection;
pub mod cpu;
pub mod device;
pub mod gpu;
pub mod hardware;
pub mod identity;
pub mod memory;
pub mod process;
pub mod service;
pub mod session;
pub mod snapshot;
pub mod startup;
pub mod system;
pub mod thread;
pub mod units;

pub use identity::{ProcessKey, ProcessKeyRaw};
use serde::{Deserialize, Serialize};
pub use snapshot::Snapshot;
pub use units::{Bytes, Hertz, Percent, Watts};

/// Which metrics this platform can actually supply.
///
/// The UI uses this to hide columns rather than show a grid of dashes, and to say
/// why a piece of attribution is missing (not elevated, not implemented here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
// A flag set is the honest shape for this; enums would add nothing but ceremony.
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub per_process_cpu: bool,
    pub per_process_disk: bool,
    pub per_process_network: bool,
    pub per_process_gpu: bool,
    pub per_process_power: bool,
    pub core_frequency: bool,
    pub package_power: bool,
    pub thermals: bool,
    pub hybrid_core_kinds: bool,
    /// Per-thread CPU rows.
    pub threads: bool,
    /// Which services live in which process.
    pub services: bool,
    /// Which service each thread of a service host works for. On Windows it needs
    /// `SeDebugPrivilege`: running as administrator, elevated.
    pub service_tags: bool,
    /// On-demand CPU sampling of one process by module. On Windows it needs
    /// `SeSystemProfilePrivilege` and trace-session rights: administrator, elevated.
    pub cpu_sampling: bool,
    /// Graphics adapters and their load.
    pub gpu: bool,
    /// Logon sessions, for the Users page.
    pub sessions: bool,
    /// The full service list, for the Services page.
    pub service_list: bool,
    /// Whether this process runs with administrator rights, which the actions
    /// that need them (starting and stopping services, signing out another user,
    /// ending another user's process) check for.
    pub elevated: bool,
}

/// Monotonic sample counter. Increments once per full sampling pass.
///
/// This is deliberately not a timestamp: it gives the UI a cheap, exact way to tell
/// whether a value was refreshed this pass, without any float comparison.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Tick(pub u64);

impl Tick {
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}
