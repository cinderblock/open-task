//! Programs that run at sign-in, and Task Manager's switch for each.
//!
//! Windows starts programs from two kinds of place: the `Run` values under
//! `Software\Microsoft\Windows\CurrentVersion` in the user's hive, the machine's, and
//! the machine's 32-bit view (`WOW6432Node`), and the files in the user's and the
//! common Startup folders. Task Manager's Startup apps page lists all five and lets
//! each entry be turned off without removing it: it records a `REG_BINARY` value named
//! after the entry under `Explorer\StartupApproved\{Run,Run32,StartupFolder}` in the
//! same hive, whose first byte is `0x02` for enabled and `0x03` for disabled (the low
//! bit is the switch; `0x06`/`0x07` show up after upgrades and mean the same). Explorer
//! consults that key at sign-in and skips what is marked off. [`entries`] reads it all;
//! [`set_enabled`] writes the switch the same way, so Task Manager and open-task agree.
//!
//! Registry values are enumerated with `RegEnumValueW` and expanded with
//! `ExpandEnvironmentStringsW`, because `REG_EXPAND_SZ` is common there and a `%`
//! never reaches the command the shell runs. Shortcuts are resolved through the
//! shell's `IShellLinkW`: the one way to read a `.lnk` target that follows every
//! version of the format. COM is initialised apartment-threaded on the calling thread
//! for the duration of one call, and left alone if the thread already has it the other
//! way (`RPC_E_CHANGED_MODE`).
//!
//! `publisher` is left `None` here: the version-resource reader lives elsewhere and
//! fills it from `image_path` when the lists are wired together.
//!
//! Everything is read with the 64-bit registry view pinned (`KEY_WOW64_64KEY`), so
//! the three `Run` keys are three distinct keys regardless of how this binary is built.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ot_model::startup::{StartupEntry, StartupLocation};
use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
    RPC_E_CHANGED_MODE,
};
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegEnumValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY,
    REG_BINARY, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::UI::Shell::{
    FOLDERID_CommonStartup, FOLDERID_Startup, IShellLinkW, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DEFAULT,
};

use crate::ControlError;

/// The `Run` key, in whichever hive.
const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// The machine's 32-bit `Run` key.
const RUN32: PCWSTR = w!(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run");
/// Task Manager's switches for the `Run` key of the same hive.
const APPROVED_RUN: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
/// Task Manager's switches for the machine's 32-bit `Run` key.
const APPROVED_RUN32: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32");
/// Task Manager's switches for the Startup folder of the same hive, keyed by file
/// name with its extension.
const APPROVED_FOLDER: PCWSTR =
    w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder");

/// Every program registered to run at sign-in, enabled or not, sorted by name
/// without regard to case. The same program can legitimately appear under two
/// locations, so nothing is merged.
///
/// Costs tens of milliseconds (about 30 ms warm for 18 entries, six of them
/// shortcuts, on the development machine): three registry keys, two directory
/// listings, one shell-link load per shortcut, and a file lookup per command. Not for
/// the sampler: callers read it on demand, off the UI thread.
pub(super) fn entries() -> Vec<StartupEntry> {
    let mut out = Vec::new();
    run_entries(&mut out, StartupLocation::UserRun);
    run_entries(&mut out, StartupLocation::MachineRun);
    run_entries(&mut out, StartupLocation::MachineRun32);
    {
        let _com = Com::new();
        folder_entries(&mut out, StartupLocation::UserFolder);
        folder_entries(&mut out, StartupLocation::CommonFolder);
    }
    out.sort_by_cached_key(|e| e.name.to_lowercase());
    out
}

/// Let the entry run at sign-in, or stop it, by writing Task Manager's switch for its
/// location. Disabling records the time, as Task Manager does; enabling writes zeros.
/// The entry itself is never removed.
///
/// # Errors
/// [`ControlError::NotPermitted`] when the switch lives in the machine's hive and
/// this process is not elevated; [`ControlError::Os`] for any other registry failure.
pub(super) fn set_enabled(entry: &StartupEntry, on: bool) -> Result<(), ControlError> {
    let (root, subkey) = approved_place(entry.location);
    let name = approved_name(entry);
    let mut data = [0u8; 12];
    data[0] = if on { 0x02 } else { 0x03 };
    if !on {
        // SAFETY: plain call returning a value.
        let now = unsafe { GetSystemTimeAsFileTime() };
        data[4..8].copy_from_slice(&now.dwLowDateTime.to_le_bytes());
        data[8..12].copy_from_slice(&now.dwHighDateTime.to_le_bytes());
    }
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the key is closed by `Key`.
    let status = unsafe {
        RegCreateKeyExW(
            root,
            subkey,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &raw mut key,
            None,
        )
    };
    if status == ERROR_ACCESS_DENIED {
        return Err(ControlError::NotPermitted);
    }
    status
        .ok()
        .map_err(|e| ControlError::os("RegCreateKeyExW StartupApproved", e))?;
    let key = Key(key);
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: the key is open for setting values; the name and data outlive the call.
    let status =
        unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(&data)) };
    if status == ERROR_ACCESS_DENIED {
        return Err(ControlError::NotPermitted);
    }
    status
        .ok()
        .map_err(|e| ControlError::os("RegSetValueExW StartupApproved", e))
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

