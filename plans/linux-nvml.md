# Linux: NVIDIA GPUs through NVML

## Goal

On Linux, NVIDIA GPUs should show what amdgpu and Intel GPUs already show: total
utilization, memory and per-process GPU use, so the Performance page and the
History view's GPU chart work. Until now they were left out on purpose
(`UNREAD_DRIVERS = ["nvidia"]` in `crates/ot-probe/src/imp/linux/gpu.rs`).

## Environment / context

- Branch `linux-nvml`, worktree `C:\Users\camer\.t3\worktrees\open-task\linux-nvml`,
  base `15a308f` (v0.18.0 plus a plan note).
- WSL builds: from PowerShell, `wsl.exe -e bash <script>`, with
  `CARGO_TARGET_DIR=~/.cache/open-task-target-nvml` (a parallel thread uses the
  main target dir and takes CPU measurements in WSL: keep compiles to what is
  needed).
- **Also set `CARGO_BUILD_BUILD_DIR=~/.cache/open-task-target-nvml/build-private`:**
  WSL's cargo config has one `build-dir` for every worktree, and it handed this
  worktree another's artifacts (see Findings).
- Heavy work goes through `node ~/.claude/bin/cpu-slots.mjs run --slots N`. Claiming
  4 of 4 next to a long A/B suite that holds 2 waited 30 minutes without starting:
  claim 2 with `CARGO_BUILD_JOBS=2`, in the background with a long timeout.
