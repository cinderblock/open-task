//! The console launcher, `open-task.com`, and how `open-task.exe` answers it.
//!
//! Release builds of `open-task.exe` are GUI programs, and no shell waits for one:
//! typed at a prompt, `open-task --version` would come back to the prompt at once
//! and print after it, and `cmd /c` would be gone before it printed at all. The
//! launcher is a console program that a shell does wait for, shipped beside the exe
//! as `open-task.com`; `.COM` comes before `.EXE` in `PATHEXT`, so it is what
//! `open-task` runs from a terminal. It starts `open-task.exe` with the same
//! arguments and the terminal's handles, then waits for whichever comes first:
//!
//! - the exe exits, having run a command-line mode: the launcher exits with its
//!   code;
//! - the exe says it is opening a window: the launcher exits at once, giving the
//!   prompt back, and the window carries on as if started from Explorer.
//!
//! Only the exe knows which arguments are command-line modes, so the launcher parses
//! none: they go through exactly as the launcher got them. "Opening a window" is an
//! inheritable event; the launcher puts its handle value in [`EVENT_VAR`], the exe
//! takes it with [`Launcher::adopt`] and signals it with [`Launcher::release`].
//!
//! The exe inherits four handles and nothing else: the three standard handles and
//! the event (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). Without the list it would also
//! inherit every other inheritable handle the launcher holds, among them the
//! launcher's own copy of a pipe it was started with, and a window that never knew
//! about that copy would hold the pipe open, and its reader waiting, for as long as
//! the window was up.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Console::{
    GetStdHandle, SetConsoleCtrlHandler, SetStdHandle, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Environment::GetCommandLineW;
use windows::Win32::System::Threading::{
    CreateEventW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, SetEvent, UpdateProcThreadAttribute,
    WaitForMultipleObjects, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

/// The environment variable that carries the launcher's event, as a decimal handle
/// value.
pub const EVENT_VAR: &str = "OPEN_TASK_LAUNCHER_EVENT";

/// The program the launcher starts, found beside the launcher itself.
const EXE: &str = "open-task.exe";

/// Run the launcher: start `open-task.exe` beside this program with this program's
/// arguments, and return the exit code to leave with.
#[must_use]
pub fn run() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(me) => me.with_file_name(EXE),
        Err(e) => {
            eprintln!("open-task: cannot tell where this launcher is: {e}");
            return 1;
        }
    };
    // Ctrl+C and Ctrl+Break reach every process on the console, the exe included;
    // the launcher lets the exe decide what they mean and relays how it ended. A
    // handler routine, not SetConsoleCtrlHandler(None, TRUE): that form is
    // inherited, and would make the exe ignore Ctrl+C too.
    // SAFETY: the routine is a plain function that lives as long as the process.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(leave_interrupts_to_the_exe), true);
    }
    let event = match inheritable_event() {
        Ok(e) => Owned(e),
        Err(e) => {
            eprintln!("open-task: cannot create the launcher's event: {e}");
            return 1;
        }
    };
    // Where the exe finds the event: the environment it inherits from this one.
    std::env::set_var(EVENT_VAR, (event.0 .0 as usize).to_string());
    let process = match spawn(&exe, event.0) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("open-task: cannot start {}: {e}", exe.display());
            return 1;
        }
    };
    // SAFETY: both handles stay open for the whole wait.
    let woke = unsafe { WaitForMultipleObjects(&[process.0, event.0], false, INFINITE) };
    if woke.0 == WAIT_OBJECT_0.0 + 1 {
        // A window is opening; the terminal is done with.
        return 0;
    }
    let mut code = 1u32;
    // SAFETY: the process handle is open, and has exited.
    unsafe {
        let _ = GetExitCodeProcess(process.0, &raw mut code);
    }
    // An NTSTATUS such as 0xC000013A (ended by Ctrl+C) goes back out as it came.
    code.cast_signed()
}

/// Console control handler for the launcher: swallow Ctrl+C and Ctrl+Break, pass
/// on the rest (closing the console, logging off, shutting down).
unsafe extern "system" fn leave_interrupts_to_the_exe(kind: u32) -> BOOL {
    BOOL::from(kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT)
}

/// A manual-reset event, unsignaled, that child processes inherit.
fn inheritable_event() -> windows::core::Result<HANDLE> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: BOOL::from(true),
    };
    // SAFETY: `attributes` outlives the call, and a null name makes an unnamed event.
    unsafe { CreateEventW(Some(&raw const attributes), true, false, PCWSTR::null()) }
}