/// Opens `subkey` under `root` for reading, in the 64-bit view. `None` when it does
/// not exist or cannot be read.
fn open(root: HKEY, subkey: PCWSTR) -> Option<Key> {
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the key is closed by `Key`.
    let status = unsafe {
        RegOpenKeyExW(
            root,
            subkey,
            None,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &raw mut key,
        )
    };
    if status != ERROR_SUCCESS {
        if status != ERROR_FILE_NOT_FOUND {
            // SAFETY: the constant is a valid NUL-terminated string.
            let subkey = unsafe { subkey.to_string() }.unwrap_or_default();
            tracing::debug!(?status, subkey, "startup key not readable");
        }
        return None;
    }
    Some(Key(key))
}

/// One registry value: its name, type and raw data.
struct Value {
    name: String,
    kind: u32,
    data: Vec<u8>,
}

/// Every value of `subkey` under `root`, in registry order. Empty for a key that
/// does not exist.
fn values(root: HKEY, subkey: PCWSTR) -> Vec<Value> {
    let Some(key) = open(root, subkey) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Value names are at most 16383 characters; data is grown on demand.
    let mut name = vec![0u16; 16384];
    let mut data = vec![0u8; 1024];
    let mut index = 0u32;
    loop {
        let mut name_len = name.len() as u32;
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        // SAFETY: the buffers and their lengths match; the out-pointers are locals.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &raw mut name_len,
                None,
                Some(&raw mut kind),
                Some(data.as_mut_ptr()),
                Some(&raw mut data_len),
            )
        };
        if status == ERROR_MORE_DATA {
            data.resize((data_len as usize).max(data.len() * 2), 0);
            continue;
        }
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            tracing::debug!(?status, index, "RegEnumValueW failed");
            break;
        }
        let name_len = (name_len as usize).min(name.len());
        let data_len = (data_len as usize).min(data.len());
        out.push(Value {
            name: String::from_utf16_lossy(&name[..name_len]),
            kind,
            data: data[..data_len].to_vec(),
        });
        index += 1;
    }
    out
}

