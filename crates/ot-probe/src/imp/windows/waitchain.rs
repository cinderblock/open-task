//! Wait chains: what each thread of a process is waiting on, and on whom.
//!
//! Task Manager's "Analyze wait chain" is the Wait Chain Traversal API
//! (`wct.h`): `OpenThreadWaitChainSession` opens a session, and
//! `GetThreadWaitChain` follows one thread from what it waits on (a critical
//! section, a mutex, a `SendMessage` to another window's thread, an ALPC or COM
//! call, another thread's or process's exit, socket or SMB I/O) to the thread that
//! holds that, and on, up to [`WCT_MAX_NODE_COUNT`] nodes, saying whether the
//! chain comes back on itself: a deadlock. Each thread's chain is read
//! synchronously here, with every "look across processes" flag set, so a hang on
//! a lock in another process is followed into it. COM calls are followed through
//! the callbacks `RegisterWaitChainCOMCallback` takes, `CoGetCallState` and
//! `CoGetActivationState`, which ole32 exports by name but no header declares;
//! they are looked up once, as Microsoft's "Using WCT" sample does, and when
//! they are missing a COM wait shows as what the kernel sees (an ALPC call).
//!
//! **One analysis at a time per process.** Two threads each reading chains in
//! their own sessions crash the process (an access violation inside the API,
//! 10 runs in 10 with the tests in parallel on 2026-10-04, none once
//! serialized), so [`analyze`] holds a process-wide lock for its whole run. The
//! documentation says nothing about this; the lock is cheap, as an analysis is a
//! user's click.
//!
//! Reading another user's threads needs the debug privilege; without it their
//! nodes come back as `NoAccess`, which the model keeps rather than hiding. The
//! process behind the key is opened first, so a recycled PID is refused as the
//! other actions refuse it. A thread node names its process by image, read once
//! per PID through a limited-rights handle.
//!
//! Cost: a few hundred microseconds per thread on this machine; a process with a
//! hundred threads takes tens of milliseconds, which is why the shell runs this on
//! a worker thread.

use std::collections::HashMap;
use std::sync::{Mutex, Once, PoisonError};

use ot_model::process::{ThreadWait, WaitChain, WaitKind, WaitNode, WaitStatus};
use ot_model::ProcessKey;
use windows::core::{s, w, BOOL};
use windows::Win32::System::Diagnostics::Debug::{
    CloseThreadWaitChainSession, GetThreadWaitChain, OpenThreadWaitChainSession,
    RegisterWaitChainCOMCallback, OPEN_THREAD_WAIT_CHAIN_SESSION_FLAGS, PCOGETACTIVATIONSTATE,
    PCOGETCALLSTATE, WAITCHAIN_NODE_INFO, WAIT_CHAIN_THREAD_OPTIONS, WCT_MAX_NODE_COUNT,
    WCT_NETWORK_IO_FLAG, WCT_OBJECT_STATUS, WCT_OBJECT_TYPE, WCT_OUT_OF_PROC_COM_FLAG,
    WCT_OUT_OF_PROC_CS_FLAG, WCT_OUT_OF_PROC_FLAG,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use super::control::{image_stem, open, os};
use crate::ControlError;

/// Follow waits into other processes, through critical sections, COM calls and
/// network I/O alike.
const FOLLOW_EVERYTHING: WAIT_CHAIN_THREAD_OPTIONS = WAIT_CHAIN_THREAD_OPTIONS(
    WCT_OUT_OF_PROC_FLAG.0
        | WCT_OUT_OF_PROC_COM_FLAG.0
        | WCT_OUT_OF_PROC_CS_FLAG.0
        | WCT_NETWORK_IO_FLAG,
);

/// A session handle, closed on drop.
struct Session(*mut core::ffi::c_void);

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: opened by OpenThreadWaitChainSession, closed once.
        unsafe { CloseThreadWaitChainSession(self.0) };
    }
}

/// Hand WCT ole32's COM state functions, once per process, so COM waits are
/// followed. Missing exports leave COM unresolved, which is not an error.
fn register_com_callbacks() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: loading a system DLL by name; the module stays loaded for the
        // life of the process, as the callbacks must.
        let Ok(ole32) = (unsafe { LoadLibraryW(w!("ole32.dll")) }) else {
            tracing::debug!("ole32 did not load; COM waits are not followed");
            return;
        };
        // SAFETY: names of exported functions; null when absent.
        let (call, activation) = unsafe {
            (
                GetProcAddress(ole32, s!("CoGetCallState")),
                GetProcAddress(ole32, s!("CoGetActivationState")),
            )
        };
        let (Some(call), Some(activation)) = (call, activation) else {
            tracing::debug!("ole32 lacks the COM state exports; COM waits are not followed");
            return;
        };
        // SAFETY: both are the functions WCT documents for these slots, whose
        // signatures `PCOGETCALLSTATE` and `PCOGETACTIVATIONSTATE` describe.
        unsafe {
            let call: PCOGETCALLSTATE = Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(i32, *mut u32) -> windows::core::HRESULT,
            >(call));
            let activation: PCOGETACTIVATIONSTATE = Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(
                    windows::core::GUID,
                    u32,
                    *mut u32,
                ) -> windows::core::HRESULT,
            >(activation));
            RegisterWaitChainCOMCallback(call, activation);
        }
    });
}

