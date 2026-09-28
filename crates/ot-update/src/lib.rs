//! Self-updater for open-task.
//!
//! Releases are GitHub releases of tags `vX.Y.Z`. Each carries `SHA256SUMS`, signed
//! with minisign by the release workflow; the updater trusts only what that
//! signature covers, with the public key compiled in ([`feed`]). It compares the
//! release with what the running binary is ([`version`]), downloads the Windows
//! installer, checks it against the signed sums, and runs it silently
//! ([`Updater`]). Setup closes the app, replaces it, and starts the new version.
//!
//! Only a copy that the installer put in place updates itself
//! ([`installation`]). Any other copy (a zip, `cargo install`, a development build)
//! is told that a release exists, and nothing more.
//!
//! Checking and installing are separate: by default the app checks and says so,
//! downloads only when asked (or when the user turned on automatic downloads), and
//! installs only on a click, since that closes the app and, for an install in
//! Program Files, asks for elevation.
//!
//! Platform code (HTTP, the registry) lives in `imp`; everything else is portable
//! and tested on every platform.

#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod feed;
mod imp;
mod updater;
pub mod version;

pub use feed::{Feed, FeedError, Release};
pub use updater::{installer_args, latest, Config, Stage, Status, Updater};
pub use version::{Build, Version};

/// Why an update step failed. The messages are for people.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Feed(#[from] FeedError),
    #[error("the latest release is not signed, or there is none")]
    Unsigned,
    #[error("the release's signature and checksums disagree about its version")]
    Mismatch,
    #[error("{url} answered HTTP {status}")]
    Http { url: String, status: u32 },
    #[error("{url} is larger than the {limit} bytes expected")]
    TooLarge { url: String, limit: u64 },
    #[error("{0}")]
    Network(String),
    #[error("could not {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("the release has no Windows installer")]
    NoInstaller,
    #[error("the download does not match the release's signed checksum")]
    Checksum,
    #[error("updating is not supported on this platform yet")]
    Unsupported,
}

impl UpdateError {
    fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { what, source }
    }
}

/// What the updater needs from the operating system: [`native`] in the app, a
/// stand-in in tests.
pub trait Platform: Send + Sync + 'static {
    /// GET `url`, following redirects, handing the body to `sink` as it arrives.
    /// More than `limit` bytes is an error. `progress` gets the bytes received so
    /// far and the total when the server said.
    ///
    /// # Errors
    /// [`UpdateError`] for a network failure, a status other than 200, or a
    /// response over `limit`.
    fn fetch(
        &self,
        url: &str,
        limit: u64,
        sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), UpdateError>;

    /// Run the installer and wait for it. Returns its exit code. When the install
    /// goes ahead, Setup closes this process first, so this never returns.
    ///
    /// # Errors
    /// [`UpdateError`] if it cannot be started.
    fn run_installer(&self, path: &Path, args: &[&str]) -> Result<i32, UpdateError>;
}

/// The platform's own implementation.
#[must_use]
pub fn native(user_agent: &str) -> Arc<dyn Platform> {
    imp::native(user_agent)
}

/// Where the installer put open-task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub scope: Scope,
    pub dir: PathBuf,
}

/// Who an installation is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Program Files, for every user; updating needs elevation.
    Machine,
    /// The user's profile (`/CURRENTUSER`).
    User,
}

/// The installation the running binary belongs to: the installer's uninstall entry
/// names the folder it is running from. `None` for any other copy.
#[must_use]
pub fn installation() -> Option<Installation> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    imp::installations()
        .into_iter()
        .find(|i| same_dir(&i.dir, dir))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase(),
        _ => false,
    }
}

/// Where downloads go: `%LOCALAPPDATA%\open-task\updates` on Windows.
#[must_use]
pub fn default_download_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("open-task")
        .join("updates")
}