/// A `REG_SZ` or `REG_EXPAND_SZ` value's text, up to its first NUL. `None` for any
/// other type.
fn string_of(v: &Value) -> Option<String> {
    if v.kind != REG_SZ.0 && v.kind != REG_EXPAND_SZ.0 {
        return None;
    }
    let units: Vec<u16> = v
        .data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// The hive and key that hold the switches for `location`.
fn approved_place(location: StartupLocation) -> (HKEY, PCWSTR) {
    let root = if location.machine_wide() {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    };
    let subkey = match location {
        StartupLocation::UserRun | StartupLocation::MachineRun => APPROVED_RUN,
        StartupLocation::MachineRun32 => APPROVED_RUN32,
        StartupLocation::UserFolder | StartupLocation::CommonFolder => APPROVED_FOLDER,
    };
    (root, subkey)
}

/// The switches under `root\subkey`: the first byte of each `REG_BINARY` value, by
/// lower-cased name.
fn approved(root: HKEY, subkey: PCWSTR) -> HashMap<String, u8> {
    values(root, subkey)
        .into_iter()
        .filter(|v| v.kind == REG_BINARY.0)
        .filter_map(|v| v.data.first().map(|&b| (v.name.to_lowercase(), b)))
        .collect()
}

/// Whether a switch byte means the entry runs. Missing means enabled; the low bit
/// set (`0x03`, `0x07`) means disabled.
fn is_enabled(switch: Option<&u8>) -> bool {
    switch.is_none_or(|b| b & 1 == 0)
}

/// The value name the switch for `entry` is kept under: the entry's own name for a
/// registry entry, the file's full name for a folder entry.
fn approved_name(entry: &StartupEntry) -> String {
    match entry.location {
        StartupLocation::UserRun | StartupLocation::MachineRun | StartupLocation::MachineRun32 => {
            entry.name.clone()
        }
        StartupLocation::UserFolder | StartupLocation::CommonFolder => {
            // The entry carries the stem only; find the file to recover its extension.
            // A shortcut is the overwhelmingly common case, so that is the fallback.
            folder_of(entry.location)
                .and_then(|dir| {
                    std::fs::read_dir(dir).ok()?.flatten().find_map(|f| {
                        let file = f.file_name().to_string_lossy().into_owned();
                        let stem = Path::new(&file).file_stem()?.to_string_lossy();
                        stem.eq_ignore_ascii_case(&entry.name).then_some(file)
                    })
                })
                .unwrap_or_else(|| format!("{}.lnk", entry.name))
        }
    }
}

/// Appends the `Run` values of `location`.
fn run_entries(out: &mut Vec<StartupEntry>, location: StartupLocation) {
    let (root, subkey) = match location {
        StartupLocation::UserRun => (HKEY_CURRENT_USER, RUN),
        StartupLocation::MachineRun => (HKEY_LOCAL_MACHINE, RUN),
        StartupLocation::MachineRun32 => (HKEY_LOCAL_MACHINE, RUN32),
        StartupLocation::UserFolder | StartupLocation::CommonFolder => return,
    };
    let (approved_root, approved_key) = approved_place(location);
    let switches = approved(approved_root, approved_key);
    for v in values(root, subkey) {
        let Some(raw) = string_of(&v) else {
            continue;
        };
        let command = expand_env(&raw);
        let image_path = image_of(&command);
        out.push(StartupEntry {
            enabled: is_enabled(switches.get(&v.name.to_lowercase())),
            name: v.name,
            location,
            command,
            publisher: None,
            image_path,
        });
    }
}

/// The Startup folder for `location`, from the shell. `None` for the registry
/// locations or if the shell cannot say.
fn folder_of(location: StartupLocation) -> Option<PathBuf> {
    let id = match location {
        StartupLocation::UserFolder => &FOLDERID_Startup,
        StartupLocation::CommonFolder => &FOLDERID_CommonStartup,
        _ => return None,
    };
    // SAFETY: plain call; the returned string is freed below.
    let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
    // SAFETY: the shell returned a NUL-terminated string allocated with CoTaskMemAlloc.
    let s = unsafe {
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.as_ptr().cast::<c_void>().cast_const()));
        s
    };
    s.map(PathBuf::from)
}

/// Appends the files of the Startup folder for `location`. Every file counts: the
/// shell opens whatever is there, so a script or a document starts too. `desktop.ini`
/// is the folder's own metadata and is skipped.
fn folder_entries(out: &mut Vec<StartupEntry>, location: StartupLocation) {
    let Some(dir) = folder_of(location) else {
        return;
    };
    let Ok(listing) = std::fs::read_dir(&dir) else {
        return;
    };
    let (approved_root, approved_key) = approved_place(location);
    let switches = approved(approved_root, approved_key);
    for file in listing.flatten() {
        let path = file.path();
        if !path.is_file() {
            continue;
        }
        let file_name = file.file_name().to_string_lossy().into_owned();
        if file_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let name = path
            .file_stem()
            .map_or_else(|| file_name.clone(), |s| s.to_string_lossy().into_owned());
        let is_link = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        let (command, image_path) = if is_link {
            match link_target(&path) {
                Some((target, args)) => {
                    let image = Path::new(&target).is_file().then(|| target.clone());
                    (join_command(&target, &args), image)
                }
                // An unreadable shortcut is still an entry; the shell will try it.
                None => (path.to_string_lossy().into_owned(), None),
            }
        } else {
            let p = path.to_string_lossy().into_owned();
            (p.clone(), Some(p))
        };
        out.push(StartupEntry {
            name,
            location,
            command,
            enabled: is_enabled(switches.get(&file_name.to_lowercase())),
            publisher: None,
            image_path,
        });
    }
}

