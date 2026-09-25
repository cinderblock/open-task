# Process tree view, and the jump between tree and sorted list

> **Status:** done, unpushed · **Started:** 2026-09-25 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md` (step 6, "tree/grouping").

## Goal

The user asked for "an alternate process tree view, and a quick jump / visual link
between the tree view and the CPU usage sort."

Concretely:

1. A **tree mode** for the process table: parent → children hierarchy, indented, with
   expand/collapse, like Process Explorer's default view.
2. A **quick jump** between the two modes that keeps the selected process: pick a hot
   process in the CPU-sorted list, jump to the tree with it revealed (ancestors
   expanded, row centered), and back.
3. A **visual link** so the two views read as one thing: the same sort column governs
   both (siblings in the tree are ordered by it), the selected row's sibling block gets
   an accent indent guide in the tree, and a breadcrumb of the selected process's
   ancestry shows above the table in both modes.

## Environment / context

- Rust workspace; UI logic lives in `crates/ot-ui` (no platform code, fully unit
  testable). The Windows shell (`crates/ot-shell-win/src/window.rs`) only translates
  Win32 messages into `ot_ui::UiEvent`.
- The table widget (`crates/ot-ui/src/table.rs`) is generic over a `RowSource` trait
  and knows nothing about processes.
- Per-process parent comes from `ProcessStatic::parent` (`crates/ot-model`).
- Checks before pushing are listed in the parent plan under "Verify before pushing".

## Decisions already made (don't re-ask)

1. **One table, two modes**, not two side-by-side panes. Process Explorer and TMOG both
   do it this way; it keeps the columns, sort, selection and scroll logic in one place.
2. **Tree keeps the column sort.** Siblings are ordered by the active sort column and
   direction. Process Explorer drops to a flat sort the moment you click a column; we
   do not, because "hottest child first, at every level" is the whole point of the link
   between the tree and the CPU sort.
3. **Siblings sort by subtree totals, not own values,** for the additive columns (CPU,
   memory, working set, disk, threads, handles). This keeps the order stable when a
   node is collapsed or expanded, and it surfaces a branch whose children are hot even
   when the parent is idle.
4. **Collapsed rows show subtree totals and a descendant count** (`chrome.exe (41)`),
   the way Task Manager's app groups do. Expanded rows show their own values, because
   the children are right there accounting for themselves.
5. **Parent identity is resolved in the probe, once per process,** at first sight: PID
   hint → the live process with that PID, accepted only if it was created no later than
   the child. A recycled PID is therefore never adopted as a parent. This is the
   Process Explorer rule. The tree in the UI then trusts `ProcessStatic::parent` as a
   real identity and never does PID matching itself.
6. **Ctrl+T toggles the tree** (Process Explorer's binding). The shell maps the key to a
   semantic `UiEvent::Command(ToggleView)`; the UI owns the meaning. This is the shape
   a native menu + accelerator table will feed later.
7. **Left/Right** in tree mode: Left collapses, or moves to the parent if already
   collapsed or a leaf; Right expands, or moves to the first child if already expanded.
   In list mode they do nothing (no surprise mode switch from an arrow key).
8. Everything is expanded by default. Collapse state is per process identity and
   survives resorting and processes coming and going.
9. `--view list|tree` picks the initial mode from the command line, mainly so the
   screenshot script can capture tree mode. Persisting the preference is later.

## Plan / steps

All steps below are done; kept as the record of what was built.

1. ~~Read the code, write this plan.~~
2. Probe: resolve parents at first sight (Windows). Model docs updated to say the
   field is an identity.
3. `ot-ui`: `RowSource` hierarchy hooks (`parent`, `cell_collapsed`, `heat_collapsed`,
   `compare_subtree`), tree ordering in `Table` (CSR children, sibling sort, pre-order
   walk skipping collapsed subtrees, cycle safety net), expander hit-testing and
   painting, indent guides with the active-block highlight, keyboard Left/Right,
   reveal-selected on mode switch.
4. `ot-ui`: `ProcessTree` (index by key, parent indices, cycle-breaking depth, subtree
   rollups) and `ProcessRows` moved to `process_rows.rs`; breadcrumb helper.
5. `ot-ui`: toolbar with the List/Tree segmented control, breadcrumb, process count;
   `Command`/`ViewMode` events; `Key::{Left, Right}`.
6. Shell: map VK_LEFT/VK_RIGHT and Ctrl+T; `--view` option.
7. Tests for all of the above (table tree order, rollups, reveal, keyboard, hit test,
   probe-independent parent semantics).
8. README + parent plan updates. Verify: fmt, clippy on three targets, tests, headless
   run, screenshots of both modes. Commit.

## Findings / gotchas

- **`ProcessStatic::parent` was a PID-only hint** (`ProcessKey::new(ppid, 0)`); the
  probe comment said "the core resolves this", but nothing did. Fixed in the probe
  (decision 5). The Windows probe's `was_new` local was inverted (`true` meant "seen
  before"); renamed while there.
- `System` (PID 4) reports parent PID 0 (the idle pseudo-process). PID 0 is hidden by
  the table's `visible()` hook, so the table treats a row whose parent is hidden as a
  root. That is the general rule: parent missing, hidden, out of range, or self ⇒ root.
- The table's `hover` is a display position, and positions shift when a subtree
  collapses. It is recomputed on the next mouse move, which is soon enough.
- **`scripts/screenshot.ps1` captured the wrong window.** It looked the window up by
  class and title, and the user's installed copy (`~/.cargo/bin/open-task.exe`, PID
  44712, running since 15:15) matched first, so the "new" screenshots showed the old
  UI. The script now enumerates top-level windows and takes the one owned by the PID
  it launched. It never kills anything but that PID, so the installed copy is safe.
- Clippy pedantic bites to remember: `similar_names` (`rect` vs `rest`),
  `needless_range_loop` (build the vector with `extend` and a closure instead),
  `range_plus_one` (`..=n`).
- A test helper that filters painted text by "looks like a process name" also caught
  the toolbar breadcrumb once a process was selected. Only the table body is inside a
  clip, so the helper now reads text between `PushClip` and `PopClip`.

## Progress log

- [x] Plan written.
- [x] Probe parent resolution (`resolve_parents` in the Windows probe; model doc).
- [x] Table tree mode (`RowSource::{parent, cell_collapsed, heat_collapsed,
      compare_subtree}`, CSR children, sibling sort, pre-order walk, cycle safety net,
      expander hit-test, chevrons, indent guides with active block, Left/Right,
      reveal-on-switch).
- [x] `ProcessTree` (index, parents, cycle-breaking depth, rollups) and `ProcessRows`
      moved to `process_rows.rs`; ancestry breadcrumb.
- [x] Toolbar (List/Tree segmented control, breadcrumb or Ctrl+T hint, process count);
      `Command`, `ViewMode`, `Key::{Left, Right}`.
- [x] Shell maps VK_LEFT/VK_RIGHT and Ctrl+T; `--view list|tree`.
- [x] Tests: 39 in `ot-ui` (14 new for the table, 6 for the process tree and rows, 8
      for the view), all green.
- [x] README ("Using it" section, `--view`, screenshot note); parent plan updated.
- [x] Verified 2026-09-25: `cargo fmt --check`, clippy `-D warnings` on the Windows,
      Linux and macOS targets, `cargo test --workspace`, headless smoke run, and
      screenshots of both modes (kept out of the repo; they show the user's processes).
- [x] `scripts/screenshot.ps1` targets the launched PID (see findings).

## Status

Done as asked. Not pushed: pushing needs per-action approval per the parent plan.

## Open questions for the user

None blocking. Two judgement calls worth a look once it is running:

1. Sibling order by subtree totals (decision 3) versus own values. Recommend
   subtree; it is what keeps rows from jumping when you click a chevron.
2. Whether the breadcrumb belongs in the toolbar or in a future details pane. It is in
   the toolbar for now because there is no details pane yet.

## Things not to do

- Do not sort the tree by a PID-matching heuristic in the UI. Parent identity is the
  probe's job (decision 5).
- Do not make an arrow key switch modes. Ctrl+T and the toolbar do that.
- Do not rebuild the tree per frame. It is rebuilt once per snapshot in
  `App::set_snapshot` and on sort/collapse changes; painting only walks the prebuilt
  display order.
