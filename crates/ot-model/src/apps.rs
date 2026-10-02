//! Installed programs, as the system's uninstall list has them.

use crate::units::Bytes;

/// One installed program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
    /// `YYYY-MM-DD`, when recorded.
    pub installed_on: Option<String>,
    /// What the installer said it takes on disk.
    pub size: Option<Bytes>,
    /// Where it is installed, when recorded.
    pub location: Option<String>,
    /// The command that removes it, when there is one.
    pub uninstall: Option<String>,
    /// Installed for this user only, rather than for the machine.
    pub per_user: bool,
    /// A 32-bit program on a 64-bit system.
    pub x86: bool,
}