- `sed -i` and `cargo update` write LF; the repo is CRLF. Normalize with Python.
  The Bash tool cannot take Python heredocs with triple quotes: scripts go in
  `%TEMP%\ot-nvml\`.
- `sentinel` (`ssh sentinel`): read-only checks and a test binary in a temp
  directory, removed afterwards. Nothing installed, no settings changed. It runs
  production GPU work (`stitchd`, `whisper-server`, `ffmpeg`), so measuring must
  not hammer it. The read-only NVML scripts `%TEMP%\ot-nvml\probe{1,2,3}.py` are
  piped in over ssh (`ssh sentinel 'python3 -' < probe1.py`), so nothing lands on
  its disk.

## Decisions already made (don't re-ask)

- Load `libnvidia-ml.so.1` at run time; no new NEEDED entry. Its absence means no
  NVIDIA GPU here, quietly.
- Don't merge, push or release until the user asks. (They asked: released as
  v0.19.0, 2026-10-30.)

Mine, on the evidence below:

- **A thin hand-written binding**, no crate. The workspace already does this for
  libsystemd (`sessions.rs`) and the toolkit plugins, and every call here has to
  fail on its own, which a copyable table of optional function pointers gives
  directly: an unsupported call is set to `None` for that device.
- **Only load NVML when the kernel says there is an NVIDIA GPU:**
  `/sys/bus/pci/drivers/nvidia/<slot>`. The library costs ~20 MB of resident
  memory, so a machine without the driver bound pays nothing, and nouveau is not
  in that directory.
- **A GPU is its PCI slot.** A DRM device whose driver is `nvidia` and an NVML
  device are the same adapter when the slot matches, so nothing is counted twice,
  and an NVIDIA GPU with no `nvidia-drm` nodes (headless compute) still shows.
- **Inline in the pass, not on a thread:** ~3.9 ms flat, no periodic part.
  Windows' GPU probe costs 2.3 ms. A worker thread stays the lever if a real run
  shows otherwise.
- **`nvmlDeviceGetProcessUtilization`, not `...ProcessesUtilizationInfo`:** the
  newer call only adds JPEG and OFA per process (both 0 on sentinel, under a heavy
  video pipeline), needs the older one as a fallback anyway (535+ only), and one
  path means the path measured on sentinel is the path every user runs. JPEG and
  OFA still show for the whole device.
- **The 3D/Compute engine is one engine,** named `3D/Compute`: NVML's SM reading
  cannot split them, and two engines would break "busiest engine".
- **Let a sleeping laptop GPU sleep** (see Findings): never touch NVML while
  `power/runtime_status` says `suspended`, and on a GPU with runtime D3 enabled,
  shut NVML down when the GPU is idle so it can go to sleep.

## Plan / steps

1. Research: fdinfo, NVML semantics, cost on sentinel. (done)
2. Binding, a fake layer, wiring into `GpuProbe`. (done)
3. Tests, clippy, smoke test, run on sentinel. (done)
4. README, plans, commit. (done)
5. Merged, pushed, released as v0.19.0 by tag (`99e782a`, 2026-10-30 3:34 PM PDT
   (22:34Z)): CI green on the commit before tagging, all 8 release jobs green with
   the workflow's own GUI smoke test, 11 assets. The published x86_64 binary, its
   checksum matching: `libnvidia-ml` in it, and on sentinel it reads the RTX 4090
   beside the Radeon (61 %, 7.6G/24.0G, `stitchd` on Video Encode; `nvidia-smi` 61 %,
   7784 MiB) at a probe cost of 19.7 ms mean, 21.9 ms worst over 10 passes. (done)
6. **Wait for a laptop** (current): the runtime D3 path has only run against fakes.

## Findings / gotchas

- **sentinel is not what `ops/servers/README.md` says:** Ubuntu 26.04 (not 24.04),
  kernel 7.0.0-38, NVIDIA **open** kernel modules 595.91.07, persistence mode off,
  and a second GPU: the 7900X3D's iGPU on `amdgpu` (`card1`, `renderD129`). The
  RTX 4090 is `card0`, `renderD128`, driver `nvidia`, `0000:01:00.0`. So it tests
  the mixed case too. `servers/sentinel/README.md` already had the OS right; the
  table in `servers/README.md` did not. Both edited in the ops checkout at the
  user's request (2026-10-30). Not committed: ops needs a yes for each commit.
- **fdinfo has nothing for NVIDIA.** Open modules, on sentinel: `/dev/dri/card0`
  and `/dev/nvidia0` both show only `pos`, `flags`, `mnt_id`, `ino`. In the
  source, `nv_drm_fops` (`kernel-open/nvidia-drm/nvidia-drm-drv.c`) sets no
  `show_fdinfo`, and `drm-engine` appears nowhere in the tree. The proprietary
  modules build the same nvidia-drm source. Compute processes hold `/dev/nvidia0`,
  not `/dev/dri`, so fdinfo could not have found them anyway.
- **NVML sees other users' processes without root.** As `cameron` on sentinel:
  `stitchd` and `whisper-server` (uid 0, in containers, host pids) and a uid 1001
  python are all in the running list with their memory, and `stitchd` in the
  per-process utilization. Better than fdinfo, which needs root for them.
- **Per-process semantics (measured).** The driver keeps a ring of 72 samples,
  one every 200 ms (14.4 s). Each names one process per engine: "the process that
  was active on the engine when the sample was taken" (`ctrl2080perf.h`).
  `nvmlDeviceGetProcessUtilization` with `lastSeenTimeStamp` returns one row per
  process with something non-zero since then, averaged over the whole window, so
  **the rows sum to the engine's load**: over 15 s at a 1 s pace, SM 41-44 against
  a whole-device 41-45, encoder 60-61 against 61-64, decoder 49-50 against 49-57.
  So it means what Windows' per-process engine shares mean.
  - The sum can exceed the whole-device reading by a little (44 against 41), so an
    engine's load is the larger of the two.
  - A row's timestamp is that process's latest sample, not the window's end, and
    other processes' rows can be older: `last` must be the newest seen.
  - Rows can be bogus per nvtop's bug #110 (over 100, stale timestamps): kept to
    0-100 and filtered by timestamp. Not seen on sentinel.
  - `NVML_ERROR_NOT_FOUND` is "nothing since then", not a failure.
- **`ProcessUtilization` wants room for 72 entries, whatever it returns:**
  capacity 8, 32, 64 give `INSUFFICIENT_SIZE` with count=72; 100 and up succeed
  with 1-4 rows. A failed size query costs 3 us; a success 1.5 ms.
- **The whole-device reading is a short snapshot;** `nvmlDeviceGetSamples` gives
  the interval (five 200 ms samples at a 1 s pace, 1.5 ms). On sentinel's steady
  load they agree within 2 points, but a snapshot aliases bursty work at 1 Hz, and
  `GpuSample::utilization` is defined over the interval, so SM uses the samples
  and falls back on the snapshot. Encoder and decoder use the snapshot (2 us each)
  topped up by the rows: five `GetSamples` would be 7.5 ms a pass.
- **Cost on sentinel** (60 rounds at 10 Hz, 7 processes):

  | Call | Median | Max |
  | --- | --- | --- |
  | `GetUtilizationRates` | 0.020 ms | 2.8 ms (first call) |
  | `GetMemoryInfo_v2` | 0.012 ms | 0.020 ms |
  | Encoder, Decoder, Jpg, Ofa | 0.003 ms each | 0.005 ms |
  | `GetComputeRunningProcesses_v3` | 0.43 ms | 0.57 ms |
  | `GetGraphicsRunningProcesses_v3` (empty) | 0.11 ms | 0.19 ms |
  | `GetMPSComputeRunningProcesses_v3` (empty) | 0.07 ms | 0.16 ms |
  | `GetProcessUtilization` | 1.55 ms | 1.85 ms (one 6.8 ms later) |
  | `GetSamples`, one type | 1.5 ms | 1.87 ms |

  `nvmlInit_v2` 7.4 ms, handle 6.0 ms, both once. Nothing periodic.
- **Memory:** `nvmlDeviceGetMemoryInfo_v2`'s `used` (8161460224) is what
  `nvidia-smi` prints and is within ~48 MiB of the processes' sum (7736 MiB). v1's
  `used` adds the 481 MiB reserved for the driver and firmware. A pid in two
  running lists has the same figure in each ("all of the memory used by the
  application"), so it is not added.
- **`nvmlInit_v2` is not lazy.** The header says it allocates no device state; on
  sentinel it opens `/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm` and a caps
  node. `nvmlShutdown` closes all of them, and init + handle again is ~13 ms. Init
  is reference-counted, so each attached device can hold one.
- **NVML leaves one helper thread** (`cuda00001c00008`), blocked in `poll`: no
  wakeups and no CPU over 10 s, and it survives `nvmlShutdown`.
- **A 1 Hz poller keeps a laptop's dGPU awake.** Reports: `nvidia-smi --loop=1`
  held a GPU at ~10 W on PRIME on-demand; nvtop likewise. In the driver
  (`src/nvidia/arch/nvalloc/unix/src/dynamic-power.c`, `kernel-open/nvidia/nv.c`):
  runtime D3 waits for 5 s of idleness (`GC6_PRECONDITION_CHECK_TIME`), then a
  5 s hold-off between cycles; in coarse-grained mode an open device file alone
  holds the GPU up ("this unref will do nothing which will cause the GPU to remain
  powered up"). Runtime D3 is on by default for Ampere+ notebooks, off on desktops.
  `/sys/bus/pci/devices/<slot>/power/runtime_status` is the kernel's own state and
  wakes nothing. `/proc/driver/nvidia/gpus/<slot>/power` takes RM locks but no
  power reference; it reads `Runtime D3 status: Enabled (fine-grained)`,
  `Disabled by default` (sentinel), `?` until RM is initialized.

- **The shared cargo `build-dir` gave a green build of the wrong code.** WSL's
  `~/.cargo/config.toml` sets `build-dir = "{cargo-cache-home}/build"`, with no
  per-workspace part, and the `qt-gpu-charts` worktree builds there too. From
  this worktree, `build-linux.sh --release` said `Finished`, and the result:
  - `release/open-task` had **no** `libnvidia-ml` in it (`grep -c` 0), so it ran on
    sentinel as v0.18.0 does: only the Radeon, 6.4 MB, 2 threads. That A/B's
    "branch" rows were baseline against baseline and are thrown away.
  - the Qt plugin failed `the plugin's table is too short` in the smoke test, with
    the same result run alone, so not load. Both worktrees' `ot-shell-abi` are
    identical, so it is a plugin of some other build, not an ABI change. The "GTK:
    too short" beside it was an echo: the second `dlopen` of `/proc/self/fd/N`
    matched the first by name and returned the Qt plugin, though GTK itself loads.
  Cargo's hashes for path crates are relative to the workspace, which is the likely
  way two worktrees meet in one directory; not proven. The first release build here
  (9:29 AM PDT, 16:29Z) did have NVML. From a private `build-dir` the same source
  passes the smoke test with both toolkits, and `grep -c libnvidia-ml` is 1. The
  test binary's name is `ot_probe-d481073b63f97911` in both directories, so two
  worktrees really do share artifact names. Logged in `ISSUES.md`. Check a binary
  before measuring it: `grep -c libnvidia-ml`.
