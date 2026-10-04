//! What the view asks the shell to do with native means, beyond ending a process:
//! menus with submenus and check marks, the Run dialog, relaunching elevated, the
//! clipboard, Explorer's Properties sheet, bringing a window forward, the actions
//! on services, sessions and startup entries, and the lists read off the UI
//! thread.
//!
//! Anything that can block (stopping a service waits for it; a dump of a large
//! process takes seconds; reading the installed programs walks a thousand registry
//! keys) runs on a worker thread and reports back to the window with a posted
//! message, the way CPU sampling does, so the table keeps moving meanwhile.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::path::PathBuf;

use ot_model::process::{WaitChain, WaitKind, WaitStatus};
use ot_model::ProcessKey;
use ot_probe::{
    Affinity, ControlError, Inventory as _, PlatformControl, ProcessControl, ServiceControl,
    SessionControl, StartupControl,
};
use ot_ui::{Inventory, MenuAction, MenuEntry, ProcessAction, Query, ServiceAction, SessionAction};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, ShellExecuteW, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetAncestor, GetWindowThreadProcessId, IsIconic,
    PostMessageW, SetForegroundWindow, ShowWindow, TrackPopupMenuEx, WindowFromPoint, GA_ROOT,
    MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, SW_RESTORE,
    SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
};

/// `CF_UNICODETEXT`.
const CF_UNICODETEXT: u32 = 13;

/// What a worker thread hands back to the window: what was attempted, in words
/// for a message box, and how it went.
pub(crate) struct ActionOutcome {
    /// `end chrome.exe`, `stop the service Spooler`.
    pub what: String,
    pub result: Result<Option<String>, String>,
}

/// Build and show a popup menu from the view's entries, submenus and check marks
/// included. Blocks until the user picks or dismisses; messages keep flowing
/// meanwhile, so the table under it stays live. Returns the chosen action.
pub(crate) fn show_menu(
    hwnd: HWND,
    at: windows::Win32::Foundation::POINT,
    entries: &[MenuEntry],
    control: PlatformControl,
) -> Option<MenuAction> {
    // Item ids are 1-based indices into `actions`; 0 means dismissed.
    let mut actions: Vec<MenuAction> = Vec::new();
    // SAFETY: the menu tree is destroyed before returning; AppendMenuW copies
    // labels, and a submenu handed to MF_POPUP is owned by its parent from then on.
    let chosen = unsafe {
        let menu = build_menu(entries, &mut actions, control)?;
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let chosen = TrackPopupMenuEx(menu, flags.0, at.x, at.y, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0
    };
    let index = usize::try_from(chosen).ok()?.checked_sub(1)?;
    actions.get(index).copied()
}

/// One level of the menu. `actions` collects every item's action; an item's id is
/// its position there plus one.
///
/// # Safety
/// The caller destroys the returned menu (which destroys the submenus).
unsafe fn build_menu(
    entries: &[MenuEntry],
    actions: &mut Vec<MenuAction>,
    control: PlatformControl,
) -> Option<windows::Win32::UI::WindowsAndMessaging::HMENU> {
    // SAFETY: plain call; the caller owns the result.
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    for e in entries {
        let r = match e {
            MenuEntry::Item {
                action,
                label,
                enabled,
                checked,
            } => {
                actions.push(*action);
                let mut flags = MF_STRING;
                if !enabled {
                    flags |= MF_GRAYED;
                }
                if *checked {
                    flags |= MF_CHECKED;
                }
                // SAFETY: the label outlives the call, which copies it.
                unsafe { AppendMenuW(menu, flags, actions.len(), &HSTRING::from(label.as_ref())) }
            }
            MenuEntry::Submenu { label, entries } => {
                // SAFETY: as for this function; the submenu is owned by `menu`.
                let Some(sub) = (unsafe { build_menu(entries, actions, control) }) else {
                    continue;
                };
                // SAFETY: a popup handle is passed in the id slot, as MF_POPUP asks.
                unsafe { AppendMenuW(menu, MF_POPUP, sub.0 as usize, &HSTRING::from(*label)) }
            }
            MenuEntry::Affinity { target } => {
                let affinity = control.affinity(*target);
                let known = affinity.is_ok();
                // SAFETY: as above.
                let Some(sub) = (unsafe { affinity_menu(&affinity, actions) }) else {
                    continue;
                };
                let flags = if known {
                    MF_POPUP
                } else {
                    MF_POPUP | MF_GRAYED
                };
                // SAFETY: as above.
                unsafe { AppendMenuW(menu, flags, sub.0 as usize, w!("Set affinity")) }
            }
            // SAFETY: a separator has no label.
            MenuEntry::Separator => unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) },
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "AppendMenuW");
        }
    }
    Some(menu)
}

