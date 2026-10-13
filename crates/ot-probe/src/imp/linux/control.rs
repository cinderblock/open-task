//! Actions on processes, and the lists read on demand.
//!
//! Every action names its process by [`ProcessKey`]. It opens a pidfd for the
//! PID, then checks that the process behind it has the key's start time. A pidfd
//! keeps naming the process it was opened for even if that process exits and its
//! PID is reused, so once the check passes, the signal cannot reach a stranger.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use ot_model::connection::Connection;
use ot_model::process::Priority;
use ot_model::startup::StartupEntry;
use ot_model::system::SystemFacts;
use ot_model::ProcessKey;

use crate::{
    Affinity, ControlError, Inventory, ProcessControl, ServiceControl, SessionControl,
    StartupControl,
};

/// Process actions and on-demand lists for Linux.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxControl;

/// A pidfd for `key`'s process, after checking it is that process.
fn open(key: ProcessKey) -> Result<OwnedFd, ControlError> {
    let pid = libc::pid_t::try_from(key.pid).map_err(|_| ControlError::Gone)?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(errno_error("pidfd_open"));
    }
    // SAFETY: the kernel just returned this descriptor, and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    // Read after the pidfd is open: if the start time matches now, the pidfd names
    // the process the key does.
    let stat = std::fs::read(format!("/proc/{}/stat", key.pid)).map_err(|_| ControlError::Gone)?;
    if super::process::starttime(&stat) != Some(key.birth.0) {
        return Err(ControlError::Gone);
    }
    Ok(fd)
}

fn signal(key: ProcessKey, sig: libc::c_int) -> Result<(), ControlError> {
    let fd = open(key)?;
    // SAFETY: a valid pidfd, a valid signal number, no siginfo, no flags.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if rc < 0 {
        return Err(errno_error("pidfd_send_signal"));
    }
    Ok(())
}

/// The last OS error as a [`ControlError`]: the two kinds a caller acts on are
/// named, the rest carried as they are.
fn errno_error(context: &'static str) -> ControlError {
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ESRCH) => ControlError::Gone,
        Some(libc::EPERM | libc::EACCES) => ControlError::NotPermitted,
        _ => ControlError::Os { context, source: e },
    }
}

/// The threads of `key`'s process, after checking it is that process.
///
/// Priority and affinity belong to each thread on Linux, and the calls that set
/// them take a thread id, not a pidfd. The ids come from the verified process's
/// own `task` directory; a thread that exits in the moment between listing and
/// acting is skipped.
fn threads_of(key: ProcessKey) -> Result<Vec<libc::pid_t>, ControlError> {
    let _verified = open(key)?;
    let dir =
        std::fs::read_dir(format!("/proc/{}/task", key.pid)).map_err(|_| ControlError::Gone)?;
    Ok(dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect())
}

/// Apply `f` to every thread; a thread that has exited meanwhile is not an error.
fn each_thread(
    key: ProcessKey,
    context: &'static str,
    mut f: impl FnMut(libc::pid_t) -> libc::c_int,
) -> Result<(), ControlError> {
    for tid in threads_of(key)? {
        if f(tid) != 0 {
            match errno_error(context) {
                ControlError::Gone => {}
                e => return Err(e),
            }
        }
    }
    Ok(())
}

/// The nice value that stands for each Windows priority class; see
/// `process::priority` for the reverse.
const fn nice_of(priority: Priority) -> libc::c_int {
    match priority {
        Priority::Idle => 19,
        Priority::BelowNormal => 5,
        Priority::Normal | Priority::Realtime => 0,
        Priority::AboveNormal => -5,
        Priority::High => -10,
    }
}

/// The online processors as a mask of the first 64.
fn online_mask() -> u64 {
    super::fs::read_trimmed("/sys/devices/system/cpu/online")
        .map(|s| super::fs::parse_cpu_list(&s))
        .unwrap_or_default()
        .into_iter()
        .filter(|&c| c < 64)
        .fold(0, |m, c| m | 1 << c)
}

impl ProcessControl for LinuxControl {
    /// `SIGKILL`, which, like Windows' `TerminateProcess`, cannot be caught.
    fn terminate(&self, key: ProcessKey) -> Result<(), ControlError> {
        signal(key, libc::SIGKILL)
    }

