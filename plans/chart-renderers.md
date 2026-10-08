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
7. **[current]** Frame stats and profiles for each at full history (6 min runs).
8. Report the comparison to the user.

## Findings / gotchas

- **Parity (WARP, the test's frame, 96 and 144 DPI):** CPU vs GPU at most 2-3 /255
  apart, mean 0.07. Each vs Direct2D: mean 0.2-0.3 /255, 0.6-0.9 % of pixels more
  than 8 apart, worst 68-71, all along steep line edges (a different edge filter;
  side by side at 8x they look the same). `OT_PARITY_DUMP=<absolute dir>` writes
  the frames (a relative path lands under the crate: tests run in it).
- **The driver's 8 x 4 bricks follow Direct2D's chart fills.** `OT_CHECK_DAMAGE`,
  History, 30 s each: Direct2D 30 differing frames (29 of them 8 x 4 blocks inside
  the History chart); CPU 1; GPU 1 (each a few pixels).
- `OT_CHECK_DAMAGE` makes `draw_ms` meaningless (the whole frame is drawn again
  inside it): measure with the check off.
- Git Bash heredocs break on an apostrophe in the text (`open-task's`): write patch
  scripts to `target/tmp/*.py` with the Write tool.

## Progress log

- [x] 2026-10-07: commands, CPU rasterizer, painters, renderer (D2D, CPU, GPU),
  setting, parity test; clippy on three targets, all tests
- [ ] Measured at full history and reported

## Open questions for the user

None.
