# Issues

Problems noticed while working on something else, left for a thread of their own.

## Linux: a chart's image is drawn whole when only part of it changed

Seen 2026-10-30 (`plans/qt-gpu-charts.md`). Once History has run a few minutes,
`partial_drawn_pct` falls to about 25-30 % of the window at 1800x1000: the log
time axis leaves the older part of the plot still. But `Charts::prepare`
(`ot-shell-linux/src/charts.rs`) redraws a run's whole image whenever the damage
touches it, on the CPU and the GPU alike, and that image is about half of a
mature History frame's CPU (4.2 of 8.3 ms). Drawing, and reading back, only the
columns the damage covers should save much of it, for every Linux user, with no
GPU involved. `ChartRaster` has no clip to do that with today.

Matters because it is the largest cost left on the History view that is ours.

## The build label can name an older commit and miss `-dirty`

Seen 2026-10-12: `target\debug\open-task.exe`, built at 17:15 from `master` at
`db1435a` with uncommitted changes in `crates/ot-shell-win`, showed `45f39be`
(an earlier commit, no `-dirty`) in the sidebar's version line. An hour earlier
a build showed `a6166dc-dirty` correctly. In between, a second checkout (a
temporary linked worktree at `45f39be`, `git worktree add`) was built with its
own `CARGO_TARGET_DIR`, then removed. `crates/ot-app/build.rs` `watch()`
decides when the label is worked out again.

Matters because the label is how a screenshot or a bug report says which build
it came from.

## Linux: outside Settings, the UI still offers what the shell cannot do

Seen 2026-10-21 (v0.17.6 + `plans/settings-per-platform.md`). The Settings page
now hides cards for what the shell lacks (`ot_ui::Capabilities`), but elsewhere
on Linux:

- the rail's update button says "Check for updates", and a click fails with
  "updating is not supported on this platform yet" (`ot-update` has no Linux
  platform; `UpdateView` has no "unavailable" state);
- the crosshair beside "Run new task" (PickWindow), the process menu's Switch
  to, and Services' "Open Services" reach `HostEffect`s that answer "This is not
  available on Linux yet" (`ot-shell-linux/src/host.rs`, `host_effect`).

Matters because a button that only ever fails is worse than none.
`Capabilities` (`tray`, `switch_to`, `updates`, ...) is the place to say so; a
few more fields and the views asking them would hide these too.

## WSL: worktrees share one cargo build dir, and a green build can be another's code

Seen 2026-10-30 ~9:40 AM PDT (16:40Z), `linux-nvml` beside `qt-gpu-charts`. WSL's
`~/.cargo/config.toml` has `build-dir = "{cargo-cache-home}/build"`, with no
per-workspace part. Cargo's artifact names match across worktrees of one repo
(`ot_probe-d481073b63f97911` in both), and cdylibs have no hash at all. From
`linux-nvml`, `build-linux.sh --release` printed `Finished`, yet the binary held no
NVML code (`grep -c libnvidia-ml` gave 0) and its Qt plugin failed with `the plugin's
table is too short`. With `CARGO_BUILD_BUILD_DIR` set to a private directory the same
source was right. To see it: build two worktrees that differ, one after the other.

Matters because it gives a wrong binary with no error: a measurement or a smoke test
of the wrong code, in either worktree.

## Linux: when one embedded plugin is rejected, the next load gets the same plugin

Seen 2026-10-30, the same run: `Qt: the plugin's table is too short` was followed by
`GTK: the plugin's table is too short`, though that GTK plugin loads by itself. The
two load from `/proc/self/fd/<n>` with the same `n` (`ot-shell-linux/src/loader.rs`,
`open_bytes`), which looks like the same library to `dlopen`.

Matters only if a plugin is rejected after loading, so the fallback hides a real one.
