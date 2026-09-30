//! The console launcher, shipped on Windows as `open-task.com` beside
//! `open-task.exe`.
//!
//! Release builds of `open-task.exe` are GUI programs, and no shell waits for one,
//! so `open-task --version` typed at a prompt would print after the prompt came
//! back. This console program is what a terminal runs instead (`.COM` comes before
//! `.EXE` in `PATHEXT`): it starts the exe beside it with the same arguments and
//! the terminal's handles, waits while a command-line mode runs, and returns at once
//! when the exe opens its window. See `ot_shell_win::launcher`.

#![forbid(unsafe_code)]

fn main() {
    #[cfg(windows)]
    std::process::exit(ot_shell_win::launcher::run());
    #[cfg(not(windows))]
    {
        eprintln!("open-task-console is the Windows console launcher; run open-task itself");
        std::process::exit(2);
    }
}
