//! Graphics adapters and their load.
//!
//! A GPU is many engines (3D, copy, video decode, compute, ...) that run on their
//! own. Its "utilization" is what Task Manager shows: the busiest engine's share of
//! the interval, because the engines do not add up, a GPU with 3D at 60 % and copy
//! at 60 % is not 120 % busy. The engines are reported too, for the chart.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::units::{Bytes, Percent};

/// A graphics adapter's facts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GpuInfo {
    /// Stable while the adapter is present (the adapter LUID on Windows); the key
    /// its history is kept under.
    pub id: u64,
    /// `GPU 0`, `GPU 1`, in the order the system enumerates them.
    pub name: String,
    /// The adapter as its maker names it: `NVIDIA GeForce RTX 4070`.
    pub adapter: String,
    /// Memory on the adapter itself.
    pub dedicated_total: Option<Bytes>,
    /// System memory the adapter may use.
    pub shared_total: Option<Bytes>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    /// Where it sits: `PCI bus 1, device 0, function 0`.
    pub location: Option<String>,
    /// A rendering adapter with no display output, or a software one.
    pub software: bool,
}

/// One engine's load over the interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSample {
    /// `3D`, `Copy`, `Video Decode`, `Compute 0`, as the platform names them.
    pub name: Arc<str>,
    pub usage: Percent,
}

/// A graphics adapter over one sampling interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSample {
    /// Not serialized: shared by pointer between passes, so the Flight Recorder
    /// writes each distinct value once in a table and restores it on read.
    #[serde(skip)]
    pub info: Arc<GpuInfo>,
    /// The busiest engine's share of the interval, `0..=100`.
    pub utilization: Percent,
    /// Every engine that did anything, busiest first.
    pub engines: Vec<EngineSample>,
    pub dedicated_used: Bytes,
    pub shared_used: Bytes,
}
