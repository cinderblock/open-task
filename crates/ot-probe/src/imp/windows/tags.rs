//! Service tags: which service each thread of a shared host is working for.
//!
//! When the service control manager starts a service inside a shared `svchost.exe`,
//! it stamps a *service tag* on the service's threads (`SubProcessTag` in the TEB),
//! and every thread the service creates inherits it. Summing thread CPU by tag is
//! how per-service CPU is obtained; there is no other source.
//!
//! Reading a tag means reading one machine word of another process's memory, which
//! needs `PROCESS_VM_READ`. A SYSTEM-owned host grants that to a caller holding
//! `SeDebugPrivilege` (an administrator, elevated), so that privilege, actually
//! enabled, is what decides whether tags are available at all. Everything here is read-only: no thread is suspended, no memory written,
//! no debugger attached. Tag numbers are turned into names with
//! `advapi32!I_QueryTagInformation`, the undocumented-but-stable call Process
//! Explorer and System Informer use for the same purpose.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Arc;

use windows::core::{s, w, PCWSTR, PWSTR};
use windows::Wdk::System::Threading::{NtQueryInformationThread, ThreadBasicInformation};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL, STATUS_SUCCESS};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::{
    OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    THREAD_QUERY_LIMITED_INFORMATION,
};

use super::access::enable_privilege;
use super::nt::{ThreadBasicInformation as Tbi, TEB_SUB_PROCESS_TAG_OFFSET};

/// A handle closed on drop.
#[derive(Debug)]
pub(super) struct OwnedHandle(pub HANDLE);

// SAFETY: a kernel handle is a process-wide token, valid from any thread; nothing
// here is thread-affine. The sampler owns and closes it on its own thread.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by this module and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `I_QueryTagInformation(MachineName, InfoLevel, TagInfo)`.
type QueryTagFn = unsafe extern "system" fn(PCWSTR, u32, *mut c_void) -> u32;

/// `eTagInfoLevelNameFromTag`.
const TAG_INFO_LEVEL_NAME_FROM_TAG: u32 = 1;

/// `TAG_INFO_NAME_FROM_TAG`: `{ InParams { dwPid, dwTag }, OutParams { eTagType,
/// pszName } }`. The out pointer must be freed with `LocalFree`.
#[repr(C)]
struct TagInfoNameFromTag {
    pid: u32,
    tag: u32,
    tag_type: u32,
    name: PWSTR,
}

/// Reads and names service tags. One per probe; created only when the process can
/// read service hosts.
#[derive(Debug)]
pub(super) struct TagProbe {
    query: Option<QueryTagFn>,
    /// `(pid, tag)` to service name, or `None` when the SCM could not name it.
    names: HashMap<(u32, u32), Option<Arc<str>>>,
}

impl TagProbe {
    /// Set up tag reading. Returns `None` when `SeDebugPrivilege` cannot be enabled,
    /// since without it no SYSTEM-owned service host can be read.
    pub fn new() -> Option<Self> {
        if !enable_privilege(w!("SeDebugPrivilege")) {
            return None;
        }
        // SAFETY: advapi32 is loaded in every process that touched the SCM; the
        // export has had this signature since Vista.
        let query = unsafe {
            GetModuleHandleW(w!("advapi32.dll"))
                .ok()
                .and_then(|m| GetProcAddress(m, s!("I_QueryTagInformation")))
                .map(|f| std::mem::transmute::<_, QueryTagFn>(f))
        };
        if query.is_none() {
            tracing::warn!("I_QueryTagInformation not found; service tags will be numbers");
        }
        Some(Self {
            query,
            names: HashMap::new(),
        })
    }

    /// A process handle good for reading thread tags, or `None` if refused.
    pub fn open(pid: u32) -> Option<OwnedHandle> {
        // SAFETY: plain call; the handle is owned by the guard.
        unsafe {
            OpenProcess(
                PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
        }
        .ok()
        .map(OwnedHandle)
    }

    /// The service tag of thread `tid` in the process behind `process`. `Some(0)`
    /// means untagged; `None` means it could not be read (the thread exited, or the
    /// process refused).
    pub fn thread_tag(process: HANDLE, tid: u32) -> Option<u32> {
        // SAFETY: plain call; the handle is owned by the guard.
        let thread = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) }
            .ok()
            .map(OwnedHandle)?;
        let mut tbi = Tbi::default();
        let mut len = 0u32;
        // SAFETY: `tbi` is a valid out-struct of the size passed.
        let status = unsafe {
            NtQueryInformationThread(
                thread.0,
                ThreadBasicInformation,
                (&raw mut tbi).cast(),
                size_of::<Tbi>() as u32,
                &raw mut len,
            )
        };
        if status != STATUS_SUCCESS || tbi.TebBaseAddress.is_null() {
            return None;
        }
        let mut tag = 0u32;
        // SAFETY: reads 4 bytes of the target's TEB into a local; the address is
        // the kernel-reported TEB base plus a fixed field offset.
        let ok = unsafe {
            ReadProcessMemory(
                process,
                tbi.TebBaseAddress
                    .cast::<u8>()
                    .wrapping_add(TEB_SUB_PROCESS_TAG_OFFSET)
                    .cast(),
                (&raw mut tag).cast(),
                size_of::<u32>(),
                None,
            )
        };
        ok.is_ok().then_some(tag)
    }

    /// Name of the service behind `tag` in `pid`, cached.
    pub fn name(&mut self, pid: u32, tag: u32) -> Option<Arc<str>> {
        if tag == 0 {
            return None;
        }
        if let Some(n) = self.names.get(&(pid, tag)) {
            return n.clone();
        }
        let name = self.query.and_then(|query| {
            let mut info = TagInfoNameFromTag {
                pid,
                tag,
                tag_type: 0,
                name: PWSTR::null(),
            };
            // SAFETY: `info` is the structure the call expects for this level; on
            // success the name is a NUL-terminated string we free once.
            unsafe {
                let r = query(
                    PCWSTR::null(),
                    TAG_INFO_LEVEL_NAME_FROM_TAG,
                    (&raw mut info).cast(),
                );
                if r != 0 || info.name.is_null() {
                    return None;
                }
                let s = info.name.to_string().ok();
                let _ = LocalFree(Some(HLOCAL(info.name.0.cast())));
                s.map(Arc::from)
            }
        });
        self.names.insert((pid, tag), name.clone());
        name
    }

    /// Forget names for a process that exited, so a recycled PID cannot inherit them.
    pub fn forget(&mut self, pid: u32) {
        self.names.retain(|&(p, _), _| p != pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_threads_read_as_untagged() {
        // Not a service: every thread's tag is 0. Works unelevated too, because we
        // can always read our own memory.
        let process = OwnedHandle(
            // SAFETY: opening ourselves with read rights always succeeds.
            unsafe {
                OpenProcess(
                    PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    std::process::id(),
                )
            }
            .unwrap(),
        );
        // SAFETY: plain call.
        let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        assert_eq!(TagProbe::thread_tag(process.0, tid), Some(0));
    }

    #[test]
    fn tags_are_offered_exactly_when_the_debug_privilege_is_held() {
        let holds = super::super::access::holds_privilege(w!("SeDebugPrivilege"));
        assert_eq!(TagProbe::new().is_some(), holds);
    }
}
