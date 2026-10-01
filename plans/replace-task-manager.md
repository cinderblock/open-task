# Replace Task Manager

> **Status:** shipped in v0.5.0 (2026-10-01); one manual check left for the user (the UAC prompt) · **Started:** 2026-09-26 (planned), resumed and built 2026-09-30 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md`. Follows `plans/windows-installer.md`.

## Goal

The user asked for Process Explorer's *Options → Replace Task Manager*: afterwards
Ctrl+Shift+Esc, the taskbar's Task Manager item, the Ctrl+Alt+Del screen, and
anything else that starts `taskmgr.exe` open open-task instead.

History: the user first asked on 2026-09-26, after v0.2.1. A plan was written that
day (an Options menu on the toolbar, dark popup menus, v0.3.0) and then sat untracked
and dormant; no code was ever written (checked 2026-09-30: nothing in `crates/` or
`installer/`, nor in any `refs/t3/checkpoints/*` snapshot). On 2026-09-30 the user
asked "Did we implement it? Please implement it." Since then the app gained a Settings
page (v0.3.0), and the installer already went all-users by default (`d3ef424`), so the
plan was revised (decisions below).

## Environment / context

- The mechanism, same as Process Explorer: registry key
  `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe`,
  string value `Debugger` = `"<full path to open-task.exe>"`. Windows starts the named
  "debugger" instead of `taskmgr.exe`, with the original command line appended.
  Writing HKLM needs administrator rights. Removing the value restores Task Manager.
  A value naming a missing file breaks Ctrl+Shift+Esc for every user of the machine.
- Only the 64-bit registry view matters (Explorer and Winlogon are 64-bit; there was
  no `WOW6432Node` IFEO key for taskmgr here either). The app opens it with
  `KEY_WOW64_64KEY`, the installer with `HKLM64`.
- The Claude tool session on this machine runs **elevated** (High integrity). The app
  normally does not. Test windows appear on the user's desktop.
- Installed here: open-task **v0.3.1** in `C:\Program Files\open-task` (all users),
  with no tasks selected. Inno Setup is portable under `target/tools/innosetup`
  (7.1.0) and `target/tools/innosetup6` (6.7.3); `scripts/build-installer.ps1` finds
  them. The Inno help decompiles with `hh.exe -decompile out ISetup.chm` only in a
  folder whose path has no spaces (it silently produced nothing under the repo path).
- `plans/open-task-architecture.md`: **no push to a remote without explicit
  per-action approval.** Releases so far each had the user's go-ahead.

## Decisions already made (don't re-ask)

1. **The switch lives on the Settings page**, in a "Windows" section, as a card
   like the others: "Replace Task Manager". Revised from the 2026-09-26 plan (an
   Options popup menu on the toolbar, with uxtheme's undocumented dark-menu calls):
   that predates the Settings page, which now holds every other option. Windows 11's
   Task Manager also keeps its options on a Settings page. No menus, no uxtheme.
2. **Replacing is machine state, not a user setting.** It is read from the registry
   (at start, whenever the window is activated, and after every change), never
   stored in `HKCU\Software\open-task`. The card shows what Windows will actually do.
3. **The switch is On only when Windows opens *this* copy.** If another program is
   the replacement (Process Explorer, another copy of open-task), the switch shows
   Off and the card names it; turning the switch on overwrites it, as Process
   Explorer would. Turning it off removes the value only if it names this copy, and
   deletes the key only if nothing else is left in it.
4. **Elevation:** the app first tries the write itself (it works when open-task runs
   as administrator). On "access denied" it starts itself elevated
   (`ShellExecuteEx`, verb `runas`) with `--replace-task-manager` or
   `--restore-task-manager`, on a worker thread so the window stays live while the
   UAC prompt is up, and re-reads the registry when the helper ends. A cancelled UAC
   prompt changes nothing and says nothing; any other failure is shown in a message
   box. The helper's exit code is the Win32 error code (0 for success).
5. **The two flags are also for people and scripts.** From a terminal that is not
   elevated they fail with "access denied" and say to use an elevated terminal; they
   never pop a UAC prompt on their own.
6. **Only a launch as Task Manager's stand-in is single-instance.** When Windows
   starts open-task in Task Manager's place (first argument is a path ending in
   `taskmgr.exe`) and an open-task window is already open, that window comes forward
   and the new process exits, as Task Manager does. Launching open-task any other way
   still opens a new window, so "Run as administrator" beside an unelevated window
   keeps working. A named mutex (`Local\open-task.window`, held until the process
   ends) covers two presses in quick succession; a registered message
   (`open-task.raise`), allowed through UIPI with `ChangeWindowMessageFilterEx`,
   reaches an elevated window from an unelevated stand-in. The check runs in `main`
   before the probe is set up (performance counters, hardware), so the open window
   comes forward without that delay. Revised from the old plan's "the app is
   single-instance".
7. **Any copy may replace Task Manager, and the card says what that means.** The
   app does not refuse a copy that is not installed for all users (as Process
   Explorer does not), but the card says: installed for you only, so other accounts
   on this PC cannot open it; or not installed, so moving or deleting it breaks
   Ctrl+Shift+Esc. The installer offers the task only for all-users installs.
8. **Installer task "Use open-task instead of Task Manager", off by default**, shown
   only in all-users mode. Its checkbox shows the machine's current state (whether
   Windows opens this install), not the choice remembered from the last install.
   Unticking it restores Task Manager if it was this install. **Silent installs
   (every update) never touch it** unless `/TASKS` or `/MERGETASKS` names
   `replacetaskmgr`: an update must not undo what the user changed in the app since.
   **The uninstaller always removes the value if it names the copy being
   uninstalled**, whoever set it; a per-user uninstall does it through the elevated
   helper, and warns if that is declined. Never leave a dangling debugger entry.
9. **Version: v0.5.0** (a feature, minor bump). The user, 2026-10-01: "go ahead and
   release v0.5.0".
10. **The Settings page scrolls** (wheel, below the page title). A fourth section put
    the page at about 530 DIP, past the bottom of a 760 px window at 150 %; clipped
    cards would have hidden the new switch. Same pattern as the Performance list.

## Plan / steps

1. ~~Confirm nothing was implemented; measure how Windows starts the replacement
   (see Findings); rewrite this plan.~~
2. ~~`ot-ui`: `TaskManager` state (`crates/ot-ui/src/task_manager.rs`),
   `Effect::ReplaceTaskManager(bool)`, the Windows section and card on the Settings
   page, notes, scrolling. Tests.~~
3. ~~`ot-shell-win`: `task_manager` module (read, replace, restore, the elevated
   helper, parsing; tests against a scratch HKCU key), `instance` module (mutex,
   raise), window wiring (effect, `WM_APP_TASK_MANAGER`, `WM_ACTIVATEAPP` refresh,
   the registered message).~~
4. ~~`ot-app`: `--replace-task-manager` / `--restore-task-manager`; the stand-in
   launch raises an open window before the probe is set up.~~
5. ~~Installer: task, checkbox from current state, silent-install rule, uninstall
   (per-user through the elevated app).~~
6. ~~Verify here (see Progress log for what ran and how).~~ The one thing not done
   here: the unelevated Settings switch through a **real UAC prompt**, which needs a
   person to click it (UAC is at the default, prompting on the secure desktop).
7. ~~README, plans; commit.~~
8. ~~Release v0.5.0; check it from the published assets.~~ **[current]** The user's
   UAC click-through (open question 1).

## Findings / gotchas

- **The replacement runs with the caller's rights, not Task Manager's.** Measured
  2026-09-30 by pointing the value at `cmd.exe /c probe.cmd` (which logs its
  arguments and `whoami /groups`), then removing the key again:
  - `explorer.exe C:\Windows\System32\taskmgr.exe` (unelevated Explorer):
    `"C:\Windows\System32\Taskmgr.exe"`, **Medium** integrity. Task Manager itself
    would have auto-elevated; the debugger does not.
  - Ctrl+Shift+Esc (synthesized with `keybd_event`): `"C:\WINDOWS\System32\Taskmgr.exe" /2`,
    **Medium** integrity.
  - `Start-Process taskmgr.exe` from the elevated session: High (inherited).
  So open-task standing in for Task Manager is unelevated, like a Start-menu launch:
  no silent elevation, and so no elevation hole from a copy in a user-writable
  folder (decision 7 rests on this). It also means service tags and CPU sampling
  are unavailable there until the user runs open-task as administrator.
- Windows appends its own arguments (`/2` here); open-task ignores them, and
  recognises the stand-in launch by the first argument's file name.
- **Inno Setup steps through the wizard pages in a silent install too**, calling
  `CurPageChanged(wpSelectTasks)` without showing it. The first version of the
  "checkbox shows the current state" code therefore unticked the task that
  `/MERGETASKS="replacetaskmgr"` had just selected: 7 of 12 silent checks failed
  (`setup-1.log` showed the task on the command line, and nothing written). Fixed by
  mirroring the state only when `not WizardSilent` and the command line does not
  name the task.
- **An elevated helper started without a console exits 0.** Writes to a missing
  stdout succeed silently in Rust (what the console launcher already relies on), so
  `out!` does not turn a success into exit code 1. Checked with
  `Start-Process -Verb RunAs -WindowStyle Hidden open-task.exe --replace-task-manager`.
- From an elevated process, `runas` does not prompt: that is how the helper path
  (`change_elevated`) was exercised here, with a temporary debug build forced onto
  it (removed again; `grep TEMP-RTM` finds nothing).
- Driving Inno's wizard from another process: its buttons and radio buttons answer
  `BM_CLICK`; `GetWindowText` returns only captions across processes, so the Ready
  page's summary (`TNewMemo`) has to be read with `WM_GETTEXT`; the summary is empty
  when an upgrade has no tasks to list.
- `hh.exe -decompile` wrote nothing into a folder whose path has spaces.
- The ARM64 Windows standard library is not installed for this toolchain, so
  `clippy --target aarch64-pc-windows-msvc` cannot run here; CI covers it.
- **CI installs the newest stable Rust; this machine's "stable" was two releases
  behind.** Rust 1.99.0 came out on release day (2026-10-01). The first CI run on
  the release commit (36921399675) failed clippy on all four build jobs, on seven
  lines of code this change never touched (`assert_is_empty` on five bare
  `assert!(x.is_empty())` in tests; `double_must_use` on two functions returning
  `impl Iterator`), and so never ran the tests. Local clippy (1.97.1) had passed.
  Fixed by `rustup update stable` and `89e4f66`; nothing was tagged until CI was
  green. The pre-push list in `plans/open-task-architecture.md` now starts with
  the toolchain update.
- **The shared tree was mid-edit by another session during the release**
  (`plans/cycles-used.md`; the tree did not compile). The lint fix was made and
  checked in a throwaway worktree (`target/verify-wt`, branch `clippy-1.99`, both
  removed since) and pushed from there. `git merge --ff-only` then refused in the
  shared tree, because that session had `crates/ot-ui/src/process_rows.rs` open and
  the fix changes one line of it. Local master was fast-forwarded by hand
  (`target/tmp/ff-master.sh`): a `git stash create` snapshot stored first
  (`stash@{0}` at the time, "before fast-forwarding master to 89e4f66…", theirs
  to drop), the one line applied on top of their copy, `git update-ref`, the five
  untouched files checked out, the index entry for the shared file reset. Their
  diff of that file was 65 added, 7 removed before and after.

## Progress log

- [x] Checked: not implemented anywhere; plan revised.
- [x] IFEO behavior measured; test key removed, `taskmgr.exe` IFEO key absent again.
- [x] ot-ui state, card, scrolling; 8 new tests (133 in ot-ui).
- [x] Shell: `task_manager`, `instance`, window wiring; 8 new tests against a
      scratch `HKCU\Software\open-task-tests` key, which they remove.
- [x] App flags and stand-in launch.
- [x] Installer.
- [x] Checks: `cargo fmt --check`; clippy `-D warnings` on x64 Windows, Linux and
      macOS; `cargo test --workspace`; `--headless --passes 2`.
- [x] Flags, elevated: replace, restore, restore with nothing set, both flags (exit
      2), another program's value left alone. Unelevated (Medium, via Explorer):
      exit 5 with the "terminal opened as administrator" hint, nothing written.
- [x] Live, `target/tmp/rtm-drive.ps1` (17/17): `taskmgr.exe` through Explorer
      starts open-task with Task Manager's path as its argument; Ctrl+Shift+Esc
      (synthesized) with the window open, and with it minimized, brings that one
      window forward; an ordinary launch still opens a second window; an unelevated
      stand-in restores and raises an elevated window; two presses 80 ms apart with
      nothing open give one window.
- [x] Settings card, elevated (direct write), `target/tmp/rtm-settings.ps1`: on,
      off (key removed), another program's value named ("missing file" for a path
      that does not exist, with `-e` after it parsed off), refreshed on
      `WM_ACTIVATEAPP`, turning it on takes over, off removes it; a 900x420 window
      scrolls to the card. Screens in `target/tmp/rtm-shots/`.
- [x] Settings card through the elevated helper (`change_elevated`,
      `WM_APP_TASK_MANAGER`), with a temporary debug build: on and off.
- [x] Installer, against a test copy with its own AppId, name, folder and
      shortcuts (`target/tmp/rtm-make-test-iss.py`), so the real v0.3.1 install
      was never touched. Silent (`rtm-installer-test.ps1`, 12/12): the task
      replaces; it is remembered; an update keeps it; after the app turns it off an
      update does not turn it back on; `/MERGETASKS` on and `!` off; another
      program's value survives unticking; `/TASKS` takes over; uninstall removes
      ours and the key; install without the task leaves it alone; uninstall leaves
      another program's value. Interactive wizard (`rtm-wizard-test.ps1`, 5/5):
      remembered-but-off shows unticked, ticking replaces, on shows ticked,
      unticking restores. Per-user uninstall of a copy Task Manager points at
      restores it through the elevated app (`rtm-peruser-test.ps1`). The real
      `open-task.iss` compiles with Inno Setup 6.7.3 and 7.1.0.
- [x] Machine left as found: no IFEO key for taskmgr, v0.3.1 still installed with
      its Start Menu shortcut, no test installs, no open-task processes.
- [x] README (Using it, Install), this plan; committed (`362281e`).
- [x] **Shipped in v0.5.0.** Release commit `db08e59`, then `89e4f66` for the lints
      Rust 1.99's clippy added (see Findings); CI green on `89e4f66` on all five
      jobs (run 36924561876: rustfmt, x64 and ARM64 Windows, Linux, macOS, with
      tests). Annotated tag `v0.5.0` on `89e4f66`, pushed 13:55 PDT; Release run
      36925139498 green on all eight jobs; published 13:59 PDT (20:59:00 UTC).
      Checked from the published assets in `target/release-check/v0.5.0`: minisign
      verifies (trusted comment `open-task v0.5.0`), the x64 zip and `setup.exe`
      match SHA256SUMS, the exe and installer say 0.5.0, `--version` and
      `--headless --passes 2` run, `--restore-task-manager` answers, and the live
      script passes 17/17 with the published exe (`rtm-drive.ps1 -Exe …`, which
      now only sees and closes windows of the exe under test). The published
      installer was not run here: it would upgrade the user's own v0.3.1 install.
      Its script is the one tested (`git diff 362281e v0.5.0 -- installer` is
      empty).
- [x] Machine left as found again: no IFEO key for taskmgr, v0.3.1 installed.
- [ ] The unelevated switch through a real UAC prompt: needs the user (Yes, and No).

## Open questions for the user

1. Please try the switch once from an ordinary (unelevated) open-task v0.5.0:
   Settings → Windows → Replace Task Manager → the UAC prompt → Yes (the card goes
   On, then Ctrl+Shift+Esc opens open-task), and again with No (nothing changes, no
   message). The installed copy here is still v0.3.1; its update button offers
   v0.5.0.
2. Follow-up, not in this change: Task Manager runs elevated for administrators
   without a prompt, open-task in its place does not (Findings). A "Run as
   administrator" button (Process Explorer's "Show details for all processes")
   would close that gap. Recommendation: yes, as its own change.

## Things not to do

- Do not write the value without also owning its removal (the app's switch, the
  flags, the installer and the uninstaller all can). A dangling entry breaks
  Ctrl+Shift+Esc for everyone on the machine.
- Do not delete a `Debugger` value that names another program, and do not delete
  the IFEO key while it still holds anything.
- Do not let a silent install (an update) set or clear the value from a remembered
  task choice.
- Do not block the UI thread on the elevated helper: the UAC prompt can sit for as
  long as the user likes.
- Do not test by writing the real IFEO key from unit tests: tests use a scratch key
  under HKCU. Anything that touches the real key is a manual check, undone after.
- Do not leave this machine replaced after testing; check the key is gone.
