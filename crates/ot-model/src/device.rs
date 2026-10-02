//! Disks and network adapters.
//!
//! Each sample points at an `Arc` of facts that do not change while the device is
//! attached, the same split processes use, so a pass copies pointers, not strings.

use std::sync::Arc;

use crate::units::{Bytes, Percent};

/// A physical disk's facts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskInfo {
    /// The disk's number (`N` in `\\.\PhysicalDriveN` on Windows). Stable while the
    /// disk is attached; the key its history is kept under.
    pub number: u32,
    /// For display: `Disk 0 (C: D:)`.
    pub name: String,
    /// The drive's model, as it reports it.
    pub model: Option<String>,
    /// True for solid state (no seek penalty), false for spinning media. `None`
    /// when the drive would not say.
    pub ssd: Option<bool>,
    pub capacity: Option<Bytes>,
    /// USB sticks, card readers, some external drives.
    pub removable: bool,
    /// The bus it is on: `NVMe`, `SATA`, `USB`, `SD`.
    pub bus: Option<String>,
}

/// A disk over one sampling interval.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskSample {
    pub info: Arc<DiskInfo>,
    /// Share of the interval the disk had requests outstanding, `0..=100`. What
    /// Task Manager calls active time; 100% means saturated, not "full".
    pub active: Percent,
    pub read_per_sec: Bytes,
    pub write_per_sec: Bytes,
    /// Average time to complete one transfer, in milliseconds.
    pub response_ms: Option<f32>,
}

/// What kind of link an adapter is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LinkKind {
    Ethernet,
    WiFi,
    Cellular,
    /// A tunnel or other software link, such as a VPN.
    Virtual,
    #[default]
    Other,
}

/// A network adapter's facts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterInfo {
    /// Stable while the adapter exists (the interface LUID on Windows); the key its
    /// history is kept under.
    pub id: u64,
    /// The connection's name: `Wi-Fi`, `Ethernet 2`, `vEthernet (Default Switch)`.
    pub name: String,
    /// The adapter: `Intel(R) Wi-Fi 6 AX201 160MHz`.
    pub adapter: String,
    pub kind: LinkKind,
    /// A physical adapter, as opposed to a virtual switch port or a VPN.
    pub hardware: bool,
    /// The adapter's addresses, IPv4 first, in the order the system lists them.
    pub addresses: Vec<std::net::IpAddr>,
    /// The connection-specific DNS suffix, when there is one.
    pub dns_suffix: Option<String>,
    /// The hardware address, formatted `00-11-22-33-44-55`.
    pub mac: Option<String>,
}

/// A mounted volume (a drive letter, or a mount point), for the disk panes and
/// TMOG's Disk Space view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSample {
    /// `C:`, or a mount path.
    pub mount: String,
    /// The volume's label, when it has one.
    pub label: Option<String>,
    /// `NTFS`, `ReFS`, `exFAT`.
    pub filesystem: Option<String>,
    pub total: Bytes,
    pub free: Bytes,
    /// The physical disk it lives on, when known.
    pub disk: Option<u32>,
    /// Holds the running operating system.
    pub system: bool,
    /// Holds a page file.
    pub page_file: bool,
}

/// A network adapter over one sampling interval.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterSample {
    pub info: Arc<AdapterInfo>,
    pub rx_per_sec: Bytes,
    pub tx_per_sec: Bytes,
    /// Negotiated link speed in bits per second, the faster direction.
    pub link_bps: Option<u64>,
}
