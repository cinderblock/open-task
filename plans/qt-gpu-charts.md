# GPU chart drawing for the Qt plugin on Linux

## Goal

Draw charts with a shader under the Qt plugin, as the Windows shell's Direct3D
renderer does, if it pays. The user runs KDE on a Radeon 6000-series GPU and asked
how to cut open-task's CPU use (`plans/linux-cpu-cost.md`). After v0.18.0's
partial repaints, rasterizing chart images on the CPU is the biggest thing left
on the History view. A measured "no" is a good result: prototype the cheapest
honest version, compare it with v0.18.0, and only then build it properly.

## Environment / context

- Branch `qt-gpu-charts`, worktree
  `C:\Users\camer\.t3\worktrees\open-task\qt-gpu-charts`. Measured on `15a308f`
  (v0.18.0 plus a plan note), then rebased onto v0.19.0 (`f6da418`), which changed
  none of the code measured here. Commit hashes below are the rebased ones.
- WSL `Ubuntu` 24.04, Qt 6.4.2. `CARGO_TARGET_DIR=~/.cache/open-task-target-qt-gpu`.
  Run WSL from PowerShell (`wsl.exe -e bash <script>`). Builds go through the
  CPU broker: `node ~/.claude/bin/cpu-slots.mjs run --slots 4 ...`.
