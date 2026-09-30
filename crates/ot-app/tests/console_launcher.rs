//! The command-line modes, run the way a terminal runs them: through the console
//! launcher (`open-task-console`, shipped as `open-task.com`), which starts
//! `open-task` beside it and relays its output and exit code. Cargo builds the two
//! side by side in the target directory, as the release ships them.

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_open-task-console");
const APP: &str = env!("CARGO_BIN_EXE_open-task");

#[test]
fn the_version_comes_back_through_the_launcher() {
    let out = Command::new(LAUNCHER)
        .arg("--version")
        .output()
        .expect("the launcher runs");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        concat!("open-task v", env!("OT_VERSION"))
    );
}

#[test]
fn the_exit_code_comes_back_through_the_launcher() {
    // No process has the largest PID: the sample fails, with code 4.
    let out = Command::new(LAUNCHER)
        .args(["--headless", "--sample", "4294967295", "--seconds", "1"])
        .output()
        .expect("the launcher runs");
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no process with PID 4294967295"),
        "{stderr}"
    );
}

#[test]
fn a_reader_that_stops_early_ends_the_output_quietly() {
    let started = Instant::now();
    let mut child = Command::new(APP)
        .args(["--headless", "--passes", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the app runs");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let mut first = String::new();
    stdout.read_line(&mut first).expect("a line");
    assert!(first.starts_with("hardware"), "{first}");
    // Stop reading, as `| Select-Object -First 1` does.
    drop(stdout);
    let status = child.wait().expect("it ends");
    // Code 0, not a panic, and at the next write, not after all 30 passes.
    assert_eq!(status.code(), Some(0));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
}
