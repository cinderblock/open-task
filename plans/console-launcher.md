# A console launcher (`open-task.com`) so command-line modes work from a terminal

> **Status:** active · **Started:** 2026-09-29 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Found while checking v0.4.0 (`plans/usage-map.md`, open question 1). Ships as v0.4.1.

## Goal

The user, 2026-09-29: "ship the --version fix". The release `open-task.exe` is a
GUI-subsystem program, so no shell waits for it. Typed at a prompt, `open-task
--version` (and `--headless`, `--check-update`, `--sample`) returns to the prompt at
once and prints afterwards. PowerShell's `> file` leaves the file empty. `cmd /c`
exits first, and the exe's `println!` then panics with "failed printing to stdout:
The pipe is being closed. (os error 232)". Fix it properly, and stop the panic.

## Environment / context

- `crates/ot-app/src/main.rs`: `#![windows_subsystem = "windows"]` in release,
  `attach_parent_console()` (in `ot-shell-win/src/lib.rs`) for `--headless`,
  `--version`, `--check-update`. `--sample` without `--headless` never attached
  a console, so it printed nowhere in a release build.
- `crates/ot-app/build.rs` writes the Windows `.res` (version + icon) and links it
  into every bin (`cargo:rustc-link-arg-bins`).
- `windows` crate 0.62 in `ot-shell-win` (`HANDLE(*mut c_void)`).
- Packaging: `.github/workflows/release.yml` (zip per target, then the installer
  from both Windows zips), `scripts/build-installer.ps1`,
  `installer/windows/open-task.iss` (PATH entry is an opt-in task, `addtopath`).
- The release profile is `panic = "abort"`, so the stdout panic aborts the process.
- Other threads work in this tree; `plans/replace-task-manager.md` is not ours.

## Decisions already made (don't re-ask)

1. **User, 2026-09-29: "ship the --version fix"**, which approved the recommendation:
   a console-subsystem `open-task.com` beside `open-task.exe` (the `devenv.com`
   pattern), plus no panic when stdout goes away. Released as a patch, v0.4.1.
2. **The launcher is a second bin of `ot-app`** (`open-task-console`), renamed to
   `open-task.com` when packaged. Cargo cannot emit a `.com`. Being in `ot-app`
   gives its integration tests both binaries side by side (`CARGO_BIN_EXE_*`).
3. **One source of truth for "is this a command-line mode":** the exe decides.
   The launcher does not parse arguments. It starts the exe with the same arguments
   and the terminal's handles, then waits for whichever comes first:
   - the exe exits: the launcher exits with the exe's code;
   - the exe signals "opening a window": the launcher exits 0 at once.
4. **The signal is an inheritable event** whose handle value travels in the
   environment variable `OPEN_TASK_LAUNCHER_EVENT`. The exe removes the variable at
   startup, so processes it starts later (the updater's installer, a relaunch)
   cannot see a stale handle. Both sides of the protocol live in one file,
   `ot-shell-win/src/launcher.rs`.
5. **Going to a window, the exe drops the terminal's handles** (`SetStdHandle` to
   null, then closes them), so a pipe reader is not held open by the window. It
   then behaves as if started from Explorer.
6. **The launcher ignores Ctrl+C and Ctrl+Break with a handler routine**, not
   `SetConsoleCtrlHandler(NULL, TRUE)`: the NULL form is inherited by children and
   would make the exe ignore Ctrl+C too. The exe gets the event (it is attached
   to the same console), exits, and the launcher relays its code.
7. **Stdout writes that fail exit quietly** instead of panicking: code 0 on a
   broken pipe (the reader chose to stop, as ripgrep does), 1 otherwise.

## Plan / steps

1. ~~`ot-shell-win::launcher`: `run()` (launcher side), `release()` (exe side),
   unit test of the handshake.~~
2. ~~`ot-app`: `src/console.rs` bin; `main.rs` decides the mode first; `out!` for
   stdout; `build.rs` gives the launcher its own version resource. Integration
   tests through the launcher (`--version`, an exit code relayed).~~
