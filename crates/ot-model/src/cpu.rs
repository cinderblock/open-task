//! CPU topology and utilization.

use crate::units::{Hertz, Percent, Watts};

/// What kind of core this is, on a hybrid (big.LITTLE / P+E) processor.
///
/// Attributing load to the wrong core class is the single most misleading thing a
/// task manager can do on a modern hybrid CPU: 100% on an E-core and 100% on a
/// P-core mean very different things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CoreKind {
    /// Performance core.
    Performance,
    /// Efficiency core.
    Efficiency,
    /// Homogeneous processor, or class could not be determined.
    #[default]
    Unknown,
}

/// One logical processor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalCore {
    /// Index as the OS numbers it.
    pub index: u32,
    /// Which physical core this logical processor belongs to. SMT siblings share one.
    pub physical: u32,
    /// Performance vs efficiency class.
    pub kind: CoreKind,
    /// Utilization over the last sampling interval.
    pub usage: Percent,
    /// Current clock, if the platform can report it per-core.
    pub frequency: Option<Hertz>,
}

/// Whole-package CPU state for one sample.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuSample {
    /// Aggregate utilization across all logical processors, 0..=100.
    pub total: Percent,
    /// Per-logical-core detail, ordered by `LogicalCore::index`.
    pub cores: Vec<LogicalCore>,
    /// Package power draw, where the platform exposes it (RAPL, SMC, etc.).
    pub package_power: Option<Watts>,
    /// Hottest on-die sensor reading in degrees Celsius.
    pub hotspot_celsius: Option<f32>,
}
