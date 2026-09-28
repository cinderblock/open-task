//! The update button: which build this is, what the updater is doing, and what a
//! click does. It sits on the rail above Settings and, with room for the details,
//! as a card on the Settings page.
//!
//! A click checks for a release and, if this copy can install it, downloads it;
//! once it is downloaded and verified, a click installs it. A copy the installer
//! did not put in place is only told about a release, and a click opens its page.

use std::fmt::Write as _;

use ot_model::Bytes;
use ot_update::{Build, Stage, Status};

use crate::format;

/// What a click on the update button asks the shell to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Look for a release, and download it if this copy can install it.
    Check,
    /// Download the release a scheduled check found.
    Download,
    /// Run the downloaded installer; it closes the app.
    Install,
    /// Open the release's page: this copy cannot install it.
    OpenReleasePage,
}

/// The running build and the updater's state, as the view shows them.
#[derive(Debug, Clone)]
pub struct UpdateView {
    /// The whole version, e.g. `0.2.1-20-gdbfe022-dirty`.
    version: String,
    /// What fits on the rail: `v0.2.1` for a release, otherwise the commit and
    /// whether it was dirty, `dbfe022-dirty`.
    short: String,
    status: Status,
    can_install: bool,
}

impl Default for UpdateView {
    fn default() -> Self {
        Self::new("", false)
    }
}

impl UpdateView {
    /// `version` as `build.rs` made it; `can_install` if the installer put this
    /// copy in place.
    #[must_use]
    pub fn new(version: &str, can_install: bool) -> Self {
        let short = match Build::parse(version) {
            Ok(b) => match &b.hash {
                Some(hash) if b.dirty => format!("{hash}-dirty"),
                Some(hash) => hash.clone(),
                None => format!("v{}", b.base),
            },
            Err(_) => format!("v{version}"),
        };
        Self {
            version: version.to_owned(),
            short,
            status: Status::Idle,
            can_install,
        }
    }

    /// Take the updater's latest status. Returns whether anything shown changed.
    pub fn set_status(&mut self, status: Status) -> bool {
        std::mem::replace(&mut self.status, status) != self.status
    }

    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    #[must_use]
    pub fn can_install(&self) -> bool {
        self.can_install
    }

    /// The short form, for the rail.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.short
    }

    /// What a click does now; `None` while something is under way.
    #[must_use]
    pub fn action(&self) -> Option<UpdateAction> {
        match self.status {
            Status::Idle | Status::UpToDate { .. } | Status::Failed { .. } => {
                Some(UpdateAction::Check)
            }
            Status::Available { .. } if self.can_install => Some(UpdateAction::Download),
            Status::Available { .. } => Some(UpdateAction::OpenReleasePage),
            Status::Ready { .. } => Some(UpdateAction::Install),
            Status::Checking | Status::Downloading { .. } | Status::Installing { .. } => None,
        }
    }

    /// Whether the button should catch the eye: a newer release is known and
    /// waiting for a click, or the last attempt failed.
    #[must_use]
    pub fn attention(&self) -> bool {
        matches!(
            self.status,
            Status::Available { .. } | Status::Ready { .. } | Status::Failed { .. }
        )
    }

    /// Whether the updater is working.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.status.busy()
    }

    /// The rail's second line.
    pub fn rail_status(&self, out: &mut String) {
        out.clear();
        let _ = match &self.status {
            Status::Idle => write!(out, "Check for updates"),
            Status::Checking => write!(out, "Checking\u{2026}"),
            Status::UpToDate { .. } => write!(out, "Up to date"),
            Status::Available { version } => write!(out, "v{version} available"),
            Status::Downloading {
                received,
                total: Some(total),
                ..
            } => write!(out, "Downloading {}%", percent(*received, *total)),
            Status::Downloading { .. } => write!(out, "Downloading\u{2026}"),
            Status::Ready { version } => write!(out, "Install v{version}"),
            Status::Installing { .. } => write!(out, "Installing\u{2026}"),
            Status::Failed { stage, .. } => write!(
                out,
                "{} failed",
                match stage {
                    Stage::Check => "Check",
                    Stage::Download => "Download",
                    Stage::Install => "Install",
                }
            ),
        };
    }

    /// The Settings card's title.
    pub fn title(&self, out: &mut String) {
        out.clear();
        let _ = write!(out, "open-task v{}", self.version);
    }

    /// The Settings card's line under the title.
    pub fn detail(&self, out: &mut String) {
        out.clear();
        let _ = match &self.status {
            Status::Idle => write!(out, "Looks on GitHub for a newer release."),
            Status::Checking => write!(out, "Looking on GitHub for a newer release\u{2026}"),
            Status::UpToDate { latest } => {
                write!(out, "Up to date. The latest release is v{latest}.")
            }
            Status::Available { version } if self.can_install => {
                write!(out, "v{version} is available.")
            }
            Status::Available { version } => {
                write!(out, "v{version} is available on its release page.")
            }
            Status::Downloading {
                version,
                received,
                total,
            } => {
                // `format::bytes` starts its buffer over, so each number gets its own.
                let mut n = String::new();
                format::bytes(&mut n, Bytes(*received));
                let _ = write!(out, "Downloading v{version}: {n}");
                if let Some(total) = total {
                    format::bytes(&mut n, Bytes(*total));
                    let _ = write!(out, " of {n}");
                }
                Ok(())
            }
            Status::Ready { version } => write!(
                out,
                "v{version} is downloaded and verified. Installing closes open-task and \
                 starts the new version."
            ),
            Status::Installing { version } => write!(
                out,
                "Installing v{version}. open-task will close and start again."
            ),
            Status::Failed { stage, message } => write!(
                out,
                "Could not {}: {message}",
                match stage {
                    Stage::Check => "check for updates",
                    Stage::Download => "download the update",
                    Stage::Install => "install the update",
                }
            ),
        };
    }

    /// A line of its own on the Settings card, when this copy cannot update itself.
    #[must_use]
    pub fn note(&self) -> Option<&'static str> {
        (!self.can_install).then_some(
            "This copy was not installed by open-task's installer, so it cannot update itself.",
        )
    }

    /// The Settings card's button, and whether it does anything now.
    pub fn button(&self, out: &mut String) -> bool {
        out.clear();
        let _ = match (&self.status, self.action()) {
            (Status::Checking, _) => write!(out, "Checking\u{2026}"),
            (
                Status::Downloading {
                    received,
                    total: Some(total),
                    ..
                },
                _,
            ) => write!(out, "{}%", percent(*received, *total)),
            (Status::Downloading { .. }, _) => write!(out, "Downloading"),
            (Status::Installing { .. }, _) => write!(out, "Installing"),
            (Status::Failed { .. }, _) => write!(out, "Try again"),
            (_, Some(UpdateAction::Download)) => write!(out, "Download"),
            (_, Some(UpdateAction::Install)) => write!(out, "Install"),
            (_, Some(UpdateAction::OpenReleasePage)) => write!(out, "Open page"),
            (_, _) => write!(out, "Check now"),
        };
        self.action().is_some()
    }
}