/// A handle this process owns, closed on drop. Null stands for none.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle is ours and nothing uses it after this.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Start `exe` with this launcher's arguments, handing it the terminal: this
/// process's standard handles become its own. Those and `event` are all it
/// inherits.
fn spawn(exe: &Path, event: HANDLE) -> windows::core::Result<Owned> {
    // Inheritable copies: a handle list takes only inheritable handles, and the
    // originals need not be. A missing standard handle stays null.
    let standard = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(inheritable_copy);
    let mut inherit: Vec<HANDLE> = standard
        .iter()
        .map(|h| h.0)
        .filter(|h| !h.is_invalid())
        .collect();
    inherit.push(event);
    let list = AttributeList::new(1)?;
    // SAFETY: the list keeps a pointer to `inherit`, which outlives the process
    // creation below.
    unsafe {
        UpdateProcThreadAttribute(
            list.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherit.as_ptr().cast()),
            size_of_val(inherit.as_slice()),
            None,
            None,
        )?;
    }
    let info = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: standard[0].0,
            hStdOutput: standard[1].0,
            hStdError: standard[2].0,
            ..Default::default()
        },
        lpAttributeList: list.list,
    };
    let mut command_line = command_line(exe);
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: every pointer is to a local that outlives the call, and the command
    // line is writable and nul-terminated, as CreateProcessW requires.
    unsafe {
        CreateProcessW(
            &HSTRING::from(exe.as_os_str()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR::null(),
            &raw const info.StartupInfo,
            &raw mut process,
        )?;
        let _ = CloseHandle(process.hThread);
    }
    Ok(Owned(process.hProcess))
}

/// An inheritable duplicate of one of this process's standard handles; null when
/// there is none.
fn inheritable_copy(which: STD_HANDLE) -> Owned {
    let mut copy = HANDLE::default();
    // SAFETY: plain handle-table calls on this process's own handles.
    unsafe {
        if let Ok(original) = GetStdHandle(which) {
            if !original.is_invalid() {
                let me = GetCurrentProcess();
                let _ = DuplicateHandle(
                    me,
                    original,
                    me,
                    &raw mut copy,
                    0,
                    true,
                    DUPLICATE_SAME_ACCESS,
                );
            }
        }
    }
    Owned(copy)
}