/// The affinity submenu: "All processors", then one check item per logical
/// processor the system has. Choosing one toggles that processor in the mask;
/// the resulting mask is what the action carries.
///
/// # Safety
/// As for [`build_menu`].
unsafe fn affinity_menu(
    affinity: &Result<Affinity, ControlError>,
    actions: &mut Vec<MenuAction>,
) -> Option<windows::Win32::UI::WindowsAndMessaging::HMENU> {
    // SAFETY: plain call; owned by the caller's menu.
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    let Ok(a) = affinity else {
        return Some(menu);
    };
    let all = a.mask == a.system;
    actions.push(MenuAction::SetAffinity(a.system));
    let flags = if all {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    // SAFETY: static label.
    let _ = unsafe { AppendMenuW(menu, flags, actions.len(), w!("All processors")) };
    // SAFETY: no label.
    let _ = unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) };
    let mut label = String::new();
    for cpu in 0..64u32 {
        let bit = 1u64 << cpu;
        if a.system & bit == 0 {
            continue;
        }
        let on = a.mask & bit != 0;
        // Toggling the last processor would leave nothing to run on; the menu
        // offers it anyway and the action refuses an empty mask.
        let next = a.mask ^ bit;
        actions.push(MenuAction::SetAffinity(next));
        let flags: MENU_ITEM_FLAGS = if on {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        label.clear();
        let _ = write!(label, "CPU {cpu}");
        // SAFETY: the label outlives the call, which copies it.
        let _ = unsafe { AppendMenuW(menu, flags, actions.len(), &HSTRING::from(label.as_str())) };
    }
    Some(menu)
}

/// Carry out a process action through the platform's control, right away. The
/// quick ones; a dump goes through [`spawn_action`] instead.
pub(crate) fn process_action(
    control: PlatformControl,
    target: ProcessKey,
    action: &ProcessAction,
) -> Result<(), ControlError> {
    match action {
        ProcessAction::SetPriority(p) => control.set_priority(target, *p),
        ProcessAction::SetAffinity(mask) => control.set_affinity(target, *mask),
        ProcessAction::Suspend => control.suspend(target),
        ProcessAction::Resume => control.resume(target),
        ProcessAction::EfficiencyMode(on) => control.set_efficiency_mode(target, *on),
        ProcessAction::WriteDump
        | ProcessAction::Restart { .. }
        | ProcessAction::WaitChain { .. } => Ok(()),
    }
}

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

