//! Actions on processes, and the lists read on demand.
//!
//! Every action names its process by [`ProcessKey`]. It opens a pidfd for the
//! PID, then checks that the process behind it has the key's start time. A pidfd
//! keeps naming the process it was opened for even if that process exits and its
//! PID is reused, so once the check passes, the signal cannot reach a stranger.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use ot_model::ProcessKey;

use crate::{
    ControlError, Inventory, ProcessControl, ServiceControl, SessionControl, StartupControl,
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

impl ProcessControl for LinuxControl {
    /// `SIGKILL`, which, like Windows' `TerminateProcess`, cannot be caught.
    fn terminate(&self, key: ProcessKey) -> Result<(), ControlError> {
        signal(key, libc::SIGKILL)
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
impl Inventory for LinuxControl {}

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
