//! Text styling.
//!
//! Styles are small `Copy` values so a display list can carry one per text run
//! without indirection, and they hash so backends can key layout caches on them.

use std::hash::{Hash, Hasher};

/// Logical font family. Each backend maps these to the platform UI fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontFamily {
    /// The system UI font: Segoe UI Variable on Windows 11, SF Pro on macOS, the
    /// desktop's interface font on Linux.
    #[default]
    Ui,
    /// A monospace font for paths, command lines and hex.
    Mono,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWeight {
    #[default]
    Regular,
    SemiBold,
    Bold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// How a run of text should be shaped.
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub family: FontFamily,
    /// Size in DIPs.
    pub size: f32,
    pub weight: FontWeight,
    /// Use tabular (fixed-width) figures so columns of numbers line up. Any table
    /// of live numbers wants this on; prose wants it off.
    pub tabular_numbers: bool,
}

impl TextStyle {
    #[must_use]
    pub const fn ui(size: f32) -> Self {
        Self {
            family: FontFamily::Ui,
            size,
            weight: FontWeight::Regular,
            tabular_numbers: false,
        }
    }

    #[must_use]
    pub const fn mono(size: f32) -> Self {
        Self {
            family: FontFamily::Mono,
            size,
            weight: FontWeight::Regular,
            tabular_numbers: true,
        }
    }

    #[must_use]
    pub const fn weight(self, weight: FontWeight) -> Self {
        Self { weight, ..self }
    }

    #[must_use]
    pub const fn tabular(self) -> Self {
        Self {
            tabular_numbers: true,
            ..self
        }
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::ui(13.0)
    }
}

// `size` is an f32, so derive(Eq, Hash) is unavailable. Compare and hash the bit
// pattern instead; two styles with the same bits are the same style for caching.
impl PartialEq for TextStyle {
    fn eq(&self, other: &Self) -> bool {
        self.family == other.family
            && self.size.to_bits() == other.size.to_bits()
            && self.weight == other.weight
            && self.tabular_numbers == other.tabular_numbers
    }
}

impl Eq for TextStyle {}

impl Hash for TextStyle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.family.hash(state);
        self.size.to_bits().hash(state);
        self.weight.hash(state);
        self.tabular_numbers.hash(state);
    }
}
