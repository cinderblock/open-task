//! What the shell does with native means: menus with submenus and check marks,
//! the Save dialog, starting programs, relaunching elevated, the clipboard,
//! Explorer's Properties sheet, and bringing a window forward. Everything that
//! needs no window (ending processes, services, sessions, the lists read off
//! the UI thread) is `ot_shell_core`'s.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::path::PathBuf;

use ot_shell_core::MenuNode;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::HBITMAP;
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
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetAncestor, GetMenuItemCount,
    GetWindowThreadProcessId, IsIconic, SetForegroundWindow, SetMenuItemInfoW, ShowWindow,
    TrackPopupMenuEx, WindowFromPoint, GA_ROOT, HMENU, MENUITEMINFOW, MF_BITMAP, MF_CHECKED,
    MF_DISABLED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MIIM_BITMAP, SW_RESTORE,
    SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
};

use crate::menu_art;

/// `CF_UNICODETEXT`.
const CF_UNICODETEXT: u32 = 13;

/// Starting programs, for `ot_shell_core`'s Restart and uninstallers.
#[derive(Debug)]
pub(crate) struct WinPlatform;

impl ot_shell_core::Platform for WinPlatform {
    fn launch(&self, command_line: &str, directory: Option<&str>) -> Result<(), String> {
        launch(command_line, directory)
    }
}

/// Show a popup menu built from `nodes`, submenus and check marks included,
/// each item with its icon and a picture of a [`MenuNode::Header`] on top
/// ([`crate::menu_art`]), at `dpi`, dark or light. Blocks until the user picks or
/// dismisses; messages keep flowing meanwhile, so the table under it stays live.
/// Returns the chosen item's id.
pub(crate) fn show_menu(
    hwnd: HWND,
    at: windows::Win32::Foundation::POINT,
    nodes: &[MenuNode],
    dpi: u32,
    dark: bool,
) -> Option<usize> {
    // The header's picture, which must outlive the menu.
    let header = nodes.iter().find_map(|n| match n {
        MenuNode::Header(h) => {
            menu_art::header_bitmap(&h.title, &h.lines, h.path.as_deref(), dpi, dark)
        }
        _ => None,
    });
    let look = Look {
        dpi,
        dark,
        header: header.as_ref().map(|h| h.0),
    };
    // SAFETY: the menu tree is destroyed before returning, and the header with
    // it; AppendMenuW copies labels, and a submenu handed to MF_POPUP is owned by
    // its parent from then on.
    let chosen = unsafe {
        let menu = build_menu(nodes, &look)?;
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let chosen = TrackPopupMenuEx(menu, flags.0, at.x, at.y, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0
    };
    drop(header);
    // Command ids are item ids plus one; 0 means dismissed.
    usize::try_from(chosen).ok()?.checked_sub(1)
}

/// How a menu is drawn: at what DPI, dark or not, and its header's picture.
struct Look {
    dpi: u32,
    dark: bool,
    header: Option<HBITMAP>,
}

/// One level of the menu.
///
/// # Safety
/// The caller destroys the returned menu (which destroys the submenus), and keeps
/// `look.header` alive until then.
unsafe fn build_menu(nodes: &[MenuNode], look: &Look) -> Option<HMENU> {
    // SAFETY: plain call; the caller owns the result.
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    for node in nodes {
        let (r, icon) = match node {
            MenuNode::Item {
                id,
                label,
                enabled,
                checked,
                icon,
            } => {
                let mut flags = MF_STRING;
                if !enabled {
                    flags |= MF_GRAYED;
                }
                if *checked {
                    flags |= MF_CHECKED;
                }
                // SAFETY: the label outlives the call, which copies it.
                let r = unsafe { AppendMenuW(menu, flags, id + 1, &HSTRING::from(label.as_str())) };
                // The icon and the check share a place: a checked item shows its check.
                (r, icon.filter(|_| !checked).map(|i| (i, *enabled)))
            }
            MenuNode::Submenu {
                label,
                enabled,
                icon,
                children,
            } => {
                // SAFETY: as for this function; the submenu is owned by `menu`.
                let Some(sub) = (unsafe { build_menu(children, look) }) else {
                    continue;
                };
                let flags = if *enabled {
                    MF_POPUP
                } else {
                    MF_POPUP | MF_GRAYED
                };
                // SAFETY: a popup handle is passed in the id slot, as MF_POPUP asks.
                let r = unsafe {
                    AppendMenuW(menu, flags, sub.0 as usize, &HSTRING::from(label.as_str()))
                };
                (r, icon.map(|i| (i, *enabled)))
            }
            // SAFETY: a separator has no label.
            MenuNode::Separator => (
                unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) },
                None,
            ),
            MenuNode::Header(_) => {
                let Some(h) = look.header else {
                    continue;
                };
                // A picture, never chosen: disabled, but not grayed (it has its
                // own colors), with no id an item has.
                // SAFETY: a bitmap item takes the bitmap's handle as its "text";
                // the bitmap outlives the menu.
                let r = unsafe {
                    AppendMenuW(menu, MF_BITMAP | MF_DISABLED, 0, PCWSTR(h.0 as *const u16))
                };
                (r, None)
            }
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "AppendMenuW");
            continue;
        }
        let Some(bitmap) =
            icon.and_then(|(i, on)| menu_art::icon_bitmap(i, look.dpi, look.dark, on))
        else {
            continue;
        };
        let info = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_BITMAP,
            hbmpItem: bitmap,
            ..Default::default()
        };
        // SAFETY: the item was just appended, last; the bitmap stays alive in
        // `menu_art`'s cache.
        unsafe {
            let last = GetMenuItemCount(Some(menu)) - 1;
            if let Err(e) = SetMenuItemInfoW(menu, last as u32, true, &raw const info) {
                tracing::warn!(error = %e, "SetMenuItemInfoW");
            }
        }
    }
    Some(menu)
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
