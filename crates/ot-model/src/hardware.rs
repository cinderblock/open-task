//! Facts about the machine that do not change while it runs.

use crate::units::{Bytes, Hertz};

/// Read once when the probe starts and shared by pointer with every snapshot, the
/// way process statics are. Every field is optional or countable-to-zero, because
/// each comes from a different OS query and any one of them can be unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hardware {
    /// The processor's name as its maker reports it, e.g.
    /// `Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz`.
    pub cpu_name: Option<String>,
    /// The processor's rated clock, what Task Manager calls base speed.
    pub base_frequency: Option<Hertz>,
    /// Physical processor packages.
    pub sockets: u32,
    /// Physical cores across all packages.
    pub physical_cores: u32,
    /// Logical processors (hardware threads).
    pub logical_processors: u32,
    /// Cache totals across the whole machine: every core's L1 data and instruction
    /// caches together, every L2, every L3.
    pub cache_l1: Option<Bytes>,
    pub cache_l2: Option<Bytes>,
    pub cache_l3: Option<Bytes>,
    /// When the machine booted, milliseconds since the Unix epoch.
    pub boot_unix_ms: Option<i64>,
    /// Whether hardware virtualization is turned on in firmware (what Task
    /// Manager's CPU pane calls "Virtualization: Enabled").
    pub virtualization: Option<bool>,
    /// Whether a hypervisor is running (Hyper-V, or the virtualization-based
    /// security that uses it).
    pub hypervisor: Option<bool>,
    /// Memory in the machine as the firmware reports it. The difference from the
    /// operating system's total is the hardware-reserved part.
    pub installed_memory: Option<Bytes>,
    /// Configured speed of the memory, in MT/s, when the firmware reports it.
    pub memory_speed_mts: Option<u32>,
    /// Memory slots on the board, and how many hold a module.
    pub memory_slots: Option<u32>,
    pub memory_slots_used: Option<u32>,
    /// `DIMM`, `SODIMM`, `Row of chips`, when all modules agree.
    pub memory_form_factor: Option<String>,
}