/// A program and its arguments as one command line, quoting the program when it
/// has a space.
fn join_command(program: &str, args: &str) -> String {
    let program = if program.contains(' ') && !program.starts_with('"') {
        format!("\"{program}\"")
    } else {
        program.to_owned()
    };
    if args.is_empty() {
        program
    } else {
        format!("{program} {args}")
    }
}

/// COM on this thread for the life of the guard. Initialised apartment-threaded, as
/// the shell's objects want; if the thread already has COM the other way that is
/// fine too, and then it is not ours to uninitialise.
struct Com {
    ours: bool,
}

impl Com {
    fn new() -> Self {
        // SAFETY: plain call, balanced by `CoUninitialize` in `drop` when it succeeds.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            tracing::warn!(?hr, "CoInitializeEx failed; shortcut targets unresolved");
        }
        Self { ours: hr.is_ok() }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.ours {
            // SAFETY: balances the successful CoInitializeEx in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// A shortcut's target path and arguments, through the shell. Needs COM on this
/// thread. `None` when the file cannot be loaded as a shortcut.
fn link_target(lnk: &Path) -> Option<(String, String)> {
    // SAFETY: plain creation of the shell's in-process link object.
    let shell: IShellLinkW =
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.ok()?;
    let file: IPersistFile = shell.cast().ok()?;
    let wide: Vec<u16> = lnk.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: the path is NUL-terminated and outlives the call.
    unsafe { file.Load(PCWSTR(wide.as_ptr()), STGM_READ) }.ok()?;
    // Long paths are allowed in shortcuts; 32 KiB is the shell's own ceiling.
    let mut path = vec![0u16; 32 * 1024];
    // SAFETY: the buffer and its length match; no find data is asked for.
    unsafe { shell.GetPath(&mut path, std::ptr::null_mut(), 0) }.ok()?;
    let mut args = vec![0u16; 32 * 1024];
    // SAFETY: the buffer and its length match.
    if unsafe { shell.GetArguments(&mut args) }.is_err() {
        args.clear();
    }
    Some((wide_str(&path), wide_str(&args)))
}

/// The text up to the first NUL.
fn wide_str(units: &[u16]) -> String {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// `s` with `%VAR%` references expanded; `s` itself when it has none or the
/// expansion fails.
fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let wide: Vec<u16> = s.encode_utf16().chain([0]).collect();
    // SAFETY: the source is NUL-terminated; with no buffer the call reports the size.
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(wide.as_ptr()), None) };
    if needed == 0 {
        return s.to_owned();
    }
    let mut out = vec![0u16; needed as usize];
    // SAFETY: the buffer holds `needed` units, which is what the call asked for.
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(wide.as_ptr()), Some(&mut out)) };
    if written == 0 || written as usize > out.len() {
        return s.to_owned();
    }
    wide_str(&out)
}

/// The executable a command line starts, when it can be found on disk.
///
/// A command line names its program either quoted, or unquoted up to a space; but an
/// unquoted path with spaces is legal and common (`C:\Program Files\x\y.exe /a`), so,
/// like `CreateProcess`, every prefix ending at a space is tried in turn, with `.exe`
/// appended when the prefix has no extension. A bare name (`OneDrive.exe`,
/// `rundll32.exe`) is looked up the way the loader would, with `SearchPathW`.
fn image_of(command: &str) -> Option<String> {
    program_of(command, &resolve_on_disk)
}

