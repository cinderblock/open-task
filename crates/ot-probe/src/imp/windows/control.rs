//! Actions on processes.
//!
//! Terminating by PID alone is how a task manager kills the wrong thing: the target
//! exits on its own, the PID is handed to a new process, and the click lands on that.
//! Every action here opens the PID, reads its creation time, and compares it with
//! the [`ProcessKey`]'s birth stamp before doing anything. On Windows the two are the
//! same FILETIME, so the comparison is exact. [`open`] does that for every action,
//! asking for the action's right plus `PROCESS_QUERY_LIMITED_INFORMATION` for the
//! times, and turns `ERROR_ACCESS_DENIED` into [`ControlError::NotPermitted`] so the
//! UI can say "run as administrator" rather than quote an error code.
//!
//! The actions are the ones Task Manager and Process Explorer offer, done the way
//! they do them: priority through `SetPriorityClass` (Realtime silently becomes High
//! without `SeIncreaseBasePriorityPrivilege`, so the class is read back and a
//! mismatch reported as not permitted); affinity through the affinity mask of
//! processor group 0; suspend and resume through `NtSuspendProcess` and
//! `NtResumeProcess`, which freeze every thread at once rather than one at a time;
//! efficiency mode through `ProcessPowerThrottling` plus the idle priority class,
//! exactly the pair Task Manager sets; and dumps through `MiniDumpWriteDump` with
//! full memory, as Task Manager's "Create dump file".

use std::io::Seek;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use std::sync::{Mutex, PoisonError};

use ot_model::apps::InstalledApp;
use ot_model::connection::Connection;
use ot_model::process::Priority;
use ot_model::startup::StartupEntry;
use ot_model::system::SystemFacts;
use ot_model::ProcessKey;
use windows::core::{HRESULT, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_PARTIAL_COPY, FILETIME,
    HANDLE, NTSTATUS, STATUS_ACCESS_DENIED,
};
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpWithFullMemory, MiniDumpWithHandleData, MiniDumpWithThreadInfo,
    MiniDumpWithUnloadedModules, MiniDumpWriteDump,
};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessTimes, OpenProcess, ProcessPowerThrottling,
    QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask, SetProcessInformation,
    TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS,
    PROCESS_CREATION_FLAGS, PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, PROCESS_VM_READ, REALTIME_PRIORITY_CLASS,
};

use super::connections::ConnectionProbe;
use super::nt::{NtResumeProcess, NtSuspendProcess};
use super::{installed, services, sessions, startup, system};
use crate::{Affinity, ControlError, ProcessControl};

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

/// An OS failure, with access denied named as what it is.
fn os(context: &'static str, e: windows::core::Error) -> ControlError {
    if e.code() == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
        ControlError::NotPermitted
    } else {
        ControlError::Os {
            context,
            source: std::io::Error::other(e),
        }
    }
}

/// An NT status failure, the same way.
fn nt(context: &'static str, status: NTSTATUS) -> ControlError {
    if status == STATUS_ACCESS_DENIED {
        ControlError::NotPermitted
    } else {
        ControlError::Os {
            context,
            source: std::io::Error::other(format!("NTSTATUS 0x{:08X}", status.0 as u32)),
        }
    }
}

/// Open the process behind `key` with `access` (plus the right to read its times)
/// and verify it is still the process the key names.
fn open(key: ProcessKey, access: PROCESS_ACCESS_RIGHTS) -> Result<Handle, ControlError> {
    // SAFETY: plain call; the handle is owned by the guard.
    let h = unsafe { OpenProcess(access | PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) }
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
    Ok(h)
}

fn priority_class(p: Priority) -> PROCESS_CREATION_FLAGS {
    match p {
        Priority::Idle => IDLE_PRIORITY_CLASS,
        Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        Priority::Normal => NORMAL_PRIORITY_CLASS,
        Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        Priority::High => HIGH_PRIORITY_CLASS,
        Priority::Realtime => REALTIME_PRIORITY_CLASS,
    }
}

