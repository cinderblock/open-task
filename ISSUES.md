# Issues

Problems noticed while working on something else, left for a thread of their own.

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
