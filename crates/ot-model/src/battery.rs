//! The battery, where the machine has one.

use std::time::Duration;

use crate::units::Watts;

/// What the battery is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BatteryState {
    Charging,
    Discharging,
    /// On external power and full, or holding at a charge limit.
    Idle,
    #[default]
    Unknown,
}

impl BatteryState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Charging => "Charging",
            Self::Discharging => "Discharging",
            Self::Idle => "Not charging",
            Self::Unknown => "",
        }
    }
}

/// The battery over one sampling interval.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BatterySample {
    /// Charge as a share of what the battery can hold now, `0..=100`.
    pub charge: Option<f32>,
    pub state: BatteryState,
    /// Power into (charging) or out of (discharging) the battery, always positive.
    pub rate: Option<Watts>,
    /// Estimated time to empty while discharging, or to full while charging.
    pub time_left: Option<Duration>,
    /// Whether the machine is on external power.
    pub ac_power: Option<bool>,
    /// What the battery holds when full, in milliwatt-hours.
    pub full_capacity_mwh: Option<u32>,
    /// What it held when new, in milliwatt-hours. Health is the ratio of the two.
    pub design_capacity_mwh: Option<u32>,
    /// Charge cycles so far, where the battery counts them.
    pub cycle_count: Option<u32>,
    pub manufacturer: Option<String>,
    pub chemistry: Option<String>,
}
