# Chart renderers: GPU (Direct3D) and CPU, beside Direct2D

> **Status:** active · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Part of `plans/perf-review-v0.11.md` (step 3b).

## Goal

At full history the History view costs about a third of a core in the foreground,
and about 70 % of that is Direct2D antialiasing and tessellating chart shapes on the
CPU every frame (`plans/perf-review-v0.11.md`, steady-state profile). Take chart
drawing off that path with two new renderers, compare them, and let the user pick.

## Decisions already made (don't re-ask)

- **Build both** a Direct3D shader renderer and our own CPU rasterizer, compare
  them, and offer the choice in Settings. **Default: GPU (Direct3D).** (User,
  2026-10-07: "implementing BOTH methods, compare them, and give the user the
  option to pick which one they want. default to 3D in gpu".)
- Direct2D stays as a third choice: it is today's drawing, so it costs nothing to
  keep and is the reference the other two are checked against.
- No WARP/software fallback as a fix (user). If the GPU renderer cannot start
  (shader compile, feature level), fall back to the CPU renderer and log it.

## Design

- **Two new display-list commands** (`ot-paint`), the only chart shapes:
  - `DrawCmd::Band { top, bottom, color }`: the region between two piecewise
    linear functions of x (`top` and `bottom` are point spans, each ascending in x,
    each with its own xs), over the x-range both cover. Chart wash (bottom = a
    two-point baseline), min-max envelope, History bands.
  - `DrawCmd::Graph { points, width, color }`: a line through points ascending in
    x. Chart mean lines, History band hairlines.
  - Bounds, damage comparison and splicing as for `Polyline`.
- **Renderer** (`ot-shell-win`): a run of consecutive `Band`/`Graph` commands
  (same clip) is one *chart run*. Before `BeginDraw`, each run that this frame
  draws (all of them for a whole frame, those touching damage for a partial one) is
  rendered into a bitmap of its own, sized to the run's bounds within its clip, in
  device pixels; the Direct2D pass then draws that bitmap 1:1 in the run's place.
  No Direct3D work happens between `BeginDraw` and `EndDraw`.
  - Direct2D choice: no bitmaps; `Band` as a polygon, `Graph` as a polyline, as
    today.
  - CPU: `ot-paint::chart_raster` fills a premultiplied buffer; uploaded with
    `CopyFromMemory`.
  - GPU: vertex data for all runs in one dynamic structured buffer, primitive
    parameters in another; one instanced draw of quads per run into the run's
    texture (premultiplied source-over); the pixel shader computes coverage.
- **Coverage, the same in CPU and GPU:**
  - Band: per pixel, 4 sub-samples across x; at each, top and bottom by linear
    interpolation; exact vertical overlap of `[top, bottom]` with the pixel row;
    coverage = the mean.
  - Graph: distance d from the pixel center to the nearest segment; coverage =
    clamp(w/2 + 0.5 - d, 0, 1).
- **Choice:** Settings ("Chart drawing": GPU / CPU / Direct2D), saved as
  `ChartDrawing` (0 GPU, 1 CPU, 2 Direct2D) in `HKCU\Software\open-task`; `OT_CHARTS=gpu|cpu|d2d` overrides it
  for measurement.

## Plan / steps

1. ~~`ot-paint`: `Band`, `Graph` (+ bounds, damage, splice, tests).~~
2. ~~`ot-paint`: `chart_raster` CPU rasterizer + tests.~~
3. ~~`ot-ui`: sparkline and History emit `Band`/`Graph`.~~
4. ~~`ot-shell-win`: chart runs (`charts.rs`); Direct2D path; CPU path; GPU path
   (`chart_gpu.rs`, HLSL compiled at start with `D3DCompile`).~~
5. ~~Setting ("Chart drawing" stepper card) + prefs (`ChartDrawing`) + `OT_CHARTS`;
   README.~~
6. ~~Parity test on WARP (`gfx::tests::the_gpu_and_cpu_chart_renderers_draw_what_direct2d_draws`);
   screenshots; `OT_CHECK_DAMAGE`.~~
7. ~~Frame stats for each at full history (6 min runs).~~
8. ~~CPU rasterizer sped up (`683221a`).~~
9. ~~Report the comparison to the user.~~ Released in v0.12.0.
10. Possible later, if the user wants the CPU path faster still: SIMD by hand for
    the edge pixels and lines, rasterizing only the columns a partial frame
    damages, or sharing the work across threads (wall time only, not CPU).

## Findings / gotchas

- **Parity (WARP, the test's frame, 96 and 144 DPI):** CPU vs GPU at most 2-3 /255
  apart, mean 0.07. Each vs Direct2D: mean 0.2-0.3 /255, 0.6-0.9 % of pixels more
  than 8 apart, worst 68-71, all along steep line edges (a different edge filter;
  side by side at 8x they look the same). `OT_PARITY_DUMP=<absolute dir>` writes
  the frames (a relative path lands under the crate: tests run in it).
- **The driver's 8 x 4 bricks follow Direct2D's chart fills.** `OT_CHECK_DAMAGE`,
  History, 30 s each: Direct2D 30 differing frames (29 of them 8 x 4 blocks inside
  the History chart); CPU 1; GPU 1 (each a few pixels).
- **Full history, History view, 6 min runs, window in the background (30 fps),
  `target/tmp/modes-long.ps1`, averages over the last minute** (Intel driver
  31.0.101.2145 for the second set):

  | Run | GPU | CPU | Direct2D |
  | --- | --- | --- | --- |
  | first (`7d7fa5c`) | 7.2 % | 51.6 % | 23.0 % |
  | after the CPU speed-up (`683221a`) | **5.9 %** | 24.2 % | 21.3 % |

  Per frame (second set): GPU chart prep 0.18 ms + draw 0.71 ms; CPU chart prep
  6.6 ms + draw 7.3 ms; Direct2D draw 6.0 ms. Paint (display list) 0.45-0.5 ms in
  all three, the History's per-frame rebuild (fixed in `e4b3035`).
- CPU rasterizer timing test (`chart_raster::tests::history_sized_frame_timing`,
  670 x 290, nine bands + hairlines, fastest of 15 batches): 9.3 ms -> 2.3 ms after
  row-run fills (vectorizable 16-byte steps), packed blending and precomputed
  segments. In the app the History is larger (about 670 x 430) and the bitmap is
  uploaded, so CPU mode only matches Direct2D there.
- Micro-timings on this busy machine wander +-40 % run to run; take the fastest
  of several batches, and interleave A/B runs.
- `OT_CHECK_DAMAGE` makes `draw_ms` meaningless (the whole frame is drawn again
  inside it): measure with the check off.
- Git Bash heredocs break on an apostrophe in the text (`open-task's`): write patch
  scripts to `target/tmp/*.py` with the Write tool.

## Progress log

- [x] 2026-10-07: commands, CPU rasterizer, painters, renderer (D2D, CPU, GPU),
  setting, parity test; clippy on three targets, all tests
- [x] 2026-10-07: measured at full history (two sets); CPU path sped up
- [x] 2026-10-08: reported; released in v0.12.0

## Open questions for the user

None.
