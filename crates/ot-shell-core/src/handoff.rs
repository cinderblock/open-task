//! Keeping the charts across an update: when and where the history is handed on.
//!
//! An update cannot overlap the two instances: Setup's Restart Manager has to end
//! this one before it can replace the program, and only then starts the new one.
//! So the history goes through a file. The closing instance writes it ([`write`])
//! as the update closes it, on a thread of its own, and waits for it at exit for
//! no longer than [`WRITE_DEADLINE`]: the update never waits on the history. The
//! new instance takes it as it starts ([`take`]) if it is fresh. Whatever goes
//! wrong, the new instance starts with empty charts, as it did before, and the
//! log says why.
//!
//! The file is written under a temporary name and renamed into place, so a reader
//! never sees half of one; it is claimed by renaming it to a name of the
//! claimant's own, so two instances starting at once cannot both take it.
//!
//! The window started by Setup has no console, so each step also writes a line to
//! `handoff.log` beside the file ([`LOG`]).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ot_core::handoff::{self, Meta};
use ot_core::{Retention, Timeline, Usage};

use crate::Platform;

/// The longest the closing instance waits for its history to be written. Setup
/// is waiting for it to close.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(2);
/// How old a handoff may be and still be carried on from. An update takes
/// seconds; a file older than this is from an update that did not start the new
/// version, and the charts would have a hole longer than what they show of it.
pub const MAX_AGE: Duration = Duration::from_secs(5 * 60);
/// How far in the future a handoff may claim to have been written: clocks are
/// adjusted. Further than this, the clock went back and the times in it are wrong.
const MAX_SKEW: Duration = Duration::from_secs(60);
/// The file, in [`dir`].
const FILE: &str = "history.bin";
/// The log, in [`dir`].
pub const LOG: &str = "handoff.log";
/// The log is cut to its newest half when it grows past this.
const LOG_MAX: u64 = 64 << 10;
/// A larger file is not a history.
const MAX_FILE: u64 = 512 << 20;
/// A temporary file older than this was left by a write that timed out.
const STALE_TEMP: Duration = Duration::from_secs(10 * 60);

/// Where the handoff and its log are kept: `%LOCALAPPDATA%\open-task\handoff` on
/// Windows, `$XDG_STATE_HOME/open-task/handoff` (`~/.local/state/...`) elsewhere.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))
    };
    base.map(|b| b.join("open-task").join("handoff"))
}

/// A write in progress. Dropping it waits for the write, but no longer than
/// [`WRITE_DEADLINE`] from when it started.
#[derive(Debug)]
pub struct Pending {
    done: mpsc::Receiver<()>,
    started: Instant,
    log: Log,
}

