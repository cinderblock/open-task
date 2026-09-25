//! Newtypes for physical units.
//!
//! A task manager mixes bytes, percentages, hertz and watts constantly, and mixing
//! them up silently is the most common class of bug in this kind of tool. These cost
//! nothing at runtime and make the errors compile failures instead.

/// A quantity of bytes. Always exact; never pre-scaled to KB/MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bytes(pub u64);

impl Bytes {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_kib(kib: u64) -> Self {
        Self(kib * 1024)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Saturating difference, for deltas between samples where a counter may reset.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

/// A fraction expressed in percent. May exceed 100 for multi-core CPU totals.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Percent(pub f32);

impl Percent {
    pub const ZERO: Self = Self(0.0);

    #[must_use]
    pub fn from_ratio(ratio: f32) -> Self {
        Self(ratio * 100.0)
    }

    #[must_use]
    pub fn get(self) -> f32 {
        self.0
    }

    /// Clamp into `0..=max`, guarding against sampling jitter producing >100% on a
    /// single core or a negative value from a counter that went backwards.
    #[must_use]
    pub fn clamped(self, max: f32) -> Self {
        Self(self.0.clamp(0.0, max))
    }
}

/// A frequency in hertz. Stored in Hz to avoid rounding at the MHz boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Hertz(pub u64);

impl Hertz {
    #[must_use]
    pub const fn from_mhz(mhz: u64) -> Self {
        Self(mhz * 1_000_000)
    }

    #[must_use]
    pub fn as_mhz(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

/// Instantaneous power draw in watts.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Watts(pub f32);
