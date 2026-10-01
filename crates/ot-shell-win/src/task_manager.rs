//! Replacing Task Manager, the way Process Explorer does it.
//!
//! Windows looks up every program it starts under
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options`
//! (IFEO). A `Debugger` value under `taskmgr.exe` makes it start that program
//! instead, with Task Manager's command line appended: Ctrl+Shift+Esc, the taskbar's
//! Task Manager item, the Ctrl+Alt+Del screen, and `taskmgr` typed anywhere all
//! start open-task. The replacement runs with the rights of whatever started it
//! (unelevated for Ctrl+Shift+Esc), not with Task Manager's automatic elevation.
//!
//! The value is only ever written as this copy's own path, quoted, and removed only
//! when it names this copy: another program's replacement (Process Explorer, another
//! copy of open-task) is overwritten by turning this on, but never removed by
//! turning it off. The key goes when nothing is left in it. Writing needs
//! administrator rights; without them the window starts this program elevated
//! ([`change_elevated`]) with `--replace-task-manager` or `--restore-task-manager`,
//! which run [`replace`] and [`restore`] and exit with the Win32 error code.
//!
//! Only the 64-bit registry view matters: Explorer and Winlogon are 64-bit.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use ot_ui::Replacement;
use windows::core::{w, Error, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, E_FAIL, HWND,
    WIN32_ERROR,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE,
    KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Task Manager's IFEO key, under `HKEY_LOCAL_MACHINE`.
const KEY: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
const VALUE: PCWSTR = w!("Debugger");

/// The flags the elevated helper runs with.
pub const REPLACE_FLAG: &str = "--replace-task-manager";
pub const RESTORE_FLAG: &str = "--restore-task-manager";

/// What [`restore`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restored {
    /// This copy was the replacement; Task Manager is back.
    Restored,
    /// Nothing was replacing Task Manager.
    NotReplaced,
    /// Another program is the replacement, named here; it was left alone.
    Other(String),
}

/// Whether this process was started in Task Manager's place: Windows puts Task
/// Manager's own path first among the arguments (`"C:\WINDOWS\System32\Taskmgr.exe"
/// /2` for Ctrl+Shift+Esc).
#[must_use]
pub fn is_stand_in(args: &[String]) -> bool {
    args.first()
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|name| name.eq_ignore_ascii_case("taskmgr.exe"))
}

/// Make Windows start this copy in Task Manager's place. Returns the path it now
/// starts.
///
/// # Errors
/// Access denied without administrator rights; any other registry failure.
pub fn replace() -> Result<PathBuf, Error> {
    let exe = this_exe()?;
    replace_in(&Place::machine(), &exe)?;
    Ok(exe)
}

/// Stop Windows starting this copy in Task Manager's place, if it does.
///
/// # Errors
/// Access denied without administrator rights; any other registry failure.
pub fn restore() -> Result<Restored, Error> {
    restore_in(&Place::machine(), &this_exe()?)
}

/// This program's path, which is what the value names.
fn this_exe() -> Result<PathBuf, Error> {
    std::env::current_exe().map_err(|e| match e.raw_os_error() {
        Some(code) => Error::from_hresult(HRESULT::from_win32(code.cast_unsigned())),
        None => Error::new(E_FAIL, e.to_string()),
    })
}

/// What Windows starts in Task Manager's place, for the Settings card.
/// [`Replacement::Unavailable`] if it cannot be read.
pub(crate) fn replacement() -> Replacement {
    let Ok(exe) = std::env::current_exe() else {
        return Replacement::Unavailable;
    };
    match read(&Place::machine()) {
        Ok(value) => classify(value.as_deref(), &exe),
        Err(e) => {
            tracing::warn!(error = %e, "could not read Task Manager's replacement");
            Replacement::Unavailable
        }
    }
}

/// Make the change directly, as [`replace`] or [`restore`] would.
///
/// # Errors
/// As those.
pub(crate) fn change(on: bool) -> Result<(), Error> {
    if on {
        replace().map(|_| ())
    } else {
        restore().map(|_| ())
    }
}

/// Whether `e` is "access denied": the change needs administrator rights.
#[must_use]
pub fn denied(e: &Error) -> bool {
    e.code() == ERROR_ACCESS_DENIED.to_hresult()
}

/// How an elevated helper's change went.
#[derive(Debug)]
pub(crate) enum Elevated {
    Done,
    /// The user said no to the UAC prompt.
    Cancelled,
    Failed(Error),
}

