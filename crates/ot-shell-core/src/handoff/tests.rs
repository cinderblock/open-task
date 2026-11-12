use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ot_core::{Retention, Snapshot, Timeline, Usage};
use ot_model::cpu::CpuSample;
use ot_model::process::{ProcessSample, ProcessStatic};
use ot_model::{Percent, ProcessKey, Tick};

use super::*;

#[derive(Debug)]
struct Local;

impl Platform for Local {
    fn launch(&self, _: &str, _: Option<&str>) -> Result<(), String> {
        Err("not in a test".into())
    }

    fn local_time(&self, _: SystemTime) -> Option<String> {
        Some("LOCAL".into())
    }
}

fn platform() -> Arc<dyn Platform> {
    Arc::new(Local)
}

/// A directory of the test's own, removed when it ends.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "ot-handoff-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn log(&self) -> String {
        fs::read_to_string(self.0.join(LOG)).unwrap_or_default()
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn history(seconds: u64) -> (Timeline, Usage) {
    let mut timeline = Timeline::new(Retention::covering(3_600_000));
    let mut usage = Usage::new(0.05);
    for s in 0..=seconds {
        let snap = Snapshot {
            tick: Tick(s + 1),
            taken_at: Some(SystemTime::now() - Duration::from_secs(seconds - s)),
            interval: Duration::from_secs(u64::from(s > 0)),
            cpu: CpuSample {
                total: Percent(s as f32),
                ..CpuSample::default()
            },
            processes: vec![ProcessSample {
                statics: Arc::new(ProcessStatic {
                    key: ProcessKey::new(4, 1),
                    name: "busy.exe".into(),
                    ..ProcessStatic::default()
                }),
                cycles: s * 1_000_000_000,
                ..ProcessSample::default()
            }],
            ..Snapshot::default()
        };
        timeline.observe(&snap);
        usage.observe(&snap);
    }
    (timeline, usage)
}

fn hand_on(dir: &Dir) {
    let (timeline, usage) = history(30);
    let pending = write(
        dir.0.clone(),
        timeline,
        usage,
        "0.1.0",
        "the update to v0.2.0",
        platform(),
    );
    assert!(pending.wait());
}

fn take_now(dir: &Dir, now: SystemTime) -> Taken {
    take(
        &dir.0,
        &Retention::covering(3_600_000),
        now,
        "0.2.0",
        platform(),
    )
}

