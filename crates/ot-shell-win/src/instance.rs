//! One window for Task Manager's stand-in.
//!
//! Task Manager is single-instance: Ctrl+Shift+Esc with it open brings it forward.
//! When Windows starts open-task in its place ([`crate::task_manager`]) and an
//! open-task window is already open, the new process does the same and exits. Any
//! other launch opens a window of its own, as before, so "Run as administrator"
//! beside an unelevated window still works.
//!
//! Every window process holds a named mutex until it ends ([`mark`]), which tells
//! a stand-in that a window exists or is on its way; the stand-in then waits
//! briefly for it to show and asks it, with a registered message, to come forward.
//! The window lets that message through UIPI ([`accept_raise`]), since the stand-in
//! is unelevated (Ctrl+Shift+Esc starts it so) and the window may be elevated.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HWND, LPARAM, WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, FindWindowW, GetWindowThreadProcessId,
    IsWindowVisible, RegisterWindowMessageW, SendMessageTimeoutW, MSGFLT_ALLOW, SMTO_ABORTIFHUNG,
};

/// Held by every process with open-task's window, in this session.
const MUTEX: PCWSTR = w!(r"Local\open-task.window");
/// Asks a window to come forward. It answers 1 if it did.
const RAISE: PCWSTR = w!("open-task.raise");
/// How long a stand-in waits for a window that is still starting to show.
const PATIENCE: Duration = Duration::from_secs(3);
/// How long a window has to answer.
const ANSWER_MS: u32 = 2_000;

/// Whether another window process had marked itself when this one did.
static ANOTHER: OnceLock<bool> = OnceLock::new();

/// Mark this process as one with open-task's window, once; the handle stays open
/// until the process ends. Says whether another such process already exists: one
/// elevated beyond this one's reach counts, since its mutex is there to be refused.
pub(crate) fn mark() -> bool {
    *ANOTHER.get_or_init(|| {
        // SAFETY: a named mutex with default security; the name is a static string.
        // The handle is never closed: holding it is the mark.
        match unsafe { CreateMutexW(None, false, MUTEX) } {
            // SAFETY: read straight after the call that set it.
            Ok(_) => (unsafe { GetLastError() }) == ERROR_ALREADY_EXISTS,
            Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
        }
    })
}

/// The message that asks a window to come forward.
pub(crate) fn raise_message() -> u32 {
    // SAFETY: the name is a static string.
    unsafe { RegisterWindowMessageW(RAISE) }
}

/// Let the raise message reach `hwnd` from less privileged processes.
pub(crate) fn accept_raise(hwnd: HWND, message: u32) {
    // SAFETY: hwnd is valid; no filter details are asked for.
    if let Err(e) = unsafe { ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None) } {
        tracing::warn!(error = %e, "an unelevated Task Manager stand-in cannot raise this window");
    }
}

/// Bring forward the window of class `class` that another process has open,
/// waiting a moment for one that is still starting. Returns whether a window came
/// forward; if not, the caller opens its own.
pub(crate) fn raise_existing(class: PCWSTR) -> bool {
    let message = raise_message();
    let deadline = Instant::now() + PATIENCE;
    loop {
        // SAFETY: the class name is a static string; a null title matches any.
        let found = unsafe { FindWindowW(class, PCWSTR::null()) }
            .ok()
            // SAFETY: plain query of a window handle that may have gone.
            .filter(|&hwnd| unsafe { IsWindowVisible(hwnd) }.as_bool());
        if let Some(hwnd) = found {
            return ask_to_raise(hwnd, message);
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ask_to_raise(hwnd: HWND, message: u32) -> bool {
    let mut pid = 0u32;
    let mut answer = 0usize;
    // SAFETY: plain calls on a window handle; the out-pointers are locals.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        // This process was started by the user's keypress, so it may hand the
        // right to take the foreground to the window's process.
        let _ = AllowSetForegroundWindow(pid);
        let sent = SendMessageTimeoutW(
            hwnd,
            message,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            ANSWER_MS,
            Some(&raw mut answer),
        );
        if sent.0 == 0 {
            tracing::warn!(pid, "the open window did not answer");
            return false;
        }
    }
    answer == 1
}
