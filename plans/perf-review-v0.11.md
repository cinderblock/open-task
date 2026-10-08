# Holistic performance review (after v0.11.1)

> **Status:** active · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/chart-frame-cost.md` and `plans/smooth-chart-scrolling.md`.

## Goal

The user, 2026-10-07: "I also want a wholistic performance review. It's taking more
processor usage than I expected still." Find where open-task's CPU goes in every
state (foreground, background, hidden, each page), measure it rather than guess, and
cut what can be cut without giving up smooth charts.

## Environment / context

- Machine: Intel UHD Graphics (driver 30.0.101.1660, 2022-03-17, being updated by
  the user in another thread — re-measure totals after it changes), plus a Fresco
  Logic USB display adapter. 12 logical processors. Window 1164 x 721 px at 96 DPI
  in the test runs.
- Profilers: `wpr` (C:\Windows\System32) and `xperf` (Windows Performance Toolkit,
  on PATH). Release builds have PDBs in `target/release`, so stacks resolve.
- Diagnostics in the app: `OT_CHECK_DAMAGE`, `OT_WARP` (README, "Two environment
  variables...").
- Earlier numbers (v0.11.1, 60 Hz, Processes page): 8–10 % of one core in the
  foreground, about 5 % in the background, about 2 % on Settings (no frames).

## Decisions already made (don't re-ask)

- No "fewer frames when motion is slow" (declined in `chart-frame-cost.md`).
- No software rendering (WARP) as a fallback or fix: the user does not want a slow
  path. WARP stays a diagnostic switch only.

## Static review (subagent read, 2026-10-07; unmeasured estimates)

Ranked by its estimated impact:

1. Chart geometry rebuilt and tessellated every frame (3 paths per chart: wash,
   envelope, line). Ideas: aliased wash, envelope only where max > min, bevel
   joins (done in `4257222` for correctness), wider columns.
2. Row-slide frames after a re-sort are full paints; damage often > half the
   window, so `draw_all` (about 300 text draws, 2.3–3.7 ms).
3. The per-sample frame: new strings miss the layout cache.
4. Sampling pass about 10 ms/s: GPU engine PDH wildcard (2.4–3.2 ms), services
   enumeration every pass, efficiency-mode reads, thread maps.
5. Per-sample UI work for pages not shown and while hidden (`show_snapshot`,
   `system.set_snapshot`, the snapshot handler while minimized/tray).
6. Hovering a chart forces a full paint every frame (`marking`).
7. History view repaints its whole layer per frame; likely > `WHOLE_AT`.
8. Full-canvas copy and full `Present` every frame (DWM recomposes the whole
   window); empty-damage frames still copy and present.
9. Spliced frames: three full passes over the command list.
10. Small: sampler wakes every 50 ms; each icon forces a whole redraw; tray
    updates.

## Plan / steps

1. ~~Baseline: CPU per state and a sampled CPU profile.~~ Diagnostics added:
   `OT_FRAME_STATS`, `[profile.profiling]`.
2. **[current]** Steady-state (full history) profile of the History view.
3. Fixes, each measured with the same method:
   a. ~~`WM_PAINT`: `ValidateRect` instead of `BeginPaint`/`EndPaint`.~~ (made,
      unmeasured)
   b. The chart drawing cost that grows with history (the big one; approach to be
      chosen with the user).
   c. Row slides when sorting by a volatile column.
   d. Per-sample work while minimized or for hidden pages; the sampler's own cost.
4. Re-measure everything after the driver update.

## Findings / gotchas

- **The user's saved layout is Processes + History view** (`HKCU\Software\open-task`
  `Layout`: `page=Processes;view=History;sort=Cycles;desc=1`). Every launch with
  `--page processes` and no `--view` measures History. Measure each view
  explicitly (`--view list|history|map`).
- **The History view's frame cost grows with the history.** `OT_FRAME_STATS`, 6 min
  run (`target/tmp/frame-stats-run.ps1 -FgSec 370`, History, window lost the
  foreground early so 30 fps): partial draw 0.65 ms at 5 s, 2 ms at 45 s, 3–4 ms at
  2 min, **4.4–6.2 ms from 3 min on**; paint (display list) 0.08 -> 0.37 ms; process
  CPU 5 % -> **17–23 % of a core at 30 fps**. Short runs badly understate it.
  Cause: each band is a filled polygon of about 2 x plot-width points plus a
  1-DIP polyline along its top, up to 9 bands (about 18k points), antialiased and
  tessellated by Direct2D on the CPU every frame.
- Per view, foreground, 60 fps, 20 s runs (so short history):
  Map 8–11 % (only the two Processes charts redraw; draw 0.5–0.75 ms); History
  10–13 %; list sorted by Cycles 15–18 % (50 slide frames per 5 s, each a whole
  redraw at 4–7 ms); Performance 8–14 %.
- Chart pieces on the Map view (`OT_EXP_CHART`, scratch): skipping wash, envelope,
  line, or all three moved the partial draw only 0.62 -> 0.46 ms: for the two small
  sparklines the fixed cost (clips, card text, `EndDraw`) dominates.
- Aliased fills (`OT_EXP_ALIASFILL`, scratch), History, 26 s: partial draw 1.48 /
  1.20 ms -> 1.05 / 1.02 ms (two interleaved rounds). A partial fix only.
- Profile (xperf sampled, symbol build `target/profiling`, History, 30 s from
  launch, 14 % CPU): `repaint` 72 % of samples; `EndDraw` 46 % (fill tessellation
  32 %, of it `FillPath`/`RasterizeEdges` 20–23 %; `StrokePath` 8.5 %); `Present`
  7 %; canvas copy 3 %; **`BeginPaint` 5 % (its `GetDCEx`)**; sampler thread 11 %
  (`sample_processes` 8 %, of it `NtQuerySystemInformation` 4.8 %).
- **Profiling gotchas.** The release profile strips symbols (`strip = "symbols"`):
  everything of ours lands on one wrong symbol. Use `cargo build --profile
  profiling` and copy exe+pdb aside before tracing, since a rebuild overwrites the
  PDB. `wpr -start CPU` records win32k events with stacks: about a third of the
  samples were the profiler's own stack walks. Use `xperf -on
  PROC_THREAD+LOADER+PROFILE -stackwalk Profile`; analyze with `xperf -a stack
  -butterfly 20 -process open-task` and `target/tmp/butterfly.py`
  (`-event SampledProfile` matched nothing).
- The machine is busy (about 39 % total load from Electron/Chrome), so process-CPU
  numbers wander by several points between identical runs; draw times from
  `OT_FRAME_STATS` and profile shares are steadier.

## Progress log

- [x] 2026-10-07: static review
- [ ] Baseline measured
- [ ] Fixes chosen, made, measured
- [ ] After-driver re-measure

## Open questions for the user

None yet.
