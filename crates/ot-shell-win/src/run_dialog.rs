//! The Run dialog: "Run new task", as Task Manager has it.
//!
//! A native modal dialog built from an in-memory template (so it needs no resource
//! file): a label, a text field for the command line, a Browse button, a check box
//! for starting it as administrator, OK and Cancel. The command is parsed and
//! started by the caller; this module only asks.
//!
//! Windows' own Run dialog (`RunFileDlg`, shell32 ordinal 61) would have done the
//! typing part, but it has no "as administrator" choice and starts the program
//! itself, so the elevated case could not be offered. Task Manager draws its own
//! for the same reason.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{IsDlgButtonChecked, EM_SETSEL};
use windows::Win32::UI::WindowsAndMessaging::{
    DialogBoxIndirectParamW, EndDialog, GetDlgItemTextW, GetWindowRect, SendDlgItemMessageW,
    SetDlgItemTextW, SetWindowPos, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    DLGTEMPLATE, DS_CENTER, DS_MODALFRAME, DS_SETFONT, ES_AUTOHSCROLL, HWND_TOP, SWP_NOSIZE,
    SWP_NOZORDER, WM_COMMAND, WM_INITDIALOG, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

/// What the user asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRequest {
    pub command: String,
    pub elevated: bool,
}

const ID_COMMAND: i32 = 100;
const ID_ELEVATED: i32 = 101;
const ID_BROWSE: i32 = 102;
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;
/// Control classes by ordinal, as a dialog template names them.
const CLASS_BUTTON: u16 = 0x0080;
const CLASS_EDIT: u16 = 0x0081;
const CLASS_STATIC: u16 = 0x0082;

thread_local! {
    /// What the dialog is for and what it produced: set before the dialog opens,
    /// read after it closes. The dialog is modal on the UI thread, so one slot
    /// suffices.
    static STATE: RefCell<DialogState> = RefCell::new(DialogState::default());
}

#[derive(Debug, Default)]
struct DialogState {
    /// The command shown when the dialog opens: what was run last.
    initial: String,
    result: Option<RunRequest>,
}

/// Show the dialog over `owner` and wait. `None` if cancelled.
pub(crate) fn ask(owner: HWND, initial: &str) -> Option<RunRequest> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        initial.clone_into(&mut s.initial);
        s.result = None;
    });
    let template = template();
    // SAFETY: the template is a valid DLGTEMPLATE block that outlives the call;
    // the procedure is this module's; the module handle is ours.
    let r = unsafe {
        let hinstance = GetModuleHandleW(None).ok();
        DialogBoxIndirectParamW(
            hinstance.map(windows::Win32::Foundation::HINSTANCE::from),
            template.as_ptr().cast::<DLGTEMPLATE>(),
            Some(owner),
            Some(dialog_proc),
            LPARAM(0),
        )
    };
    if r <= 0 {
        return None;
    }
    STATE.with(|s| s.borrow_mut().result.take())
}

/// The dialog's procedure: fill the field on open, read it on OK.
unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    _lparam: LPARAM,
) -> isize {
    match msg {
        WM_INITDIALOG => {
            let initial = STATE.with(|s| s.borrow().initial.clone());
            // SAFETY: hwnd is the dialog; strings outlive the calls.
            unsafe {
                let _ = SetDlgItemTextW(
                    hwnd,
                    ID_COMMAND,
                    &windows::core::HSTRING::from(initial.as_str()),
                );
                // Select the text so typing replaces it.
                SendDlgItemMessageW(hwnd, ID_COMMAND, EM_SETSEL, WPARAM(0), LPARAM(-1));
                let _ = SendDlgItemMessageW(hwnd, ID_ELEVATED, BM_SETCHECK, WPARAM(0), LPARAM(0));
                center_over_owner(hwnd);
            }
            // The field keeps the focus the template gives it.
            0
        }
        WM_COMMAND => {
            let id = i32::try_from(wparam.0 & 0xFFFF).unwrap_or_default();
            match id {
                ID_OK => {
                    let mut buf = [0u16; 4096];
                    // SAFETY: the buffer and its length agree; hwnd is the dialog.
                    let n = unsafe { GetDlgItemTextW(hwnd, ID_COMMAND, &mut buf) } as usize;
                    let command = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
                    // SAFETY: hwnd is the dialog.
                    let elevated = unsafe { IsDlgButtonChecked(hwnd, ID_ELEVATED) } == 1;
                    let command = command.trim().to_owned();
                    if command.is_empty() {
                        return 1;
                    }
                    STATE.with(|s| s.borrow_mut().result = Some(RunRequest { command, elevated }));
                    // SAFETY: hwnd is the dialog.
                    let _ = unsafe { EndDialog(hwnd, 1) };
                    1
                }
                ID_CANCEL => {
                    // SAFETY: hwnd is the dialog.
                    let _ = unsafe { EndDialog(hwnd, 0) };
                    1
                }
                ID_BROWSE => {
                    if let Some(path) = browse(hwnd) {
                        let quoted = if path.contains(' ') {
                            format!("\"{path}\"")
                        } else {
                            path
                        };
                        // SAFETY: hwnd is the dialog; the string outlives the call.
                        let _ = unsafe {
                            SetDlgItemTextW(hwnd, ID_COMMAND, &windows::core::HSTRING::from(quoted))
                        };
                    }
                    1
                }
                _ => 0,
            }
        }
        _ => 0,
    }
}

