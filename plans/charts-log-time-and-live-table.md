# Charts on a log time axis, synced hover, resize repaint, and a calmer live table

> **Status:** active · **Started:** 2026-09-28 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md` (decision 8: log-scale time axis).

## Goal

The user's feedback on 2026-09-28, five items:

1. "Resizing the window doesn't redraw the UI."
2. "The charts don't look to be in log time." They are not: `sparkline.rs` was a
   linear axis over a 600-point ring (10 minutes at 1 Hz). Decision 8 of the
   architecture plan asks for a log-scale time axis with multi-resolution history.
3. "Hovering charts should show a faint vertical line in all charts at the same
   time, and show a readout of how long ago."
4. "Do we need some hysteresis to keep the tree/list views from jumping around?
   What options do we have here?" A question: answer with options and a
   recommendation, build what the user picks.
5. "What if it wasn't a flat list but more a 2D visualizer that shows how much of
   the pie each process has been using?" A design question: options and a
   recommendation.

## Environment / context

- Windows 11, the claude tool session runs elevated; the primary display here is
  96 DPI (the app logged `dpi=96.0`). The user's own monitor may differ.
- An installed `open-task.exe` (PID 15760 at the start) was running; leave it alone.
  `scripts/screenshot.ps1` only captures and kills the instance it launched.
- `target/resize-test.ps1` (scratch, untracked under `target/`) launches the debug
  build, resizes the window with `SetWindowPos` from outside, and screenshots at
  60 ms and 1.5 s after each resize.
- `plans/replace-task-manager.md` is another thread's untracked plan (dormant since
  2026-09-26). Not ours; do not stage it.
- **An active peer thread** (`plans/performance-view.md`) shares this tree. It
  added the navigation rail (`nav.rs`), the Performance page (`perf.rs`) and turned
  the summary charts into a general `ChartGroup` (`charts.rs`), commits `df83a78`,
  `b7ce60a`. It has uncommitted work in `charts.rs`, `theme.rs`, `format.rs`,
  `timeline.rs`, the probe and the model (disks, network). Its plan asks this
  thread to keep `view.rs` edits small and put new pages in their own modules, and
  says not to push while this thread's commits are unpushed. **Do not edit files
  it has uncommitted changes in**; stage only our own files.

## Decisions already made (don't re-ask)

1. **Time axis mapping:** `x = right - w * ln(1 + age/τ) / ln(1 + span/τ)` with
   τ = 1 s and span = 1 hour. Newest sample on the right edge; with those numbers
   the last 10 s take ~29 % of the width, the last minute ~50 %, 10 minutes ~78 %.
   Fixed span, data grows leftward as the session ages (same as the linear chart
   did). Span and τ are constants in `sparkline.rs`, easy to change.
2. **Storage is multi-resolution in `ot-core::Series`:** a raw tier (every sample,
   its own timestamp) plus coarser tiers of wall-clock-aligned buckets holding
   min / max / mean. Readers get one stitched newest-to-oldest walk: raw where raw
   exists, then the next tier older than that. Timeline retention: raw 600
   samples, 10 s buckets for 2 hours. Covers the 1 h span with margin.
3. **Chart marks follow the dataviz skill:** mean line, ~10 % area wash under it,
   a faint min-max envelope where a point aggregates several samples (so a spike
   20 minutes ago still shows), hairline recessive gridlines, an x-axis label band
   inside the card ("1h 10m 1m 10s now").
4. **Hover:** the crosshair snaps to the nearest plotted point of the chart under
   the pointer; every chart draws a faint hairline at that same age and a marker
   dot on its line; each chart's label band shows the readout (value first, then
   how long ago) flush against the line on whichever side has more room,
   replacing the tick labels while hovering. (First cut centered it in a fixed
   260-DIP box; near an edge that pushed the text far from the line. The view has
   no text measurement, so anchoring to the line is the robust choice.)
   The pointer is re-snapped every frame, so as new samples slide under a still
   pointer the line stays on a sample.
5. **Resize fix:** render synchronously inside `WM_SIZE` (then validate the window
   so the queued `WM_PAINT` does not draw the same frame again), instead of
   relying on `WM_PAINT`, which a live drag starves.
6. **Item 4, user's pick (2026-09-28): A, B, E, F, and G with an enable toggle in
   settings.** C and D not wanted.
7. **B is per-row sticky sort keys, not a fuzzy comparator.** Each row keeps the
   key it last sorted by and only takes its new value when the value leaves a dead
   band around the old key: `max(abs, rel * |key|)` per column (CPU 1 point / 15 %;
   disk rates 64 KiB per interval / 25 %; memory and working set 1 MiB / 2 %;
   threads and handles 2 / 2 %; text and PID exact). The table then sorts exactly
   by those keys, which is a total order, so Rust's sort cannot panic, and two rows
   are out of order only when their values are within the two dead bands. Keys
   reset to exact values when the sort column or the arrangement changes. Pinned
   rows (a CPU sample in progress or done) keep `INFINITY`.
8. **Every numeric comparison in `ProcessRows` goes through one `key(row, col)`.**
   The old `compare_subtree` compared two processes by their rollup of the column
   but a process against a service by CPU, which is not transitive when sorting
   by memory with services expanded (possible sort panic). Rows without a figure
   for a column (services and threads outside the CPU column) now have none and
   sort below those that do, then by the existing kind-and-name tie-break.
9. **A = hold the keys.** While the pointer is over the table (header or body)
   the keys do not move at all; new rows get their exact key, gone rows drop out.
   When the pointer leaves, the keys update at once and the table re-sorts
   (animated if G is on). The toolbar says "Order held" meanwhile.
10. **E applies to data-driven re-sorts only** (a snapshot, a CPU sample row
    appearing): if the selected row was on screen, the scroll moves so it keeps its
    screen position. Sort clicks, list/tree switches, collapsing and the search
    behave as before.
11. **F = Space pauses the display, not the sampler.** The table, the cards, the
    charts and the Performance page freeze on the snapshot and history of the
    moment; sampling and history recording continue behind it (the charts paint
    from a clone of the timeline taken at the pause, about 1 MB at most), so
    resuming loses nothing. Space pauses only when the search field does not have
    focus; with focus it types a space. The toolbar shows "Paused" and the window
    title gets " (paused)".
12. **G = rows slide to their new place (150 ms, ease-out)** after a data-driven
    re-sort. Rows coming from far away enter from just outside the visible edge;
    rows leaving slide out to it. The shell keeps painting while the view reports
    it is animating (vsync-paced by `Present(1)`), then goes back to event-driven.
    **Default follows Windows' "Animation effects"** (`SPI_GETCLIENTAREAANIMATION`,
    re-read on `WM_SETTINGCHANGE`) until the user flips the switch in Settings;
    from then on the explicit choice wins (`Settings::animate_rows:
    Option<bool>`, `None` = follow Windows, not written to the registry). Changed
    from "system off always wins" on finding this machine has animation effects
    off: the user asked for G and would never have seen it.
    **User, 2026-09-28: the setting must show that Windows' animations are
    globally off, if they are.** So whenever they are off the card carries an
    extra accent-colored line, in all three states: never chosen ("so this is
    off until you turn it on here"), on ("open-task animates rows anyway"), off
    ("off too"). The page repaints when `WM_SETTINGCHANGE` reports a change.
13. **Settings live on a Settings page** at the bottom of the navigation rail (the
    Windows 11 Task Manager layout), in its own module `settings.rs`. Persisted per
    user in `HKCU\Software\open-task` (DWORD values) by the Windows shell; `ot-ui`
    only describes the settings and emits an effect when one changes. Later
    options (theme, update speed, replace Task Manager, run as administrator) go
    on the same page.
14. **Item 5, user (2026-09-28): "I like the treemap idea. The icicle strip sounds
    cool too."** Both, after item 4: the treemap as a "Map" arrangement, the icicle
    as a strip. Needs cumulative per-process CPU time published in
    `ProcessSample` (probe and model files the peer thread is editing: coordinate).

## Plan / steps

1. ~~Read the code, reproduce the resize report.~~
2. ~~Resize: synchronous render in `WM_SIZE`.~~ Commit `2ccdd44`. The user still
   has to confirm it with a real drag on their monitor.
3. ~~`ot-core`: multi-resolution `Series` + tests. Timeline uses it.~~
4. ~~`ot-ui`: `TimeAxis` (age <-> x), log-axis painting with envelope, gridlines and
   label band. Tests.~~
5. ~~`ot-ui`: chart hover state in `App`, crosshair in every chart, readout.~~
6. ~~`format::ago` for the readout.~~
7. ~~README (charts paragraph), architecture plan (decision 8 status), this plan.~~
8. ~~Checks and screenshots.~~
9. ~~Items 4 and 5: options put to the user.~~ Picks in decisions 6 and 14.
10. ~~Item 4~~ (all of a to f done; commit after this plan update):
    a. `ProcessRows::key` / `raw_key`, one numeric key per row and column; sticky
       keys (`steady.rs`), held while pointing. Tests.
    b. `Table`: remember row ids with the order; `refresh()` for data-driven
       re-sorts keeps the selection's screen position (E) and records slide
       animations (G); `tick(now)` / `animating()`. Tests.
    c. `App`: hold on pointer over the table; pause (Space, `Command::TogglePause`),
       frozen snapshot and timeline; toolbar status; settings; `paint_at(now)`.
       Tests.
    d. `settings.rs` Settings page; `Page::Settings` pinned to the rail's bottom.
    e. Shell: settings in the registry, animation frame loop, system animation
       setting, window title while paused.
    f. README, plans; checks on three targets; screenshots; commit.
11. **[current]** Item 5: treemap "Map" arrangement, then the icicle strip. Needs
    its own design pass first (data: cumulative CPU per process; layout: squarified
    and stable; interaction: shared selection). The peer thread is idle in the
    probe and model now (its work is committed), so publishing cumulative CPU
    time in `ProcessSample` no longer collides.

## Findings / gotchas

- **Programmatic resizes repaint fine.** `SetWindowPos` from another process to
  1500x950 and back to 900x600: the screenshot 60 ms later already shows the new
  layout (`target/resize-test/1-bigger-60ms.png`). So the report is about a live
  drag (or snap), not about `WM_SIZE` handling as such.
- **Why a live drag can show stale content:** during the modal sizing loop,
  `WM_PAINT` is only synthesized when the queue has nothing else, and the mouse
  keeps queueing `WM_MOUSEMOVE`s, each of which resizes the window and the swap
  chain (`ResizeBuffers` leaves the buffer blank until the next `Present`). With
  `WS_EX_NOREDIRECTIONBITMAP` and a DirectComposition visual there is no GDI
  redirection surface for DWM to stretch, so what shows is whatever the last
  `Present` left, at its old size. Rendering in `WM_SIZE` puts a `Present` inside
  every resize step. Not reproduced with a real mouse drag here, because a
  synthetic drag would move the user's cursor on their live desktop. A 0 ms
  screenshot after a cross-process `SetWindowPos` cannot tell old from new
  either: the old binary had painted by then too.
- **Screenshotting a hover without touching the real cursor:** a posted
  `WM_MOUSEMOVE` makes the app call `TrackMouseEvent`, which posts `WM_MOUSELEAVE`
  at once because the real cursor is elsewhere, and the leave is handled before
  the `WM_PAINT`. For the screenshots, a scratch build whose `WM_MOUSELEAVE` arm
  returned `Outcome::Done` (marked `// SCRATCH-HOVER-SHOT`, reverted right after,
  `git diff` empty) was copied to `target/open-task-hovershot.exe` and driven by
  `target/hover-shot.ps1` (`-X`/`-Y` in client pixels). Results:
  `target/hover2-cards.png`.
