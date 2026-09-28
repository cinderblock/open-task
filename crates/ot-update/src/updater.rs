//! The update state machine: check, download, install, one at a time, each on a
//! worker thread, with a callback after every change so a UI can follow along.
//!
//! ```text
//! Idle ─check─▶ Checking ─▶ UpToDate
//!                        ├─▶ Available ─download─▶ Downloading ─▶ Ready ─install─▶ Installing
//!                        └─▶ Downloading (a check asked to download)          │
//!                                                   Ready ◀── cancelled ───────┘
//! ```
//!
//! Any step can end in `Failed`, except that a check nobody asked for (the daily
//! one) fails quietly: it is logged, and the status goes back to what it was.
//!
//! Installing hands over to the Windows installer, which closes this process
//! (Restart Manager), replaces the files, and starts the new version. If Setup
//! exits while this process is still running, the install did not happen.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::feed::{self, Feed, Release};
use crate::version::{Build, Version};
use crate::{Installation, Platform, Scope, UpdateError};

/// Largest signature file accepted.
const SIGNATURE_LIMIT: u64 = 4 << 10;
/// Largest checksums file accepted.
const SUMS_LIMIT: u64 = 64 << 10;
/// Largest installer accepted. v0.2.1's was 2.7 MB.
const INSTALLER_LIMIT: u64 = 256 << 20;
/// Progress is reported at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// What the updater is doing, or last did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing checked yet.
    Idle,
    Checking,
    /// The latest release is not newer than this build.
    UpToDate {
        latest: Version,
    },
    /// A newer release exists and has not been downloaded: this copy cannot
    /// install it (see [`Updater::can_install`]), or nobody asked for it yet.
    Available {
        version: Version,
    },
    Downloading {
        version: Version,
        received: u64,
        total: Option<u64>,
    },
    /// Downloaded and verified. Installing closes the app.
    Ready {
        version: Version,
    },
    /// Setup is running and will close this app.
    Installing {
        version: Version,
    },
    Failed {
        stage: Stage,
        message: String,
    },
}

/// Which step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Check,
    Download,
    Install,
}

impl Status {
    /// Something is running; a new request is ignored.
    #[must_use]
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading { .. } | Self::Installing { .. }
        )
    }

    /// The newer release this status is about, if any.
    #[must_use]
    pub fn newer(&self) -> Option<&Version> {
        match self {
            Self::Available { version }
            | Self::Downloading { version, .. }
            | Self::Ready { version }
            | Self::Installing { version } => Some(version),
            _ => None,
        }
    }
}

/// What the updater works with.
#[derive(Debug, Clone)]
pub struct Config {
    /// The running build.
    pub build: Build,
    pub feed: Feed,
    /// Where installers are downloaded to. Old ones are removed from it.
    pub download_dir: PathBuf,
    /// The installer-managed install this binary runs from. Without one, updates
    /// are announced but never installed.
    pub installation: Option<Installation>,
}

/// The installer as downloaded and checked, held open so that nothing can replace
/// it before Setup runs (see [`hold`]).
#[derive(Debug)]
struct Verified {
    version: Version,
    path: PathBuf,
    _file: std::fs::File,
}

struct State {
    status: Status,
    /// A worker is running. Not the same as [`Status::busy`]: a scheduled check
    /// runs without showing "Checking".
    working: bool,
    /// The latest release seen, verified.
    release: Option<Release>,
    ready: Option<Verified>,
    last_check: Option<Instant>,
}

struct Shared {
    config: Config,
    platform: Arc<dyn Platform>,
    notify: Box<dyn Fn() + Send + Sync>,
    state: Mutex<State>,
}

