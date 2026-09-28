//! Symbolic icons.
//!
//! Views name an icon by meaning; each backend maps it to the platform's own icon
//! set (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10, SF
//! Symbols or a freedesktop theme elsewhere). That keeps icons crisp at every DPI,
//! matching the system's look, and keeps codepoints out of view code.

/// An icon, by meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Three horizontal lines: show or hide navigation labels.
    Menu,
    /// A list of items: the process table.
    Processes,
    /// A pulse line: live performance graphs.
    Performance,
    /// A gear: the app's settings.
    Settings,
}

impl Icon {
    /// Every icon, for backends that want to check their mapping is complete.
    pub const ALL: [Self; 4] = [
        Self::Menu,
        Self::Processes,
        Self::Performance,
        Self::Settings,
    ];
}