- **Clippy pedantic** flags `&self` on an 8-byte `Copy` type
  (`trivially_copy_pass_by_ref`) only on private fns; `TimeAxis` methods take
  `self`. Tests may not use `==` on floats or `as i64` from `usize`.
- `sed -i` in Git Bash wrote `crates/ot-core/src/lib.rs` back with LF; the repo
  pins `*.rs eol=crlf`. Normalize with `perl -pi -e 's/\r?\n/\r\n/'` after any
  whole-file write.
- A 10 s bucket that straddles the oldest raw sample is skipped, not clipped:
  `history()` yields only buckets wholly older than everything yielded so far, so
  the seam has a gap of up to one bucket (one or two DIPs at ten minutes' age)
  and never an overlap.
- **Latent sort panic, fixed on the way (item 4):** the old `compare_subtree`
  compared two processes by the column's rollup but a process against a service
  by CPU. Sorting the tree by memory with a service host expanded could therefore
  hand `sort_unstable_by` a cycle (9 > 8 by memory, 8 > Alpha by CPU, Alpha > 9 by
  CPU), and Rust's sort may panic on a comparator that is not a total order.
  Every numeric comparison now goes through `ProcessRows::key`; the test
  `mixed_siblings_sort_transitively_by_memory` pins the cycle case.
- **This machine has Windows' animation effects off** (`SPI_GETCLIENTAREAANIMATION`
  returns 0; `HKCU\Control Panel\Desktop\WindowMetrics\MinAnimate` is 0).