fn percent(received: u64, total: u64) -> u64 {
    (received.saturating_mul(100) / total.max(1)).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_update::Version;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn texts(u: &UpdateView) -> (String, String, String, bool) {
        let (mut rail, mut detail, mut button) = (String::new(), String::new(), String::new());
        u.rail_status(&mut rail);
        u.detail(&mut detail);
        let enabled = u.button(&mut button);
        (rail, detail, button, enabled)
    }

    #[test]
    fn the_rail_shows_the_release_or_the_commit() {
        assert_eq!(UpdateView::new("0.2.1", true).short(), "v0.2.1");
        assert_eq!(
            UpdateView::new("0.2.1-20-gdbfe022", true).short(),
            "dbfe022"
        );
        assert_eq!(
            UpdateView::new("0.2.1-20-gdbfe022-dirty", true).short(),
            "dbfe022-dirty"
        );
        let u = UpdateView::new("0.2.1-20-gdbfe022-dirty", true);
        let mut title = String::new();
        u.title(&mut title);
        assert_eq!(title, "open-task v0.2.1-20-gdbfe022-dirty");
    }

    #[test]
    fn a_click_checks_then_downloads_then_installs() {
        let mut u = UpdateView::new("0.2.1", true);
        assert_eq!(u.action(), Some(UpdateAction::Check));
        assert_eq!(
            texts(&u),
            (
                "Check for updates".into(),
                "Looks on GitHub for a newer release.".into(),
                "Check now".into(),
                true
            )
        );
        assert!(u.set_status(Status::Checking));
        assert!(!u.set_status(Status::Checking), "no change");
        assert_eq!(u.action(), None);
        assert!(!texts(&u).3, "the button does nothing while checking");

        u.set_status(Status::Available {
            version: v("0.3.0"),
        });
        assert_eq!(u.action(), Some(UpdateAction::Download));
        assert!(u.attention());

        u.set_status(Status::Downloading {
            version: v("0.3.0"),
            received: 1_500_000,
            total: Some(3_000_000),
        });
        let (rail, detail, button, enabled) = texts(&u);
        assert_eq!(rail, "Downloading 50%");
        assert!(detail.starts_with("Downloading v0.3.0: "), "{detail}");
        assert_eq!((button.as_str(), enabled), ("50%", false));

        u.set_status(Status::Ready {
            version: v("0.3.0"),
        });
        assert_eq!(u.action(), Some(UpdateAction::Install));
        assert_eq!(texts(&u).0, "Install v0.3.0");
        assert_eq!(texts(&u).2, "Install");

        u.set_status(Status::Installing {
            version: v("0.3.0"),
        });
        assert_eq!(u.action(), None);

        u.set_status(Status::Failed {
            stage: Stage::Install,
            message: "Setup failed (exit code 4).".into(),
        });
        let (rail, detail, button, enabled) = texts(&u);
        assert_eq!(rail, "Install failed");
        assert_eq!(
            detail,
            "Could not install the update: Setup failed (exit code 4)."
        );
        assert_eq!((button.as_str(), enabled), ("Try again", true));
        assert_eq!(u.action(), Some(UpdateAction::Check));

        u.set_status(Status::UpToDate { latest: v("0.2.1") });
        assert_eq!(texts(&u).0, "Up to date");
        assert!(!u.attention());
    }

    #[test]
    fn a_copy_that_cannot_install_opens_the_release_page() {
        let mut u = UpdateView::new("0.2.1-20-gdbfe022", false);
        assert!(u.note().is_some());
        u.set_status(Status::Available {
            version: v("0.3.0"),
        });
        assert_eq!(u.action(), Some(UpdateAction::OpenReleasePage));
        assert_eq!(texts(&u).2, "Open page");
        assert!(UpdateView::new("0.2.1", true).note().is_none());
    }
}
