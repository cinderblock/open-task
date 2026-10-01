//! open-task entry point.
//!
//! On Windows this opens the native window. `--headless` (the only mode on other
//! platforms until their shells exist) runs the measuring core and prints live
//! snapshots to the terminal instead. The headless path is permanent: it is the
//! CI smoke test for the probe on every platform and the seed of a future CLI.
//!
//! `--replace-task-manager` and `--restore-task-manager` make Windows start this
//! copy in Task Manager's place, or stop, as the Settings page's switch does (which
//! runs them elevated when it needs to). When Windows does start it that way, Task
//! Manager's own path and arguments come first on the command line and are ignored.

#![forbid(unsafe_code)]
// Release builds are GUI-subsystem so launching the app does not open a terminal.
// Debug builds keep the console for logs. The command-line modes attach to the
// parent console at startup so they can still print from a release build, and a
// terminal waits for them through the console launcher, `open-task.com`
// (src/console.rs).
#![cfg_attr(
    all(windows, not(debug_assertions), not(test)),
    windows_subsystem = "windows"
)]

use std::fmt::Write as _;
use std::time::Duration;

use ot_core::{Sampler, SamplerConfig};
use ot_model::Bytes;
use ot_probe::{CpuSampler, PlatformProbe, PlatformSampler, SystemProbe};

/// This build's version: the release (`0.2.1`), or where it stands relative to one
/// (`0.2.1-19-g892159c`, `-dirty` with uncommitted changes). Set by `build.rs`.
const VERSION: &str = env!("OT_VERSION");

/// `println!` for the command-line modes, which must not panic when stdout goes
/// away: a pipe whose reader stopped early (`| Select-Object -First 3`), or a shell
/// that did not wait. See [`stdout_line`].
macro_rules! out {
    () => {
        stdout_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        stdout_line(format_args!($($arg)*))
    };
}