- **`cargo fmt --all` reformats the peer thread's uncommitted files too.** In a
  shared tree run `rustfmt --edition 2021 <own files>` instead. Happened once here
  (formatting only; the peer committed afterwards).
- **Git Bash rewrites `/f` into a path** (`reg delete ... /f` fails with "Invalid
  syntax"). Use the PowerShell tool for `reg`, or `//f`.
- **`PrintWindow(PW_RENDERFULLCONTENT)` takes about 300 ms a frame here**, too slow
  to time a 150 ms slide; one burst frame still caught rows mid-slide
  (`target/drive/burst-1-crop.png`), which shows the frame loop runs.
- **Mid-slide, crossing rows overlap visibly**: row backgrounds are translucent
  (over Mica), so a row sliding past another does not hide it. Could give moving
  rows an opaque backing (`bg_solid`); not done, to be raised with the user.
- Driving the app without touching the real cursor: `target/drive.ps1` (steps
  `move`, `click`, `char`, `wait`, `shot`, `title`; client pixels), plus the
  scratch `SCRATCH-HOVER-SHOT` build (above) for anything needing the pointer to
  stay over the window. Rail expanded at the default window width, so the
  Settings card centre is about (678, 108).

## Progress log

- [x] Plan written; code read; resize report narrowed to live drags.
- [x] Resize fix (`2ccdd44`); awaiting the user's live-drag confirmation.
- [x] Multi-resolution series (`ot-core::timeline`: `Retention`, `Resolution`,
      `Bucket`, `Series::history`).
- [x] Log time axis charts (`ot-ui::sparkline`: `TimeAxis`, `Plot`).
- [x] Synced hover crosshair and readout (`view.rs`: `Charts`).
- [x] README and plans.
- [x] Checks: workspace tests, clippy `-D warnings` on windows / linux / macOS
      targets, fmt; screenshots `target/charts-20s.png`, `target/hover2-cards.png`.
- [x] Options for items 4 and 5 put to the user (below).
- [x] Item 4 (A, B, E, F, G + Settings page): `steady.rs`, `ProcessRows::key`,
      `Table::refresh`/slide, pause, hold, `settings.rs`, `Page::Settings`,
      `Icon::Settings`, `DrawCmd::StrokeRoundRect`, shell `prefs.rs`, frame loop,
      title. 98 `ot-ui` tests; clippy clean on three targets. Checked in the app:
      Settings page off/on and the registry value (then removed again; the key did
      not exist before), pause (title, status, table unchanged pixel for pixel over
      2.5 s), "Order held" with rows staying put, a mid-slide frame.
- [x] Settings card shows Windows' animation effects being off in every state
      (checked in the app: `target/drive/note-states.png`; registry key removed
      again). The live repaint on a Windows change is untested here: testing it
      means toggling the user's own Windows animation setting.
- [ ] Item 5: treemap, then icicle strip.

## Item 4: keeping the table from jumping (options put to the user)

Today: every 1 s snapshot re-sorts the whole table by that second's value (CPU is
a one-second average); equal keys tie-break on id. Selection follows the process,
but the scroll position does not, and hover is by position, so a re-sort can move
a different process under a pointer that is about to click.

