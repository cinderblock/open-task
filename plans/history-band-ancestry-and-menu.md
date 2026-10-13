# History chart: a band's ancestry on hover, and "Show in List / Tree" on right-click

> **Status:** done, not released · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

The user, 2026-10-13: "when hovering over part of a chart for one process, I should
be able to see the parent process of each process, all the way to the root process.
even if those processes have since gone away. also, right clicking a process in the
graph should give options, including to jump to that process in the list/tree views".

## Environment / context

- The "chart" is the History arrangement: `crates/ot-ui/src/usage_chart.rs`
  (`UsageChart`). Its bands are *programs* (every process of one label), not single
  processes, so "each process" means each process in the hovered band at the
  hovered moment.
- The per-program history lives in `crates/ot-core/src/usage.rs` (`Usage`). It knew
  nothing of which processes made up a program at a moment, and dropped an exited
  process once its fading total was negligible.
- Right-click and the process menu: `crates/ot-ui/src/view.rs`
  (`context_menu`, `menu_action`, `select_program`, `set_view`). The shell builds
  native menus generically from `MenuEntry`, so a new `MenuAction` needs no shell
  code.
- Another thread has uncommitted Linux probe work (`crates/ot-probe/src/imp/linux/`).
  Don't touch or stage it; format only own files with `rustfmt --edition 2021`.

## Decisions (made in this thread, by me; the user can revisit)

1. **Who is in a band at a moment:** the program's processes seen alive in any of
   the stretches the hovered column covers.
2. **Lineage is kept in `ot-core::Usage`** (`Lineage`: per process seen, its parent
   key, program, first and last seen), for as long as the history reaches back,
   plus every ancestor of anything kept, so a chain never breaks. Cheap: ~40 bytes
   a process, no strings (the name is the program's label).
3. **Shown as a small tree in the hover chip**, root at the top, the band's own
   processes as leaves. Siblings of the band's program under one parent fold into
   one line, `30 × chrome.exe (Renderer)`. Exited processes are marked `exited`.
   The chip shows in every legend style, not only "in the bands", since the tree is
   new information the legend does not give.
4. **Right-click in the Map and the History** gains **Show in List** and **Show in
   Tree** at the top (switch arrangement, select, reveal). The process picked for a
   band is the busiest still-running process of those in the band at the pointed
   moment, else the program's busiest running process (click uses the same rule).
5. **A parent that exited before open-task saw it** (user, 2026-10-13: "do it"):
   `ProcessStatic::parent_pid` keeps the PID the system reported, resolved or not
   (recording format 5; formats 1-4 read with none, through `ProcessStaticV4`).
   `Seen::lost_parent` carries it where `parent` is absent (not the idle
   process's 0), and the tree shows `PID n · exited before open-task saw it` as
   the root above such processes, grouping children of the same dead PID.

## Plan / steps

1. ~~`ot-core`: lineage (`Seen`, `Usage::seen`, `seen_in`, `running`) + tests.~~
2. ~~`usage_chart.rs`: column start time (`Pt::after_ms`), `moment_at`, ancestry
   tree (`ancestry`, `Tree`, cached in `Ancestry`) painted in the chip
   (`paint_tree`). Tests.~~
3. ~~`view.rs`: `MenuAction::ShowIn(ViewMode)`, entries in Map/History menus,
   moment-aware `select_program`, `process_menu` split out. Tests.~~
4. ~~README (Map and History paragraphs).~~
5. ~~Checks; app check; commit.~~
6. ~~Parents that exited before open-task saw them: format 5 `parent_pid`,
   `Seen::lost_parent`, `Node::Lost` in the tree. Tests; checks; app check.~~

## Findings / gotchas

- **Parents that die before the first sample are lost.** Checked in the app: a
  spinner started by `cmd /c start /b powershell ...` (cmd exits at once) shows as
  a root, `powershell.exe · 41336`, no hint of a parent. The Windows probe only
  keeps a parent it can resolve against the live table at first sight
  (`resolve_parents` in `ot-probe/src/imp/windows/mod.rs`), and at 1 Hz a launcher
  that lives milliseconds is never seen. Common in practice: `cmd /c start`,
  `userinit.exe` → `explorer.exe`. Fixed by decision 5; checked in the app: two
  spinners from two `cmd /c start` read `PID 9508 · exited before open-task saw
  it` and `PID 17632 · …`, matching the cmd PIDs `Win32_Process` reported.
- The drive script must pass `--page processes`: the app restores the last page
  from the shared preferences, and the user's own instance had left Settings.
  Focus restore still fails now and then (`SetForegroundWindow` from a background
  process is not always allowed).
- Every `ProcessStatic` constructor in the workspace uses `..Default`, so a new
  field compiles everywhere; the cost of one is the recording format (postcard is
  positional: a version 5 with a v4 statics reader, as `AdapterInfoV3` was done).
- Real-app check (scratch `target/ancestry-shot.ps1`, hover-shot build ignoring
  `WM_MOUSELEAVE`, reverted): deep chains read fine (13 levels for
  `pwsh › vp › vp › node › node › electron › electron › claude › bash › bash ›
  bash › python › java`); msedgewebview2 shows four ancestries with
  `5 × msedgewebview2.exe` folds; "Everything else" shows only its header; the
  native menu starts with Show in List / Show in Tree.
- **The drive script steals focus.** `Start-Process` activates the window and
  open-task sends printable keys to its search: the user's typing landed in it
  ("isono"). The script now calls `SetForegroundWindow(previous)` right after the
  window appears (worked twice, failed once when the previous foreground was a
  just-started hidden `cmd`) and posts Escape before shooting.
- `sed -i` on a CRLF file in Git Bash writes LF (`ot-core/src/lib.rs`); fixed
  with a Python normalize.

## Progress log

- [x] Code read, design written.
- [x] Lineage in ot-core (2 tests).
- [x] Chip tree (2 tests: the layout, and the readout in two legend styles).
- [x] Menu (1 new test; the History test also checks the tree strings).
- [x] README.
- [x] Checks: `cargo test --workspace`; clippy `-D warnings` on x64 Windows,
      Linux and macOS; rustfmt on own files. App checked (screenshots in
      `target/ancestry/`).
- [x] Committed.
- [x] Open question 1 answered "do it"; built (format 5, lost parent in the
      tree), tests (record v4 read-back, `lost_parent`, the tree's PID root),
      checks on three targets, app check, README (History and Recording).

## Open questions for the user

1. **Answered 2026-10-13: "do it"**, built as decision 5. The question was:
   **Show a parent that exited before open-task saw it?** Today such a process
   reads as a root. Windows still reports the parent's PID (not its name, which
   is gone with it). Option: add `ProcessStatic::parent_pid` (what the OS
   reported, resolved or not), bump the recording format to 5 with a v4 reader,
   and show the tree's top as `PID 13356 · exited before open-task saw it`.
   Touches ot-model, ot-record, both probes (one line each). Recommendation: do
   it; it is the case the request names ("even if those processes have since
   gone away"), and short-lived launchers are common.

## Things not to do

- Don't stage the Linux probe files another thread is editing.
- Don't run `cargo fmt --all` while other threads have uncommitted files.
