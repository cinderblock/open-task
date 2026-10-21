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

## Plan / steps

1. Profile the probe pass under load. (done)
2. Cut the process pass: fd count by size, `statm` every pass and `status`
   every 5, idle multi-threaded processes keep their thread rows for up to 5
   passes. (done)
3. GPU probe relisting. (done)
4. **Ask the user for an on-machine split** (current): headless `probe cost`,
   and `OT_FRAME_STATS=1` with the window open, to see how much of the 5 % is the
   window drawing at 60 frames a second.
5. Depending on 4: the GUI's idle frame rate (design question for the user),
   or `openat` relative to the pid directory.

## Progress log

- [x] Baseline probe cost under load in WSL.
- [x] Profile (before and after).
- [x] Process pass cuts; tests + clippy pass in WSL.
- [x] GPU probe relist limits; `gpu_cost` measurement test.
- [ ] On-machine numbers from the user.
- [ ] GUI idle cost, if it is the bigger part.

## Open questions for the user

1. On your Linux machine, what do `open-task --headless --passes 10` (the
   `probe cost` column) and `OT_FRAME_STATS=1 open-task` (the log lines every
   5 s) print? And is the 5 % per core (top, htop) or of the whole machine?

## Things not to do

- Don't judge GUI cost from WSL/Xvfb: GSK there is software-rendered.
- Don't bring back a periodic full relist of every process's fds on one pass.