- Scratch harness in `%TEMP%\ot-linux\` (not in the repo): `qg-build.sh <name>`
  builds this worktree into `~/.cache/ot-ab/open-task-<name>`.
- GL in WSL: `GALLIUM_DRIVER=d3d12 LD_LIBRARY_PATH=/usr/lib/wsl/lib` gives
  `D3D12 (Intel(R) UHD Graphics)`, an i7-10710U's iGPU through a translation
  layer. Not the user's Radeon. **It works on a private Xvfb too**, which older
  plans deny: without those variables Xvfb gets llvmpipe. So no GUI test needs the
  user's desktop.

## Decisions already made (don't re-ask)

- GTK is out of scope.
- The default stays CPU unless the numbers argue otherwise.
- `ChartDrawing::Gpu` must fall back to the CPU cleanly with no usable GL, and
  `charts::OFFERED` / `Capabilities` offer it only when true.
- Qt 6.2 stays the oldest supported; a build must work there.
- Don't write a second rasterizer in the plugin.

## Findings / gotchas

- What KDE distros ship: Kubuntu 26.04 LTS has Qt 6.10.2 and Plasma 6.6, and no
  X11 session, so the user is probably on Wayland.
- **The v0.18.0 number to beat came from a young chart on an idle machine.** The
  5.6 ms History frame was measured 60 s into a run on a quiet WSL. Two things
  are wrong with that frame for this question:
  - Age: the time axis is log-scale and fills its width from the first sample, so
    in the first minutes the whole plot moves every frame. After the default
    5 minutes of history only the newest part does. Same binary, History 1800x1000
    on WSLg, 330 s in: 2.97 ms a frame, charts 0.85 ms, 25 % of the window drawn,
    versus 4.1-7.0 ms, charts 1.6 ms, 44 % at 60 s. Cost keeps falling for minutes
    (`partial_drawn_pct` 43 -> 25 over 5 min).
  - Content: a quiet WSL has two programs, so two bands. A desktop has nine and
    "Everything else", the frame Windows' numbers came from.
- **Workload for every number below:** `%TEMP%\ot-linux\qg-load.sh` runs 14 named
  programs (`plasmashell`, `firefox`, ...), each a small C loop with a wandering
  duty cycle, about 0.3 of a core in all, so History has its nine bands.
- **WSLg window size: the harness resized the wrong window.** `xdotool search
  --pid $app --name '^open-task'` finds a hidden Qt window; the real one comes
  from `--pid` alone (seen: real 4194310, 1180x760; hidden 4194312). Xvfb happened
  to pick the real one. `ab-measure.sh` has the same line, so the WSLg rows of
  `plans/linux-cpu-cost.md` labelled 1800x1000, and v0.18.0's "5.6 ms", were very
  likely at 1180x760. `qg-measure.sh` logs the geometry it got.
- **Prototype R** (scratch worktree `%TEMP%\ot-qg-proto`, not for committing): the
  plugin gives an offscreen GL context, the host draws each run with a shader
  ported from `chart_gpu.rs` and reads it back into the `OtRaster` image. Nothing
  else changes: the widget, the backing store, partial repaints.
  - Same-frame parity with `ChartRaster` (`OT_GL_COMPARE`): at most 4/255 on
    llvmpipe, 3/255 on D3D12, nothing over 8. `OT_CHARTS=cpu` on the prototype
    matches v0.18.0 (7.55 vs 7.51 ms).
  - One quick round on WSLg (at 1180x760, before the size fix): v0.18.0 7.51 ms at
    25.7 fps, GPU 8.02 ms at 21.9 fps. Rasterizer gone from the profile
    (`open-task` 55 % -> 20 % of user time), replaced by driver time (`libgallium`,
    `libigd12um`, `libd3d12core`, 12 %), kernel time, memcpy, and an 8 ms wall wait
    in `glReadPixels`.
  - GL timer queries report 24-46 ms a frame on D3D12 and on llvmpipe alike:
    not believable as GPU busy time. Don't quote them.
- **Native-driver control, same Intel UHD, same kind of frame** (D3D11,
  `gfx::tests::chart_readback_timing` in the scratch worktree, 704x448, nine
  bands, three rounds, per frame):

  | | CPU, thread cycles | Wall |
  | --- | --- | --- |
  | CPU raster | 3.06-3.73 Mcycles | 1.9-2.4 ms |
  | GPU, no readback | 1.14-1.25 Mcycles | 0.8 ms |
  | GPU + staging copy, map, memcpy | 1.28-1.68 Mcycles | 1.9-2.1 ms |

  Reading back itself: 0.17-0.23 Mcycles of CPU; the rest of its 1.15 ms is the
  thread asleep on the GPU. So readback is not what is expensive; WSLg's D3D12
  across the VM boundary is. This is an iGPU, whose staging memory is ordinary RAM.
- sentinel's RTX 4090 was not used: it is shared and was 44 % busy with someone
  else's work, and a proprietary NVIDIA driver says nothing about Mesa on a Radeon.
- **Separate runs cannot be compared on this machine.** i7-10710U: 15 W, 1.1 GHz
  base, CPU and iGPU share the power budget. `qg-suite.sh`, same binary, same
  load, 1800x1000, mature History: 8.45 / 29.74 / 26.77 ms a frame, chart
  rasterizing 4.3 / 12.7 / 10.5 ms. `quiet()` only sees inside WSL; Windows-side
  load and turbo are invisible to it. So the suite's rounds 2 and 3 are not
  evidence, and more rounds would not help a bimodal spread. Fix: `OT_GL_FLIP`
  (prototype): one process alternates CPU and GPU every 10 s, with `getrusage` per
  interval, each GPU interval paired with the CPU intervals either side
  (`qg-flip.sh`, `qg-flip.py`).
- **Paired result, prototype** (`flip-h1`, `flip-h2`, `flip-l1`, `flip-l2`, WSLg,
  D3D12 on Intel UHD, 1800x1000, nine bands, CPU per frame, process total):

  | | CPU mode | GPU mode | GPU/CPU, median (IQR) | fps |
  | --- | --- | --- | --- | --- |
  | History full (> 300 s), run 1 | 10.11 ms | 7.58 ms | 0.735 (0.70-0.82) | 39.9 -> 33.0 |
  | History full (> 300 s), run 2 | 8.27 ms | 6.03 ms | 0.750 (0.71-0.77) | 42.5 -> 33.7 |
  | History young (< 90 s), runs 1, 2 | 15.84, 12.11 ms | 11.96, 9.06 ms | 0.75 | 38 -> 32 |
  | List, runs 1, 2 | 3.07, 2.10 ms | 3.88, 2.78 ms | 1.30 | 54 -> 46 |

  So the reversal from the one quick round was noise. Chart images are about half
  of a mature frame (4.2 of 8.3 ms). In GPU mode `glReadPixels` holds the UI
  thread ~9 ms on WSLg; `cut` 0.8, `upload` 0.35, `draw` 0.07 ms. Total process
  CPU falls too: 35 % -> 20 % of a core. The frame rate falls because of that wait.
  **By the decision rule above: build it.**
- **Paired result, the code as built** (`final-h1`, `final-h2`, a scratch copy of
  `e59ebc5` with only `OT_GL_FLIP` added; same conditions):

  | | CPU mode | GPU mode | GPU/CPU, median (IQR) | fps |
  | --- | --- | --- | --- | --- |
  | History full (> 300 s), run 1 | 11.63 ms | 9.33 ms | 0.762 (0.66-0.89) | 23.9 -> 23.2 |
  | History full (> 300 s), run 2 | 10.45 ms | 7.86 ms | 0.752 (0.72-0.79) | 24.0 -> 23.0 |

  Frame rate almost level here (24 vs 23 fps, chart step 6.3 vs 5.0 ms wall),
  against 42 -> 34 fps (10.8 vs 4.2 ms) in the prototype's session. Why those two
  sessions differ is not known: the rate is capped lower here, perhaps by the
  window being inactive (half rate), and the wait in `glReadPixels` was shorter.
  The honest statement: on WSLg the wait cost 0-20 % of the frame rate. Neither
  says what a Radeon does.
- **Paired result on a hidden display** (`xflip-h1`, `xflip-l1`: private Xvfb,
  D3D12, no focus, no pointer, nobody typing; `e59ebc5` + `OT_GL_FLIP`). The host
  was busy with other work, 85 % of 12 cores near the end:

  | | CPU mode | GPU mode | GPU/CPU, median (IQR) | fps |
  | --- | --- | --- | --- | --- |
  | History full (> 300 s) | 23.58 ms | 14.42 ms | 0.597 (0.56-0.63) | 19.2 -> 20.5 |
  | History 90-300 s | 24.26 ms | 14.96 ms | 0.636 (0.57-0.65) | 18.7 -> 20.4 |
  | List (no GPU work either way, charts 0.33 vs 0.36 ms) | 3.74 ms | 3.99 ms | 1.07 (1.06-1.13) | 27.9 -> 27.7 |

  A second History run was killed by the 30-minute limit after waiting 18 minutes
  for CPU slots, and was not repeated: pairing is what protects this figure. The
  CPU rasterizer suffers more on a busy host, so 0.60 flatters the GPU a little.
  **The range to quote: 25-40 % less CPU a frame on a full History.** List in
  this build spends the same on charts; the 7 % there is 4 pairs, and is a
  process that holds a GL context, which only someone who picked the GPU has.
- **Fallbacks, exercised on the real driver** (scratch build, `OT_GL_BREAK`, D3D12):
  in the trial: function missing, shader compile error, test drawing wrong
  ("255/255 off the rasterizer's"), `SIGSEGV` ("the trial failed: signal: 11"), a
  hang (killed at 40 s); in the app after a passed trial: start failing, context
  lost mid-run (12 of 12, frames going on at ~25 fps). Every time the app stayed up,
  logged why and drew on the CPU. Also `QT_XCB_GL_INTEGRATION=none`: "no OpenGL
  context", on the CPU. **`MESA_GL_VERSION_OVERRIDE=2.1` and
  `MESA_GLSL_VERSION_OVERRIDE=120` exercise nothing**: Mesa ran the shaders anyway.
- A default user has zero GL driver mappings in `/proc/<pid>/maps`, and so does
  someone who opens Settings (39 MB, the trial being another process); a saved `0`
  from v0.18.0 stays on the CPU; on llvmpipe the card shows CPU and QPainter only.
  Same-frame parity in the real binary: 4/255 on llvmpipe, 3 at scale 2 on llvmpipe,
  3 on D3D12. Picked, the app is at 156 MB.
- **A GL context costs about 100 MB.** RSS with nothing drawn with it: 39 -> 133 MB
  (llvmpipe), 39 -> 152 MB (D3D12). That sank the first design, which probed in
  the app on opening Settings: anyone who looked at that page would have paid it
  for the rest of the run. Hence the trial process, which also means a driver
  crash or hang cannot take a task manager with it, and a saved GPU cannot
  crash-loop at start. The cost stays for someone who picks the GPU (README).
- **A "stall" that was the user's space bar.** Runs on WSLg's desktop sometimes
  stopped painting for good within seconds (1 of 30, 0 of 60, 1 of 120, in CPU mode
  too: 2 of 75), process alive, 0 CPU, main thread idle in `QCoreApplication::exec`
  (`SIGUSR1` stack). Counters: wakes rising, `animating=0`, timer stopped, repaints
  frozen. `xdotool key space` on a private Xvfb gives exactly that: the app is
  paused. The test windows were taking the foreground while the user typed. With
  no input at all: **0 stalls in 240 starts** (120 GPU, 120 CPU, D3D12 on Xvfb).
  Not a bug. The runs of all-`fulls` frames were likewise the desktop's pointer or
  keyboard (on Xvfb, `ticks` as normal after both failures).
  - Wrong theories, for the record: a skipped drain (`with_shell` logs "host state
    busy", and it never did), a driver hang (the stack says no), minimized (the
    host does nothing with `OT_EVENT_VISIBILITY`).
  - Trap: `grep -c "frames frames"` on these logs counts 0, colour codes sit
    between the words; it reported 12 false stalls. Grep a field name
    (`paint_avg_us`). A Python string turns `\x1b` into a real ESC in a script.
- **A run that vanished** (`final-l1`, List, 11:49 AM PDT (18:49Z)) ended ~28 s in
  with no panic, no Qt message, no crash dump. Not a crash: `wsl-capture-crash`
  works here (a `SIGSEGV`ed `sleep` left a dump in `%TEMP%\wsl-crashes`) and that
  folder did not exist before. Seven reruns lived until killed (exit 143). Most
  likely a stray key in the same way, or its window closed. Not proven.
- GUI `--replay` is no use for a repeatable load: it opens a paused, scrubbed
  view, not a running chart.
- No `sudo` in WSL without a password, and `perf_event_paranoid` is 2: profiles
  are user-space only (`perf record -e cpu-clock:u`).
- Over a History chart, after its run in the display list: inline band labels,
  the scale, the crosshair, the floating legend (translucent backing), the chip.
  So a GPU layer cannot simply sit above the widget's pixels, and a child widget
  below them needs its own copy of what is under the chart.
- Qt's raster engine splits big blends over a thread pool (`Thread (pooled)`
  threads in `/proc`): part of a frame's CPU is not on the UI thread.

## Decision rule (written 2026-10-30 ~10:10 AM PDT, before the suite's numbers)

WSLg cannot prove or refute this for a native driver: its D3D12 layer adds costs
a native driver does not have, and the native control is D3D11 on an iGPU. So:

- **Build** if, from the suite's mature nine-band History frame, removing the
  chart rasterizer and adding the native control's readback overhead saves at
  least 15 % of the frame's CPU, and List can be kept at v0.18.0's cost (small
  runs stay on the CPU, so no GPU work there), and the default stays CPU.
- **Stop** with a measured "no" if the chart images are under about a fifth of the
  mature frame, so that 15 % is out of reach whatever the driver does.
- A GPU path that only wins on the young chart does not count: that frame lasts
  minutes, the mature one lasts as long as the app runs.

## Plan / steps

1. Frame budget of v0.18.0 on History. (done; the first figures were a young,
   two-band chart and, on WSLg, the wrong window size)
2. Prototype R, parity, native control. (done)
3. Paired A/B, decision rule: build. (done)
4. Built: `af34120` (shapes moved), `e59ebc5` (the feature), `644e23f` (the trial
   process). (done)
5. **Current: the user's call.** Not merged, pushed or released. See the open
   questions.

### The design as built

- Host owns geometry, shader and GL (Rust); the plugin gives only a current
  offscreen context and `getProcAddress`. `QOpenGLContext` and `QOffscreenSurface`
  are in QtGui, so no new link dependency, and they exist on Qt 6.2. Raw GL 3.3
  core (or ES 3.0): works on every supported Qt and needs nothing optional.
- Images go to the plugin as `OtRaster`, as the CPU's do, so partial repaints and
  the backing store are untouched. That is the answer to "how do GPU charts and
  partial repaints live together": they do not meet.
- `Shapes`, `Piece`, `Columns` move from `ot-shell-win` to `ot-paint`.
- **Keeping the default CPU needs a migration.** `settings_store::encode` always
  writes `ChartDrawing`, and `Settings::default()` is `Gpu`, so on Linux a saved `0`
  today means "never chose". Only adding `Gpu` to `OFFERED` would switch those
  users. Plan: `FilePrefs` reads `Gpu` as `Cpu` unless a new key says the user
  picked it.
- **The driver is tried in a process of its own** (`open-task --try-gl`, ABI
  `gl_trial`), started on first opening Settings, or at start for a saved GPU.
  Trial = context, renderer string (refuse software: llvmpipe was 48 ms a frame on
  Xvfb, unless forced), shaders, and a small test draw checked against
  `ChartRaster`. The app loads the driver only once the GPU is picked and proved.
  Any failure, then or later: log, CPU, `Capabilities` without Gpu.
- Small runs stay on the CPU, so List and the like never touch the GPU.

## Progress log

- [x] Baseline built from `15a308f`; a representative workload; harness fixed.
- [x] Prototype, parity, native control, paired A/B.
- [x] Decision rule applied: build.
- [x] Shapes in `ot-paint`; Windows tests and clippy.
- [x] ABI v4, plugin context, `chart_gl.rs`, `charts.rs`, migration, README.
- [x] Trial process, after the 100 MB finding.
- [x] Every failure path exercised on D3D12; smoke test, with a GPU step, passes.
- [x] fmt, tests and clippy on Windows and in WSL at `644e23f`.
- [x] Output checked against the CPU path: same-frame parity at scale 1 and 2, and
  by eye at both.
- [ ] Qt 6.2: the plugin's new code has only been built on 6.4.2. CI's `ubuntu:22.04`
  will be the first build there, and its smoke test the first run.
- [ ] A real Radeon; KDE Wayland; anything on native Mesa.

**Not verified anywhere here:** a Radeon, Mesa's native drivers, Wayland (the
offscreen surface is created differently there), `glReadPixels`'s wait on real
hardware, GPU load or power. GL timer queries here are not believable, so nothing
says how much GPU time this costs under Linux. All Linux GPU numbers are D3D12 on
an Intel UHD across a VM boundary. The Windows control suggests that layer makes
them pessimistic, and an iGPU is a best case for readback, so neither direction
is certain.

## Open questions for the user

1. Merge? Recommended: push the branch so CI proves Qt 6.2, and merge. It is
   opt-in, and the default path is untouched for everyone who does not pick it.
2. On the Radeon, with the build: `OT_CHARTS=cpu OT_FRAME_STATS=1 open-task
   --view history` and then `OT_CHARTS=gpu OT_FRAME_STATS=1 OT_CHARTS_CHECK=1 ...`,
   each past 5 minutes, watching `top` and `/sys/class/drm/card*/device/gpu_busy_percent`.
   That is the only number that can change the default.

## Things not to do

- **Don't run GUI tests on WSLg's desktop** (`DISPLAY=:0`). Use a private Xvfb, with
  D3D12 for the real GPU. A window in the foreground takes the user's typing, and
  a space bar pauses the app. Global rule: "Keep Off My Screen".
- Don't take a screenshot on WSLg (`import` hangs the app), `windowactivate`,
  or a Windows-side screenshot.
- Don't compare separate runs on this machine: pair within one process.
- Don't judge History at 60 s or on an idle machine: the chart is young and bare.
- Don't quote GL timer queries here.
- Don't load a GL driver in the app for anyone who has not picked the GPU.
- Don't compare runs without a fresh `XDG_CONFIG_HOME`, or by CPU % alone.
- Don't use `sed -i` on repo files: they are CRLF.