fn main() {
    // First, before any other thread exists: the console launcher that started this
    // process, if one did (it passes an environment variable, which this removes).
    #[cfg(windows)]
    let launcher = ot_shell_win::launcher::Launcher::adopt();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let version = args.iter().any(|a| a == "--version" || a == "-V");
    let check_update = args.iter().any(|a| a == "--check-update");
    let sample = arg_value(&args, "--sample").and_then(|s| s.parse::<u32>().ok());
    let replace_task_manager = args.iter().any(|a| a == "--replace-task-manager");
    let restore_task_manager = args.iter().any(|a| a == "--restore-task-manager");
    let task_manager = replace_task_manager || restore_task_manager;
    let headless = args.iter().any(|a| a == "--headless") || !cfg!(windows);
    // Everything but the window is a command-line mode, and prints.
    let window = !(version || check_update || task_manager || sample.is_some() || headless);
    #[cfg(windows)]
    {
        if window {
            // The launcher can give the terminal its prompt back now.
            if let Some(launcher) = launcher {
                launcher.release();
            }
        } else {
            ot_shell_win::attach_parent_console();
        }
    }
    if version {
        out!("open-task v{VERSION}");
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    tracing::info!(version = VERSION, "open-task");
    if check_update {
        std::process::exit(run_check_update());
    }
    if task_manager {
        if replace_task_manager && restore_task_manager {
            eprintln!("--replace-task-manager and --restore-task-manager: choose one");
            std::process::exit(2);
        }
        std::process::exit(run_task_manager(replace_task_manager));
    }
    // Started in Task Manager's place with a window already open: bring that one
    // forward, as Task Manager would, before any of the setup below.
    #[cfg(windows)]
    if window && ot_shell_win::task_manager::is_stand_in(&args) && ot_shell_win::raise_open_window()
    {
        tracing::info!("started in Task Manager's place; brought the open window forward");
        return;
    }

    let theme = arg_value(&args, "--theme").unwrap_or("system");
    let view = arg_value(&args, "--view").unwrap_or("list");
    let page = arg_value(&args, "--page").unwrap_or("processes");
    let passes: usize = args
        .iter()
        .position(|a| a == "--passes")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let probe = match PlatformProbe::new() {
        Ok(p) => p,
        Err(e) => fail(&format!("failed to initialize probe: {e}"), 2, window),
    };
    tracing::info!(caps = ?probe.capabilities(), "probe capabilities");

    let config = SamplerConfig {
        interval: Duration::from_secs(1),
    };

    if let Some(pid) = sample {
        let seconds: u64 = arg_value(&args, "--seconds")
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        run_sample(Box::new(probe), config, pid, seconds);
    } else if headless {
        run_headless(Box::new(probe), config, passes);
    } else {
        run_gui(Box::new(probe), config, theme, view, page);
    }
}

/// `--replace-task-manager` (`replace`) or `--restore-task-manager`. Exits with 0,
/// or the Win32 error code, which the Settings page reads back when it ran this
/// elevated.
#[cfg(windows)]
fn run_task_manager(replace: bool) -> i32 {
    use ot_shell_win::task_manager::{self, Restored};
    let result = if replace {
        task_manager::replace().map(|exe| {
            format!(
                "Windows now starts {} in Task Manager's place (Ctrl+Shift+Esc, the taskbar, \
                 Ctrl+Alt+Del).",
                exe.display()
            )
        })
    } else {
        task_manager::restore().map(|restored| match restored {
            Restored::Restored => "Task Manager is back: Windows starts it again.".to_owned(),
            Restored::NotReplaced => "Task Manager is not replaced; nothing to do.".to_owned(),
            Restored::Other(program) => format!(
                "Windows starts {program} in Task Manager's place, not this copy; left as it is."
            ),
        })
    };
    match result {
        Ok(message) => {
            out!("{message}");
            0
        }
        Err(e) => {
            let verb = if replace { "replace" } else { "restore" };
            eprintln!("could not {verb} Task Manager: {}", e.message());
            if task_manager::denied(&e) {
                eprintln!("this needs administrator rights: run it from a terminal opened as administrator");
            }
            task_manager::exit_code(&e)
        }
    }
}

#[cfg(not(windows))]
fn run_task_manager(_replace: bool) -> i32 {
    eprintln!("replacing Task Manager is a Windows feature");
    2
}

/// `--check-update`: what the update button's check does, printed. Exit code 0
/// whether or not there is an update; 1 if the check failed.
fn run_check_update() -> i32 {
    let build = match ot_update::Build::parse(VERSION) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("this build's version does not parse: {e}");
            return 1;
        }
    };
    out!("open-task v{build}");
    match ot_update::installation() {
        Some(i) => out!("installed for {:?} in {}", i.scope, i.dir.display()),
        None => out!("not installed by the installer: updates are announced, not installed"),
    }
    let feed = ot_update::Feed::official();
    let platform = ot_update::native(&format!("open-task/{VERSION}"));
    match ot_update::latest(platform.as_ref(), &feed) {
        Ok(release) => {
            let v = &release.version;
            if build.is_older_than(v) {
                out!("v{v} is available: {}", feed.release_page(v));
            } else {
                out!("up to date (the latest release is v{v})");
            }
            0
        }
        Err(e) => {
            eprintln!("could not check for updates: {e}");
            1
        }
    }
}