- **The real A/B on sentinel** (2026-10-30 10:50 AM PDT (17:50Z), 30 passes twice
  each, 454-478 processes, not root, binary checked to hold NVML):

  | | v0.18.0 | This branch |
  | --- | --- | --- |
  | Probe cost, mean | 15.6, 15.8 ms | 19.7, 19.9 ms |
  | Probe cost, worst pass | 17.9, 18.6 ms | 27.7, 21.5 ms |
  | CPU of the process | 1.64, 1.60 % of a core | 2.00, 2.04 % |
  | Resident memory | 6.5, 6.6 MB | 28.1, 28.1 MB |
  | Threads | 2 | 3 |

  So +4.1 ms a pass, +0.4 % of a core, +21.6 MB. `real_gpu_cost` (all of
  `GpuProbe::sample`, the Radeon's fdinfo pass included), three runs: mean 3.8-3.9 ms,
  median 3.8 ms, worst 4.3-6.7 ms, first pass ~21 ms (connecting, first listing of
  fds). No periodic spike; the 27.7 ms is one pass in one run, not explained.
- **Readings against `nvidia-smi`, same minute:** `Video Encode 61 %` (61),
  `Video Decode 49-55 %` (47-59), `3D/Compute 42 %` (41-44), `7.6G/24.0G` (7784 MiB).
  `stitchd` 60-61 % encode, against pmon's 60-61. Over 30 passes, shares went to
  `stitchd` (every pass), `ball-counter` (15), and both `ffmpeg`s (decode, 13).
- **The GPU reads higher than `nvidia-smi`'s headline:** 61 % against 43 %, because
  utilization is the busiest engine, here the encoder, as on Windows and in Task
  Manager; `nvidia-smi`'s is the 3D/Compute engine. Intended (`ot-model/src/gpu.rs`).
- **The window, with real data:** recorded 60 passes on sentinel (`--record`, 5.9K a
  frame), replayed on Xvfb under Qt. Performance > GPU 0: `NVIDIA GeForce RTX 4090`,
  62 %, 7.6 / 24.0 GB, Video Encode 62 %, Video Decode 51 %, 3D/Compute 41 %, driver
  `nvidia 595.91.07`, `PCI bus 1, device 0, function 0`; GPU 1 is the Radeon. History
  > GPU > Percent: `stitchd` under a thin "Not in a process"; Dedicated: `stitchd`,
  `whisper-server`, `ffmpeg`, the pythons. A recording replays with a seek, but
  the History chart only fills from frames played.
- **Not verified, for want of the hardware:**
  - The whole runtime D3 path (`connect`, letting go, `suspended`): fake trees and a
    fake library only. Sentinel is a desktop: `Disabled by default`. Whether shutting
    NVML down is enough for a real laptop GPU to sleep, and what waking costs, is
    read from the driver source and forum reports, not seen.
  - A process with a graphics context, so a pid in both lists: sentinel's lists are
    compute only. The graphics list's cost with many clients is unmeasured (~45 us
    an entry, going by the compute list).
  - An idle GPU (`NOT_FOUND`, skipping the rows, `Reading::idle`): sentinel is never
    idle.
  - Bursty load: that the samples beat the snapshot is reasoned, not measured.
    Dropping them would save 1.5 ms a pass.
  - Connecting to a GPU with no clients and persistence off, which the driver has
    to initialize: reputedly seconds. It happens on the sampler thread, in the first
    pass, not before the window.
  - The proprietary kernel modules, older drivers (the v1 and v2 process entries, no
    `GetSamples`), MIG, several NVIDIA GPUs at once, running as root, and NVML in a
    container. WSL with a Windows NVIDIA GPU has no `/sys/bus/pci/drivers/nvidia`,
    so it reads as no GPU here.

