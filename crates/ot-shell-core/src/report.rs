//! Words for messages: what an action is called, and a wait chain as a report.

use std::fmt::Write as _;

use ot_model::process::{WaitChain, WaitKind, WaitStatus};
use ot_ui::ProcessAction;

/// What a [`ProcessAction`] is called in a message.
pub(crate) fn describe(action: &ProcessAction, name: &str) -> String {
    match action {
        ProcessAction::SetPriority(p) => format!("set the priority of {name} to {}", p.label()),
        ProcessAction::SetAffinity(_) => format!("set the affinity of {name}"),
        ProcessAction::Suspend => format!("suspend {name}"),
        ProcessAction::Resume => format!("resume {name}"),
        ProcessAction::EfficiencyMode(true) => format!("put {name} in efficiency mode"),
        ProcessAction::EfficiencyMode(false) => format!("take {name} out of efficiency mode"),
        ProcessAction::WriteDump => format!("write a dump of {name}"),
        ProcessAction::WaitChain { .. } => format!("analyze the wait chain of {name}"),
        ProcessAction::Restart { .. } => format!("restart {name}"),
    }
}

/// The wait chain as text: a line per thread that waits on something nameable,
/// the chain written left to right, and a verdict first.
pub(crate) fn wait_chain(name: &str, pid: u32, chain: &WaitChain) -> String {
    let mut out = String::new();
    let threads = chain.threads.len();
    let blocked = chain.blocked();
    if chain.deadlocked() {
        let _ = writeln!(out, "{name} (PID {pid}) is deadlocked.");
    } else if blocked == 0 {
        let _ = writeln!(
            out,
            "{name} (PID {pid}): none of its {threads} threads is waiting on another \
             thread or process. They are running, or waiting on something the system \
             cannot trace (an event, a timer, I/O)."
        );
    } else {
        let _ = writeln!(
            out,
            "{name} (PID {pid}): {blocked} of {threads} threads wait on another thread \
             or process."
        );
    }
    for t in chain.threads.iter().filter(|t| t.nodes.len() > 1) {
        let _ = write!(
            out,
            "\n{}Thread {}",
            if t.cycle { "DEADLOCK: " } else { "" },
            t.tid
        );
        for n in t.nodes.iter().skip(1) {
            match n.kind {
                WaitKind::Thread => {
                    let process = n.process.as_deref().unwrap_or("another process");
                    if n.pid == pid {
                        let _ = write!(out, " \u{2192} thread {} ({})", n.tid, n.status.label());
                    } else {
                        let _ = write!(
                            out,
                            " \u{2192} thread {} of {process} (PID {}, {})",
                            n.tid,
                            n.pid,
                            n.status.label()
                        );
                    }
                }
                kind => {
                    let _ = write!(out, " \u{2192} {}", kind.label());
                    if !n.name.is_empty() {
                        let _ = write!(out, " \"{}\"", n.name);
                    }
                    if n.status != WaitStatus::Owned {
                        let _ = write!(out, " ({})", n.status.label());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_chain_report_reads_left_to_right_and_calls_a_cycle_a_deadlock() {
        use ot_model::process::{ThreadWait, WaitNode};
        let thread = |pid, tid, status, process: Option<&str>| WaitNode {
            kind: WaitKind::Thread,
            status,
            pid,
            tid,
            process: process.map(str::to_owned),
            wait_ms: 0,
            name: String::new(),
        };
        let lock = |kind, name: &str, status| WaitNode {
            kind,
            status,
            pid: 0,
            tid: 0,
            process: None,
            wait_ms: 0,
            name: name.to_owned(),
        };
        let chain = WaitChain {
            threads: vec![
                ThreadWait {
                    tid: 10,
                    nodes: vec![thread(7, 10, WaitStatus::Running, Some("app"))],
                    cycle: false,
                },
                ThreadWait {
                    tid: 11,
                    nodes: vec![
                        thread(7, 11, WaitStatus::Blocked, Some("app")),
                        lock(WaitKind::Mutex, "Global\\Lock", WaitStatus::Owned),
                        thread(9, 42, WaitStatus::Blocked, Some("other")),
                        lock(WaitKind::SendMessage, "", WaitStatus::Blocked),
                        thread(7, 11, WaitStatus::Blocked, Some("app")),
                    ],
                    cycle: true,
                },
            ],
        };
        let text = wait_chain("app.exe", 7, &chain);
        assert!(text.starts_with("app.exe (PID 7) is deadlocked."), "{text}");
        assert!(
            text.contains(
                "DEADLOCK: Thread 11 \u{2192} mutex \"Global\\Lock\" \u{2192} thread 42 of other \
                 (PID 9, blocked) \u{2192} SendMessage (blocked) \u{2192} thread 11 (blocked)"
            ),
            "{text}"
        );
        assert!(
            !text.contains("Thread 10"),
            "a running thread is not listed: {text}"
        );

        let quiet = WaitChain {
            threads: vec![ThreadWait {
                tid: 10,
                nodes: vec![thread(7, 10, WaitStatus::Running, None)],
                cycle: false,
            }],
        };
        let text = wait_chain("app.exe", 7, &quiet);
        assert!(text.contains("none of its 1 threads is waiting"), "{text}");
    }
}