    /// Realtime is the round-robin realtime policy; every other class is a nice
    /// value under the normal policy. Raising priority needs root (or
    /// `CAP_SYS_NICE`); lowering it does not.
    fn set_priority(&self, key: ProcessKey, priority: Priority) -> Result<(), ControlError> {
        each_thread(key, "setting priority", |tid| {
            // SAFETY: plain syscalls on a thread id with valid parameter structs.
            unsafe {
                if priority == Priority::Realtime {
                    let p = libc::sched_param { sched_priority: 1 };
                    return libc::sched_setscheduler(tid, libc::SCHED_RR, &raw const p);
                }
                let policy = libc::sched_getscheduler(tid);
                if policy == libc::SCHED_FIFO || policy == libc::SCHED_RR {
                    let p = libc::sched_param { sched_priority: 0 };
                    if libc::sched_setscheduler(tid, libc::SCHED_OTHER, &raw const p) != 0 {
                        return -1;
                    }
                }
                libc::setpriority(libc::PRIO_PROCESS, tid.cast_unsigned(), nice_of(priority))
            }
        })
    }

    fn affinity(&self, key: ProcessKey) -> Result<Affinity, ControlError> {
        let _verified = open(key)?;
        // SAFETY: a zeroed cpu_set_t is empty; sched_getaffinity fills it.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::sched_getaffinity(
                key.pid.cast_signed(),
                size_of::<libc::cpu_set_t>(),
                &raw mut set,
            )
        };
        if rc != 0 {
            return Err(errno_error("reading affinity"));
        }
        // SAFETY: CPU_ISSET reads a bit of a valid set.
        let mask = (0..64).fold(0u64, |m, c| {
            if unsafe { libc::CPU_ISSET(c, &set) } {
                m | 1 << c
            } else {
                m
            }
        });
        Ok(Affinity {
            mask,
            system: online_mask(),
        })
    }

    fn set_affinity(&self, key: ProcessKey, mask: u64) -> Result<(), ControlError> {
        if mask == 0 {
            return Err(ControlError::os(
                "setting affinity",
                std::io::Error::from_raw_os_error(libc::EINVAL),
            ));
        }
        // SAFETY: a zeroed cpu_set_t is empty; CPU_SET sets bits below its size.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for c in (0..64).filter(|c| mask & 1 << c != 0) {
            unsafe { libc::CPU_SET(c, &mut set) };
        }
        each_thread(key, "setting affinity", |tid| {
            // SAFETY: a thread id and a valid set of the stated size.
            unsafe { libc::sched_setaffinity(tid, size_of::<libc::cpu_set_t>(), &raw const set) }
        })
    }

    fn suspend(&self, key: ProcessKey) -> Result<(), ControlError> {
        signal(key, libc::SIGSTOP)
    }

    fn resume(&self, key: ProcessKey) -> Result<(), ControlError> {
        signal(key, libc::SIGCONT)
    }
}

impl ServiceControl for LinuxControl {}
impl SessionControl for LinuxControl {}
impl StartupControl for LinuxControl {}

impl Inventory for LinuxControl {
    fn startup_entries(&self) -> Vec<StartupEntry> {
        super::inventory::startup_entries()
    }

    fn connections(&self) -> Vec<Connection> {
        super::inventory::connections()
    }

    fn system_facts(&self) -> SystemFacts {
        super::inventory::system_facts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_of(pid: u32) -> ProcessKey {
        let stat = std::fs::read(format!("/proc/{pid}/stat")).unwrap();
        ProcessKey::new(pid, super::super::process::starttime(&stat).unwrap())
    }

    #[test]
    fn suspend_resume_and_terminate_a_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let key = key_of(child.id());
        let c = LinuxControl;
        c.suspend(key).unwrap();
        c.resume(key).unwrap();
        c.terminate(key).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn priority_and_affinity_apply_to_a_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let key = key_of(child.id());
        let c = LinuxControl;
        // Lowering priority needs no privilege.
        c.set_priority(key, Priority::BelowNormal).unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", key.pid)).unwrap();
        let after_name = &stat[stat.rfind(')').unwrap() + 2..];
        let nice: i64 = after_name.split(' ').nth(16).unwrap().parse().unwrap();
        assert_eq!(nice, 5);

        let a = c.affinity(key).unwrap();
        assert_ne!(a.system, 0);
        assert_eq!(a.mask & !a.system, 0);
        let first = a.system & a.system.wrapping_neg();
        c.set_affinity(key, first).unwrap();
        assert_eq!(c.affinity(key).unwrap().mask, first);
        assert!(c.set_affinity(key, 0).is_err());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_stale_key_touches_nothing() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let real = key_of(child.id());
        let stale = ProcessKey::new(real.pid, real.birth.0.wrapping_sub(1));
        assert!(matches!(
            LinuxControl.terminate(stale),
            Err(ControlError::Gone)
        ));
        // Still running: the wrong key reached nothing.
        assert!(child.try_wait().unwrap().is_none());
        let _ = child.kill();
        let _ = child.wait();
    }
}