/// The wait chain of each of `threads`, which belong to the process behind `key`.
pub(super) fn analyze(key: ProcessKey, threads: &[u32]) -> Result<WaitChain, ControlError> {
    // See the module notes: the API is not safe to use from two threads at once.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    register_com_callbacks();
    // Confirms the process is still the one the key names.
    let _process = open(key, PROCESS_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: a synchronous session needs no callback.
    let session =
        unsafe { OpenThreadWaitChainSession(OPEN_THREAD_WAIT_CHAIN_SESSION_FLAGS(0), None) };
    if session.is_null() {
        return Err(os(
            "OpenThreadWaitChainSession",
            windows::core::Error::from_thread(),
        ));
    }
    let session = Session(session);
    let mut names: HashMap<u32, Option<String>> = HashMap::new();
    let mut nodes = [WAITCHAIN_NODE_INFO::default(); WCT_MAX_NODE_COUNT as usize];
    let mut chain = WaitChain::default();
    for &tid in threads {
        let mut count = WCT_MAX_NODE_COUNT;
        let mut cycle = BOOL(0);
        // SAFETY: `nodes` holds `count` entries; the out-pointers are locals.
        let ok = unsafe {
            GetThreadWaitChain(
                session.0,
                None,
                FOLLOW_EVERYTHING,
                tid,
                &raw mut count,
                nodes.as_mut_ptr(),
                &raw mut cycle,
            )
        };
        let mut thread = ThreadWait {
            tid,
            nodes: Vec::new(),
            cycle: false,
        };
        if ok.is_ok() {
            thread.cycle = cycle.as_bool();
            let count = (count as usize).min(nodes.len());
            thread.nodes = nodes[..count]
                .iter()
                .map(|n| node_of(n, &mut names))
                .collect();
        } else {
            tracing::debug!(tid, error = %windows::core::Error::from_thread(), "GetThreadWaitChain failed");
        }
        chain.threads.push(thread);
    }
    Ok(chain)
}

/// One node of the API's array as the model's node.
fn node_of(n: &WAITCHAIN_NODE_INFO, names: &mut HashMap<u32, Option<String>>) -> WaitNode {
    let kind = kind_of(n.ObjectType);
    let status = status_of(n.ObjectStatus);
    let mut node = WaitNode {
        kind,
        status,
        pid: 0,
        tid: 0,
        process: None,
        wait_ms: 0,
        name: String::new(),
    };
    if kind == WaitKind::Thread {
        // SAFETY: a thread node fills the ThreadObject view of the union.
        let t = unsafe { n.Anonymous.ThreadObject };
        node.pid = t.ProcessId;
        node.tid = t.ThreadId;
        node.wait_ms = t.WaitTime;
        node.process.clone_from(
            names
                .entry(t.ProcessId)
                .or_insert_with(|| image_name(t.ProcessId)),
        );
    } else {
        // SAFETY: every other node fills the LockObject view.
        let l = unsafe { n.Anonymous.LockObject };
        let end = l
            .ObjectName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(l.ObjectName.len());
        node.name = String::from_utf16_lossy(&l.ObjectName[..end]);
    }
    node
}

/// The image name of a process, by PID, when it can be opened.
fn image_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: plain call; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let name = image_stem(h, pid);
    // SAFETY: opened above, closed once.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(h);
    }
    Some(name)
}

fn kind_of(t: WCT_OBJECT_TYPE) -> WaitKind {
    match t.0 {
        1 => WaitKind::CriticalSection,
        2 => WaitKind::SendMessage,
        3 => WaitKind::Mutex,
        4 => WaitKind::Alpc,
        5 => WaitKind::Com,
        6 => WaitKind::ThreadWait,
        7 => WaitKind::ProcessWait,
        8 => WaitKind::Thread,
        9 => WaitKind::ComActivation,
        11 => WaitKind::SocketIo,
        12 => WaitKind::SmbIo,
        _ => WaitKind::Unknown,
    }
}