## Progress log

- [x] Research: fdinfo, semantics, cost, power.
- [x] `nvml.rs` binding and fake: fake function table, 11 tests.
- [x] `gpu.rs` wiring, 12 tests with a fake library and fake trees.
- [x] Headless prints each GPU and who uses it (it printed none).
- [x] README, `linux-probe.md`, `linux-history-cpu-gpu.md`.
- [x] Windows: clippy clean, tests pass.
- [x] Sentinel, first run: real GPU, numbers below.
- [x] WSL, private build dir: clippy clean, workspace tests pass, smoke test passes
  on debug and release, NEEDED unchanged (libgcc_s, libm, libc).
- [x] Sentinel A/B against v0.18.0; all 74 probe tests pass there with the real
  library. Nothing left behind: temp directories removed, no process running.
- [x] Real data through the window: Performance and History, by replay.
- [x] Merged, pushed, released as v0.19.0; the published binary checked on sentinel.
- [x] Ops docs edited, not committed.
- [ ] A real laptop: the runtime D3 path.

## Open questions for the user

1. A laptop with an NVIDIA GPU, if there is one to try: does the GPU reach
   `suspended` with open-task running? (`cat /sys/bus/pci/devices/*/power/runtime_status`)
2. `ot-probe` could drop `GetSamples` for 1.5 ms a pass. Recommendation: keep it,
   unless bursty load turns out not to need it.
3. Commit the two ops doc lines? Recommendation: yes.
4. Should `qt-gpu-charts` recheck its A/B numbers, given the shared build dir?
   Recommendation: yes, with `grep` for a string only its build holds, since this
   worktree's binary held none of its own code without any error. The build dir
   itself is being fixed in its own thread (`wsl-cargo-build-dir`).

## Things not to do

- Don't change the ops repo or anything on sentinel.
- Don't call process utilization with a buffer under 72 entries.
- Don't call any NVML function, `nvmlInit` included, while the GPU is `suspended`.
- Don't add a pid's memory across the compute, graphics and MPS lists.
- Don't build in WSL without a private `build-dir`, and don't trust `Finished`.
- Don't claim all 4 slots beside a long holder of 2: it never starts.
