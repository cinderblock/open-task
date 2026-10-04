//! Services hosted by a process.
//!
//! On Windows most system services share a handful of `svchost.exe` processes, so a
//! busy service host is opaque unless the table can say which services live in it.
//! The service control manager reports which process each service runs in; the
//! probe attaches that list to the process. Per-service CPU is not a property of the
//! service itself but of the threads that carry its tag (see
//! [`crate::thread::ThreadSample::service`]); consumers sum it from the threads.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Lifecycle state of a service, reduced to what a task manager needs to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// When a service starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StartType {
    /// Started by the boot loader (a driver).
    Boot,
    /// Started during kernel initialization (a driver).
    System,
    /// Started at boot by the service control manager.
    Automatic,
    /// Started shortly after boot, once the automatic services are up.
    AutomaticDelayed,
    /// Started on demand, by a program or a trigger.
    Manual,
    Disabled,
    #[default]
    Unknown,
}

impl StartType {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Boot => "Boot",
            Self::System => "System",
            Self::Automatic => "Automatic",
            Self::AutomaticDelayed => "Automatic (delayed)",
            Self::Manual => "Manual",
            Self::Disabled => "Disabled",
            Self::Unknown => "",
        }
    }
}

/// One service of the machine, running or not, for the Services page.
///
/// [`ServiceInfo`] is the running service seen from its host process; this is the
/// service as the control manager lists it, whether or not anything hosts it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    /// Key name, e.g. `BrokerInfrastructure`.
    pub name: Arc<str>,
    /// Human name, e.g. `Background Tasks Infrastructure Service`.
    pub display_name: Arc<str>,
    pub description: Option<Arc<str>>,
    pub state: ServiceState,
    pub start: StartType,
    /// The process hosting it, while it runs.
    pub pid: Option<u32>,
    /// The service group it shares a host with (`-k netsvcs`), when it does.
    pub group: Option<Arc<str>>,
    /// Whether the control manager will let this caller stop it: a service that
    /// does not accept stop, or one the caller lacks rights over, cannot be.
    pub can_stop: bool,
}