/// A process attribute list with room for a number of attributes, deleted on drop.
struct AttributeList {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    /// The memory `list` points into.
    _buffer: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> windows::core::Result<Self> {
        let mut size = 0usize;
        // SAFETY: with no list, the call only reports the size needed (and fails,
        // by design, with ERROR_INSUFFICIENT_BUFFER).
        let _ = unsafe { InitializeProcThreadAttributeList(None, count, None, &raw mut size) };
        // `usize`s, for the pointer alignment the list needs; the heap block does
        // not move when the Vec does.
        let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
        // SAFETY: `buffer` holds at least `size` bytes.
        unsafe { InitializeProcThreadAttributeList(Some(list), count, None, &raw mut size)? };
        Ok(Self {
            list,
            _buffer: buffer,
        })
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`, and its buffer is still alive.
        unsafe { DeleteProcThreadAttributeList(self.list) }
    }
}

/// The command line to start `exe` with: its path, quoted, then this process's own
/// arguments exactly as they were given, so no quoting is lost or added on the way.
fn command_line(exe: &Path) -> Vec<u16> {
    // SAFETY: this process's command line, nul-terminated, valid for the life of
    // the process.
    let own = unsafe { GetCommandLineW() };
    // SAFETY: as above.
    let own = unsafe { own.as_wide() };
    let quote = u16::from(b'"');
    let mut line = vec![quote];
    line.extend(exe.as_os_str().encode_wide());
    line.push(quote);
    let rest = arguments(own);
    if !rest.is_empty() {
        line.push(u16::from(b' '));
        line.extend_from_slice(rest);
    }
    line.push(0);
    line
}

/// What follows the program name in a command line, split as the C runtime and
/// Rust split it: the name runs to the first space or tab outside quotes, with no
/// escapes inside it, and the spaces and tabs after it are skipped.
fn arguments(line: &[u16]) -> &[u16] {
    const QUOTE: u16 = b'"' as u16;
    const SPACE: u16 = b' ' as u16;
    const TAB: u16 = b'\t' as u16;
    let mut quoted = false;
    let mut i = 0;
    while let Some(&c) = line.get(i) {
        match c {
            QUOTE => quoted = !quoted,
            SPACE | TAB if !quoted => break,
            _ => {}
        }
        i += 1;
    }
    while matches!(line.get(i), Some(&(SPACE | TAB))) {
        i += 1;
    }
    &line[i..]
}

/// The launcher that started this process, if one did.
#[derive(Debug)]
pub struct Launcher {
    /// The launcher's event, inherited.
    event: HANDLE,
}

impl Launcher {
    /// Take the launcher's event from the environment, and remove the variable so
    /// that nothing this process starts later (the updater's installer, a relaunch)
    /// sees a handle that means nothing to it. Call it first thing in `main`, before
    /// any other thread exists.
    #[must_use]
    pub fn adopt() -> Option<Self> {
        Self::from_var(&std::env::var_os(EVENT_VAR)?)
    }

    /// [`Self::adopt`], given the variable's value.
    fn from_var(value: &OsStr) -> Option<Self> {
        std::env::remove_var(EVENT_VAR);
        let raw: usize = value.to_str()?.parse().ok()?;
        (raw != 0).then_some(Self {
            event: HANDLE(raw as *mut std::ffi::c_void),
        })
    }

    /// A window is opening: let the launcher give the terminal back its prompt, and
    /// let go of the terminal's handles, so a pipe it was reading is not held open
    /// for as long as the window is. From here on the process is as if started from
    /// Explorer.
    pub fn release(self) {
        self.signal();
        drop_standard_handles();
    }

    /// Signal the launcher's event. It is closed only if signaling worked, which
    /// also shows the handle was an event: a stray value from the environment is
    /// left alone.
    fn signal(self) {
        // SAFETY: SetEvent fails harmlessly on anything that is not an event.
        unsafe {
            if SetEvent(self.event).is_ok() {
                let _ = CloseHandle(self.event);
            }
        }
    }
}

/// Point standard input, output and error nowhere, and close what they were. Writes
/// to stdout and stderr are then dropped silently.
fn drop_standard_handles() {
    let mut closed: [HANDLE; 3] = [HANDLE::default(); 3];
    for (i, which) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .enumerate()
    {
        // SAFETY: plain handle-table calls; each handle is closed at most once,
        // after it is no longer a standard handle.
        unsafe {
            let Ok(h) = GetStdHandle(which) else {
                continue;
            };
            let _ = SetStdHandle(which, HANDLE::default());
            if !h.is_invalid() && !closed.contains(&h) {
                let _ = CloseHandle(h);
                closed[i] = h;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::WaitForSingleObject;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn rest(line: &str) -> String {
        String::from_utf16(arguments(&wide(line))).expect("UTF-16")
    }

    #[test]
    fn the_arguments_go_through_exactly_as_given() {
        assert_eq!(rest("open-task --version"), "--version");
        assert_eq!(
            rest(r#""C:\Program Files\open-task\open-task.com" --view  "a \"b\" c"  "#),
            r#"--view  "a \"b\" c"  "#
        );
        assert_eq!(rest("open-task"), "");
        assert_eq!(rest("open-task \t "), "");
        // Quotes toggle anywhere in the name, as the C runtime reads it.
        assert_eq!(rest(r#""a b"c d"#), "d");
        assert_eq!(rest("x\t\t--headless"), "--headless");
    }

    #[test]
    fn the_exe_is_named_by_its_full_path_quoted() {
        let line = command_line(Path::new(r"C:\Program Files\open-task\open-task.exe"));
        let text = String::from_utf16(&line[..line.len() - 1]).expect("UTF-16");
        assert!(
            text.starts_with(r#""C:\Program Files\open-task\open-task.exe""#),
            "{text}"
        );
        assert_eq!(line.last(), Some(&0));
    }

    #[test]
    fn opening_a_window_signals_the_launcher_and_clears_the_variable() {
        let event = inheritable_event().expect("an event");
        // A second handle to watch it by: signaling closes the adopted one.
        let mut watch = HANDLE::default();
        // SAFETY: duplicating a handle this process owns, within this process.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                event,
                GetCurrentProcess(),
                &raw mut watch,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .expect("a second handle");
        }
        let value = (event.0 as usize).to_string();
        let launcher = Launcher::from_var(OsStr::new(&value)).expect("adopted");
        assert!(std::env::var_os(EVENT_VAR).is_none());
        // SAFETY: `watch` is open.
        assert_ne!(unsafe { WaitForSingleObject(watch, 0) }, WAIT_OBJECT_0);
        launcher.signal();
        // SAFETY: `watch` is open until closed here.
        unsafe {
            assert_eq!(WaitForSingleObject(watch, 0), WAIT_OBJECT_0);
            let _ = CloseHandle(watch);
        }
    }

    #[test]
    fn a_value_that_is_not_a_handle_is_ignored() {
        assert!(Launcher::from_var(OsStr::new("not a number")).is_none());
        assert!(Launcher::from_var(OsStr::new("0")).is_none());
    }
}