/// `SetPriorityClass` and check it took: Realtime without the privilege comes
/// back as High, which is a refusal, not a success.
fn set_priority_class(h: HANDLE, class: PROCESS_CREATION_FLAGS) -> Result<(), ControlError> {
    // SAFETY: valid handle with PROCESS_SET_INFORMATION.
    unsafe { SetPriorityClass(h, class) }.map_err(|e| os("SetPriorityClass", e))?;
    // SAFETY: valid handle with a query right; zero means failure.
    let now = unsafe { GetPriorityClass(h) };
    if now == 0 {
        return Err(os("GetPriorityClass", windows::core::Error::from_thread()));
    }
    if now != class.0 {
        return Err(ControlError::NotPermitted);
    }
    Ok(())
}

/// The image's file name without `.exe`, for naming a dump.
fn image_stem(h: HANDLE, pid: u32) -> String {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer is `len` units long; `len` is a valid in-out pointer.
    let ok = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &raw mut len)
    };
    if ok.is_err() {
        return format!("pid{pid}");
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid{pid}"))
}

impl crate::ServiceControl for WindowsControl {
    fn start_service(&self, name: &str) -> Result<(), ControlError> {
        services::start(name)
    }

    fn stop_service(&self, name: &str) -> Result<(), ControlError> {
        services::stop(name)
    }
}

impl crate::SessionControl for WindowsControl {
    fn disconnect_session(&self, id: u32) -> Result<(), ControlError> {
        sessions::disconnect(id)
    }

    fn logoff_session(&self, id: u32) -> Result<(), ControlError> {
        sessions::logoff(id)
    }
}

impl crate::StartupControl for WindowsControl {
    fn set_startup_enabled(&self, entry: &StartupEntry, on: bool) -> Result<(), ControlError> {
        startup::set_enabled(entry, on)
    }
}

impl crate::Inventory for WindowsControl {
    fn startup_entries(&self) -> Vec<StartupEntry> {
        startup::entries()
    }

    fn installed_apps(&self) -> Vec<InstalledApp> {
        installed::installed_apps()
    }

    /// One table reader for the process, so its buffer is kept between lists;
    /// callers from different threads take turns.
    fn connections(&self) -> Vec<Connection> {
        static PROBE: Mutex<Option<ConnectionProbe>> = Mutex::new(None);
        let mut guard = PROBE.lock().unwrap_or_else(PoisonError::into_inner);
        let probe = guard.get_or_insert_with(ConnectionProbe::new);
        let mut out = Vec::new();
        probe.list(&mut out);
        out
    }

    fn system_facts(&self) -> SystemFacts {
        system::facts()
    }
}

impl ProcessControl for WindowsControl {
    fn terminate(&self, key: ProcessKey) -> Result<(), ControlError> {
        let h = open(key, PROCESS_TERMINATE)?;
        // SAFETY: valid handle with PROCESS_TERMINATE.
        unsafe { TerminateProcess(h.0, 1) }.map_err(|e| os("TerminateProcess", e))
    }

    fn set_priority(&self, key: ProcessKey, priority: Priority) -> Result<(), ControlError> {
        let h = open(key, PROCESS_SET_INFORMATION)?;
        set_priority_class(h.0, priority_class(priority))
    }

    fn affinity(&self, key: ProcessKey) -> Result<Affinity, ControlError> {
        let h = open(key, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut mask = 0usize;
        let mut system = 0usize;
        // SAFETY: two valid out-pointers.
        unsafe { GetProcessAffinityMask(h.0, &raw mut mask, &raw mut system) }
            .map_err(|e| os("GetProcessAffinityMask", e))?;
        Ok(Affinity {
            mask: mask as u64,
            system: system as u64,
        })
    }

    fn set_affinity(&self, key: ProcessKey, mask: u64) -> Result<(), ControlError> {
        if mask == 0 {
            return Err(ControlError::Os {
                context: "SetProcessAffinityMask",
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the mask is empty; a process needs at least one processor",
                ),
            });
        }
        let h = open(key, PROCESS_SET_INFORMATION)?;
        // SAFETY: valid handle with PROCESS_SET_INFORMATION.
        unsafe { SetProcessAffinityMask(h.0, mask as usize) }
            .map_err(|e| os("SetProcessAffinityMask", e))
    }