- **A. Hold the order while the pointer is over the table.** Values keep
  updating in place; rows re-sort when the pointer leaves (or after it has been
  still for a few seconds). A small "Order held" note in the header says why.
  Fixes "the row moved as I clicked". Cost: the order can be stale while you look.
- **B. Sticky order (a dead band).** Start from the previous order and let a row
  pass its neighbour only when it leads by a margin, e.g. max(1 point, 25 %).
  Kills the constant shuffle among the 0-3 % rows while a real jump still moves at
  once. Must not be done with `sort_by` and a fuzzy comparator (not a total order;
  Rust's sort may panic on that since 1.81); a margin-aware adjacent-swap pass over
  the nearly sorted previous order converges in a few passes for ~500 rows.
- **C. Sort on a smoothed value.** An exponential moving average (~3 s) as the
  sort key, the one-second value still displayed (or both). Rows move on sustained
  change, not on one noisy sample. Cost: number and position can briefly disagree.
- **D. Re-sort less often.** Every 3-5 s while values update every second. Simple,
  but movement becomes periodic jumps.
- **E. Keep the selected row where it is on screen.** When a re-sort moves the
  selection, scroll by the same amount (until it would leave the list).
- **F. Pause (Space), as in Process Explorer.** Freezes updates entirely, values too.
- **G. Animate moves** (~120 ms slides). Does not reduce churn, makes it traceable;
  costs frames while animating.

Recommendation: A + B + E, with F as a cheap extra. C is the alternative to B if a
smoothed number is wanted anyway.

## Item 5: a 2D "share of the pie" view (options put to the user)

- **Treemap** as a third arrangement next to List and Tree ("Map"): area = CPU
  time used over a window, nested by the process tree (or grouped by app / user /
  service host), labels on the big tiles, click selects (same selection, same
  breadcrumb, Ctrl+T cycles). Best use of 2D space; needs a stable layout or it
  jumps worse than the list (ties to item 4).
- **Sunburst**: rings are tree depth, angle is share. Literally a pie, hierarchy
  explicit; weaker for comparing slices, small slices unlabeled, corners wasted.
- **Icicle / partition strip**: one band per tree level, width = share. A compact
  strip that could sit between the charts and the table; labels fit; order is easy
  to keep stable.
- **Stacked area over time** on the same log axis: top N processes plus "Other",
  band thickness = CPU share at that moment. The only form that answers "who was
  using it twenty minutes ago"; shares the crosshair (hover lists the breakdown at
  that moment). Needs per-process history, which the multi-resolution `Series`
  now makes cheap (about 33 KB per tracked process with the current retention:
  600 raw samples of 16 bytes plus 720 buckets of 32).