#[test]
fn the_next_instance_carries_on_once() {
    let dir = Dir::new();
    hand_on(&dir);
    assert_eq!(dir.files(), [LOG, FILE]);
    match take_now(&dir, SystemTime::now()) {
        Taken::History {
            meta,
            timeline,
            usage,
        } => {
            assert_eq!(meta.app_version, "0.1.0");
            assert_eq!(meta.pid, std::process::id());
            assert_eq!(timeline.cpu_total.len(), 31);
            assert_eq!(usage.programs(), 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(dir.files(), [LOG], "claimed, then removed");
    assert!(matches!(take_now(&dir, SystemTime::now()), Taken::Nothing));
    let log = dir.log();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(
        lines[0].starts_with("LOCAL (") && lines[0].contains("Z)  pid "),
        "{log}"
    );
    assert!(
        lines[0].contains("v0.1.0: wrote the history for the update to v0.2.0: 30 s of it,"),
        "{log}"
    );
    assert!(
        lines[1].contains("v0.2.0: carried on from v0.1.0's history")
            && lines[1].contains(": 30 s of it,"),
        "{log}"
    );
}

#[test]
fn a_copy_taking_over_waits_for_the_history_but_not_forever() {
    let dir = Dir::new();
    let started = std::time::Instant::now();
    assert!(!wait_for(&dir.0, Duration::from_millis(200)));
    assert!(started.elapsed() >= Duration::from_millis(200));
    let writer = {
        let path = dir.0.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let (timeline, usage) = history(5);
            write(path, timeline, usage, "0.1.0", "a restart", platform()).wait()
        })
    };
    assert!(wait_for(&dir.0, TAKEOVER_WAIT));
    assert!(writer.join().unwrap());
    assert!(matches!(
        take_now(&dir, SystemTime::now()),
        Taken::History { .. }
    ));
}

#[test]
fn nothing_waiting_is_an_ordinary_start() {
    let dir = Dir::new();
    assert!(matches!(take_now(&dir, SystemTime::now()), Taken::Nothing));
    assert!(dir.log().is_empty(), "an ordinary start logs nothing");
}

fn refused(dir: &Dir, now: SystemTime) -> String {
    let taken = take_now(dir, now);
    let Taken::Refused(why) = taken else {
        panic!("{taken:?}");
    };
    assert!(
        !dir.files().contains(&FILE.to_owned()),
        "a refused file goes"
    );
    assert!(dir.log().contains(&format!("started fresh: {why}")));
    why
}

#[test]
fn a_stale_handoff_is_refused() {
    let dir = Dir::new();
    hand_on(&dir);
    let why = refused(&dir, SystemTime::now() + MAX_AGE + Duration::from_secs(60));
    assert!(why.contains("too old"), "{why}");
}

#[test]
fn a_handoff_from_the_future_is_refused() {
    let dir = Dir::new();
    hand_on(&dir);
    let why = refused(&dir, SystemTime::now() - Duration::from_secs(600));
    assert!(why.contains("the clock went back"), "{why}");
}

#[test]
fn a_damaged_or_unknown_handoff_is_refused() {
    let dir = Dir::new();
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(dir.0.join(FILE), b"not a history at all").unwrap();
    assert!(refused(&dir, SystemTime::now()).contains("not a handoff file"));

    hand_on(&dir);
    let path = dir.0.join(FILE);
    let mut bytes = fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&(handoff::FORMAT + 1).to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    let why = refused(&dir, SystemTime::now());
    let named = format!("written by v0.1.0 in format {}", handoff::FORMAT + 1);
    assert!(why.contains(&named), "{why}");
}

#[test]
fn a_failed_write_does_not_hold_up_the_close() {
    let dir = Dir::new();
    // A file where the directory should be.
    fs::write(&dir.0, b"in the way").unwrap();
    let (timeline, usage) = history(5);
    let pending = write(
        dir.0.clone(),
        timeline,
        usage,
        "0.1.0",
        "a test",
        platform(),
    );
    assert!(pending.wait());
    drop(pending);
    let _ = fs::remove_file(&dir.0);
}

#[test]
fn leftovers_of_timed_out_writes_are_cleared() {
    let dir = Dir::new();
    fs::create_dir_all(&dir.0).unwrap();
    let temp = dir.0.join("history.99.tmp");
    fs::write(&temp, b"half").unwrap();
    // Young: a write that may still be going on.
    assert!(matches!(take_now(&dir, SystemTime::now()), Taken::Nothing));
    assert!(temp.exists());
    let later = SystemTime::now() + STALE_TEMP + Duration::from_secs(1);
    assert!(matches!(take_now(&dir, later), Taken::Nothing));
    assert!(!temp.exists());
}

#[test]
fn the_log_keeps_its_newest_half() {
    let dir = Dir::new();
    let log = Log::new(&dir.0, "0.1.0", platform());
    let filler = "x".repeat(200);
    for i in 0..400 {
        log.line(&format!("{i} {filler}"));
    }
    let text = dir.log();
    assert!(text.len() as u64 <= LOG_MAX + 400, "{}", text.len());
    assert!(text.lines().last().unwrap().contains("399 "));
    assert!(!text.lines().any(|l| l.ends_with(&format!(": 0 {filler}"))));
    assert!(
        text.lines().all(|l| l.starts_with("LOCAL (")),
        "whole lines"
    );
}

#[test]
fn utc_times_are_right() {
    let at = |s: u64| utc_time(UNIX_EPOCH + Duration::from_secs(s));
    assert_eq!(at(0), "1970-01-01 00:00:00Z");
    assert_eq!(at(1_794_343_443), "2026-11-10 20:44:03Z");
    assert_eq!(at(951_782_400), "2000-02-29 00:00:00Z");
    assert_eq!(at(4_107_542_399), "2100-02-28 23:59:59Z");
}
