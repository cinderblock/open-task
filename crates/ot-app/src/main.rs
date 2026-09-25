//! open-task entry point.
//!
//! Until the native shell lands, this runs the measuring core headless and prints a
//! live table to the terminal. That path stays useful permanently: it is the smoke
//! test for the probe on every platform in CI, and the basis of a future CLI mode.

#![forbid(unsafe_code)]

use std::time::Duration;

use ot_core::{Sampler, SamplerConfig};
use ot_model::Bytes;
use ot_probe::{PlatformProbe, SystemProbe};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let passes: usize = args
        .iter()
        .position(|a| a == "--passes")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let probe = match PlatformProbe::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to initialize probe: {e}");
            std::process::exit(2);
        }
    };
    let caps = probe.capabilities();
    tracing::info!(?caps, "probe capabilities");

    let sampler = Sampler::start(
        Box::new(probe),
        SamplerConfig {
            interval: Duration::from_secs(1),
        },
    );

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
