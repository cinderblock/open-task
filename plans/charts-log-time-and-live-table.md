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
   how long ago) centered on the line, replacing the tick labels while hovering.
   The hovered age is held fixed while the pointer is still, so data slides under
   the line as new samples arrive.
5. **Resize fix:** render synchronously inside `WM_SIZE` (then validate the window
   so the queued `WM_PAINT` does not draw the same frame again), instead of
   relying on `WM_PAINT`, which a live drag starves.

## Plan / steps

1. ~~Read the code, reproduce the resize report.~~
2. **[current]** Resize: synchronous render in `WM_SIZE`. Verify with the scratch
   script (0 ms screenshot) and ask the user to confirm the live drag.
3. `ot-core`: multi-resolution `Series` + tests. Timeline uses it.
4. `ot-ui`: `TimeAxis` (age <-> x), log-axis painting with envelope, gridlines and
   label band. Tests.
5. `ot-ui`: chart hover state in `App`, crosshair in every chart, readout. Tests.
6. `format::ago` for the readout.
7. README (charts paragraph), architecture plan (decision 8 status), this plan.
8. Checks: `cargo test`, clippy on windows / linux / macOS targets, fmt;
   screenshots of the charts, with and without hover.
9. Commit per logical step.
10. Items 4 and 5: present options, record the user's answer here.

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
  synthetic drag would move the user's cursor on their live desktop.

## Progress log

- [x] Plan written; code read; resize report narrowed to live drags.
- [ ] Resize fix.
- [ ] Multi-resolution series.
- [ ] Log time axis charts.
- [ ] Synced hover crosshair and readout.
- [ ] README and plans.
- [ ] Checks and screenshots.
- [ ] Options for items 4 and 5 put to the user.

## Open questions for the user

1. Item 4 (hysteresis): which of the options to build (see the chat answer; to be
   copied here once chosen).
2. Item 5 (2D "pie" view): which form, and where it lives.

## Things not to do

- Do not synthesize mouse drags with `SendInput` to test live resize: it moves the
  user's real cursor on the desktop they are using.
- Do not stage `plans/replace-task-manager.md` (another thread's).
