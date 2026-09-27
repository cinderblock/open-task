# Process details, search, column resize, and End task

> **Status:** active · **Started:** 2026-09-26 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md` (step 6). Follows `plans/process-tree-view.md`
> and the installer work in `plans/windows-installer.md` (v0.2.1, another thread).

## Goal

The remaining items of architecture step 6, so the Processes view is usable as a
daily task manager before the Performance view starts:

1. **Process details** from the Windows probe: image path, command line, user,
   integrity. Shown as columns (User, Command line) and used by search.
2. **Search filter**: type to filter the table by name, PID, path, command line, user.
3. **Column resize** by dragging header dividers, with horizontal scrolling so wide
   columns (command line) fit.
4. **Right-click context menu** with End task, End process tree, Open file location,
   backed by a process-control API that verifies process identity before killing.

## Environment / context

- Another Claude thread is active in this working tree: it shipped the installer and
  tagged v0.2.1 (`967eeda`, `8eea9d6`) while this plan was being written. Stage only
  this cycle's files; expect README and the parent plan to move underneath.
- UI logic lives in `crates/ot-ui` (unit-testable, no platform code); the Windows
  shell (`crates/ot-shell-win/src/window.rs`) only translates Win32 messages into
  `ot_ui::UiEvent` and performs effects the UI asks for.
- Probe: `crates/ot-probe/src/imp/windows/mod.rs`. Statics are `Arc<ProcessStatic>`
  and replaced wholesale when more is learned (the pattern `resolve_parents` set).
- Checks before pushing: parent plan, "Verify before pushing".

## Decisions already made (don't re-ask)

1. **Details are collected in the probe, once per process, under a per-pass time
   budget**, not lazily on demand from the UI. Every consumer (sort by user, search
   by path, the future Flight Recorder) then sees the same statics. The budget keeps
   the first pass cheap on a machine with 600 processes; leftovers fill in over the
   next passes.
2. **Handle-light Windows path.** `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`,
   which even protected processes grant; `QueryFullProcessImageNameW` for the path;
   `NtQueryInformationProcess(ProcessCommandLineInformation)` for the command line
   (no PEB reading, works across bitness); token user via `OpenProcessToken` +
   `LookupAccountSidW` with a SID → name cache; integrity from the token's integrity
   SID. Processes that refuse to open keep `None`.
3. **User display follows Task Manager**: `SYSTEM`, `LOCAL SERVICE`, `NETWORK
   SERVICE`, a bare name for local accounts, `DOMAIN\name` otherwise.
4. **Search is type-to-filter.** Printable keys go to the filter box whether or not
   it has focus; Escape clears it; Ctrl+F focuses it. Case-insensitive substring
   over name, PID, user, image path and command line. In tree mode an ancestor of a
   match stays visible so the match is reachable. No caret blink: the app redraws
   nothing when idle, and a blinking caret would cost two frames a second.
5. **Effects, not callbacks.** `App::handle` returns a `Reaction { repaint, effect }`.
   The UI decides *what* (show this menu, terminate these processes, open this
   folder); the shell does *how* with native APIs. Menus are native (`TrackPopupMenu`),
   per architecture decision 3.
6. **Terminate verifies identity.** `ot-probe` grows a `ProcessControl` trait; the
   Windows implementation opens the PID, checks `GetProcessTimes` creation time
   against `ProcessKey::birth`, and only then calls `TerminateProcess`. A recycled
   PID is never killed by mistake.
7. **Confirm before killing**, Process Explorer style, with a native Yes/No box that
   defaults to No. End process tree kills the parent first, then descendants, so a
   parent cannot respawn children mid-kill.
8. Column reorder and persisted widths are later. Column resize is drag-only (no
   double-click auto-fit: `ot-ui` has no text metrics).

## Plan / steps

1. ~~Read the code, write this plan.~~
2. **[current]** Probe: details enrichment with budget; `ProcessControl`.
3. `ot-ui`: User and Command line columns; horizontal scroll; column resize;
   `Reaction`/`Effect`; search filter with tree-aware visibility; context menu model.
4. Shell: WM_CHAR, capture, WM_SETCURSOR, native popup menu, confirm box,
   `ShellExecuteW` for Open file location, `ProcessControl` wiring.
5. Tests, README, parent plan; verify (fmt, clippy on three targets, tests, headless,
   screenshots); commit at each logical step.

## Findings / gotchas

(filled in as work proceeds)

## Progress log

- [x] Plan written.
- [ ] Probe details + budget.
- [ ] `ProcessControl` (terminate with identity check).
- [ ] Columns, horizontal scroll, column resize.
- [ ] Search filter.
- [ ] Reaction/Effect, context menu, End task / End tree / Open file location.
- [ ] Shell wiring.
- [ ] README + parent plan; verified; committed.

## Open questions for the user

None yet.

## Things not to do

- Do not read the target process's PEB for the command line; the
  `ProcessCommandLineInformation` class exists for this and works cross-bitness.
- Do not kill by PID alone (decision 6).
- Do not touch the installer files or `Cargo.toml` version: another thread owns the
  v0.2.1 release.