/// Run `work` on a worker thread and post its outcome to `hwnd` as `message`,
/// with a `Box<ActionOutcome>` in `lparam`. `what` names the attempt for the
/// message box if it fails.
pub(crate) fn spawn_action(
    hwnd: HWND,
    message: u32,
    what: String,
    work: impl FnOnce() -> Result<Option<String>, String> + Send + 'static,
) {
    let hwnd_bits = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("ot-action".into())
        .spawn(move || {
            let result = work();
            post_box(hwnd_bits, message, Box::new(ActionOutcome { what, result }));
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start a worker thread");
    }
}

/// Read a list on a worker thread and post it back as `message` with a
/// `Box<Inventory>` in `lparam`.
pub(crate) fn spawn_query(hwnd: HWND, message: u32, query: Query) {
    let hwnd_bits = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("ot-inventory".into())
        .spawn(move || {
            let control = PlatformControl;
            let inventory = match query {
                Query::Startup => Inventory::Startup(control.startup_entries()),
                Query::InstalledApps => Inventory::InstalledApps(control.installed_apps()),
                Query::Connections => Inventory::Connections(control.connections()),
                Query::System => Inventory::System(Box::new(control.system_facts())),
            };
            post_box(hwnd_bits, message, Box::new(inventory));
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start the inventory thread");
    }
}

/// Post a boxed value to a window; if the window is gone the box is reclaimed.
fn post_box<T>(hwnd_bits: isize, message: u32, value: Box<T>) {
    let ptr = Box::into_raw(value);
    // SAFETY: posting to a window handle is thread-safe. If the post fails the
    // message was not delivered and the box is still ours to free.
    let posted = unsafe {
        PostMessageW(
            Some(HWND(hwnd_bits as *mut c_void)),
            message,
            WPARAM(0),
            LPARAM(ptr as isize),
        )
    };
    if posted.is_err() {
        // SAFETY: not delivered, so this is the only owner.
        drop(unsafe { Box::from_raw(ptr) });
    }
}

/// Start, stop or restart a service, blocking; for a worker thread.
pub(crate) fn service_action(name: &str, action: ServiceAction) -> Result<(), ControlError> {
    let control = PlatformControl;
    match action {
        ServiceAction::Start => control.start_service(name),
        ServiceAction::Stop => control.stop_service(name),
        ServiceAction::Restart => {
            control.stop_service(name)?;
            control.start_service(name)
        }
    }
}

pub(crate) fn session_action(id: u32, action: SessionAction) -> Result<(), ControlError> {
    let control = PlatformControl;
    match action {
        SessionAction::Disconnect => control.disconnect_session(id),
        SessionAction::SignOut => control.logoff_session(id),
    }
}

pub(crate) fn startup_action(
    entry: &ot_model::startup::StartupEntry,
    on: bool,
) -> Result<(), ControlError> {
    PlatformControl.set_startup_enabled(entry, on)
}

/// Write a dump of `target` into the temporary directory; for a worker thread.
/// Returns the file's path for the message.
pub(crate) fn write_dump(target: ProcessKey) -> Result<Option<String>, String> {
    let dir = std::env::temp_dir();
    PlatformControl
        .write_dump(target, &dir)
        .map(|p| Some(p.display().to_string()))
        .map_err(|e| e.to_string())
}

/// Analyze the wait chain of `threads` of the process behind `target`, blocking;
/// for a worker thread. The result is a report for a message box.
pub(crate) fn wait_chain(
    target: ProcessKey,
    name: &str,
    threads: &[u32],
) -> Result<Option<String>, String> {
    let chain = PlatformControl
        .wait_chain(target, threads)
        .map_err(|e| e.to_string())?;
    Ok(Some(wait_chain_report(name, target.pid, &chain)))
}

/// The wait chain as text: a line per thread that waits on something nameable,
/// the chain written left to right, and a verdict first.
fn wait_chain_report(name: &str, pid: u32, chain: &WaitChain) -> String {
    use std::fmt::Write as _;
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

/// The Save dialog for a Flight Recorder file: the path chosen, or `None` if the
/// user cancelled. Modal; pumps messages, so call it with no state borrowed.
pub(crate) fn save_recording_dialog(owner: HWND) -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut file = [0u16; 4096];
    let default: Vec<u16> = "open-task.otrec".encode_utf16().collect();
    file[..default.len()].copy_from_slice(&default);
    // Pairs of description and pattern, each NUL-terminated, then an empty one.
    let filter: Vec<u16> = "Flight Recorder (*.otrec)\0*.otrec\0All files\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: w!("Record to"),
        lpstrDefExt: w!("otrec"),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    // SAFETY: every pointer in `ofn` references a local that outlives the call.
    if !unsafe { GetSaveFileNameW(&raw mut ofn) }.as_bool() {
        return None;
    }
    let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(String::from_utf16_lossy(&file[..end]))
}

/// Start `command_line` as a new process, in `directory` when given, with this
/// process's rights. For Restart, the Run dialog and uninstallers.
pub(crate) fn launch(command_line: &str, directory: Option<&str>) -> Result<(), String> {
    let mut cmd: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
    let dir = directory.map(HSTRING::from);
    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: the command line is a writable NUL-terminated buffer, as the API
    // requires; the directory string outlives the call; the handles returned are
    // closed below.
    let r = unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &raw const si,
            &raw mut pi,
        )
    };
    match r {
        Ok(()) => {
            // SAFETY: both handles came from CreateProcessW and are closed once.
            unsafe {
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(pi.hThread);
            }
            Ok(())
        }
        Err(e) => Err(e.message()),
    }
}