    fn suspend(&self, key: ProcessKey) -> Result<(), ControlError> {
        let h = open(key, PROCESS_SUSPEND_RESUME)?;
        // SAFETY: valid handle with PROCESS_SUSPEND_RESUME.
        let status = unsafe { NtSuspendProcess(h.0) };
        if status.is_ok() {
            Ok(())
        } else {
            Err(nt("NtSuspendProcess", status))
        }
    }

    fn resume(&self, key: ProcessKey) -> Result<(), ControlError> {
        let h = open(key, PROCESS_SUSPEND_RESUME)?;
        // SAFETY: valid handle with PROCESS_SUSPEND_RESUME.
        let status = unsafe { NtResumeProcess(h.0) };
        if status.is_ok() {
            Ok(())
        } else {
            Err(nt("NtResumeProcess", status))
        }
    }

    fn set_efficiency_mode(&self, key: ProcessKey, on: bool) -> Result<(), ControlError> {
        let h = open(key, PROCESS_SET_INFORMATION)?;
        // The control bit says "set this policy explicitly"; the state bit says on.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: if on {
                PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            } else {
                0
            },
        };
        // SAFETY: `state` is a valid struct of the size passed.
        unsafe {
            SetProcessInformation(
                h.0,
                ProcessPowerThrottling,
                (&raw const state).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        }
        .map_err(|e| os("SetProcessInformation", e))?;
        let class = if on {
            IDLE_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        set_priority_class(h.0, class)
    }

    fn write_dump(&self, key: ProcessKey, dir: &Path) -> Result<PathBuf, ControlError> {
        let h = open(key, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
        let stem = image_stem(h.0, key.pid);
        let mut path = dir.join(format!("{stem}.DMP"));
        if path.exists() {
            path = dir.join(format!("{stem}-{}.DMP", key.pid));
        }
        let mut file = std::fs::File::create_new(&path).map_err(|e| ControlError::Os {
            context: "create the dump file",
            source: e,
        })?;
        let kind = MiniDumpWithFullMemory
            | MiniDumpWithHandleData
            | MiniDumpWithUnloadedModules
            | MiniDumpWithThreadInfo;
        // A full-memory dump reads the whole address space while the target keeps
        // running; a region freed or a thread exiting mid-read fails the call with
        // ERROR_PARTIAL_COPY. That is a moment, not a condition, so the dump is
        // tried again from the start a couple of times before giving up.
        let partial = HRESULT::from_win32(ERROR_PARTIAL_COPY.0);
        let mut attempts = 0;
        loop {
            attempts += 1;
            // SAFETY: both handles are open and valid for the call; the file
            // handle is owned by `file`, which outlives the call.
            let written = unsafe {
                MiniDumpWriteDump(
                    h.0,
                    key.pid,
                    HANDLE(file.as_raw_handle()),
                    kind,
                    None,
                    None,
                    None,
                )
            };
            match written {
                Ok(()) => return Ok(path),
                Err(e) if e.code() == partial && attempts < DUMP_ATTEMPTS => {
                    let restarted = file
                        .set_len(0)
                        .and_then(|()| file.seek(std::io::SeekFrom::Start(0)));
                    if restarted.is_ok() {
                        std::thread::sleep(DUMP_RETRY_PAUSE);
                        continue;
                    }
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(os("MiniDumpWriteDump", e));
                }
                Err(e) => {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(os("MiniDumpWriteDump", e));
                }
            }
        }
    }
}

/// How many times a dump is tried when the target's memory changes under it.
const DUMP_ATTEMPTS: u32 = 3;
/// Pause between attempts, for whatever was in flight to finish.
const DUMP_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

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
pub(super) mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Threading::{GetCurrentProcess, CREATE_NO_WINDOW};

    /// A child that lives until dropped, with no window of its own: tests run on
    /// the user's desktop and must not flash a console there.
    pub(in crate::imp::windows) struct Child(std::process::Child);

    impl Child {
        pub(in crate::imp::windows) fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Spawn a quiet child that runs for a minute unless killed.
    pub(in crate::imp::windows) fn quiet_child() -> Child {
        let child = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        Child(child)
    }

    /// The real key of a live process, from its creation time.
    pub(in crate::imp::windows) fn key_of(pid: u32) -> ProcessKey {
        // SAFETY: plain call; closed by the guard.
        let h = Handle(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .expect("open the process"),
        );
        let (birth, _) = times_of(h.0).expect("times");
        ProcessKey::new(pid, birth)
    }

    fn own_key() -> ProcessKey {
        // SAFETY: the pseudo-handle is always valid and needs no closing.
        let (birth, _) = times_of(unsafe { GetCurrentProcess() }).expect("own times");
        ProcessKey::new(std::process::id(), birth)
    }

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

    #[test]
    fn every_action_refuses_a_wrong_birth() {
        let key = ProcessKey::new(std::process::id(), 1);
        let c = WindowsControl;
        assert!(matches!(
            c.set_priority(key, Priority::Normal),
            Err(ControlError::Gone)
        ));
        assert!(matches!(c.affinity(key), Err(ControlError::Gone)));
        assert!(matches!(c.set_affinity(key, 1), Err(ControlError::Gone)));
        assert!(matches!(c.suspend(key), Err(ControlError::Gone)));
        assert!(matches!(c.resume(key), Err(ControlError::Gone)));
        assert!(matches!(
            c.set_efficiency_mode(key, false),
            Err(ControlError::Gone)
        ));
        assert!(matches!(
            c.write_dump(key, &std::env::temp_dir()),
            Err(ControlError::Gone)
        ));
    }

    #[test]
    fn own_affinity_is_within_the_system_and_can_be_set_back() {
        let key = own_key();
        let a = WindowsControl.affinity(key).expect("affinity");
        assert_ne!(a.mask, 0);
        assert_eq!(a.mask & a.system, a.mask, "{a:?}");
        WindowsControl.set_affinity(key, a.mask).expect("same mask");
        assert!(matches!(
            WindowsControl.set_affinity(key, 0),
            Err(ControlError::Os { .. })
        ));
    }

    #[test]
    fn own_priority_can_be_lowered_and_restored() {
        let key = own_key();
        // SAFETY: the pseudo-handle is always valid.
        let class = || unsafe { GetPriorityClass(GetCurrentProcess()) };
        WindowsControl
            .set_priority(key, Priority::BelowNormal)
            .expect("below normal");
        assert_eq!(class(), BELOW_NORMAL_PRIORITY_CLASS.0);
        WindowsControl
            .set_priority(key, Priority::Normal)
            .expect("normal");
        assert_eq!(class(), NORMAL_PRIORITY_CLASS.0);
    }

    #[test]
    fn efficiency_mode_turns_on_and_off_for_a_child() {
        let child = quiet_child();
        let key = key_of(child.pid());
        WindowsControl.set_efficiency_mode(key, true).expect("on");
        assert_eq!(
            super::super::details::efficiency_mode_of(child.pid()),
            Some(true)
        );
        let on = WindowsControl.affinity(key).is_ok();
        assert!(on, "still open");
        WindowsControl.set_efficiency_mode(key, false).expect("off");
        assert_eq!(
            super::super::details::efficiency_mode_of(child.pid()),
            Some(false)
        );
    }

    #[test]
    fn a_child_can_be_suspended_and_resumed() {
        let child = quiet_child();
        let key = key_of(child.pid());
        WindowsControl.suspend(key).expect("suspend");
        WindowsControl.resume(key).expect("resume");
    }

    #[test]
    fn a_dump_of_a_child_is_written_and_named_after_it() {
        let child = quiet_child();
        let key = key_of(child.pid());
        // A process a few milliseconds old is still mapping its image and DLLs;
        // the dump's retry covers that, but let it settle rather than rely on it.
        std::thread::sleep(std::time::Duration::from_millis(250));
        let dir = std::env::temp_dir();
        let path = WindowsControl.write_dump(key, &dir).expect("dump");
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(name.starts_with("ping"), "{name}");
        assert_eq!(
            path.extension()
                .map(std::ffi::OsStr::to_ascii_lowercase)
                .as_deref(),
            Some("dmp".as_ref()),
            "{name}"
        );
        let len = std::fs::metadata(&path).expect("dump file").len();
        std::fs::remove_file(&path).expect("remove the dump");
        assert!(len > 0);
    }
}
