//! The result of sampling one process's CPU on demand.
//!
//! Per-thread CPU says *which thread* is busy; a service tag says *which service*.
//! Neither says what code is running or on whose behalf. A short sampled profile
//! answers the first: the modules the hot threads were executing. For a service
//! that acts as a broker, its own trace events answer the second: the client
//! (package, device, task) it was working for. Both are collected for a few seconds
//! when asked, never continuously, and only by read-only means.

use std::time::Duration;

use crate::identity::ProcessKey;

/// One bucket of a histogram: a label and how many samples or events it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub label: String,
    pub count: u32,
}

/// Where one thread's samples landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadShares {
    pub tid: u32,
    /// Samples attributed to this thread.
    pub samples: u32,
    /// By module, most first.
    pub modules: Vec<Share>,
}

/// Events from a service's own trace provider during the sample, bucketed by the
/// field that names its client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientReport {
    /// The service the provider belongs to.
    pub service: String,
    /// Provider name, for the reader who wants to go further with their own tools.
    pub provider: String,
    /// The payload field that was bucketed, e.g. `PackageFullName`.
    pub field: String,
    /// Events seen from the provider in the window.
    pub events: u32,
    /// Events the session dropped because they arrived faster than they could be
    /// consumed; a non-zero count means the buckets undercount but rank correctly.
    pub lost: u32,
    /// Most frequent first.
    pub buckets: Vec<Share>,
}

/// What a sample of one process found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub target: ProcessKey,
    /// How long the sample ran.
    pub duration: Duration,
    /// Samples attributed to the target's threads.
    pub samples: u32,
    /// By module across the whole process, most first.
    pub modules: Vec<Share>,
    /// Per thread, busiest first.
    pub threads: Vec<ThreadShares>,
    /// Present when a service in the target has a known trace provider.
    pub clients: Option<ClientReport>,
    /// Anything the reader should know: a refused provider, a truncated window.
    pub notes: Vec<String>,
}