3. ~~Packaging: `release.yml` ships `open-task.com` in the Windows zips and checks
   it with `cmd /c` redirected to a file (the case that failed); installer
   installs it; `build-installer.ps1` passes it.~~
4. ~~README; checks; a local end-to-end with release builds (`cmd /c`, PowerShell
   `>`, `Get-Command` picking the `.com`, a GUI launch returning at once).~~
5. **[current]** Commit; release v0.4.1 via tag; verify from the published assets.

## Findings / gotchas

- **`std::process::Command` leaks every inheritable handle into the child**, not
  only the three it passes: it duplicates the standard handles as inheritable and
  calls `CreateProcessW` with `bInheritHandles = TRUE`, so the launcher's own
  inheritable copy of its stdout pipe went along too. The window closed the
  duplicates it knew about and held the original, and `open-task.com | Out-String`
  waited 62 s, until the window was closed. Fix: the launcher calls
  `CreateProcessW` itself with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` (the three
  standard handles, duplicated as inheritable, and the event). Afterwards the
  launcher exits and its pipe closes 0.17 s after starting a window. Passing the
  launcher's own command-line tail verbatim (`GetCommandLineW`, argv[0] split as
  the CRT does) came with it, so no quoting is re-done.
- **This environment sets `NoDefaultCurrentDirectoryInExePath=1`**, so `cmd` does
  not search the current directory; and PowerShell's `Push-Location` does not
  change the process's working directory anyway. Twice a local `cmd /c "open-task
  --version"` therefore ran an old `cargo install` copy in `~\.cargo\bin` that
  predates `--version` and opened its window (closed again each time). The
  release check now puts the release folder on PATH instead, which is also how a
  PATH-option install is found.
- **`SetConsoleCtrlHandler(NULL, TRUE)` is inherited**; the launcher uses a
  handler routine instead. Checked: Ctrl+Break sent to the launcher's process
  group with `--headless --passes 100` ends the exe; the launcher returns 7 ms
  later with the exe's 0xC000013A.
- **v0.4.0 with a reader that stops early:** exits 0xC0000409 (the stdout panic,
  under `panic = "abort"`) at the next write. Now exit 0, directly and through the
  launcher (test `a_reader_that_stops_early_ends_the_output_quietly`).
- The launcher is 170 KB (with the icon); with `std::process` it was 259 KB.
- PowerShell does not update `$LASTEXITCODE` when `Select-Object -First` stops a
  pipeline; check exit codes with `System.Diagnostics.Process` instead.
- The local installer compile (portable Inno in `target/tools/innosetup`, without
  `/Qp`) lists `open-task.com` among the files it compresses. The installer was
  not run: a per-user test install would register a second entry under the same
  AppId as the real per-machine install.
- Scratch checks in `target/launcher-e2e/`: `cmd-check.ps1` (the release step's
  logic, any folder), `ctrl-break.ps1`, `release-step.ps1` (the step's script
  pulled out of the YAML).

## Progress log

- [x] Launcher module and handshake (`ot-shell-win/src/launcher.rs`), four unit
      tests.
- [x] ot-app bin (`src/console.rs`), mode decided first (also fixes `--sample`
      without `--headless` printing nowhere), quiet stdout (`out!`), per-binary
      version resources, three integration tests (`tests/console_launcher.rs`).
- [x] Packaging: zip ships `open-task.com`; `release.yml` checks `cmd /c` with a
      redirect through it (step run locally: passes, and fails on a wrong tag);
      installer installs it; `build-installer.ps1` requires it.
- [x] README (Install, Updates, Build); clippy clean on x64 Windows, Linux and
      macOS; `cargo test --workspace` passes; end-to-end with release builds:
      `cmd /c` redirect, PowerShell redirect, `Get-Command`/`where` pick the
      `.com`, exit code 4 relayed, early reader, window launch 0.17 s, Ctrl+Break.
- [ ] v0.4.1 released and verified.

## Open questions for the user

- None.

## Things not to do

- Do not use `SetConsoleCtrlHandler(None, TRUE)` in the launcher (inherited).
- Do not duplicate the list of command-line flags in the launcher.
- Do not stage other threads' files.
