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
2. ~~Steady-state (full history) profile of the History view.~~
3. Fixes, each measured with the same method:
   a. ~~`WM_PAINT`: `ValidateRect` instead of `BeginPaint`/`EndPaint`.~~ (made,
      unmeasured)
   b. ~~The chart drawing cost that grows with history:~~ GPU and CPU chart
      renderers with a setting (`plans/chart-renderers.md`, `7d7fa5c`,
      `683221a`). History view at full history: 21-23 % (Direct2D) -> 5.9 % (GPU)
      at 30 fps.
   b2. ~~The History rebuilt its bands every frame~~ (`e4b3035`, Usage revision).
   c. ~~Row slides when sorting by a volatile column~~: measured, nothing to gain
      from the whole-frame threshold (see Findings).
   d. ~~No frames while the window cannot be seen~~ (`947e8cc`). Per-sample UI
      work for hidden pages: not done (small).
   e. ~~Present only what changed~~ (`6c1a09e`): dwm.exe 21 -> 18 % foreground,
      25 -> 15 % background on the Map view.
4. ~~Re-measure after the driver update~~ (31.0.101.2145; the second
   full-history set and everything after are on it).
5. **[current]** Report to the user; remaining items are small (see Open
   questions).

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
- **Steady-state profile** (History, foreground, 5.5 min in, symbol build,
  `target/tmp/steady-profile.ps1`): **33.4 % of a core**. Samples: `EndDraw` 73 %;
  `FillPath`/`RasterizePath` 50–55 % (the bands' and charts' filled paths,
  antialiased on the CPU); `StrokePath` 15 % (band hairlines, chart lines);
  `UsageChart::build` 4.8 % (reweights every program over the whole history every
  frame, `assign_bands`); sampler 3.4 %; `Present` 3.3 %; `BeginPaint` 2.1 %
  (fixed, `defb530`); text 1.8 %; canvas copy 0.9 %.
- Minimized: about 2–4 % (per-sample work and one frame a sample, `repaint`
  draws on `WM_APP_SNAPSHOT` even when not `seen`); Settings page about 1.7 %.
- **Now (`6c1a09e`), History view, full history, foreground, 60 fps, symbol
  build, `target/tmp/steady-profile2.ps1`: 10.7 % of a core** (33.4 % this
  morning). Samples: `render` 65 % (of it `EndDraw` 21 %, `Present1` 13 %, the
  canvas copy 5 %, the chart runs' `DrawInstanced` 4 %), the Intel driver spread
  through those; the sampler thread 15 % (`sample_processes` 11 %, of it
  `NtQuerySystemInformation` 8.6 %); `DisplayList::bounds` 3.9 % (recomputed per
  damage area); sparkline `Plot::build` 3.5 %; History paint + build 5 %.
  What remains is mostly the fixed cost of submitting and presenting 60 frames a
  second.
- **GPU mode, full history, `modes-long.ps1`:** 10.8 % at 60 fps in the
  foreground (paint 0.21 ms, chart prep 0.23 ms, draw 0.76 ms a frame).
- **Minimized** after `947e8cc`: 1.2-2.2 % (was 2-4 %).
- **Whole-frame threshold** (`WHOLE_AT`), list view sorted by Cycles: 0.5 vs 0.95,
  two interleaved rounds: draw 1.36 / 1.37 ms vs 1.39 / 1.37 ms. No gain: only
  about 16 frames in 5 s were whole; slides are mostly drawn in part already.
- **Partial presents** (`6c1a09e`), Map view, three alternating rounds with a
  temporary full-present switch: dwm.exe 21.3 -> 17.8 % (foreground), 25.4 ->
  15.2 % (background); open-task itself unchanged. `OT_CHECK_DAMAGE` now also
  compares the buffer to present with the canvas: 0 of 3000 frames differed.
- PowerShell gotcha: the harness blocks a `Remove-Item` whose line also holds a
  regex like `\d+` (it reads it as a path); clear variables with `$env:X = $null`.
- The machine is busy (about 39 % total load from Electron/Chrome), so process-CPU
  numbers wander by several points between identical runs; draw times from
  `OT_FRAME_STATS` and profile shares are steadier.

## Progress log

- [x] 2026-10-07: static review
- [x] 2026-10-07: baseline measured (per view, 6-min growth run, steady profile)
- [x] 2026-10-07: `ValidateRect` paint (`defb530`); diagnostics (`c131cdd`)
- [x] 2026-10-07: chart renderers (`7d7fa5c`, `683221a`), History bands once a
  sample (`e4b3035`), no hidden frames (`947e8cc`), partial presents (`6c1a09e`)
- [x] 2026-10-07: re-measured on driver 31.0.101.2145
- [x] 2026-10-08: reported; released as v0.12.0 at the user's word (`b5d1e78`,
  tag pushed; Release and CI workflows passed; nine assets published)
- [ ] The user's call on the small remaining items (open questions 1 and 2)

## Open questions for the user

1. The CPU chart renderer matches Direct2D at the History's full size (24 vs 21 %
   at 30 fps; GPU 5.9 %). Leave it, or make it faster (hand SIMD for edges and
   lines, rasterize only damaged columns)? Recommendation: leave it; the GPU is the
   default and the CPU path is there as a choice.
2. Smaller remaining items, each about 0.5-1 % of a core: cache each command's
   bounds once a frame; draw chart runs straight into the canvas instead of a
   texture each; skip per-sample work for pages not shown. Worth doing?
   Recommendation: the bounds cache only (simple, safe); the rest is little for
   its risk.
3. ~~Release these as v0.12.0?~~ Released 2026-10-08.
