# Charts scroll smoothly; five minutes by default; on screen at once

> **Status:** done; checked in the running app, committed on `master` (2026-10-06), not pushed · **Started:** 2026-10-06 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/chart-span-and-history-length.md` and `plans/charts-log-time-and-live-table.md`.

## Goal

The user's requests on 2026-10-06:

1. "Can you make all of the charts draw smoothly? I want smooth scrolling with
   time (keeping logarithmic time)." Before this, every chart redrew only when a
   sample arrived, so the line jumped one interval leftward once a second.
2. "Set max time to like 5m": the default history length (the chart's reach)
   becomes five minutes.
3. "...and lower when it first starts so that the chart looks fuller": the axis's
   10 s floor left a young session's line in the right part of the chart
   (`target/shot13-summary.png`, a few seconds in).
4. "It seems to take ~3 seconds of samples before anything is shown on the
   screen. Can we do better?" (asked mid-way).

## Environment / context

- Axis and plotting: `crates/ot-ui/src/sparkline.rs` (`TimeAxis`, `Plot`).
  Chart groups and the clock: `crates/ot-ui/src/charts.rs` (`axis_of`,
  `ChartClock`, `Charted`, `newest_gap`). History view:
  `crates/ot-ui/src/usage_chart.rs`. Pages: `perf.rs` (takes `Charted`),
  `pages/summary.rs` (`Inputs::axis`), the Processes cards in `view.rs`
  (`App::paint_at` computes the frame's axis once).
- Frame pacing: `crates/ot-shell-win/src/window.rs` `repaint` loops at the
  display rate while `App::animating()`; skipped while minimized or hidden.
- Damage-tracked rendering: `crates/ot-paint/src/damage.rs`
  (`DisplayList::damage_since`, `DisplayList::bounds`) and
  `crates/ot-shell-win/src/gfx.rs` (`canvas`, `last`, `draw_damage`).
- Start-up: `crates/ot-core/src/sampler.rs` (`FIRST_RATES_AFTER`),
  `crates/ot-core/src/timeline.rs` (first sample back-filled to its interval's
  start), `ChartClock::fresh`.
- Settings: `HISTORY_STEPS` gained 5, `HISTORY_DEFAULT` = 5; `smooth_charts`
  (registry `SmoothCharts`).
- The user's machine: 60 Hz display, Windows animation effects **off**
  (`SPI_GETCLIENTAREAANIMATION` = 0), `AnimateRows` = 1 set explicitly, no
  `HistoryMinutes` stored (so the new 5 min default applies to them).

## Decisions already made (don't re-ask)

1. **A chart clock, not the newest sample, sits at the right edge.** It runs at
   the pace samples arrive (1x live, the playback speed in a replay), trails the
   newest by 1.25 intervals so a new sample slides in from past the edge, takes
   up jitter by running faster or slower (0.5x–2x, never backward, never past its
   target), jumps when more than two intervals off, and never passes the newest
   sample. Arrivals are timed by the frame that first sees them.
2. **Past the right edge the axis continues linearly** (the log's slope at zero);
   plots are clipped to their rect.
3. **Columns are grouped by power-of-two time bins**, anchored in time, so the
   compressed end does not shimmer as it scrolls.
4. **The span is `clock - oldest`, floored at 100 ms, capped at the history
   length.**
5. **Default history length 5 min**, with a 5 min step added.
6. **Smooth scrolling is its own switch, on by default, independent of Windows'
   animation effects.** First built to follow Windows like row animation; changed
   because the user has those effects off and asked for smooth charts.
7. **Only what changed is redrawn.** The renderer keeps the last frame in a canvas
   bitmap, diffs each display list against the previous one, replays the commands
   touching each damaged rectangle (clipped), and copies the canvas to the swap
   chain. More than half the window damaged, or a structural difference, draws
   whole.
8. **Fast start:** the sampler's second pass (the first with rates) runs 250 ms
   after the first; the first sample is also recorded at its interval's start; the
   clock starts with the newest sample at the edge and eases back to its lag.

## Plan / steps

1. [x] `TimeAxis`: `now_ms`, linear extension, `x_unclamped`, `ms_per_dip`,
   `bin`, floor.
2. [x] `Plot::build`: clock ages, time bins, clip, hover inside the rect only.
3. [x] `ChartClock`, `axis_of(timeline, now, length)`, `Charted`.
4. [x] App wiring, `animating()`, `Page::has_charts`, shell minimized guard.
5. [x] Usage chart: same clock and bins.
6. [x] Settings: 5 min default and step; Scroll charts smoothly card; prefs.
7. [x] Damage-tracked rendering.
8. [x] Fast start.
9. [x] Tests, fmt, clippy; visual checks; CPU measured; README; commits.

## Findings / gotchas

- **Windows animation effects off here** made the first version (following
  Windows) look broken: frames between samples were identical. Check
  `SPI_GETCLIENTAREAANIMATION` before concluding smoothing fails.
- **Cost of redrawing everything at 60 Hz:** 36 % of one core on the Processes
  page. Timed: paint (building the list) 0.45 ms, Direct2D text 2.3–3.7 ms a frame
  (about 300 `DrawTextLayout` calls; layouts were already cached), geometry
  0.02 ms. With damage tracking: 15–19 % of one core in two runs (Settings page,
  no frames: about 2 %; Processes page before this work, no smoothing: about 7 %).
  What remains a frame: paint 0.48 ms, layout bookkeeping 0.07 ms, replaying the
  two chart areas 0.2–0.7 ms, `EndDraw` 0.3 ms, the copy 0.07 ms.
- With smoothing, a lone sample a couple of seconds into a session was off-screen
  (clock 1.25 intervals behind) and the 2 s floor kept the line short: hence the
  100 ms floor and the clock's fresh start at the newest sample.
- A slew of `off * dt / SLEW` is unstable when `rate * dt` is large (fast replay,
  sparse frames): it overshoots and oscillates. The correction is now clamped to
  the paces and to `off` itself.
- Test apps: `ready()` turns smooth charts off so frames are deterministic; the
  one test that checks scrolling turns it on.
- `sed -i` in Git Bash wrote LF into CRLF files (perf.rs, view.rs); fixed with
  Python. Use `target/tmp/patchlib2.py` for edits from scripts.
- The first `bash` heredoc with Python inside failed on quoting; writing the
  script to a file works.

## Progress log

- [x] 2026-10-06: plan written; axis, plot, clock, settings, app wiring; tests.
- [x] 2026-10-06: visual check found the 10 s floor, then the Windows-animations
  default; both fixed.
- [x] 2026-10-06: CPU measured at 36 %; damage-tracked rendering brought it to
  15–19 %.
- [x] 2026-10-06: fast start (user's follow-up): first sample on screen at 0.6 s
  in the screenshot (`target/start-600ms.png`).
- [x] 2026-10-06: committed.

## Open questions for the user

1. Smooth scrolling costs about 10 % of one core more than before on the
   Processes page (about 1 % of this 12-thread machine). Worth going further? The
   biggest remaining piece is rebuilding the whole display list each frame
   (0.48 ms); a chart-only repaint path in the view would cut it. Recommendation:
   leave it unless it bothers you.

## Things not to do

- Do not reintroduce a shared `const AXIS` (see the earlier plan).
- Do not drop the axis floor to zero: a log axis over no time has no scale.
- Do not tie smooth scrolling back to Windows' animation effects without asking.
- Do not let the clock run backward to take up a lag: it scrolls the chart right.
