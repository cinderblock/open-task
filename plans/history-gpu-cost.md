# The History view's GPU cost

## Goal

The GPU History reading (`plans/history-gpu.md`) showed open-task itself taking
30–60 % of this machine's integrated GPU (3D engine) on the History view, against
5–7 % on List (`ISSUES.md`). Bring it down. User, 2026-10-12: fine "as long as it
looks good and is visually nearly pixel identical"; fewer frames are acceptable
only "if we're smart about it", i.e. only for the left of the chart where the
time axis squeezes the pixels.

## Environment / context

- Intel UHD Graphics (integrated), 96 DPI, window 1164 x 721.
- Chart drawing modes (`ChartDrawing`, setting `ChartDrawing` in
  `HKCU\Software\open-task`, `OT_CHARTS=gpu|cpu|d2d` overrides): GPU (Direct3D
  shader, default), CPU (`ot_paint::ChartRaster`), Direct2D.
- GPU renderer: `crates/ot-shell-win/src/chart_gpu.rs` (HLSL in `HLSL`), shapes
  and pieces: `crates/ot-shell-win/src/charts.rs`, frame: `gfx.rs`
  (`prepare_charts`, `draw_runs`).
- Timing: `cargo test -p ot-shell-win --release chart_frame_timing -- --ignored
  --nocapture` (full-history History frame; now prints GPU time per frame by
  timestamp queries). Live: PDH `\GPU Engine(pid_<pid>_*engtype_3d)\Utilization
  Percentage` while `target\debug\open-task.exe --view history` runs.

## Findings / gotchas

- Not the frame rate as such: with "Scroll charts smoothly" off, History still
  spiked to ~26 % in seconds where frames ran (names settling).
- **It is the GPU chart renderer.** Live, History: GPU mode 25–63 % of the 3D
  engine, CPU mode ~6 %, Direct2D ~7 % (CPU: 10 %, 33 %, 8 % of a core, short runs).
  The earlier perf work (`plans/chart-renderers.md`) measured only CPU time.
- Cause 1: each shape drawn as one quad over its bounding box, every box pixel
  running the shader. Fixed by pieces (`db1435a`): bit-identical, GPU frame
  3.3–3.5 ms -> 2.5–3.0 ms.
- Cause 2 (bigger): band pixels do 4 samples x 2 binary searches (~40 buffer
  reads). Experiments (temporary shader variants, removed): flat shader 0.44 ms;
  real hairlines + flat bands 1.0 ms; real bands + flat hairlines 3.0 ms. So bands
  ~2.5 ms, hairlines ~0.55 ms, fixed ~0.44 ms. CPU mode's frame: 0.5–0.6 ms GPU;
  Direct2D's 0.65–0.73 ms.

- Bands from per-column samples (`c8ec759`): still bit-identical to whole shapes
  (WARP and the Intel GPU, 96/120/144 DPI); the feared GPU-division rounding
  never showed in 8-bit output. Timing test: GPU 0.9–1.4 ms a frame (was
  3.1–3.4); CPU 0.5–0.6, Direct2D 0.65–0.9. CPU prep 0.4 ms (pieces first cost
  0.42 ms alone with binary searches; walking the points halved it).
- **Live, interleaved, 3 rounds** (History, 12 s warm-up, 5 x 1 s samples), build
  before pieces vs `c8ec759` content: 3D-engine share 30.3 / 34.8 / 55.3 % ->
  9.8 / 12.0 / 6.6 % (means 40.1 -> 9.5 %). CPU, 20 s windows: 7.0/9.4/9.9 % ->
  7.7/9.8/10.1 %.
- **Unexplained:** one earlier live A/B (same code, before the experiment build)
  read ~50 % for the new build too, and a mode comparison right after read GPU
  mode 48–59 %. Twenty minutes later the same code read 6–12 %, consistently,
  three times. The whole 3D engine was 74–86 % busy with other programs
  throughout, so attribution under contention is a suspect; not reproduced since.
- Live measurement noise: sampling `Get-Counter` inside the CPU window inflated
  the process's CPU reading; measure CPU in a window of its own.

## Plan / steps

1. [x] Measure, find the cause.
2. [x] Pieces (strips) instead of bounding-box quads (`db1435a`).
3. [x] Bands from per-column samples computed on the CPU (`c8ec759`).
4. [x] Re-measure (timing test and live): History's GPU share now ~10 %, on a par
       with the CPU (11 %) and Direct2D (8 %) modes; List is 5–7 %.
5. [x] Close the ISSUES.md entry.
6. [ ] User's call: whether the left-side frame idea or cheaper hairlines
       (~0.5 ms of the remaining GPU frame) are still wanted. Recommendation: no.

## Decisions already made (don't re-ask)

- Output must stay visually nearly pixel identical (user).
- Fewer frames only on the left of the chart, where pixels are small (user), and
  only if still needed after the renderer fix (mine: measure first).

## Open questions for the user

1. With History's GPU share down to ~10 % (from ~40 %), still pursue the cached
   left side / fewer frames there, or the hairlines? Recommendation: no; what is
   left is about what every mode costs to draw 60 frames a second.

## Things not to do

- Don't judge the History's cost on a few seconds of history: it grows with the
  history (`plans/perf-review-v0.11.md`). The timing test's frame is full history.
- `measure` is a PowerShell alias of Measure-Object: name helper functions
  otherwise.
