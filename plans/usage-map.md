# A 2D view of who has been using the CPU: treemap ("Map") and icicle strip

> **Status:** active · **Started:** 2026-09-29 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/charts-log-time-and-live-table.md` (item 5, decision 14).

## Goal

The user, 2026-09-28: "What if it wasn't a flat list but more a 2D visualizer that
shows how much of the pie each process has been using?" Of the options put to them
they liked the treemap and the icicle strip. On 2026-09-29 they said "keep going
with the open items", which is the go-ahead on the recommended choices below.

## Environment / context

- Per-process data today: `ProcessSample::cpu` is a one-interval percentage of one
  core. The Windows probe reads each process's cumulative kernel + user time
  (`cpu_100ns` in `ot-probe/src/imp/windows/mod.rs`) to compute it, but does not
  publish it.
- The process table: `ot-ui/src/table.rs` (generic), `process_rows.rs` (the process
  source, `ProcessTree` with rollups), `view.rs` (`App`, the toolbar with the
  List / Tree segments, search, selection by `RowId`, `row_id(ProcessKey)`).
- Other threads may work in this tree; commit only our paths with
  `git commit --only`, format only our files with `rustfmt --edition 2021`.

## Decisions already made (don't re-ask)

1. **Window: the last minute.** Size = CPU time used in the last 60 s. (Later: any
   range dragged on a chart.)
2. **Size is CPU time only.** Memory is a present amount, not "has been using"; a
   memory mode can come later.
3. **Processes that exited inside the window stay, dimmed,** until their last
   sample is older than the window.
4. **Treemap first, then the icicle strip.**
5. **The pie is the CPU that was used**, not the machine: idle would dwarf
   everything on a quiet machine. A caption gives the total ("CPU used, last minute:
   12% of 12 logical processors").
6. **The treemap is a third arrangement of the process table's area**: List / Tree /
   Map in the toolbar, sharing its selection, the ancestry strip and the search
   (non-matches dimmed, as the tree dims context rows). Ctrl+T keeps toggling List
   and Tree; from Map it goes back to whichever of those was last.
7. **Nested by the process tree, squarified.** A process with children gets a frame
   with its name on a header strip, its children inside, and its own CPU time as one
   more block among them. Tiles too small to see are not drawn; labels only where
   they fit.
8. **Color is the current CPU**, the table's heat orange at an intensity from the
   last interval's CPU, over the surface color. Area says "used over the last
   minute", color says "and is it still at it", which the table cannot show at once.
9. **Accounting lives in `ot-core`** (`usage.rs`, fed by snapshots like the
   `Timeline`): per process a short run of (time, cumulative CPU time) samples,
   usage over the window = the difference, with a process that started inside the
   window counting from zero.
10. **Layouts do not jump on noise:** the order children are laid out in uses the
    same kind of sticky keys as the table (`steady.rs`), and the one-minute window
    already smooths the values.

## Plan / steps

1. ~~Model and probe: `ProcessSample::cpu_time` (cumulative).~~
2. ~~`ot-core::usage`: window accounting with ghosts. Tests.~~
3. ~~`ot-ui::treemap`: squarified layout, nested layout. Tests.~~
4. ~~Map arrangement in `App`: toolbar segment, painting, hover readout, click to
   select, search dimming, caption. Tests.~~
5. **[current]** Icicle strip between the charts and the toolbar. Tests.
6. README, plans; checks on three targets; screenshots; commits.

## Findings / gotchas

- **A young session inflated the total** (56% of the machine over 20 s while the
  chart averaged about a third of that): a process started in the minute before
  the session counted from zero, bringing in CPU used before anyone was watching.
  The window's start is now clamped to the first snapshot, so the numbers and
  `span()` agree (fix folded into `7fd946b`; test
  `a_process_started_just_before_the_session_counts_from_when_it_was_seen`).
- **Chains of launchers nested frame in frame** (pwsh > vp > vp > node > node >
  electron, each with a name strip and padding). A process with one busy child and
  under 2% of its subtree's time to itself now folds into the child's frame,
  labelled `a.exe › b.exe`; the percentage has its own right-aligned slot so a
  long chain's ellipsis never hides it.
- **Only busy children count** (over 2% of the parent's subtree): an idle sibling
  (a conhost next to the real child) no longer breaks a chain, and a process whose
  children barely ran is one tile instead of a frame around its own time.
- The Map is drawn in the table's area and the table's own rectangles are stale
  while it shows: hover, the column-resize cursor and "hold the order" are all off
  in Map mode.
- **Bash heredocs eat backslashes** in Python source (`\u{b7}`, Windows paths):
  write patch scripts with the Write tool and run them from a file.

## Progress log

- [x] Model and probe: `ProcessSample::cpu_time` (Windows: kernel + user, 100 ns).
- [x] Usage accounting: `ot-core::Usage` (window, `span()` while the session is
      younger, started-inside-the-window from zero, interpolation, ghosts).
- [x] Treemap layout: `ot-ui::treemap::squarify` (the paper's example lays out
      exactly; no allocation once `out` is sized).
- [x] Map arrangement: `ot-ui/src/usage_map.rs`, `ViewMode::Map`, the toolbar's
      third segment, Ctrl+M, `--view map`; checked in the app
      (`target/drive/map-25s-b.png`).
- [ ] Icicle strip.
- [ ] Docs, checks, screenshots.

## Open questions for the user

- None yet; the choices above were the recommendations.

## Things not to do

- Do not size tiles by the last interval's CPU: that is the table's job, and it
  would reshuffle every second.
- Do not stage other threads' files.
