//! Programs that start when a user signs in.
use serde::{Deserialize, Serialize};

/// Where a startup entry is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StartupLocation {
    /// The user's own `Run` key.
    UserRun,
    /// The machine's `Run` key, for every user.
    MachineRun,
    /// The machine's 32-bit `Run` key on a 64-bit system.
    MachineRun32,
    /// The user's Startup folder.
    UserFolder,
    /// The Startup folder every user gets.
    CommonFolder,
}

impl StartupLocation {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UserRun => "Registry (user)",
            Self::MachineRun => "Registry (machine)",
            Self::MachineRun32 => "Registry (machine, 32-bit)",
            Self::UserFolder => "Startup folder (user)",
            Self::CommonFolder => "Startup folder (all users)",
        }
    }

    /// Whether changing the entry needs rights over the whole machine.
    #[must_use]
    pub const fn machine_wide(self) -> bool {
        matches!(
            self,
            Self::MachineRun | Self::MachineRun32 | Self::CommonFolder
        )
    }
}

/// One program that starts at sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupEntry {
    /// The entry's own name: the registry value's name, or the shortcut's file
    /// name without its extension.
    pub name: String,
    pub location: StartupLocation,
    /// What runs: the command line, or the shortcut's target.
    pub command: String,
    /// Whether the system will run it. Disabled entries stay listed, as in Task
    /// Manager, so they can be turned back on.
    pub enabled: bool,
    /// The image's company, from its version resource, when the command names a
    /// readable file.
    pub publisher: Option<String>,
    /// Where the entry's image lives, for "Open file location".
    pub image_path: Option<String>,
}
