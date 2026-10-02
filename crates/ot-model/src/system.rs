//! Facts about the machine and its operating system, for the System page.
//!
//! Read on demand, not sampled: none of it changes while the machine runs, except
//! the uptime, which the snapshot's boot time already gives.

use crate::units::Bytes;

/// One memory module, as the firmware describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryDevice {
    /// The slot's label on the board: `DIMM A1`, `ChannelA-DIMM0`.
    pub slot: String,
    /// `None` for an empty slot.
    pub size: Option<Bytes>,
    /// Configured speed in MT/s (what everybody calls MHz).
    pub speed_mts: Option<u32>,
    /// `DIMM`, `SODIMM`, `Row of chips`.
    pub form_factor: Option<String>,
    /// `DDR4`, `DDR5`, `LPDDR5`.
    pub kind: Option<String>,
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
}

/// The machine and its operating system.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemFacts {
    pub computer_name: Option<String>,
    /// `Windows 11 Pro`.
    pub os_name: Option<String>,
    /// `24H2`.
    pub os_version: Option<String>,
    /// `26100.1234`.
    pub os_build: Option<String>,
    /// When the operating system was installed, milliseconds since the Unix epoch.
    pub os_installed_unix_ms: Option<i64>,
    pub system_manufacturer: Option<String>,
    pub system_model: Option<String>,
    pub board_manufacturer: Option<String>,
    pub board_product: Option<String>,
    pub bios_vendor: Option<String>,
    pub bios_version: Option<String>,
    pub bios_date: Option<String>,
    /// Whether the firmware is UEFI (as opposed to legacy BIOS).
    pub uefi: Option<bool>,
    pub secure_boot: Option<bool>,
    /// Every memory slot, populated or not.
    pub memory_devices: Vec<MemoryDevice>,
    /// Memory in the machine, as the firmware reports it; more than the operating
    /// system can use by the hardware-reserved part.
    pub installed_memory: Option<Bytes>,
    /// The system's page files, as `path (size)` strings.
    pub page_files: Vec<String>,
}