impl Pending {
    /// Wait for the write to finish, or for the deadline. Says whether it
    /// finished.
    #[must_use = "false means the history was not written"]
    pub fn wait(&self) -> bool {
        let left = WRITE_DEADLINE.saturating_sub(self.started.elapsed());
        match self.done.recv_timeout(left) {
            // Finished, or the thread is gone; either way nothing is left to do.
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.log.line(&format!(
                    "writing the history took longer than {} s; closing without it",
                    WRITE_DEADLINE.as_secs()
                ));
                false
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let _ = self.wait();
    }
}

/// Write `timeline` and `usage` to the handoff in `dir`, on a thread of its own,
/// for the update to `to`. Failures go to the log; they never stop the update.
#[must_use]
pub fn write(
    dir: PathBuf,
    timeline: Timeline,
    usage: Usage,
    version: &'static str,
    to: &str,
    platform: Arc<dyn Platform>,
) -> Pending {
    let log = Log::new(&dir, version, platform);
    let (tx, done) = mpsc::channel();
    let started = Instant::now();
    let thread_log = log.clone();
    let to = to.to_owned();
    let spawned = std::thread::Builder::new()
        .name("ot-handoff".into())
        .spawn(move || {
            let log = thread_log;
            match write_now(&dir, &timeline, &usage, version) {
                Ok(bytes) => log.line(&format!(
                    "wrote the history for the update to v{to}: {} of it, {}, {} series, \
                     {} programs, in {} ms",
                    span(&timeline),
                    size(bytes),
                    timeline.series_count(),
                    usage.programs(),
                    started.elapsed().as_millis()
                )),
                Err(e) => log.line(&format!(
                    "could not write the history ({e}); the update goes on without it"
                )),
            }
            let _ = tx.send(());
        });
    if let Err(e) = spawned {
        log.line(&format!(
            "could not start writing the history ({e}); the update goes on without it"
        ));
    }
    Pending { done, started, log }
}

fn write_now(
    dir: &Path,
    timeline: &Timeline,
    usage: &Usage,
    version: &str,
) -> std::io::Result<usize> {
    let bytes = handoff::encode(
        timeline,
        usage,
        version,
        unix_ms(SystemTime::now()),
        std::process::id(),
    );
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!("history.{}.tmp", std::process::id()));
    let written = fs::write(&temp, &bytes).and_then(|()| fs::rename(&temp, dir.join(FILE)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.map(|()| bytes.len())
}

/// What [`take`] found.
#[derive(Debug)]
// Made once, as the app starts, and taken apart at once.
#[allow(clippy::large_enum_variant)]
pub enum Taken {
    /// No handoff was waiting: an ordinary start.
    Nothing,
    /// The history to carry on from.
    History {
        meta: Meta,
        timeline: Timeline,
        usage: Usage,
    },
    /// A handoff was waiting but cannot be used; it is gone now. Why, in words.
    Refused(String),
}

/// Take the handoff waiting in `dir`, if any, reading the timeline into
/// `retention`. A handoff found is removed whether it is used or not, and the
/// outcome logged.
#[must_use]
pub fn take(
    dir: &Path,
    retention: &Retention,
    now: SystemTime,
    version: &'static str,
    platform: Arc<dyn Platform>,
) -> Taken {
    remove_stale_temps(dir, now);
    let claimed = dir.join(format!("history.{}.claimed", std::process::id()));
    match fs::rename(dir.join(FILE), &claimed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Taken::Nothing,
        Err(e) => {
            let why = format!("the handoff could not be claimed: {e}");
            Log::new(dir, version, platform).line(&format!("started fresh: {why}"));
            return Taken::Refused(why);
        }
    }
    let read = read_claimed(&claimed, retention, now);
    let _ = fs::remove_file(&claimed);
    let log = Log::new(dir, version, platform);
    match read {
        Ok((meta, timeline, usage)) => {
            let ago = Duration::from_millis(
                u64::try_from(unix_ms(now) - meta.written_unix_ms).unwrap_or(0),
            );
            log.line(&format!(
                "carried on from v{}'s history, written {:.1} s before by process {}: \
                 {} of it, {} series, {} programs",
                meta.app_version,
                ago.as_secs_f64(),
                meta.pid,
                span(&timeline),
                timeline.series_count(),
                usage.programs()
            ));
            Taken::History {
                meta,
                timeline,
                usage,
            }
        }
        Err(why) => {
            log.line(&format!("started fresh: {why}"));
            Taken::Refused(why)
        }
    }
}

fn read_claimed(
    path: &Path,
    retention: &Retention,
    now: SystemTime,
) -> Result<(Meta, Timeline, Usage), String> {
    let len = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err(format!("the handoff is {}, too large to be one", size(len)));
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let meta = handoff::meta(&bytes).map_err(|e| e.to_string())?;
    let now_ms = unix_ms(now);
    let age_ms = now_ms - meta.written_unix_ms;
    if age_ms > MAX_AGE.as_millis() as i64 {
        return Err(format!(
            "the history v{} handed on is {} min old, too old to carry on from",
            meta.app_version,
            age_ms / 60_000
        ));
    }
    if -age_ms > MAX_SKEW.as_millis() as i64 {
        return Err(format!(
            "the history v{} handed on was written {} s in the future: the clock went \
             back",
            meta.app_version,
            -age_ms / 1000
        ));
    }
    handoff::decode(&bytes, retention).map_err(|e| e.to_string())
}

/// Remove temporary files left by writes that timed out.
fn remove_stale_temps(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let temp =
            name.starts_with("history.") && (name.ends_with(".tmp") || name.ends_with(".claimed"));
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE_TEMP);
        if temp && stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The handoff log, one line per step, each stamped with the local time (and UTC),
/// the process and its version. Mirrored to `tracing`.
#[derive(Clone)]
struct Log {
    path: PathBuf,
    version: &'static str,
    platform: Arc<dyn Platform>,
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Log")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Log {
    fn new(dir: &Path, version: &'static str, platform: Arc<dyn Platform>) -> Self {
        Self {
            path: dir.join(LOG),
            version,
            platform,
        }
    }

    fn line(&self, text: &str) {
        tracing::info!(target: "handoff", "{text}");
        let now = SystemTime::now();
        let utc = utc_time(now);
        let stamp = match self.platform.local_time(now) {
            Some(local) => format!("{local} ({utc})"),
            None => utc,
        };
        let line = format!(
            "{stamp}  pid {} v{}: {text}\n",
            std::process::id(),
            self.version
        );
        if let Err(e) = self.append(&line) {
            tracing::warn!(path = %self.path.display(), error = %e, "could not write the handoff log");
        }
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::metadata(&self.path).is_ok_and(|m| m.len() > LOG_MAX) {
            let old = fs::read(&self.path)?;
            let half = old.len() / 2;
            let cut = old[half..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(old.len(), |i| half + i + 1);
            fs::write(&self.path, &old[cut..])?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())
    }
}

fn unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// `2026-11-10 20:54:03Z`.
fn utc_time(t: SystemTime) -> String {
    let secs = unix_ms(t).div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// How far back `timeline` reaches: `12 min 5 s`.
fn span(timeline: &Timeline) -> String {
    let s = timeline.span_ms().unwrap_or(0) / 1000;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s} s"),
        (0, m, s) => format!("{m} min {s} s"),
        (h, m, _) => format!("{h} h {m} min"),
    }
}

fn size(bytes: impl TryInto<u64>) -> String {
    let b = bytes.try_into().unwrap_or(u64::MAX) as f64;
    if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} kB", (b / 1e3).ceil())
    }
}

#[cfg(test)]
mod tests;
