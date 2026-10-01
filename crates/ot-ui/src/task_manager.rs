//! Replacing Task Manager: whether the platform opens open-task where it would open
//! its own task manager (on Windows: Ctrl+Shift+Esc, the taskbar, Ctrl+Alt+Del), as
//! Process Explorer's "Replace Task Manager" does.
//!
//! This is machine state, not a user setting: the shell reads it from the system and
//! hands it over with [`crate::App::set_task_manager`], and the Settings page shows
//! it as a card with a switch. The switch is on only when the system opens *this*
//! copy. Flipping it asks the shell to change it ([`crate::Effect::ReplaceTaskManager`]);
//! what the system then does comes back the same way it came in.

use std::fmt::Write as _;

use ot_update::Scope;

/// What the system opens in its task manager's place.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Replacement {
    /// The platform has no such thing, or it could not be read: no card.
    #[default]
    Unavailable,
    /// Its own task manager, as usual.
    Off,
    /// This copy of open-task.
    ThisCopy,
    /// Another program: Process Explorer, or another copy of open-task. `exists`
    /// is false when the file is missing, in which case nothing opens at all.
    Other { path: String, exists: bool },
}

/// The Task Manager card's state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskManager {
    pub replacement: Replacement,
    /// Who the installer put this copy in place for; `None` for any other copy
    /// (a zip, a build). It decides what the card warns about.
    pub install: Option<Scope>,
    /// A change is waiting for permission (the UAC prompt) or being made.
    pub pending: bool,
}

impl TaskManager {
    /// Whether there is anything to show.
    #[must_use]
    pub fn available(&self) -> bool {
        self.replacement != Replacement::Unavailable
    }

    /// Whether the switch shows on: the system opens this copy.
    #[must_use]
    pub fn on(&self) -> bool {
        self.replacement == Replacement::ThisCopy
    }

    pub(crate) fn title() -> &'static str {
        "Replace Task Manager"
    }

    pub(crate) fn detail() -> &'static str {
        "Ctrl+Shift+Esc, the taskbar and Ctrl+Alt+Del start open-task instead of Task Manager."
    }

    /// A line of its own under the detail, if there is something to say: first a
    /// change in progress, then another program in the way, then what this copy's
    /// location means for standing in. Written to `out`; returns whether it did.
    pub(crate) fn note(&self, out: &mut String) -> bool {
        out.clear();
        if self.pending {
            out.push_str("Waiting for permission from Windows\u{2026}");
            return true;
        }
        // The path goes last, so a long one is what the ellipsis cuts.
        let _ = match (&self.replacement, self.install) {
            (Replacement::Other { path, exists: true }, _) => {
                write!(out, "Windows opens another program instead now: {path}")
            }
            (
                Replacement::Other {
                    path,
                    exists: false,
                },
                _,
            ) => write!(
                out,
                "Ctrl+Shift+Esc does nothing: Windows is set to open a missing file, {path}"
            ),
            (_, Some(Scope::User)) => write!(
                out,
                "Installed for you only, so other accounts on this PC cannot open it this way."
            ),
            (_, None) => write!(
                out,
                "This copy is not installed, so moving or deleting it would break Ctrl+Shift+Esc."
            ),
            (_, Some(Scope::Machine)) => return false,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(tm: &TaskManager) -> Option<String> {
        let mut out = String::new();
        tm.note(&mut out).then_some(out)
    }

    fn installed(replacement: Replacement) -> TaskManager {
        TaskManager {
            replacement,
            install: Some(Scope::Machine),
            pending: false,
        }
    }

    #[test]
    fn on_only_when_this_copy_opens() {
        assert!(!TaskManager::default().available());
        assert!(installed(Replacement::Off).available());
        assert!(!installed(Replacement::Off).on());
        assert!(installed(Replacement::ThisCopy).on());
        let other = installed(Replacement::Other {
            path: r"C:\Tools\procexp64.exe".into(),
            exists: true,
        });
        assert!(!other.on(), "someone else's replacement is not ours");
    }

    #[test]
    fn an_install_for_all_users_needs_no_note() {
        assert_eq!(note(&installed(Replacement::Off)), None);
        assert_eq!(note(&installed(Replacement::ThisCopy)), None);
    }

    #[test]
    fn the_note_names_whatever_is_in_the_way() {
        let other = installed(Replacement::Other {
            path: r"C:\Tools\procexp64.exe".into(),
            exists: true,
        });
        assert_eq!(
            note(&other).as_deref(),
            Some(r"Windows opens another program instead now: C:\Tools\procexp64.exe")
        );
        let missing = installed(Replacement::Other {
            path: r"C:\gone\open-task.exe".into(),
            exists: false,
        });
        assert_eq!(
            note(&missing).as_deref(),
            Some(
                r"Ctrl+Shift+Esc does nothing: Windows is set to open a missing file, C:\gone\open-task.exe"
            )
        );
    }

    #[test]
    fn a_copy_not_installed_for_everyone_says_what_that_means() {
        let mut tm = installed(Replacement::ThisCopy);
        tm.install = Some(Scope::User);
        assert!(note(&tm).unwrap().starts_with("Installed for you only"));
        tm.install = None;
        assert!(note(&tm).unwrap().starts_with("This copy is not installed"));
        // Said before turning it on, too.
        tm.replacement = Replacement::Off;
        assert!(note(&tm).unwrap().starts_with("This copy is not installed"));
    }

    #[test]
    fn waiting_for_permission_comes_first() {
        let mut tm = installed(Replacement::Other {
            path: r"C:\Tools\procexp64.exe".into(),
            exists: true,
        });
        tm.install = None;
        tm.pending = true;
        assert_eq!(
            note(&tm).as_deref(),
            Some("Waiting for permission from Windows\u{2026}")
        );
    }
}