/// [`image_of`] with the file lookup supplied, so the parsing is testable without
/// the files. `resolve` gets a candidate program name and answers with its full path
/// when it names a file.
fn program_of(command: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let try_candidate = |candidate: &str| -> Option<String> {
        let candidate = candidate.trim_end();
        if candidate.is_empty() {
            return None;
        }
        resolve(candidate).or_else(|| {
            let has_extension = Path::new(candidate)
                .extension()
                .is_some_and(|e| !e.is_empty());
            if has_extension {
                None
            } else {
                resolve(&format!("{candidate}.exe"))
            }
        })
    };
    if let Some(rest) = command.strip_prefix('"') {
        let program = rest.split('"').next().unwrap_or(rest);
        return try_candidate(program);
    }
    // Shortest prefix first, as the loader does, then the whole line.
    for (i, c) in command.char_indices() {
        if c == ' ' {
            if let Some(found) = try_candidate(&command[..i]) {
                return Some(found);
            }
        }
    }
    try_candidate(command)
}

/// A candidate program's full path when it is a file: as given when it carries a
/// directory, through the loader's search path when it is a bare name.
fn resolve_on_disk(candidate: &str) -> Option<String> {
    let bare = !candidate.contains(['\\', '/', ':']);
    if !bare {
        return Path::new(candidate).is_file().then(|| candidate.to_owned());
    }
    let wide: Vec<u16> = candidate.encode_utf16().chain([0]).collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: the name is NUL-terminated; the buffer and its length match.
    let len = unsafe {
        SearchPathW(
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            Some(&mut buf),
            None,
        )
    } as usize;
    (len > 0 && len < buf.len()).then(|| String::from_utf16_lossy(&buf[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::{RegDeleteValueW, RegGetValueW, RRF_RT_REG_BINARY};

    #[test]
    fn entries_are_sorted_by_name_and_complete() {
        let started = std::time::Instant::now();
        let list = entries();
        let cold = started.elapsed();
        let started = std::time::Instant::now();
        let again = entries();
        let warm = started.elapsed();
        eprintln!(
            "{} startup entries in {cold:?} cold, {warm:?} warm",
            list.len()
        );
        assert_eq!(again, list, "two reads agree");
        for e in &list {
            eprintln!(
                "  [{}] {:?} {} enabled={} image={:?}\n      {}",
                e.location.label(),
                e.name,
                if e.enabled { "" } else { "(off)" },
                e.enabled,
                e.image_path,
                e.command
            );
            assert_ne!(e.name, "");
            assert!(e.publisher.is_none(), "publisher is filled elsewhere");
        }
        assert!(list
            .windows(2)
            .all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase()));
    }

    #[test]
    fn the_program_is_found_in_quoted_unquoted_and_bare_commands() {
        let known = [
            r"C:\Program Files\x.exe",
            r"C:\x\y.exe",
            r"C:\Windows\system32\rundll32.exe",
        ];
        let resolve = |c: &str| -> Option<String> {
            if c.contains(['\\', '/', ':']) {
                known.iter().find(|k| k.eq_ignore_ascii_case(c))
            } else {
                known
                    .iter()
                    .find(|k| k.rsplit('\\').next().unwrap().eq_ignore_ascii_case(c))
            }
            .map(|k| (*k).to_owned())
        };
        assert_eq!(
            program_of(r#""C:\Program Files\x.exe" --flag"#, &resolve).as_deref(),
            Some(r"C:\Program Files\x.exe")
        );
        assert_eq!(
            program_of(r"C:\x\y.exe /a", &resolve).as_deref(),
            Some(r"C:\x\y.exe")
        );
        assert_eq!(
            program_of(r"C:\Program Files\x.exe --flag", &resolve).as_deref(),
            Some(r"C:\Program Files\x.exe"),
            "an unquoted path with a space"
        );
        assert_eq!(
            program_of(r"C:\x\y /a", &resolve).as_deref(),
            Some(r"C:\x\y.exe"),
            ".exe is implied"
        );
        assert_eq!(
            program_of("rundll32.exe shell32.dll,Control_RunDLL", &resolve).as_deref(),
            Some(r"C:\Windows\system32\rundll32.exe")
        );
        assert_eq!(program_of(r"C:\nowhere\z.exe", &resolve), None);
        assert_eq!(program_of("", &resolve), None);
        assert_eq!(program_of("\"", &resolve), None);
    }

    #[test]
    fn environment_variables_expand_before_the_program_is_looked_up() {
        let command = r"%SystemRoot%\system32\rundll32.exe shell32.dll,Control_RunDLL";
        let expanded = expand_env(command);
        assert!(!expanded.contains('%'), "{expanded}");
        let image = image_of(&expanded).expect("rundll32 exists on every Windows");
        assert!(
            image.to_lowercase().ends_with(r"\system32\rundll32.exe"),
            "{image}"
        );
        // Unexpanded, the path does not exist and the bare lookup does not apply.
        assert_eq!(image_of(command), None);
        // Bare names go through the loader's search path.
        assert!(image_of("rundll32.exe foo").is_some());
        assert_eq!(expand_env("no variables here"), "no variables here");
    }

    /// The first byte of a switch value, read back directly.
    fn switch_byte(name: PCWSTR) -> Option<u8> {
        let mut data = [0u8; 16];
        let mut size = data.len() as u32;
        // SAFETY: the out-pointers are locals; `size` is the buffer's byte length.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                APPROVED_RUN,
                name,
                RRF_RT_REG_BINARY,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&raw mut size),
            )
        };
        (status == ERROR_SUCCESS && size > 0).then_some(data[0])
    }

    #[test]
    fn a_user_run_switch_is_written_and_read_back() {
        const NAME: PCWSTR = w!("open-task-test-entry");
        let entry = StartupEntry {
            name: "open-task-test-entry".to_owned(),
            location: StartupLocation::UserRun,
            command: r"C:\nowhere\open-task-test.exe".to_owned(),
            enabled: true,
            publisher: None,
            image_path: None,
        };
        set_enabled(&entry, false).expect("disable in HKCU");
        assert_eq!(switch_byte(NAME), Some(0x03));
        assert!(!is_enabled(
            approved(HKEY_CURRENT_USER, APPROVED_RUN).get("open-task-test-entry")
        ));
        set_enabled(&entry, true).expect("enable in HKCU");
        assert_eq!(switch_byte(NAME), Some(0x02));
        assert!(is_enabled(
            approved(HKEY_CURRENT_USER, APPROVED_RUN).get("open-task-test-entry")
        ));
        // Clean up: remove the switch so the machine is as it was.
        let mut k = HKEY::default();
        // SAFETY: the out-pointer is a local; the key is closed below.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                APPROVED_RUN,
                None,
                KEY_SET_VALUE,
                &raw mut k,
            )
            .ok()
            .expect("open for delete");
            let status = RegDeleteValueW(k, NAME);
            let _ = RegCloseKey(k);
            status
        };
        assert_eq!(status, ERROR_SUCCESS);
        assert_eq!(switch_byte(NAME), None);
    }

    #[test]
    fn a_machine_switch_needs_elevation() {
        if super::super::access::is_elevated() {
            eprintln!("elevated; skipping the access-denied check");
            return;
        }
        let entry = StartupEntry {
            name: "open-task-test-entry".to_owned(),
            location: StartupLocation::MachineRun,
            command: String::new(),
            enabled: true,
            publisher: None,
            image_path: None,
        };
        assert!(matches!(
            set_enabled(&entry, false),
            Err(ControlError::NotPermitted)
        ));
    }

    #[test]
    fn switch_bytes_follow_the_low_bit() {
        assert!(is_enabled(None));
        assert!(is_enabled(Some(&0x02)));
        assert!(is_enabled(Some(&0x06)));
        assert!(!is_enabled(Some(&0x03)));
        assert!(!is_enabled(Some(&0x07)));
    }

    #[test]
    fn commands_are_joined_with_quoting_only_when_needed() {
        assert_eq!(join_command(r"C:\x\y.exe", ""), r"C:\x\y.exe");
        assert_eq!(join_command(r"C:\x\y.exe", "/a"), r"C:\x\y.exe /a");
        assert_eq!(
            join_command(r"C:\Program Files\x.exe", "--flag"),
            r#""C:\Program Files\x.exe" --flag"#
        );
    }
}
