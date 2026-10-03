//! Which processes own a window on the desktop, and what it says in its title bar.
//!
//! This is how Task Manager decides what an "app" is: a process is an app when it
//! has a top-level window a person could switch to. The rule for "could switch
//! to" is the taskbar's: the window is visible, has a title, is not a tool window
//! (`WS_EX_TOOLWINDOW`: palettes and the like), has no owner (an owned window is a
//! dialog of its owner, not an app of its own), and is not cloaked. Cloaking is
//! the composition engine hiding a window that is logically visible: every UWP
//! and packaged app keeps a cloaked frame window alive while it is suspended in
//! the background, and on another virtual desktop every window is cloaked. Those
//! are not on anyone's screen, so `DwmGetWindowAttribute(DWMWA_CLOAKED)` is asked
//! and a cloaked window is skipped, the way the taskbar skips it.
//!
//! One `EnumWindows` pass walks every top-level window in Z order, so the first
//! qualifying window of a process is its frontmost one; that is the one reported.
//! The whole refresh, over a desktop of several hundred top-level windows, costs
//! about 0.3 ms (measured on this machine by the ignored `refresh_cost` test), so it
//! runs once per pass. Each process's record is republished by pointer while its
//! handle, title and hung state are unchanged, so a steady desktop allocates
//! nothing; the title is read into a reused UTF-16 buffer and compared in place.
//!
//! "Hung" is `IsHungAppWindow`: the window has not picked up a message in five
//! seconds, which is exactly when Task Manager says "Not responding".

use std::collections::HashMap;
use std::sync::Arc;

use ot_model::process::WindowInfo;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
    IsHungAppWindow, IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

/// Longest title read, in UTF-16 units. Windows itself caps titles far below this.
const MAX_TITLE: usize = 1024;

/// One process's window and the refresh that last saw it.
#[derive(Debug)]
struct Entry {
    info: Arc<WindowInfo>,
    seen: u64,
}

/// The desktop's app windows by owning PID. One per probe, refreshed once per pass.
#[derive(Debug, Default)]
pub(super) struct WindowList {
    by_pid: HashMap<u32, Entry>,
    /// Refresh counter, so entries not seen this time can be dropped.
    refresh: u64,
    /// UTF-16 scratch for titles.
    title: Vec<u16>,
}

impl WindowList {
    pub fn new() -> Self {
        Self {
            by_pid: HashMap::with_capacity(64),
            refresh: 0,
            title: vec![0; MAX_TITLE],
        }
    }

    /// Walk the desktop again. Processes whose window went away are dropped;
    /// unchanged windows keep their `Arc`.
    pub fn refresh(&mut self) {
        self.refresh += 1;
        // SAFETY: the callback is given a pointer to `self` that lives for the
        // duration of the call; EnumWindows calls it synchronously on this thread
        // and never after it returns. The pointer is not held anywhere else.
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(std::ptr::from_mut(self) as isize)) };
        let refresh = self.refresh;
        self.by_pid.retain(|_, e| e.seen == refresh);
    }

    /// The frontmost app window of `pid`, if it has one.
    pub fn window_of(&self, pid: u32) -> Option<Arc<WindowInfo>> {
        self.by_pid.get(&pid).map(|e| Arc::clone(&e.info))
    }

    /// How many processes have an app window.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_pid.len()
    }

    /// Consider one top-level window, in Z order from the front.
    fn consider(&mut self, hwnd: HWND) {
        if !is_app_window(hwnd) {
            return;
        }
        let mut pid = 0u32;
        // SAFETY: `pid` is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        if pid == 0 {
            return;
        }
        // The first qualifying window is the frontmost; a later one is behind it.
        if self
            .by_pid
            .get(&pid)
            .is_some_and(|e| e.seen == self.refresh)
        {
            return;
        }
        // SAFETY: the buffer is `MAX_TITLE` units; the call writes at most that,
        // NUL-terminated, and returns the length without the terminator.
        let n = unsafe { GetWindowTextW(hwnd, &mut self.title) };
        let Ok(n) = usize::try_from(n) else {
            return;
        };
        if n == 0 {
            return;
        }
        let title = &self.title[..n];
        // SAFETY: plain call on a window handle; a stale handle just reports false.
        let hung = unsafe { IsHungAppWindow(hwnd) }.as_bool();
        let handle = hwnd.0 as usize as u64;
        let refresh = self.refresh;
        match self.by_pid.get_mut(&pid) {
            Some(e)
                if e.info.handle == handle
                    && e.info.hung == hung
                    && e.info.title.encode_utf16().eq(title.iter().copied()) =>
            {
                e.seen = refresh;
            }
            Some(e) => {
                e.info = Arc::new(WindowInfo {
                    handle,
                    title: String::from_utf16_lossy(title),
                    hung,
                });
                e.seen = refresh;
            }
            None => {
                self.by_pid.insert(
                    pid,
                    Entry {
                        info: Arc::new(WindowInfo {
                            handle,
                            title: String::from_utf16_lossy(title),
                            hung,
                        }),
                        seen: refresh,
                    },
                );
            }
        }
    }
}

/// The `EnumWindows` callback: `lparam` is the `WindowList` being refreshed.
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `refresh` passed a pointer to a live `&mut WindowList` and is blocked
    // in EnumWindows until the enumeration ends, so no other reference exists.
    let list = unsafe { &mut *(lparam.0 as *mut WindowList) };
    list.consider(hwnd);
    BOOL(1)
}

/// The taskbar's rule, without the title check (the caller reads the title into
/// its own buffer): visible, not a tool window, unowned, and not cloaked.
fn is_app_window(hwnd: HWND) -> bool {
    // SAFETY: plain calls on a window handle; a window that closed mid-walk just
    // reports false, zero or an error, each of which disqualifies it.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null()) {
            return false;
        }
        let mut cloaked = 0u32;
        let r = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        !(r.is_ok() && cloaked != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_process_has_no_window_and_something_does() {
        let mut list = WindowList::new();
        list.refresh();
        assert!(list.window_of(std::process::id()).is_none());
        // A desktop with a signed-in user has at least one app window (the
        // terminal this runs in, if nothing else); in a session with no desktop
        // there is nothing to check.
        if list.len() == 0 {
            eprintln!("no app windows on this desktop; skipping");
            return;
        }
        let found = list.by_pid.values().next().expect("one window");
        assert!(found.info.title.chars().next().is_some(), "{found:?}");
        assert_ne!(found.info.handle, 0);
    }

    #[test]
    fn unchanged_windows_keep_their_arc() {
        let mut list = WindowList::new();
        list.refresh();
        let before: Vec<(u32, Arc<WindowInfo>)> = list
            .by_pid
            .iter()
            .map(|(pid, e)| (*pid, Arc::clone(&e.info)))
            .collect();
        list.refresh();
        let mut unchanged = 0;
        for (pid, old) in &before {
            if let Some(now) = list.window_of(*pid) {
                if **old == *now {
                    assert!(Arc::ptr_eq(old, &now), "{old:?} was reallocated");
                    unchanged += 1;
                }
            }
        }
        assert!(
            before.is_empty() || unchanged > 0,
            "every window changed between two refreshes"
        );
    }

    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn refresh_cost() {
        let mut list = WindowList::new();
        list.refresh();
        let start = std::time::Instant::now();
        let n = 200;
        for _ in 0..n {
            list.refresh();
        }
        println!(
            "refresh mean {:.3} ms over {} app windows",
            start.elapsed().as_secs_f64() * 1e3 / f64::from(n),
            list.len()
        );
    }
}
