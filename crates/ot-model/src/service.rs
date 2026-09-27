//! Services hosted by a process.
//!
//! On Windows most system services share a handful of `svchost.exe` processes, so a
//! busy service host is opaque unless the table can say which services live in it.
//! The service control manager reports which process each service runs in; the
//! probe attaches that list to the process. Per-service CPU is not a property of the
//! service itself but of the threads that carry its tag (see
//! [`crate::thread::ThreadSample::service`]); consumers sum it from the threads.

use std::sync::Arc;

/// Lifecycle state of a service, reduced to what a task manager needs to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ServiceState {
    Running,
    StartPending,
    StopPending,
    Stopped,
    Paused,
    #[default]
    Unknown,
}

impl ServiceState {
    /// Short label for display.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::StartPending => "Starting",
            Self::StopPending => "Stopping",
            Self::Stopped => "Stopped",
            Self::Paused => "Paused",
            Self::Unknown => "",
        }
    }
}

/// One service, as the control manager describes it.
///
/// Strings are shared: a machine has a few hundred services whose names never
/// change, and the list is republished every pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// Key name, e.g. `BrokerInfrastructure`.
    pub name: Arc<str>,
    /// Human name, e.g. `Background Tasks Infrastructure Service`.
    pub display_name: Arc<str>,
    pub state: ServiceState,
    /// File name of the DLL the service is registered to run from (`bisrv.dll`),
    /// when the platform records one. A hint for where its code lives, not a
    /// measurement: the work can happen in other modules.
    pub dll: Option<Arc<str>>,
}