/// Checks for, downloads and installs updates. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Updater {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Updater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updater")
            .field("build", &self.shared.config.build)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Updater {
    /// Start idle. `notify` runs, on a worker thread, after every status change.
    /// Installers left in the download directory by an earlier run are removed.
    pub fn new(
        config: Config,
        platform: Arc<dyn Platform>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        remove_downloads(&config.download_dir);
        Self {
            shared: Arc::new(Shared {
                config,
                platform,
                notify: Box::new(notify),
                state: Mutex::new(State {
                    status: Status::Idle,
                    working: false,
                    release: None,
                    ready: None,
                    last_check: None,
                }),
            }),
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    #[must_use]
    pub fn build(&self) -> &Build {
        &self.shared.config.build
    }

    /// Whether this copy can install updates: it runs from the installer's
    /// install.
    #[must_use]
    pub fn can_install(&self) -> bool {
        self.shared.config.installation.is_some()
    }

    /// The page of the newest release seen, for a copy that cannot install it.
    #[must_use]
    pub fn release_page(&self) -> Option<String> {
        let state = self.shared.lock();
        let version = &state.release.as_ref()?.version;
        Some(self.shared.config.feed.release_page(version))
    }

    /// Whether a scheduled check is due: never checked, or `every` has passed, and
    /// nothing is under way or waiting to be installed.
    #[must_use]
    pub fn due(&self, every: Duration) -> bool {
        let state = self.shared.lock();
        let waiting = state.working || matches!(state.status, Status::Ready { .. });
        !waiting && state.last_check.is_none_or(|t| t.elapsed() >= every)
    }

    /// Find the latest release. `manual` says a person asked, so progress and
    /// failure are shown; otherwise a failure is only logged. With `download`, a
    /// newer release is downloaded right away, if this copy can install it.
    pub fn check(&self, manual: bool, download: bool) {
        let previous = {
            let mut state = self.shared.lock();
            if state.working {
                return;
            }
            state.working = true;
            state.last_check = Some(Instant::now());
            let previous = state.status.clone();
            if manual {
                state.status = Status::Checking;
            }
            previous
        };
        if manual {
            self.shared.changed();
        }
        self.spawn("ot-update-check", move |shared| {
            let found = latest(shared.platform.as_ref(), &shared.config.feed);
            let download = {
                let mut state = shared.lock();
                match found {
                    Ok(release) => {
                        let version = release.version.clone();
                        let newer = shared.config.build.is_older_than(&version);
                        let fetch = newer
                            && download
                            && shared.config.installation.is_some()
                            && release.installer().is_some();
                        tracing::info!(latest = %version, newer, fetch, "update check");
                        state.status = if !newer {
                            Status::UpToDate { latest: version }
                        } else if fetch {
                            Status::Downloading {
                                version,
                                received: 0,
                                total: None,
                            }
                        } else {
                            Status::Available { version }
                        };
                        state.release = Some(release);
                        state.working = fetch;
                        fetch
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, manual, "update check failed");
                        state.status = if manual {
                            Status::Failed {
                                stage: Stage::Check,
                                message: e.to_string(),
                            }
                        } else {
                            previous
                        };
                        state.working = false;
                        false
                    }
                }
            };
            shared.changed();
            if download {
                shared.download();
            }
        });
    }

    /// Download the release found by the last check.
    pub fn download(&self) {
        {
            let mut state = self.shared.lock();
            let Some(version) = state.release.as_ref().map(|r| r.version.clone()) else {
                return;
            };
            if state.working || !self.shared.config.build.is_older_than(&version) {
                return;
            }
            state.working = true;
            state.status = Status::Downloading {
                version,
                received: 0,
                total: None,
            };
        }
        self.shared.changed();
        self.spawn("ot-update-download", Shared::download);
    }

    /// Run the downloaded installer. It closes this app if it goes ahead, and with
    /// `relaunch` starts the new version when it is done (a click); without, it
    /// leaves it closed (installing as the app closes).
    pub fn install(&self, relaunch: bool) {
        let (ready, installation) = {
            let mut state = self.shared.lock();
            let Some(installation) = self.shared.config.installation.clone() else {
                return;
            };
            if state.working || !matches!(state.status, Status::Ready { .. }) {
                return;
            }
            let Some(ready) = state.ready.take() else {
                return;
            };
            state.working = true;
            state.status = Status::Installing {
                version: ready.version.clone(),
            };
            (ready, installation)
        };
        self.shared.changed();
        self.spawn("ot-update-install", move |shared| {
            let args = installer_args(installation.scope, relaunch);
            tracing::info!(installer = %ready.path.display(), ?args, "starting setup");
            let outcome = shared.platform.run_installer(&ready.path, &args);
            let mut state = shared.lock();
            state.working = false;
            match outcome {
                // Cancelled, before or during: nothing changed; offer it again.
                Ok(code @ (2 | 5)) => {
                    tracing::info!(code, "setup was cancelled");
                    state.status = Status::Ready {
                        version: ready.version.clone(),
                    };
                    state.ready = Some(ready);
                }
                Ok(code) => {
                    tracing::warn!(code, "setup exited and this app is still running");
                    state.status = Status::Failed {
                        stage: Stage::Install,
                        message: setup_exit_message(code),
                    };
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not run setup");
                    state.status = Status::Failed {
                        stage: Stage::Install,
                        message: e.to_string(),
                    };
                }
            }
            drop(state);
            shared.changed();
        });
    }

    fn spawn(&self, name: &str, f: impl FnOnce(&Shared) + Send + 'static) {
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || f(&shared));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the update thread");
            let mut state = self.shared.lock();
            state.working = false;
            state.status = Status::Failed {
                stage: Stage::Check,
                message: format!("could not start a thread: {e}"),
            };
            drop(state);
            self.shared.changed();
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic on a worker leaves a status that is still meaningful.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn changed(&self) {
        (self.notify)();
    }

    /// Download and verify the installer of the release in the state, which is
    /// already `Downloading`.
    fn download(&self) {
        let release = self.lock().release.clone();
        let result = match &release {
            Some(r) => self.fetch_installer(r),
            None => Err(UpdateError::NoInstaller),
        };
        let mut state = self.lock();
        state.working = false;
        match result {
            Ok(verified) => {
                tracing::info!(path = %verified.path.display(), "update downloaded and verified");
                state.status = Status::Ready {
                    version: verified.version.clone(),
                };
                state.ready = Some(verified);
            }
            Err(e) => {
                tracing::warn!(error = %e, "update download failed");
                state.status = Status::Failed {
                    stage: Stage::Download,
                    message: e.to_string(),
                };
            }
        }
        drop(state);
        self.changed();
    }

    fn fetch_installer(&self, release: &Release) -> Result<Verified, UpdateError> {
        let asset = release.installer().ok_or(UpdateError::NoInstaller)?;
        let dir = &self.config.download_dir;
        std::fs::create_dir_all(dir).map_err(UpdateError::io("create the download folder"))?;
        remove_downloads(dir);
        let path = dir.join(&asset.name);
        let part = dir.join(format!("{}.part", asset.name));
        let url = self.config.feed.asset_url(&release.version, &asset.name);

        let mut file = exclusive(&part).map_err(UpdateError::io("create the download"))?;
        let mut hasher = Sha256::new();
        let mut last = Instant::now();
        let fetched = self.platform.fetch(
            &url,
            INSTALLER_LIMIT,
            &mut |chunk| {
                hasher.update(chunk);
                file.write_all(chunk)
            },
            &mut |received, total| {
                if last.elapsed() >= PROGRESS_EVERY || total == Some(received) {
                    last = Instant::now();
                    self.lock().status = Status::Downloading {
                        version: release.version.clone(),
                        received,
                        total,
                    };
                    self.changed();
                }
            },
        );
        let flushed = file.flush();
        drop(file);
        let written = fetched.and_then(|()| flushed.map_err(UpdateError::io("write the download")));
        let digest: [u8; 32] = hasher.finalize().into();
        if let Err(e) = written.and_then(|()| {
            if digest == asset.sha256 {
                Ok(())
            } else {
                Err(UpdateError::Checksum)
            }
        }) {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, &path).map_err(UpdateError::io("move the download into place"))?;
        // Written and hashed through one handle; now reopened so that nothing can
        // change it, and hashed again through the handle that is kept.
        let file = hold(&path, &asset.sha256)?;
        Ok(Verified {
            version: release.version.clone(),
            path,
            _file: file,
        })
    }
}

/// Fetch and verify the latest release. What [`Updater::check`] does, without the
/// state: for a command line.
///
/// # Errors
/// [`UpdateError`] if it cannot be fetched or does not verify.
pub fn latest(platform: &dyn Platform, feed: &Feed) -> Result<Release, UpdateError> {
    let sig_url = feed.latest_signature_url();
    let signature = match get(platform, &sig_url, SIGNATURE_LIMIT) {
        Err(UpdateError::Http { status: 404, .. }) => return Err(UpdateError::Unsigned),
        other => other?,
    };
    let signature = String::from_utf8(signature).map_err(|_| feed::FeedError::SignatureFile)?;
    let claimed = feed::claimed_version(&signature)?;
    let sums = get(platform, &feed.asset_url(&claimed, feed::SUMS), SUMS_LIMIT)?;
    let release = feed.verify(&signature, &sums)?;
    if release.version != claimed {
        return Err(UpdateError::Mismatch);
    }
    Ok(release)
}

fn get(platform: &dyn Platform, url: &str, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let mut body = Vec::new();
    platform.fetch(
        url,
        limit,
        &mut |chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        },
        &mut |_, _| {},
    )?;
    Ok(body)
}