/// Make the change in a copy of this program started as administrator, and wait for
/// it. Blocks until the UAC prompt is answered and the helper ends, so call it off
/// the UI thread. `owner` is the window the prompt belongs to.
pub(crate) fn change_elevated(on: bool, owner: HWND) -> Elevated {
    let exe = match this_exe() {
        Ok(exe) => HSTRING::from(exe.as_os_str()),
        Err(e) => return Elevated::Failed(e),
    };
    let flag = HSTRING::from(if on { REPLACE_FLAG } else { RESTORE_FLAG });
    // ShellExecuteEx wants COM on the calling thread, as it may hand the work to
    // shell extensions.
    // SAFETY: plain per-thread initialization, undone below if it succeeded.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd: owner,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(flag.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    let started = unsafe { ShellExecuteExW(&raw mut info) };
    let outcome = match started {
        Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => Elevated::Cancelled,
        Err(e) => Elevated::Failed(e),
        Ok(()) if info.hProcess.is_invalid() => Elevated::Done,
        Ok(()) => {
            let mut code = 1u32;
            // SAFETY: the process handle is ours (SEE_MASK_NOCLOSEPROCESS) and is
            // closed once, after the wait.
            unsafe {
                WaitForSingleObject(info.hProcess, INFINITE);
                let _ = GetExitCodeProcess(info.hProcess, &raw mut code);
                let _ = CloseHandle(info.hProcess);
            }
            if code == 0 {
                Elevated::Done
            } else {
                Elevated::Failed(Error::from_hresult(from_exit_code(code)))
            }
        }
    };
    if com.is_ok() {
        // SAFETY: pairs with the successful CoInitializeEx above, on this thread.
        unsafe { CoUninitialize() };
    }
    outcome
}

/// The exit code for the helper that failed with `e`: the Win32 error code when it
/// is one, so the window can say what went wrong; the HRESULT otherwise.
#[must_use]
pub fn exit_code(e: &Error) -> i32 {
    let hr = e.code().0.cast_unsigned();
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        (hr & 0xFFFF).cast_signed()
    } else {
        hr.cast_signed()
    }
}

/// The error an exit code from [`exit_code`] stands for.
fn from_exit_code(code: u32) -> HRESULT {
    if code <= 0xFFFF {
        HRESULT::from_win32(code)
    } else {
        HRESULT(code.cast_signed())
    }
}

/// Where the value is kept: Task Manager's key in the machine's registry, or a
/// scratch key in tests.
struct Place {
    root: HKEY,
    key: HSTRING,
}