/// Start `file` with `parameters` through the shell: as administrator when
/// `elevated`, which asks for permission, or as the user. A document or a URL
/// opens in its program, as the Run dialog does.
pub(crate) fn shell_start(
    hwnd: HWND,
    file: &str,
    parameters: &str,
    directory: Option<&str>,
    elevated: bool,
) -> Result<(), String> {
    let file = HSTRING::from(file);
    let params = HSTRING::from(parameters);
    let dir = directory.map(HSTRING::from);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        hwnd,
        lpVerb: if elevated { w!("runas") } else { w!("open") },
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the structure is fully initialized.
    unsafe { ShellExecuteExW(&raw mut info) }.map_err(|e| e.message())
}

/// Start another copy of this program as administrator with the same arguments.
/// Returns whether it started (a declined prompt is `Ok(false)`).
pub(crate) fn relaunch_elevated(hwnd: HWND) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut params = String::new();
    for a in &args {
        if !params.is_empty() {
            params.push(' ');
        }
        if a.contains(' ') {
            let _ = write!(params, "\"{a}\"");
        } else {
            params.push_str(a);
        }
    }
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(params);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        hwnd,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the structure is fully initialized.
    match unsafe { ShellExecuteExW(&raw mut info) } {
        Ok(()) => {
            if !info.hProcess.is_invalid() {
                // SAFETY: ours through SEE_MASK_NOCLOSEPROCESS; closed once.
                unsafe {
                    let _ = CloseHandle(info.hProcess);
                }
            }
            Ok(true)
        }
        // ERROR_CANCELLED: the prompt was declined.
        Err(e) if e.code().0 as u32 == 0x8007_04C7 => Ok(false),
        Err(e) => Err(e.message()),
    }
}