/// Setup's command line for an update: no wizard (a progress window only), no
/// questions, no reboot; close the running app and do not let Restart Manager
/// restart it. With `relaunch`, the installer's own `[Run]` entry (enabled by
/// `/RELAUNCH=1`) starts the new version as the user who started Setup, not
/// elevated.
#[must_use]
pub fn installer_args(scope: Scope, relaunch: bool) -> Vec<&'static str> {
    let mut args = vec![
        "/SILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
        "/CLOSEAPPLICATIONS",
        "/NORESTARTAPPLICATIONS",
    ];
    if relaunch {
        args.push("/RELAUNCH=1");
    }
    args.push(match scope {
        Scope::Machine => "/ALLUSERS",
        Scope::User => "/CURRENTUSER",
    });
    args
}

/// Inno Setup's exit codes, in words.
fn setup_exit_message(code: i32) -> String {
    let what = match code {
        0 => "finished without closing open-task; restart it to use the new version",
        1 => "could not start",
        3 | 4 => "failed",
        7 | 8 => "could not proceed (another program may be in the way)",
        _ => "stopped",
    };
    format!("Setup {what} (exit code {code}).")
}

/// Create `path` for writing with no sharing at all.
fn exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
    options.open(path)
}

/// Open the downloaded installer read-only, letting others read it but not write,
/// delete or rename it, and check it once more through this handle. Kept open until
/// Setup is done with it, it stops anything swapping the file between this check
/// and the (elevated) run. Setup can still start: running an exe only reads it.
fn hold(path: &Path, sha256: &[u8; 32]) -> Result<std::fs::File, UpdateError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // FILE_SHARE_READ: others may read, nobody may write, delete or rename.
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 1);
    let mut file = options
        .open(path)
        .map_err(UpdateError::io("open the download"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .map_err(UpdateError::io("read the download"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if &digest != sha256 {
        return Err(UpdateError::Checksum);
    }
    Ok(file)
}

/// Remove installers from earlier runs. Only files this updater names, so a wrong
/// folder loses nothing else. One still held by another instance stays.
fn remove_downloads(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = name.starts_with("open-task-v")
            && (name.ends_with("-windows-setup.exe") || name.ends_with("-windows-setup.exe.part"));
        if ours {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::tests::{test_feed, FAKE_SETUP, SIG, SUMS_FILE};
    use std::collections::HashMap;
    use std::sync::mpsc;

    /// Serves canned responses by URL; records installer runs.
    struct Fake {
        files: HashMap<String, Vec<u8>>,
        setup_exit: i32,
        runs: Mutex<Vec<(PathBuf, Vec<String>)>>,
    }

    impl Fake {
        fn new() -> Self {
            let base = "https://example.test/releases";
            let files = [
                (
                    format!("{base}/latest/download/SHA256SUMS.minisig"),
                    SIG.as_bytes().to_vec(),
                ),
                (
                    format!("{base}/download/v0.3.0/SHA256SUMS"),
                    SUMS_FILE.to_vec(),
                ),
                (
                    format!("{base}/download/v0.3.0/open-task-v0.3.0-windows-setup.exe"),
                    FAKE_SETUP.to_vec(),
                ),
            ];
            Self {
                files: files.into_iter().collect(),
                setup_exit: 0,
                runs: Mutex::new(Vec::new()),
            }
        }
    }

    impl Platform for Fake {
        fn fetch(
            &self,
            url: &str,
            limit: u64,
            sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
            progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<(), UpdateError> {
            let body = self.files.get(url).ok_or_else(|| UpdateError::Http {
                url: url.to_owned(),
                status: 404,
            })?;
            if body.len() as u64 > limit {
                return Err(UpdateError::TooLarge {
                    url: url.to_owned(),
                    limit,
                });
            }
            let total = Some(body.len() as u64);
            let mut received = 0;
            for chunk in body.chunks(7) {
                sink(chunk).map_err(UpdateError::io("write"))?;
                received += chunk.len() as u64;
                progress(received, total);
            }
            Ok(())
        }

        fn run_installer(&self, path: &Path, args: &[&str]) -> Result<i32, UpdateError> {
            // Setup can read the file while the updater holds it.
            assert_eq!(std::fs::read(path).unwrap(), FAKE_SETUP);
            self.runs.lock().unwrap().push((
                path.to_owned(),
                args.iter().map(|a| (*a).to_owned()).collect(),
            ));
            Ok(self.setup_exit)
        }
    }

    struct Rig {
        updater: Updater,
        changes: mpsc::Receiver<()>,
        fake: Arc<Fake>,
        _dir: TempDir,
    }

    impl Rig {
        fn new(build: &str, installed: bool, fake: Fake) -> Self {
            let dir = TempDir::new();
            let fake = Arc::new(fake);
            let (tx, changes) = mpsc::channel();
            let tx = Mutex::new(tx);
            let config = Config {
                build: Build::parse(build).unwrap(),
                feed: test_feed(),
                download_dir: dir.0.join("updates"),
                installation: installed.then(|| Installation {
                    scope: Scope::Machine,
                    dir: dir.0.clone(),
                }),
            };
            let platform: Arc<dyn Platform> = Arc::<Fake>::clone(&fake);
            let updater = Updater::new(config, platform, move || {
                let _ = tx.lock().unwrap().send(());
            });
            Self {
                updater,
                changes,
                fake,
                _dir: dir,
            }
        }

        /// Wait until no worker runs, then return the status.
        fn settle(&self) -> Status {
            loop {
                let (s, working) = {
                    let state = self.updater.shared.lock();
                    (state.status.clone(), state.working)
                };
                if !working {
                    return s;
                }
                self.changes
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the updater reports progress");
            }
        }
    }

    /// A directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ot-update-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn v030() -> Version {
        Version::parse("0.3.0").unwrap()
    }

    #[test]
    fn a_current_build_is_up_to_date() {
        let rig = Rig::new("0.3.0", true, Fake::new());
        assert_eq!(rig.updater.status(), Status::Idle);
        rig.updater.check(true, true);
        assert_eq!(rig.settle(), Status::UpToDate { latest: v030() });
        // A build after 0.3.0 is not offered 0.3.0.
        let rig = Rig::new("0.3.0-2-gabcdef0-dirty", true, Fake::new());
        rig.updater.check(true, true);
        assert_eq!(rig.settle(), Status::UpToDate { latest: v030() });
    }

    #[test]
    fn a_click_checks_downloads_verifies_and_installs() {
        let rig = Rig::new("0.2.1-19-g892159c", true, Fake::new());
        rig.updater.check(true, true);
        assert_eq!(rig.settle(), Status::Ready { version: v030() });
        let path = rig
            .updater
            .shared
            .config
            .download_dir
            .join("open-task-v0.3.0-windows-setup.exe");
        assert_eq!(std::fs::read(&path).unwrap(), FAKE_SETUP);
        // Held: nobody can delete or replace it while it waits for Setup.
        #[cfg(windows)]
        assert!(std::fs::remove_file(&path).is_err());

        rig.updater.install(true);
        let after = rig.settle();
        let runs = rig.fake.runs.lock().unwrap().clone();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, path);
        assert_eq!(
            runs[0].1,
            [
                "/SILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/CLOSEAPPLICATIONS",
                "/NORESTARTAPPLICATIONS",
                "/RELAUNCH=1",
                "/ALLUSERS"
            ]
        );
        // The fake Setup exits 0 without closing the test: the app says so.
        assert!(matches!(
            after,
            Status::Failed { stage: Stage::Install, ref message } if message.contains("restart")
        ));
    }

    #[test]
    fn a_cancelled_install_can_be_tried_again() {
        let mut fake = Fake::new();
        fake.setup_exit = 2;
        let rig = Rig::new("0.2.1", true, fake);
        rig.updater.check(true, true);
        assert_eq!(rig.settle(), Status::Ready { version: v030() });
        rig.updater.install(true);
        assert_eq!(rig.settle(), Status::Ready { version: v030() });
        // Installing as the app closes: the same, without the relaunch.
        rig.updater.install(false);
        assert_eq!(rig.settle(), Status::Ready { version: v030() });
        let runs = rig.fake.runs.lock().unwrap().clone();
        assert_eq!(runs.len(), 2);
        assert!(runs[0].1.iter().any(|a| a == "/RELAUNCH=1"));
        assert!(!runs[1].1.iter().any(|a| a == "/RELAUNCH=1"));
    }

    #[test]
    fn a_copy_the_installer_did_not_put_there_only_hears_about_it() {
        let rig = Rig::new("0.2.1", false, Fake::new());
        assert!(!rig.updater.can_install());
        rig.updater.check(true, true);
        assert_eq!(rig.settle(), Status::Available { version: v030() });
        assert_eq!(
            rig.updater.release_page().as_deref(),
            Some("https://example.test/releases/tag/v0.3.0")
        );
        rig.updater.install(true);
        assert_eq!(rig.settle(), Status::Available { version: v030() });
        assert!(rig.fake.runs.lock().unwrap().is_empty());
    }

    #[test]
    fn a_background_check_notifies_and_downloads_only_when_asked() {
        let rig = Rig::new("0.2.1", true, Fake::new());
        assert!(rig.updater.due(Duration::from_secs(3600)));
        rig.updater.check(false, false);
        assert_eq!(rig.settle(), Status::Available { version: v030() });
        assert!(!rig.updater.due(Duration::from_secs(3600)), "just checked");
        rig.updater.download();
        assert_eq!(rig.settle(), Status::Ready { version: v030() });
        assert!(!rig.updater.due(Duration::ZERO), "waiting to be installed");
    }

    #[test]
    fn failures_show_after_a_click_and_stay_quiet_otherwise() {
        let mut fake = Fake::new();
        fake.files.clear();
        let rig = Rig::new("0.2.1", true, fake);
        rig.updater.check(false, false);
        assert_eq!(
            rig.settle(),
            Status::Idle,
            "a scheduled check fails quietly"
        );
        rig.updater.check(true, true);
        assert!(matches!(
            rig.settle(),
            Status::Failed { stage: Stage::Check, ref message } if message.contains("signed")
        ));
    }

    #[test]
    fn a_download_that_does_not_match_the_signed_sums_is_discarded() {
        let mut fake = Fake::new();
        let url =
            "https://example.test/releases/download/v0.3.0/open-task-v0.3.0-windows-setup.exe";
        fake.files
            .insert(url.to_owned(), b"something else".to_vec());
        let rig = Rig::new("0.2.1", true, fake);
        rig.updater.check(true, true);
        assert!(matches!(
            rig.settle(),
            Status::Failed {
                stage: Stage::Download,
                ..
            }
        ));
        let leftovers = std::fs::read_dir(&rig.updater.shared.config.download_dir)
            .unwrap()
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_tampered_feed_is_refused() {
        let mut fake = Fake::new();
        let mut sums = SUMS_FILE.to_vec();
        sums[3] ^= 1;
        fake.files.insert(
            "https://example.test/releases/download/v0.3.0/SHA256SUMS".to_owned(),
            sums,
        );
        let rig = Rig::new("0.2.1", true, fake);
        rig.updater.check(true, true);
        assert!(matches!(
            rig.settle(),
            Status::Failed {
                stage: Stage::Check,
                ..
            }
        ));
        assert!(rig.fake.runs.lock().unwrap().is_empty());
    }

    #[test]
    fn old_downloads_are_cleared_at_start_and_nothing_else() {
        let dir = TempDir::new();
        let keep = dir.0.join("notes.txt");
        let old = dir.0.join("open-task-v0.1.0-windows-setup.exe");
        let part = dir.0.join("open-task-v0.1.0-windows-setup.exe.part");
        for f in [&keep, &old, &part] {
            std::fs::write(f, b"x").unwrap();
        }
        remove_downloads(&dir.0);
        assert!(keep.exists());
        assert!(!old.exists() && !part.exists());
    }
}