/// The file picker for the Browse button.
fn browse(owner: HWND) -> Option<String> {
    let mut file = [0u16; 4096];
    let filter: Vec<u16> = "Programs\0*.exe;*.com;*.bat;*.cmd;*.msc\0All files\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: w!("Browse"),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    // SAFETY: every buffer outlives the call and the sizes agree.
    if !unsafe { GetOpenFileNameW(&raw mut ofn) }.as_bool() {
        return None;
    }
    let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(String::from_utf16_lossy(&file[..end]))
}

/// Move the dialog to the middle of its owner (`DS_CENTER` centers on the screen).
///
/// # Safety
/// `hwnd` must be the dialog.
unsafe fn center_over_owner(hwnd: HWND) {
    // SAFETY: plain calls with valid out-structs; the owner may be null.
    unsafe {
        let owner = windows::Win32::UI::WindowsAndMessaging::GetWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GW_OWNER,
        )
        .ok();
        let Some(owner) = owner else {
            return;
        };
        let (mut o, mut d) = (RECT::default(), RECT::default());
        if GetWindowRect(owner, &raw mut o).is_err() || GetWindowRect(hwnd, &raw mut d).is_err() {
            return;
        }
        let x = o.left + ((o.right - o.left) - (d.right - d.left)) / 2;
        let y = o.top + ((o.bottom - o.top) - (d.bottom - d.top)) / 2;
        let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
    }
}

/// The template, as `DialogBoxIndirectParamW` reads it: a `DLGTEMPLATE` header,
/// then `DLGITEMTEMPLATE`s, each DWORD-aligned, with their class by ordinal and
/// their text. Units are dialog units (a quarter of the font's average character
/// width by an eighth of its height), so the dialog scales with the font.
fn template() -> Vec<u32> {
    let mut w: Vec<u16> = Vec::with_capacity(256);
    let style = WS_POPUP.0
        | WS_CAPTION.0
        | WS_SYSMENU.0
        | WS_VISIBLE.0
        | DS_MODALFRAME as u32
        | DS_SETFONT as u32
        | DS_CENTER as u32;
    push_u32(&mut w, style);
    push_u32(&mut w, 0);
    w.push(7); // items
    push_i16s(&mut w, [0, 0, 300, 96]);
    w.push(0); // no menu
    w.push(0); // default class
    push_str(&mut w, "Create new task");
    w.push(9); // point size
    push_str(&mut w, "Segoe UI");

    let text = |w: &mut Vec<u16>, s: &str, x, y, cx, cy| {
        item(
            w,
            WS_CHILD.0 | WS_VISIBLE.0,
            x,
            y,
            cx,
            cy,
            u16::MAX,
            CLASS_STATIC,
            s,
        );
    };
    text(
        &mut w,
        "Type the name of a program, folder, document, or Internet resource, and open-task will open it for you.",
        8,
        8,
        284,
        18,
    );
    text(&mut w, "Open:", 8, 34, 24, 10);
    item(
        &mut w,
        WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
        36,
        31,
        256,
        13,
        ID_COMMAND as u16,
        CLASS_EDIT,
        "",
    );
    item(
        &mut w,
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32,
        36,
        50,
        240,
        10,
        ID_ELEVATED as u16,
        CLASS_BUTTON,
        "Create this task with administrative privileges",
    );
    item(
        &mut w,
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
        128,
        74,
        50,
        14,
        ID_OK as u16,
        CLASS_BUTTON,
        "OK",
    );
    item(
        &mut w,
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
        185,
        74,
        50,
        14,
        ID_CANCEL as u16,
        CLASS_BUTTON,
        "Cancel",
    );
    item(
        &mut w,
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
        242,
        74,
        50,
        14,
        ID_BROWSE as u16,
        CLASS_BUTTON,
        "Browse...",
    );
    // Hand it over 4-byte aligned, as the API requires.
    if w.len() % 2 == 1 {
        w.push(0);
    }
    w.chunks(2)
        .map(|c| u32::from(c[0]) | (u32::from(c[1]) << 16))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn item(
    w: &mut Vec<u16>,
    style: u32,
    x: i16,
    y: i16,
    cx: i16,
    cy: i16,
    id: u16,
    class: u16,
    text: &str,
) {
    // Each item starts on a DWORD boundary.
    if w.len() % 2 == 1 {
        w.push(0);
    }
    push_u32(w, style);
    push_u32(w, 0);
    push_i16s(w, [x, y, cx, cy]);
    w.push(id);
    w.push(0xFFFF);
    w.push(class);
    push_str(w, text);
    w.push(0); // no creation data
}

fn push_u32(w: &mut Vec<u16>, v: u32) {
    w.push((v & 0xFFFF) as u16);
    w.push((v >> 16) as u16);
}

fn push_i16s(w: &mut Vec<u16>, v: [i16; 4]) {
    w.extend(v.iter().map(|&i| i as u16));
}

fn push_str(w: &mut Vec<u16>, s: &str) {
    w.extend(s.encode_utf16());
    w.push(0);
}

/// Kept so the compiler checks the procedure's signature against what the API wants.
#[allow(dead_code)]
const _: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> isize = dialog_proc;

#[allow(dead_code)]
fn _types(_: BOOL, _: LRESULT, _: *mut c_void) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_well_formed() {
        let t = template();
        // Header: style, exstyle, 7 items.
        let items = (t[2] & 0xFFFF) as u16;
        assert_eq!(items, 7);
        assert!(t.len() > 40);
    }
}
