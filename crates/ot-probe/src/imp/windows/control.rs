//! Actions on processes.
//!
//! Terminating by PID alone is how a task manager kills the wrong thing: the target
//! exits on its own, the PID is handed to a new process, and the click lands on that.
//! Every action here opens the PID, reads its creation time, and compares it with
//! the [`ProcessKey`]'s birth stamp before doing anything. On Windows the two are the
//! same FILETIME, so the comparison is exact.

use ot_model::ProcessKey;
use windows::core::HRESULT;
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};

use crate::{ControlError, ProcessControl};

/// Process actions on Windows. Stateless; cheap to create.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsControl;

/// Closes on drop, so every early return below releases the handle.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn os(context: &'static str, e: windows::core::Error) -> ControlError {
    ControlError::Os {
        context,
        source: std::io::Error::other(e),
    }
}

impl ProcessControl for WindowsControl {
    fn terminate(&self, key: ProcessKey) -> Result<(), ControlError> {
        // SAFETY: plain call; the handle is owned by the guard.
        let h = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                key.pid,
            )
        }
        .map_err(|e| {
            // No such PID any more.
            if e.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) {
                ControlError::Gone
            } else {
                os("OpenProcess", e)
            }
        })?;
        let h = Handle(h);
        let (birth, exited) = times_of(h.0)?;
        if exited || birth != key.birth.0 {
            return Err(ControlError::Gone);
        }
        // SAFETY: valid handle with PROCESS_TERMINATE.
        unsafe { TerminateProcess(h.0, 1) }.map_err(|e| os("TerminateProcess", e))
    }
}

/// Creation time as the raw FILETIME value, and whether the process has exited.
pub(super) fn times_of(h: HANDLE) -> Result<(u64, bool), ControlError> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: four valid out-structs.
    unsafe {
        GetProcessTimes(
            h,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|e| os("GetProcessTimes", e))?;
    let birth = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let has_exited = exited.dwHighDateTime != 0 || exited.dwLowDateTime != 0;
    Ok((birth, has_exited))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recycled_or_wrong_birth_is_refused() {
        // Our own PID with a birth stamp that cannot be ours: must not terminate us.
        let key = ProcessKey::new(std::process::id(), 1);
        assert!(matches!(
            WindowsControl.terminate(key),
            Err(ControlError::Gone)
        ));
    }

    #[test]
    fn a_pid_that_does_not_exist_is_gone() {
        // PIDs are multiples of four; an odd one can never exist.
        let key = ProcessKey::new(u32::MAX - 2, 0);
        assert!(matches!(
            WindowsControl.terminate(key),
            Err(ControlError::Gone | ControlError::Os { .. })
        ));
    }
}