fn status_of(s: WCT_OBJECT_STATUS) -> WaitStatus {
    match s.0 {
        1 => WaitStatus::NoAccess,
        2 => WaitStatus::Running,
        3 => WaitStatus::Blocked,
        4 => WaitStatus::PidOnly,
        5 => WaitStatus::PidOnlyRpcss,
        6 => WaitStatus::Owned,
        7 => WaitStatus::NotOwned,
        8 => WaitStatus::Abandoned,
        10 => WaitStatus::Error,
        _ => WaitStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentThreadId, ReleaseMutex, WaitForSingleObject, INFINITE,
    };

    fn own_key() -> ProcessKey {
        super::super::control::tests::key_of(std::process::id())
    }

    #[test]
    fn the_calling_thread_is_running_and_a_wrong_key_is_refused() {
        // SAFETY: plain call.
        let me = unsafe { GetCurrentThreadId() };
        let chain = analyze(own_key(), &[me]).expect("own process");
        assert_eq!(chain.threads.len(), 1);
        let t = &chain.threads[0];
        assert_eq!(t.tid, me);
        assert!(!t.cycle);
        assert_eq!(t.nodes.len(), 1, "{t:?}");
        assert_eq!(t.nodes[0].kind, WaitKind::Thread);
        assert_eq!(t.nodes[0].status, WaitStatus::Running);
        assert_eq!(t.nodes[0].pid, std::process::id());
        assert!(t.nodes[0].process.as_deref().is_some_and(|n| !n.is_empty()));
        assert_eq!(chain.blocked(), 0);
        assert!(!chain.deadlocked());

        let wrong = ProcessKey::new(std::process::id(), 1);
        assert!(analyze(wrong, &[me]).is_err(), "a recycled key is refused");
    }

    #[test]
    fn a_thread_blocked_on_a_mutex_we_hold_points_back_at_us() {
        // SAFETY: an unnamed mutex, owned by this thread from creation.
        let mutex = unsafe { CreateMutexW(None, true, None) }.expect("mutex");
        let bits = mutex.0 as isize;
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            // SAFETY: plain call.
            let tid = unsafe { GetCurrentThreadId() };
            tx.send(tid).unwrap();
            // SAFETY: a valid mutex handle; returns once the owner releases it.
            unsafe {
                WaitForSingleObject(
                    windows::Win32::Foundation::HANDLE(bits as *mut core::ffi::c_void),
                    INFINITE,
                );
            }
        });
        let tid = rx.recv().unwrap();
        // Let the waiter reach its wait.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let chain = analyze(own_key(), &[tid]).expect("own process");
        let t = &chain.threads[0];
        assert!(
            t.nodes.len() >= 2,
            "the waiter should be seen waiting on something: {t:?}"
        );
        assert_eq!(t.nodes[0].status, WaitStatus::Blocked, "{t:?}");
        assert_eq!(t.nodes[1].kind, WaitKind::Mutex, "{t:?}");
        // The mutex is held by this thread.
        // SAFETY: plain call.
        let me = unsafe { GetCurrentThreadId() };
        assert!(
            t.nodes
                .get(2)
                .is_some_and(|n| n.kind == WaitKind::Thread && n.tid == me),
            "{t:?}"
        );
        assert_eq!(chain.blocked(), 1);
        assert!(!chain.deadlocked());
        // SAFETY: we own the mutex; releasing it frees the waiter.
        unsafe {
            let _ = ReleaseMutex(mutex);
        }
        waiter.join().unwrap();
        // SAFETY: created above, closed once.
        unsafe {
            let _ = CloseHandle(mutex);
        }
    }

    /// Analyses from several threads at once take turns rather than crashing the
    /// process (see the module notes).
    #[test]
    fn concurrent_analyses_take_turns() {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..20 {
                        // SAFETY: plain call.
                        let me = unsafe { GetCurrentThreadId() };
                        let chain = analyze(own_key(), &[me]).expect("own process");
                        assert_eq!(chain.threads.len(), 1);
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
    }

    #[test]
    fn kinds_and_statuses_follow_the_header() {
        assert_eq!(kind_of(WCT_OBJECT_TYPE(3)), WaitKind::Mutex);
        assert_eq!(kind_of(WCT_OBJECT_TYPE(8)), WaitKind::Thread);
        assert_eq!(kind_of(WCT_OBJECT_TYPE(99)), WaitKind::Unknown);
        assert_eq!(status_of(WCT_OBJECT_STATUS(3)), WaitStatus::Blocked);
        assert_eq!(status_of(WCT_OBJECT_STATUS(6)), WaitStatus::Owned);
        assert_eq!(status_of(WCT_OBJECT_STATUS(0)), WaitStatus::Unknown);
    }
}
