//! Symbolic icons.
//!
//! Views name an icon by meaning; each backend maps it to the platform's own icon
//! set (Segoe Fluent Icons on Windows 11, Segoe MDL2 Assets on Windows 10, SF
//! Symbols or a freedesktop theme elsewhere). That keeps icons crisp at every DPI,
//! matching the system's look, and keeps codepoints out of view code.

/// An icon, by meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Icon {
    /// Three horizontal lines: show or hide navigation labels.
    Menu,
    /// A dashboard of tiles: the Summary page.
    Summary,
    /// A list of items: the process table.
    Processes,
    /// A pulse line: live performance graphs.
    Performance,
    /// A gear: the app's settings.
    Settings,
    /// Two arrows in a circle: updates, as Windows draws Windows Update.
    Update,
    /// Two people: the Users page.
    Users,
    /// A wrench or gears: the Services page.
    Services,
    /// A power symbol: the Startup page.
    Startup,
    /// A globe or network: the Connections page.
    Connections,
    /// A box: the Installed apps page.
    Apps,
    /// A monitor: the System page.
    System,
    /// The replay transport's buttons.
    Play,
    Pause,
    Previous,
    Next,
    // The process menu's entries.
    /// A branching tree: the process tree.
    Tree,
    /// A cross: end a process.
    EndTask,
    /// A cross in a circle: end a process and all under it.
    EndTree,
    /// A circular arrow: start again.
    Restart,
    /// A leaf: efficiency mode, as Task Manager draws it.
    Efficiency,
    /// A gauge: scheduling priority.
    Priority,
    /// A chip: which processors to run on.
    Affinity,
    /// An arrow out of a box: bring a window forward.
    SwitchTo,
    /// An open folder.
    OpenFolder,
    /// A globe under a magnifier: search the web.
    SearchOnline,
    /// An i in a circle: the file's properties.
    Properties,
    /// Two sheets: copy.
    Copy,
    /// A disk: save a dump file.
    Dump,
    /// A chain link: what a process waits on.
    WaitChain,
    /// A stopwatch: sample for a while.
    Stopwatch,
}

impl Icon {
    /// The icon numbered `i` (its `repr(u32)` value), if there is one.
    #[must_use]
    pub fn from_index(i: u32) -> Option<Self> {
        Self::ALL.get(i as usize).copied()
    }

    /// Every icon, for backends that want to check their mapping is complete.
    pub const ALL: [Self; 31] = [
        Self::Menu,
        Self::Summary,
        Self::Processes,
        Self::Performance,
        Self::Settings,
        Self::Update,
        Self::Users,
        Self::Services,
        Self::Startup,
        Self::Connections,
        Self::Apps,
        Self::System,
        Self::Play,
        Self::Pause,
        Self::Previous,
        Self::Next,
        Self::Tree,
        Self::EndTask,
        Self::EndTree,
        Self::Restart,
        Self::Efficiency,
        Self::Priority,
        Self::Affinity,
        Self::SwitchTo,
        Self::OpenFolder,
        Self::SearchOnline,
        Self::Properties,
        Self::Copy,
        Self::Dump,
        Self::WaitChain,
        Self::Stopwatch,
    ];
}