/// Explorer's Properties sheet for a file.
pub(crate) fn properties(hwnd: HWND, path: &str) {
    let file = HSTRING::from(path);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd,
        lpVerb: w!("properties"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the string outlives the call; the structure is fully initialized.
    if let Err(e) = unsafe { ShellExecuteExW(&raw mut info) } {
        tracing::warn!(path, error = %e, "properties sheet");
    }
}

/// The default browser at `url`, or the default program for a file.
pub(crate) fn open(hwnd: HWND, target: &str) {
    // SAFETY: strings outlive the call; hwnd is valid.
    let r = unsafe {
        ShellExecuteW(
            Some(hwnd),
            w!("open"),
            &HSTRING::from(target),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values up to 32 are error codes, by the API's odd convention.
    if r.0 as usize <= 32 {
        tracing::warn!(target, code = r.0 as usize, "ShellExecuteW open failed");
    }
}

/// Put `text` on the clipboard as Unicode text.
pub(crate) fn copy_text(hwnd: HWND, text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes = wide.len() * 2;
    // SAFETY: the clipboard is opened and closed in this function; the global
    // block is filled while locked and then handed to the clipboard, which owns it
    // from then on (it is not freed here).
    unsafe {
        OpenClipboard(Some(hwnd)).map_err(|e| e.message())?;
        let result = (|| {
            EmptyClipboard().map_err(|e| e.message())?;
            let block: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.message())?;
            let dst = GlobalLock(block);
            if dst.is_null() {
                return Err("GlobalLock failed".to_owned());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), dst.cast::<u8>(), bytes);
            let _ = GlobalUnlock(block);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(block.0)))
                .map(|_| ())
                .map_err(|e| e.message())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Bring a top-level window to the front, restoring it if minimized.
pub(crate) fn switch_to(handle: u64) {
    let hwnd = HWND(handle as usize as *mut c_void);
    // SAFETY: a stale handle makes these calls fail harmlessly.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// The process that owns the top-level window at a screen point, for the
/// crosshair. `None` for the desktop, or for this process's own window.
pub(crate) fn pid_at(screen: windows::Win32::Foundation::POINT) -> Option<u32> {
    // SAFETY: plain calls on handles the system returns.
    unsafe {
        let hit = WindowFromPoint(screen);
        if hit.is_invalid() {
            return None;
        }
        let root = GetAncestor(hit, GA_ROOT);
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&raw mut pid));
        (pid != 0 && pid != std::process::id()).then_some(pid)
    }
}

/// Split a command line as typed into the Run dialog into the file to start and
/// its parameters: a quoted first token, or everything up to the first space.
pub(crate) fn split_command(line: &str) -> (String, String) {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_owned(), rest[end + 1..].trim().to_owned());
        }
        return (rest.to_owned(), String::new());
    }
    match line.find(' ') {
        Some(i) => (line[..i].to_owned(), line[i + 1..].trim().to_owned()),
        None => (line.to_owned(), String::new()),
    }
}

/// The folder an installed program's uninstaller or location names, for "Open
/// install location": the recorded location, else the folder of the first path
/// in the uninstall string.
pub(crate) fn install_folder(location: Option<&str>, uninstall: Option<&str>) -> Option<PathBuf> {
    if let Some(l) = location.filter(|l| !l.trim().is_empty()) {
        return Some(PathBuf::from(l.trim().trim_matches('"')));
    }
    let (file, _) = split_command(uninstall?);
    let p = PathBuf::from(file);
    p.parent().map(PathBuf::from).filter(|d| d.is_dir())
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
        let text = wait_chain_report("app.exe", 7, &chain);
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
        let text = wait_chain_report("app.exe", 7, &quiet);
        assert!(text.contains("none of its 1 threads is waiting"), "{text}");
    }

    #[test]
    fn command_lines_split_at_the_first_token() {
        assert_eq!(
            split_command(r#""C:\Program Files\x\y.exe" -a b"#),
            (r"C:\Program Files\x\y.exe".to_owned(), "-a b".to_owned())
        );
        assert_eq!(
            split_command("notepad.exe  C:\\a.txt"),
            ("notepad.exe".to_owned(), "C:\\a.txt".to_owned())
        );
        assert_eq!(split_command("  cmd "), ("cmd".to_owned(), String::new()));
    }

    #[test]
    fn install_folders_come_from_the_location_or_the_uninstaller() {
        assert_eq!(
            install_folder(Some(" \"C:\\x\" "), None),
            Some(PathBuf::from("C:\\x"))
        );
        let windows = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let uninstall = format!("\"{windows}\\notepad.exe\" /u");
        assert_eq!(
            install_folder(None, Some(&uninstall)),
            Some(PathBuf::from(&windows))
        );
        assert_eq!(install_folder(None, Some("nowhere.exe")), None);
    }

    #[test]
    fn the_clipboard_takes_text() {
        // No window: the clipboard is opened for the thread.
        copy_text(HWND::default(), "open-task test").expect("clipboard");
    }
}