/// One line to stdout, for [`out!`]. When stdout has gone away the program ends
/// quietly rather than panicking: with code 0 on a broken pipe, since the reader
/// chose to stop (as `head` expects), and 1 on any other failure.
fn stdout_line(args: std::fmt::Arguments) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout
        .write_fmt(args)
        .and_then(|()| stdout.write_all(b"\n"))
    {
        let code = i32::from(e.kind() != std::io::ErrorKind::BrokenPipe);
        std::process::exit(code);
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
fn run_gui(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    theme: &str,
    view: &str,
    page: &str,
) {
    let options = ot_shell_win::ShellOptions {
        theme: ot_shell_win::ThemePreference::parse(theme),
        view: ot_shell_win::ViewMode::parse(view),
        page: ot_shell_win::Page::parse(page).unwrap_or_default(),
        version: VERSION,
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
fn run_gui(
    probe: Box<dyn SystemProbe>,
    config: SamplerConfig,
    theme: &str,
    view: &str,
    page: &str,
) {
    let _ = (probe, config, theme, view, page);
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
        if printed == 0 {
            print_hardware(&snap.hardware);
        }
        print_snapshot(&snap);
        printed += 1;
    }
}

/// `--sample <pid>`: one snapshot to identify the process, then a CPU sample of
/// it, printed. The command-line face of the "Sample CPU" menu item.
fn run_sample(probe: Box<dyn SystemProbe>, config: SamplerConfig, pid: u32, seconds: u64) {
    let sampler = Sampler::start(probe, config);
    let snap = loop {
        std::thread::sleep(Duration::from_millis(50));
        if sampler.consecutive_errors() == u64::MAX {
            eprintln!("this platform has no probe implementation yet");
            std::process::exit(3);
        }
        let snap = sampler.latest();
        if !snap.is_empty() {
            break snap;
        }
    };
    let Some(p) = snap.processes.iter().find(|p| p.key().pid == pid) else {
        eprintln!("no process with PID {pid}");
        std::process::exit(4);
    };
    let services: Vec<String> = p.services.iter().map(|s| s.name.to_string()).collect();
    out!(
        "sampling {} (PID {pid}) for {seconds} s; services: [{}]",
        p.name(),
        services.join(", ")
    );
    match PlatformSampler.sample(p.key(), &services, Duration::from_secs(seconds)) {
        Ok(a) => print_attribution_result(&a),
        Err(e) => {
            eprintln!("sample failed: {e}");
            std::process::exit(5);
        }
    }
}

fn print_attribution_result(a: &ot_model::attribution::Attribution) {
    out!(
        "{} samples over {:.1} s",
        a.samples,
        a.duration.as_secs_f32()
    );
    let total = a.samples.max(1) as f32;
    out!("  by module:");
    for m in a.modules.iter().take(12) {
        out!(
            "    {:>5.1}%  {:>7}  {}",
            m.count as f32 / total * 100.0,
            m.count,
            m.label
        );
    }
    out!("  by thread:");
    for t in a.threads.iter().take(8) {
        let top: Vec<String> = t
            .modules
            .iter()
            .take(3)
            .map(|m| {
                format!(
                    "{} {:.0}%",
                    m.label,
                    m.count as f32 / t.samples.max(1) as f32 * 100.0
                )
            })
            .collect();
        out!(
            "    tid {:>7}  {:>5.1}%  {}",
            t.tid,
            t.samples as f32 / total * 100.0,
            top.join(", ")
        );
    }
    if let Some(c) = &a.clients {
        out!(
            "  clients of {} via {} ({} events, {} lost), by {}:",
            c.service,
            c.provider,
            c.events,
            c.lost,
            c.field
        );
        for b in c.buckets.iter().take(12) {
            out!("    {:>7}  {}", b.count, b.label);
        }
    }
    for n in &a.notes {
        out!("  note: {n}");
    }
}

/// One line of static facts, printed before the first snapshot.
fn print_hardware(hw: &ot_model::hardware::Hardware) {
    let mut line = format!(
        "hardware  {}  {} socket(s), {} cores, {} logical",
        hw.cpu_name.as_deref().unwrap_or("unknown CPU"),
        hw.sockets,
        hw.physical_cores,
        hw.logical_processors,
    );
    if let Some(f) = hw.base_frequency {
        let _ = write!(line, ", base {:.2} GHz", f.as_mhz() / 1000.0);
    }
    for (name, size) in [
        ("L1", hw.cache_l1),
        ("L2", hw.cache_l2),
        ("L3", hw.cache_l3),
    ] {
        if let Some(b) = size {
            let _ = write!(line, ", {name} {}", human(b));
        }
    }
    out!("{line}");
}

/// The clock, the memory lists, and each disk and network adapter.
fn print_devices(snap: &ot_core::Snapshot) {
    let mem = &snap.memory;
    let clocks: Vec<u64> = snap
        .cpu
        .cores
        .iter()
        .filter_map(|c| c.frequency)
        .map(|f| f.0)
        .collect();
    let mut extra = String::new();
    if !clocks.is_empty() {
        let avg = clocks.iter().sum::<u64>() / clocks.len() as u64;
        let _ = write!(extra, "clock {:.2} GHz", avg as f64 / 1e9);
    }
    for (name, b) in [
        ("modified", mem.modified),
        ("standby", mem.standby),
        ("free", mem.free),
        ("paged pool", mem.paged_pool),
        ("non-paged pool", mem.nonpaged_pool),
    ] {
        if let Some(b) = b {
            let sep = if extra.is_empty() { "" } else { "  " };
            let _ = write!(extra, "{sep}{name} {}", human(b));
        }
    }
    if !extra.is_empty() {
        out!("  {extra}");
    }
    for d in &snap.disks {
        let kind = match d.info.ssd {
            Some(true) => "SSD",
            Some(false) => "HDD",
            None => "disk",
        };
        out!(
            "  {:<16} {:>5.1}% active  read {:>8}/s  write {:>8}/s  {}  {}{}",
            d.info.name,
            d.active.get(),
            human(d.read_per_sec),
            human(d.write_per_sec),
            kind,
            d.info.model.as_deref().unwrap_or(""),
            d.info
                .capacity
                .map(|c| format!(" ({})", human(c)))
                .unwrap_or_default(),
        );
    }
    for a in &snap.adapters {
        out!(
            "  {:<28} rx {:>8}/s  tx {:>8}/s  {:?}{}  {}",
            a.info.name,
            human(a.rx_per_sec),
            human(a.tx_per_sec),
            a.info.kind,
            if a.info.hardware { "" } else { " (virtual)" },
            a.info.adapter,
        );
    }
}

fn print_snapshot(snap: &ot_core::Snapshot) {
    let mem = &snap.memory;
    out!(
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
    out!("  cores: {}", cores.join(" "));
    print_devices(snap);

    let mut procs: Vec<_> = snap.processes.iter().collect();
    procs.sort_by(|a, b| b.cpu.get().total_cmp(&a.cpu.get()));
    out!(
        "  {:>7}  {:>6}  {:>9}  {:>10}  {:>10}  {:>10}  {:>5}  {:>6}  {:<16}  name",
        "pid",
        "cpu%",
        "cpu s",
        "cycles G",
        "ws",
        "private",
        "thr",
        "hnd",
        "user"
    );
    for p in procs.iter().take(12) {
        out!(
            "  {:>7}  {:>6.1}  {:>9.1}  {:>10.1}  {:>10}  {:>10}  {:>5}  {:>6}  {:<16}  {}",
            p.key().pid,
            p.cpu.get(),
            p.cpu_time.as_secs_f64(),
            p.cycles as f64 / 1e9,
            human(p.working_set),
            human(p.private_bytes),
            p.threads,
            p.handles,
            p.statics.user.as_deref().unwrap_or(""),
            p.name(),
        );
    }

    print_attribution(snap, &procs);
}

/// Service hosts and the hottest threads: what turns "svchost.exe is busy" into a
/// service name.
fn print_attribution(snap: &ot_core::Snapshot, procs: &[&ot_model::process::ProcessSample]) {
    // Service hosts: which services, and which of them the hot threads belong to.
    // This is the view that turns "svchost.exe is busy" into a service name.
    let hosts: Vec<_> = procs
        .iter()
        .filter(|p| p.is_service_host())
        .take(3)
        .collect();
    if !hosts.is_empty() {
        out!("  service hosts (top 3 by CPU):");
        for p in hosts {
            let names: Vec<&str> = p.services.iter().map(|s| &*s.name).collect();
            out!(
                "    pid {:>6} {:>5.1}%  {}  [{}]",
                p.key().pid,
                p.cpu.get(),
                p.name(),
                names.join(", ")
            );
        }
    }

    let mut threads: Vec<(
        &ot_model::process::ProcessSample,
        &ot_model::thread::ThreadSample,
    )> = snap
        .processes
        .iter()
        .flat_map(|p| snap.threads[p.thread_range()].iter().map(move |t| (p, t)))
        .filter(|(_, t)| t.cpu.get() > 0.0)
        .collect();
    threads.sort_by(|a, b| b.1.cpu.get().total_cmp(&a.1.cpu.get()));
    if !threads.is_empty() {
        out!(
            "  threads ({} sampled; tags {}):",
            snap.threads.len(),
            if snap.capabilities.service_tags {
                "on"
            } else {
                "off, needs administrator"
            }
        );
        out!(
            "  {:>7}  {:>7}  {:>6}  {:<10}  {:<28}  process",
            "pid",
            "tid",
            "cpu%",
            "state",
            "service"
        );
        for (p, t) in threads.iter().take(8) {
            let service = match t.service {
                ot_model::thread::ServiceTag::Service(i) => {
                    p.services.get(usize::from(i)).map_or("?", |s| &*s.name)
                }
                ot_model::thread::ServiceTag::None => "-",
                ot_model::thread::ServiceTag::Unknown => "",
            };
            out!(
                "  {:>7}  {:>7}  {:>6.1}  {:<10}  {:<28}  {}",
                p.key().pid,
                t.tid,
                t.cpu.get(),
                t.state.label(),
                service,
                p.name()
            );
        }
    }
    out!();
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