impl Place {
    fn machine() -> Self {
        Self {
            root: HKEY_LOCAL_MACHINE,
            key: HSTRING::from(KEY),
        }
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key is open and closed only here.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// The `Debugger` value, `None` when there is none (or no key).
fn read(place: &Place) -> Result<Option<String>, Error> {
    let flags = RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY;
    let mut size = 0u32;
    // SAFETY: with no buffer, the call only reports the size in bytes.
    let status = unsafe {
        RegGetValueW(
            place.root,
            &place.key,
            VALUE,
            flags,
            None,
            None,
            Some(&raw mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok()?;
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `buffer` holds `size` bytes, which the call does not exceed.
    unsafe {
        RegGetValueW(
            place.root,
            &place.key,
            VALUE,
            flags,
            None,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
    }
    .ok()?;
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some(String::from_utf16_lossy(&buffer[..len])))
}

fn replace_in(place: &Place, exe: &Path) -> Result<(), Error> {
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the key is closed by `Key`.
    unsafe {
        RegCreateKeyExW(
            place.root,
            &place.key,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &raw mut key,
            None,
        )
    }
    .ok()?;
    let key = Key(key);
    let data: Vec<u8> = value_for(exe)
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: the key is open for setting values; the data outlives the call.
    unsafe { RegSetValueExW(key.0, VALUE, None, REG_SZ, Some(&data)) }.ok()?;
    tracing::info!(exe = %exe.display(), "Task Manager replaced");
    Ok(())
}

fn restore_in(place: &Place, exe: &Path) -> Result<Restored, Error> {
    let Some(value) = read(place)? else {
        return Ok(Restored::NotReplaced);
    };
    let program = program_of(&value);
    if !same_file(Path::new(program), exe) {
        return Ok(Restored::Other(program.to_owned()));
    }
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the key is closed by `Key`.
    unsafe {
        RegOpenKeyExW(
            place.root,
            &place.key,
            None,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            &raw mut key,
        )
    }
    .ok()?;
    let key = Key(key);
    // SAFETY: the key is open for setting values.
    unsafe { RegDeleteValueW(key.0, VALUE) }.ok()?;
    let (mut subkeys, mut values) = (0u32, 0u32);
    // SAFETY: the out-pointers are locals; everything else is not asked for.
    let empty = unsafe {
        RegQueryInfoKeyW(
            key.0,
            None,
            None,
            None,
            Some(&raw mut subkeys),
            None,
            None,
            Some(&raw mut values),
            None,
            None,
            None,
            None,
        )
    }
    .ok()
    .is_ok()
        && subkeys == 0
        && values == 0;
    drop(key);
    if empty {
        // The key only existed for this. Failing to remove it is harmless.
        // SAFETY: plain call with a string that outlives it.
        let status: WIN32_ERROR =
            unsafe { RegDeleteKeyExW(place.root, &place.key, KEY_WOW64_64KEY.0, None) };
        if status.is_err() {
            tracing::warn!(?status, "could not remove Task Manager's empty IFEO key");
        }
    }
    tracing::info!("Task Manager restored");
    Ok(Restored::Restored)
}

/// The value that makes Windows start `exe`: its path, quoted.
fn value_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// What `value` would start, given how Windows builds the command line from it:
/// the value, then Task Manager's command line.
fn classify(value: Option<&str>, exe: &Path) -> Replacement {
    let Some(program) = value.map(program_of).filter(|p| !p.is_empty()) else {
        return Replacement::Off;
    };
    let path = Path::new(program);
    if same_file(path, exe) {
        Replacement::ThisCopy
    } else {
        Replacement::Other {
            path: program.to_owned(),
            // A bare name is looked for on the PATH; assume it is there.
            exists: !path.is_absolute() || path.is_file(),
        }
    }
}

/// The program a `Debugger` value names. Quoted, what is inside the quotes;
/// otherwise up to the first `.exe` that ends a word, so an unquoted path with
/// spaces stays whole, or failing that up to the first space.
fn program_of(value: &str) -> &str {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"') {
        return rest.split('"').next().unwrap_or(rest);
    }
    // ASCII lowercasing keeps every byte where it was, so its offsets hold for
    // `value` too.
    let lower = value.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(".exe") {
        let end = from + i + ".exe".len();
        if lower[end..].chars().next().is_none_or(char::is_whitespace) {
            return &value[..end];
        }
        from = end;
    }
    value.split_whitespace().next().unwrap_or("")
}

/// Whether two paths name the same file: the same after resolving both, or, when
/// either cannot be resolved (a missing file), the same text ignoring case.
fn same_file(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.to_string_lossy().to_lowercase();
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => key(&a) == key(&b),
        _ => key(a) == key(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};

    /// A key of its own under `HKCU\Software\open-task-tests`, removed on drop.
    struct Scratch(Place);

    impl Scratch {
        fn new(name: &str) -> Self {
            Self(Place {
                root: HKEY_CURRENT_USER,
                key: HSTRING::from(format!(
                    r"Software\open-task-tests\{}-{name}\taskmgr.exe",
                    std::process::id()
                )),
            })
        }

        fn parent(&self) -> HSTRING {
            let key = self.0.key.to_string();
            HSTRING::from(key.rsplit_once('\\').expect("nested").0)
        }

        fn exists(&self) -> bool {
            let mut key = HKEY::default();
            // SAFETY: the out-pointer is a local; the key is closed by `Key`.
            let status = unsafe {
                RegOpenKeyExW(
                    self.0.root,
                    &self.0.key,
                    None,
                    KEY_QUERY_VALUE,
                    &raw mut key,
                )
            };
            if status.is_err() {
                return false;
            }
            drop(Key(key));
            true
        }

        /// Write any value, as another program might.
        fn set(&self, name: &str, value: &str) {
            let mut key = HKEY::default();
            // SAFETY: as in `replace_in`.
            unsafe {
                RegCreateKeyExW(
                    self.0.root,
                    &self.0.key,
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &raw mut key,
                    None,
                )
                .ok()
                .expect("scratch key");
            }
            let key = Key(key);
            let data: Vec<u8> = value
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            // SAFETY: as in `replace_in`.
            unsafe {
                RegSetValueExW(key.0, &HSTRING::from(name), None, REG_SZ, Some(&data))
                    .ok()
                    .expect("set");
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // SAFETY: plain calls on the test's own keys. The shared parent goes
            // only once no other test has a key in it (deleting a key with subkeys
            // fails).
            unsafe {
                let _ = RegDeleteTreeW(self.0.root, &self.parent());
                let _ = RegDeleteKeyExW(self.0.root, &self.parent(), 0, None);
                let _ = RegDeleteKeyExW(self.0.root, w!(r"Software\open-task-tests"), 0, None);
            }
        }
    }

    const EXE: &str = r"C:\Program Files\open-task\open-task.exe";

    #[test]
    fn replacing_writes_this_copy_quoted_and_restoring_removes_the_key() {
        let scratch = Scratch::new("round-trip");
        let exe = Path::new(EXE);
        assert_eq!(read(&scratch.0).unwrap(), None);
        assert_eq!(restore_in(&scratch.0, exe).unwrap(), Restored::NotReplaced);

        replace_in(&scratch.0, exe).unwrap();
        let value = read(&scratch.0).unwrap();
        assert_eq!(
            value.as_deref(),
            Some(r#""C:\Program Files\open-task\open-task.exe""#)
        );
        assert_eq!(classify(value.as_deref(), exe), Replacement::ThisCopy);

        assert_eq!(restore_in(&scratch.0, exe).unwrap(), Restored::Restored);
        assert_eq!(read(&scratch.0).unwrap(), None);
        assert!(!scratch.exists(), "the key only held our value");
    }

    #[test]
    fn restoring_leaves_another_program_alone() {
        let scratch = Scratch::new("other");
        let procexp = r#""C:\Tools\procexp64.exe""#;
        scratch.set("Debugger", procexp);
        let exe = Path::new(EXE);
        assert_eq!(
            restore_in(&scratch.0, exe).unwrap(),
            Restored::Other(r"C:\Tools\procexp64.exe".into())
        );
        assert_eq!(read(&scratch.0).unwrap().as_deref(), Some(procexp));
        // Replacing takes over, as Process Explorer would.
        replace_in(&scratch.0, exe).unwrap();
        assert_eq!(
            classify(read(&scratch.0).unwrap().as_deref(), exe),
            Replacement::ThisCopy
        );
    }

    #[test]
    fn restoring_keeps_a_key_that_holds_other_values() {
        let scratch = Scratch::new("shared");
        scratch.set("MitigationOptions", "keep me");
        let exe = Path::new(EXE);
        replace_in(&scratch.0, exe).unwrap();
        assert_eq!(restore_in(&scratch.0, exe).unwrap(), Restored::Restored);
        assert_eq!(read(&scratch.0).unwrap(), None);
        assert!(scratch.exists());
    }

    #[test]
    fn the_running_copy_is_recognised_however_its_path_is_spelled() {
        let exe = std::env::current_exe().unwrap();
        let shouted = exe.to_string_lossy().to_uppercase();
        let value = format!("\"{shouted}\"");
        assert_eq!(classify(Some(&value), &exe), Replacement::ThisCopy);
    }

    #[test]
    fn another_program_is_named_and_checked_for() {
        let exe = Path::new(EXE);
        assert_eq!(classify(None, exe), Replacement::Off);
        assert_eq!(classify(Some("  "), exe), Replacement::Off);
        let missing = r"C:\nowhere\at-all\open-task.exe";
        assert_eq!(
            classify(Some(&format!("\"{missing}\"")), exe),
            Replacement::Other {
                path: missing.into(),
                exists: false
            }
        );
        let here = std::env::current_exe().unwrap();
        let elsewhere = Path::new(r"C:\somewhere\else\open-task.exe");
        assert_eq!(
            classify(Some(&value_for(&here)), elsewhere),
            Replacement::Other {
                path: here.to_string_lossy().into_owned(),
                exists: true
            }
        );
        // A bare name is found on the PATH, so it is not called missing.
        assert!(matches!(
            classify(Some("procexp64.exe"), exe),
            Replacement::Other { exists: true, .. }
        ));
    }

    #[test]
    fn the_program_is_read_out_of_the_value() {
        assert_eq!(program_of(r#""C:\a b\x.exe""#), r"C:\a b\x.exe");
        assert_eq!(program_of(r#""C:\a b\x.exe" /e"#), r"C:\a b\x.exe");
        assert_eq!(program_of(r#"  "C:\a b\x.exe"  "#), r"C:\a b\x.exe");
        assert_eq!(program_of(r"C:\a b\x.EXE /e"), r"C:\a b\x.EXE");
        assert_eq!(program_of(r"C:\a\x.exe"), r"C:\a\x.exe");
        // ".exe" inside a word does not end the program.
        assert_eq!(program_of(r"C:\a.exed\x.exe -z"), r"C:\a.exed\x.exe");
        assert_eq!(program_of("cmd /c probe.cmd"), "cmd");
        assert_eq!(program_of(""), "");
    }

    #[test]
    fn a_launch_in_task_managers_place_is_recognised() {
        let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(is_stand_in(&args(&[
            r"C:\WINDOWS\System32\Taskmgr.exe",
            "/2"
        ])));
        assert!(is_stand_in(&args(&[r"C:\Windows\system32\taskmgr.exe"])));
        assert!(!is_stand_in(&args(&[])));
        assert!(!is_stand_in(&args(&["--page", "performance"])));
        assert!(!is_stand_in(&args(&[r"C:\x\taskmgr.exe.bak"])));
    }

    #[test]
    fn exit_codes_carry_the_error_both_ways() {
        let denied = Error::from_hresult(ERROR_ACCESS_DENIED.to_hresult());
        assert_eq!(exit_code(&denied), 5);
        assert_eq!(from_exit_code(5), ERROR_ACCESS_DENIED.to_hresult());
        let other = Error::from_hresult(HRESULT(0x8000_4005_u32.cast_signed()));
        let code = exit_code(&other);
        assert_eq!(from_exit_code(code.cast_unsigned()), other.code());
    }
}
