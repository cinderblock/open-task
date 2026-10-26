# Linux: open-task's own CPU use

## Goal

The user reported (2026-10-20), right after the Linux GPU probe landed
(`plans/linux-history-cpu-gpu.md`): open-task on Linux uses about 5 % CPU,
steadily. Bring it down without losing what it shows.

## Environment / context

- Probe: `crates/ot-probe/src/imp/linux/` (`process.rs` per process and thread,
  `gpu.rs` DRM clients). Headless mode prints `probe cost` per pass:
  `open-task --headless --passes N`.
- GUI frame cost: `OT_FRAME_STATS=1` logs frames and paint time every 5 s
  (`crates/ot-shell-linux/src/frame_stats.rs`).
- WSL scripts in `%TEMP%\ot-linux\` (scratch, not in the repo):
  - `load.sh`: 300 idle processes + 30 with 300 fds and 30 threads each, then
    headless `probe cost` (release build in `~/.cache/open-task-target`).
  - `gpucost.sh`: the same plus 15 processes whose fd counts churn, then the
    ignored `gpu_cost` test (fake GPU, real `/proc`).
  - `perf.sh` (run as root): `perf record --call-graph dwarf` of headless under
    load, `--children` report. Needs `cargo build --profile profiling`
    (`build-prof.sh`). perf is at
    `/usr/lib/linux-tools/6.18.54.1-microsoft-standard-WSL2/bin/perf`.
- Earlier numbers (`plans/linux-probe.md`, `plans/linux-gui-shells.md`): probe
  ~21 ms a pass for 532 processes and ~800 threads; GUI idle ~21 % of a core on
  Xvfb (software GSK), because the view animates at 60 frames a second.

## Decisions already made (don't re-ask)

- User's machine (2026-10-21): **KDE, so the Qt plugin; a Radeon 6000-series
  GPU** (amdgpu: the DRM fdinfo GPU probe and `gpu_busy_percent` apply). They
  have not run the on-machine commands yet.
- WSLg may be used for GPU-accelerated runs (windows on the user's desktop are
  fine). `ssh -X sentinel` is offered too; sentinel is Ubuntu 24.04, R9 7900X3D,
  RTX 4090 (ops `servers/README.md`): real hardware for headless probe numbers,
  but X forwarding draws without its GPU.
- User on Qt painting the whole window on the CPU: "this seems suboptimal... any
  acceleration options here?" -> look at acceleration for Qt.

- The user asked "how do we make it better?" (2026-10-20): measured cuts to the
  probe are in scope; anything that changes what the app shows or how it
  animates is a question for them.

## Findings / gotchas

- **Baseline** (WSL, 354 processes / 1270 threads, release, no GPU probe since
  WSL has no GPU): probe cost 26-39 ms a pass, ~30 ms typical = ~3 % of one
  core at the 1 s interval.
- **Profile before** (root, inclusive): listing `/proc/<pid>/fd` to count files
  ~25-30 % (`proc_readfd_common` -> `proc_fill_cache`, a dentry per fd); open +
  close of the small files ~26 %; per-thread `stat` ~6 % plus its opens;
  `status` ~6 % (`proc_pid_status`, ~55 formatted lines).
- **`stat()` of `/proc/<pid>/fd` gives the fd count as `st_size`** (Linux 6.2+),
  and works for other users' processes where listing is denied (checked in WSL:
  `/proc/1/fd` size 96 as non-root, `ls` denied). So Handles now shows for
  root's processes too.
- `statm`'s resident and resident - shared are `status`'s `VmRSS` and `RssAnon`
  (same mm counters); a test checks them against each other on a live process.
- **After the process-pass cuts:** probe cost 11-30 ms, ~16 ms typical, same load
  (about half).
- **Profile after:** what is left is mostly inherent: process `stat` (~13 %,
  `do_task_stat` sums every thread's times), path walks in `openat` (~10 %),
  `status` still ~5 % even at 1 pass in 5 (it is that costly), `statm` ~3 %.
  Possible next step: open `/proc/<pid>` once a pass and `openat` its files
  relative to it, to save the path walks (~5 %?). Not done.
- **GPU probe:** first pass lists every fd of every readable process (~48-88 ms
  for ~9000 fds). The original periodic relist every 10 passes hit all processes
  on the same pass, so that spike repeated every 10 s. Now: relist only on
  first sight, a GPU fd closing, or a changed fd count (at most every 3 passes),
  never for a process whose fds are denied. With 15 churning processes: mean
  2.3 ms, median 1.6 ms, worst 6 ms a pass.

- **GUI: chart drawing is about half the window's cost, and it is not the
  points.** WSL, Xvfb 1280x860, GTK 4.14.5 (software GSK), release, default
  Processes page (List, the CPU and Memory cards: two small charts, ~850 path
  points a frame), 60 fps, CPU of the process over 30 s after 60 s:
  - charts drawn: **21.6 %** of a core; host paint 55 us, snapshot build 475 us
    a frame, of which the Cairo path commands 54 us;
  - chart paths skipped (scratch `OT_SCRATCH_NOCHARTS`): **12.1 %**.
  So ~9.5 % of a core goes to the two charts' Cairo nodes *after* our code: GSK
  replays each Cairo node, rasterizes it on the CPU and uploads it as a new
  texture, every frame (a new node each frame, nothing to cache).
- **The data is already decimated twice.** Stored: 600 raw 1 s frames, then
  coarse steps (>= 10 s, at most 720). Drawn: rows averaged into time bins about
  one DIP wide (`sparkline::COLUMN_W = 1.0`), then the smoothed curve puts a point
  every ~2 DIPs (`SMOOTH_STEP`), at least one per bin. So an edge is ~1 point per
  1-2 DIPs whatever the history holds.
- **Fewer points changes nothing** (`gui-suite.sh`, two interleaved rounds,
  quiet WSL, CPU of the process; "quarter" = `COLUMN_W` 4 and `SMOOTH_STEP` 8,
  ~10 000 -> ~3 700 path points a frame on the History view):

  | Processes page | round 1 | round 2 |
  | --- | --- | --- |
  | History, as now | 152.5 % | 168.7 % |
  | History, quarter of the points | 155.7 % | 164.2 % |
  | History, chart paths skipped | 28.9 % | 36.5 % |
  | List, as now | 35.8 % | 24.2 % |
  | List, quarter of the points | 37.0 % | 25.3 % |

  On the History view the chart bands are ~85 % of the window's cost, and the
  cost is the area GTK rasterizes and composites each frame, not the points.
  (Software GSK on Xvfb puts compositing on the CPU too, so the absolute numbers
  are high; Cairo's rasterizing is CPU on any machine.) Our own path code fell
  from ~400 us to ~160 us a frame with a quarter of the points: real, but ~1 % of
  a core.
- **Our rasterizer as a texture barely helps on Xvfb** (`OT_SCRATCH_RASTER`,
  `ChartRaster` into a `gdk::MemoryTexture` per run): History 146 % vs 152-162 %
  with Cairo; List worse (30-40 % vs 22-33 %). The texture GSK then composites
  in software dominates. On equal terms (Cairo drawn into an image surface and
  handed over as the same kind of texture, `OT_SCRATCH_CAIROIMG`), per History
  frame: Cairo 1.9 ms (5.7 ms in a disturbed run), `ChartRaster` 1.3 ms (2.1
  ms disturbed). So about 1.5x faster: on a real GPU, where compositing is
  cheap, roughly 11 % -> 8 % of a core at 60 fps for the History's bands. Not the
  4x the Direct3D renderer gave on Windows.
- WSLg has `/dev/dxg` and Mesa's `d3d12_dri.so`, so GTK there could render on
  the host GPU, but its windows open on the user's desktop: not used without
  asking.
- **Windows met the same wall** (`plans/chart-renderers.md`): History at full
  history, 30 fps: Direct2D 21 %, our CPU rasterizer (`ot-paint::chart_raster`,
  SIMD) 24 %, the Direct3D shader renderer 5.9 %. Linux's Cairo path is the
  Direct2D-like one.
- Measuring in WSL collides with other projects' builds (DeskyPi agents ran
  `cargo test` there on 2026-10-21, load 7, host CPU 100 %): two runs were
  discarded. `%TEMP%\ot-linux\gui-suite.sh` waits for no `rustc` and a load
  under 1.5 before each run.
- Scratch instrumentation lives in a detached worktree,
  `%TEMP%\ot-scratch-wt` (env switches `OT_SCRATCH_NOCHARTS`,
  `OT_SCRATCH_COLUMN`, `OT_SCRATCH_SMOOTH`; path time and points in the
  `snapshots:` log line). Not for committing; remove with
  `git worktree remove --force` when done.

- **Qt repaints everything, every frame.** `Canvas::tick` calls `update()` on
  the whole widget at 60 fps and `Painter::draw` replays the whole list through
  `QPainter`'s raster engine: every text run and rectangle, not just the charts.
  The Linux host does not pass damage to plugins. Windows already has the
  answer: `DisplayList::damage_since` (`ot-paint/src/damage.rs`) and `gfx.rs`
  replaying only commands touching the damage onto the kept frame.
- **CPU chart drawing landed on Linux in another thread** (`c11cd91`,
  `plans/settings-per-platform.md`): the host rasterizes chart runs with
  `ot-paint`'s `ChartRaster` and hands Qt/GTK images. Qt History on Xvfb went
  from 31-39 % to 14-17 % of a core; List 14-19 %. Everything below about Qt is
  measured on top of it.
- The first damage A/B (suite6, on the older code, 2026-10-21) was ruined by
  another project's builds (the same variant read 31.5 % and 51.7 %):
  discarded.
- **WSLg GPU GL needs forcing:** by default Qt's `QOpenGLWidget` gets
  `llvmpipe` (software). `GALLIUM_DRIVER=d3d12
  LD_LIBRARY_PATH=/usr/lib/wsl/lib` gives `D3D12 (Intel(R) UHD Graphics)`;
  the first context takes ~15 s. Under Wayland (`QT_QPA_PLATFORM=wayland`) EGL
  fails to find the device and falls back to `llvmpipe`.
- **WSLg test windows:** `xdotool` clicks do not reach them, and `import` /
  `xwd` cannot read their pixels ("Resource temporarily unavailable"), so open
  History with `--view history` and check rendering on Xvfb instead. A Windows
  screenshot shows whatever the user has on top (it caught their calendar;
  deleted): don't. The app halves its frame rate when its window is not the
  active one, so compare WSLg runs per frame (`frames` log line), and don't
  `windowactivate` (it steals the user's focus).

## Plan / steps

1. Profile the probe pass under load. (done)
2. Cut the process pass: fd count by size, `statm` every pass and `status`
   every 5, idle multi-threaded processes keep their thread rows for up to 5
   passes. (done)
3. GPU probe relisting. (done)
4. Ask the user for an on-machine split: headless `probe cost`, and
   `OT_FRAME_STATS=1` with the window open. (asked; no answer yet)
5. User asked (2026-10-21): would decimating the stored data cut the drawing?
   Measured: no (above). (done)
6. Prototype: GTK draws runs of `Band`/`Graph` with `ot-paint::ChartRaster`
   into a texture instead of Cairo. Measured: ~1.5x faster rasterizing. (done)
7. Reported; user answered (KDE/Qt, Radeon 6000, WSLg OK). (done)
8. **Qt acceleration** (current):
   a. Measure today's Qt cost on Xvfb (raster `QPainter` is CPU-only, so Xvfb is
      representative): List and History views.
   b. Options, to measure before choosing: (1) damage-limited repaints (host
      passes `damage_since` rects; Qt repaints only those, replaying only the
      commands that touch them); (2) `QOpenGLWidget` canvas, `QPainter` on the
      GPU (fills by stencil, glyph cache; antialiasing needs MSAA); (3) a QRhi
      chart shader like Windows' Direct3D renderer (Qt 6.6+; 22.04 has 6.2).
   c. (current, 2026-10-25) Prototypes (1) and (2) rebuilt on `c11cd91` in the
      scratch worktree (`%TEMP%\ot-linux\proto3.py`; env `OT_SCRATCH_DAMAGE`,
      `OT_SCRATCH_GL`, `OT_SCRATCH_GL_SAMPLES`). `gui-suite7.sh` runs them on
      Xvfb (`x-*`) and on WSLg's GPU (`g-*`, `gui-measure-wslg.sh`), two rounds,
      into `%TEMP%\ot-linux\gui\suite7.txt`.
   d. Report numbers, recommend, build the chosen one properly.

## Progress log

- [x] Baseline probe cost under load in WSL.
- [x] Profile (before and after).
- [x] Process pass cuts; tests + clippy pass in WSL.
- [x] GPU probe relist limits; `gpu_cost` measurement test.
- [x] GUI baseline on Xvfb, and with charts skipped.
- [x] Fewer-points variants and History view: no gain.
- [x] ChartRaster-as-texture prototype measured (and Cairo-to-image on equal
  terms).
- [x] Qt prototypes ported onto `c11cd91` and built.
- [ ] suite7: Qt today vs damage vs OpenGL (running).
- [ ] On-machine numbers from the user.
- [ ] GUI idle cost, if it is the bigger part.

## Open questions for the user

0. Which desktop is it (KDE uses the Qt plugin, which draws everything with
   QPainter on the CPU; others GTK), and which GPU?
1. On your Linux machine, what do `open-task --headless --passes 10` (the
   `probe cost` column) and `OT_FRAME_STATS=1 open-task` (the log lines every
   5 s) print? And is the 5 % per core (top, htop) or of the whole machine?

## Things not to do

- Don't judge GUI cost from WSL/Xvfb: GSK there is software-rendered.
- Don't bring back a periodic full relist of every process's fds on one pass.
