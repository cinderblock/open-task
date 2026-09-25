//! open-task entry point.
//!
//! On Windows this opens the native window. `--headless` (the only mode on other
//! platforms until their shells exist) runs the measuring core and prints live
//! snapshots to the terminal instead. The headless path is permanent: it is the
//! CI smoke test for the probe on every platform and the seed of a future CLI.

#![forbid(unsafe_code)]
// Release builds are GUI-subsystem so launching the app does not open a terminal.
// Debug builds keep the console for logs. Headless mode attaches to the parent
// console at startup so it can still print from a release build.
#![cfg_attr(
    all(windows, not(debug_assertions), not(test)),
    windows_subsystem = "windows"
)]

use std::time::Duration;

use ot_core::{Sampler, SamplerConfig};
use ot_model::Bytes;
use ot_probe::{PlatformProbe, SystemProbe};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = args.iter().any(|a| a == "--headless") || !cfg!(windows);
    if headless {
        #[cfg(windows)]
        ot_shell_win::attach_parent_console();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let theme = arg_value(&args, "--theme").unwrap_or("system");
    let view = arg_value(&args, "--view").unwrap_or("list");
    let passes: usize = args
        .iter()
        .position(|a| a == "--passes")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let probe = match PlatformProbe::new() {
        Ok(p) => p,
        Err(e) => fail(&format!("failed to initialize probe: {e}"), 2, !headless),
    };
    tracing::info!(caps = ?probe.capabilities(), "probe capabilities");

    let config = SamplerConfig {
        interval: Duration::from_secs(1),
    };

    if headless {
        run_headless(Box::new(probe), config, passes);
    } else {
        run_gui(Box::new(probe), config, theme, view);
    }
}

/// Value following `flag`, if present.
fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[cfg(windows)]
fn run_gui(probe: Box<dyn SystemProbe>, config: SamplerConfig, theme: &str, view: &str) {
    let options = ot_shell_win::ShellOptions {
        theme: ot_shell_win::ThemePreference::parse(theme),
        view: ot_shell_win::ViewMode::parse(view),
    };
    if let Err(e) = ot_shell_win::run(probe, config, options) {
        fail(&format!("shell failed: {e}"), 1, true);
    }
}

/// Report a fatal startup error and exit. In GUI mode there may be no console, so
/// the message also goes to a message box.
fn fail(message: &str, code: i32, gui: bool) -> ! {
    eprintln!("{message}");
    #[cfg(windows)]
    if gui {
        ot_shell_win::error_box(message);
    }
    #[cfg(not(windows))]
    let _ = gui;
    std::process::exit(code)
}

#[cfg(not(windows))]
fn run_gui(probe: Box<dyn SystemProbe>, config: SamplerConfig, theme: &str, view: &str) {
    let _ = (probe, config, theme, view);
    eprintln!("no GUI shell on this platform yet; use --headless");
    std::process::exit(3);
}

fn run_headless(probe: Box<dyn SystemProbe>, config: SamplerConfig, passes: usize) {
    let sampler = Sampler::start(probe, config);

    let mut last_tick = None;
    let mut printed = 0usize;
    while printed < passes {
        std::thread::sleep(Duration::from_millis(100));
        if sampler.consecutive_errors() == u64::MAX {
            eprintln!("this platform has no probe implementation yet");
            std::process::exit(3);
        }
        let snap = sampler.latest();
        if snap.is_empty() || last_tick == Some(snap.tick) {
            continue;
        }
        last_tick = Some(snap.tick);
        // The first pass has no interval to compute rates over; skip it.
        if snap.tick.0 < 2 {
            continue;
        }
        print_snapshot(&snap);
        printed += 1;
    }
}

fn print_snapshot(snap: &ot_core::Snapshot) {
    let mem = &snap.memory;
    println!(
        "tick {}  interval {:>6.1?}  probe cost {:>7.3?}  procs {}  cpu {:>5.1}%  mem {}/{} ({} avail)",
        snap.tick.0,
        snap.interval,
        snap.probe_cost,
        snap.processes.len(),
        snap.cpu.total.get(),
        human(mem.in_use()),
        human(mem.total),
        human(mem.available),
    );

    let cores: Vec<String> = snap
        .cpu
        .cores
        .iter()
        .map(|c| {
            let k = match c.kind {
                ot_model::cpu::CoreKind::Performance => "P",
                ot_model::cpu::CoreKind::Efficiency => "E",
                ot_model::cpu::CoreKind::Unknown => "-",
            };
            format!("{k}{:>3.0}", c.usage.get())
        })
        .collect();
    println!("  cores: {}", cores.join(" "));

    let mut procs: Vec<_> = snap.processes.iter().collect();
    procs.sort_by(|a, b| b.cpu.get().total_cmp(&a.cpu.get()));
    println!(
        "  {:>7}  {:>6}  {:>10}  {:>10}  {:>5}  {:>6}  name",
        "pid", "cpu%", "ws", "private", "thr", "hnd"
    );
    for p in procs.iter().take(12) {
        println!(
            "  {:>7}  {:>6.1}  {:>10}  {:>10}  {:>5}  {:>6}  {}",
            p.key().pid,
            p.cpu.get(),
            human(p.working_set),
            human(p.private_bytes),
            p.threads,
            p.handles,
            p.name(),
        );
    }
    println!();
}

fn human(b: Bytes) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = b.get() as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{v:.0}{}", UNITS[u])
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}