Data note for all of them: "has been using" means CPU time over a window, not the
last second. The Windows probe already reads each process's cumulative kernel +
user time (`cpu_100ns` in `ot-probe/src/imp/windows/mod.rs`) to derive the
percentage, but `ProcessSample` only publishes the percentage; publishing the
cumulative time makes a window share a difference of two readings. Processes that
exited inside the window need to be kept as ghosts until the window passes.

Recommendation: the treemap as the "Map" arrangement (window: the last minute by
default, later any range dragged on a chart), then the stacked-area chart on the
CPU card for the "when" question.

## Open questions for the user

1. Item 1: does a live drag-resize now redraw smoothly on your monitor?
2. Mid-slide overlap: give moving rows an opaque backing, or leave the rows
   translucent? (Recommendation: try the backing; it reads as rows lifting.)

## Things not to do

- Do not synthesize mouse drags with `SendInput` to test live resize: it moves the
  user's real cursor on the desktop they are using.
- Do not stage `plans/replace-task-manager.md` (another thread's).
- Do not run `cargo fmt --all` while another thread has uncommitted files; format
  only our own with `rustfmt --edition 2021 <files>`.
- Do not leave test values in the user's registry: `HKCU\Software\open-task` did
  not exist before this thread; tests that write it must remove it again.
