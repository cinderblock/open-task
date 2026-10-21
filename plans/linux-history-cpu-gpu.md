# Linux: History chart CPU and GPU readings

## Goal

The user reported (2026-10-20): on Linux the History chart's CPU and GPU
readings don't work; Memory does. Make both work.

## Diagnosis

- **CPU.** The CPU readings "Total" (the default) and "Rate" chart each
  process's `ProcessSample::cycles`. Windows fills it from
  `SYSTEM_PROCESS_INFORMATION.CycleTime`; the Linux probe
  (`crates/ot-probe/src/imp/linux/process.rs`) left it at 0, so those readings,
  the Cycles column and the Usage Map were empty. CPU "Percent" uses `cpu_time`
  and did work.
- **GPU.** The Linux probe had no GPU support (`plans/linux-probe.md`, "Not
  done"). `out.gpus` was empty and every process's `gpu*` fields were zero.

## Environment / context

- Usage folding: `crates/ot-core/src/usage.rs` (`Usage::observe`, `record`).
- History chart: `crates/ot-ui/src/usage_chart.rs` (`ChartMode`, `fill_rows`).
- `crates/ot-ui/src/view.rs` `cycles_core` already treats cycles as counted at
  `Hardware::base_frequency`.
- Linux probe: `crates/ot-probe/src/imp/linux/` (`mod.rs` wires the passes).
- Windows GPU probe for reference: `crates/ot-probe/src/imp/windows/gpu.rs`,
  `plans/history-gpu.md` (definitions of share/dedicated/shared).
- Build/test in WSL: see `plans/linux-probe.md` (Linux-native `CARGO_TARGET_DIR`,
  scripts run from a file). WSL has no DRM GPU, so the GPU probe is unit-tested
  against fake `/proc` and `/sys` trees; real hardware needs the user.

## Decisions already made (don't re-ask)

User's picks, 2026-10-20:

- **CPU: cycles = CPU time × the processor's rated clock.** Windows' cycle
  counter runs at the TSC rate (the rated clock), so the units match Windows;
  only the resolution is coarser (`USER_HZ` ticks, 10 ms).
- **GPU: build the Linux GPU probe now**, from DRM client usage stats
  (`/proc/<pid>/fdinfo`, kernel 5.19+: i915, xe, amdgpu, and most open drivers)
  plus `/sys/class/drm`. NVIDIA proprietary (NVML) is a later step.

## Plan / steps

1. **CPU** (done)
   - `cpu.rs`: one rated clock, from `cpufreq/base_frequency`, else the
     `@ X.XXGHz` in the model name, else `cpuinfo_max_freq`, else `cpu MHz`.
     It is `Hardware::base_frequency`, and the rate cycles are counted at.
   - `process.rs`: `cycles = ticks × clock / USER_HZ`.
2. **GPU probe** (`imp/linux/gpu.rs`) (done, unverified on real hardware)
   - Adapters: devices behind `/sys/class/drm/card<N>` and `renderD<N>` (not
     `cardN-<connector>`), grouped by canonical device path, ordered by card
     number. Name from the PCI ID database (bracketed marketing name after the
     vendor's short name), PCI location, amdgpu `mem_info_{vram,gtt}_{total,used}`
     and `gpu_busy_percent`. Driver version: `/sys/module/<drv>/version`, else
     the kernel release.
   - Clients: each process's fds whose link target is `/dev/dri/<node>`, mapped
     to an adapter by node name; `fdinfo` read for those only. Deduped by
     `(adapter, drm-client-id)`, else by `(process, fd)`.
   - Engine share: `drm-engine-<e>` delta ns / wall; or `drm-cycles-<e>` over
     `drm-total-cycles-<e>` (Xe) or over `drm-maxfreq-<e>` x wall; divided by
     `drm-engine-capacity-<e>`. Counters that step back are held.
   - Memory: per region resident, else `drm-memory-` (amdgpu's old name), else
     total. `vram*`/`local*` = dedicated; `gtt`, `system*`, `memory` = shared;
     `cpu` and `stolen-*` neither.
   - Utilization = max(busiest engine sum, `gpu_busy_percent`). Per process:
     `gpu`/`gpu_engine` (busiest engine, `GPU 1 - 3D`), `gpu_share` (share of
     each hardware adapter's busiest engine), `gpu_dedicated`, `gpu_shared`.
   - Software adapters (driver in `SOFTWARE_DRIVERS`: simpledrm, faux-bus
     vgem/vkms, evdi, ...) listed with `software = true`; `nvidia` left out.
     `GpuProbe::new` is `None` without a hardware adapter (WSL).
   - Wired in `mod.rs` after the process pass; capabilities `gpu` and
     `per_process_gpu`.
3. Docs: README Linux section and cycles paragraph, `plans/linux-probe.md`. (done)
4. **Verify on a real Linux GPU** (needs the user). (current)
5. Later: NVIDIA via NVML (`dlopen` `libnvidia-ml.so.1`,
   `nvmlDeviceGetProcessUtilization`, `nvmlDeviceGetComputeRunningProcesses` /
   `GraphicsRunningProcesses`).

## Findings / gotchas

- Windows' process cycle counter runs at the TSC rate, which is the rated clock,
  and `crates/ot-ui/src/view.rs` `cycles_core` already assumed that clock, so
  "CPU time x rated clock" lines up with every place that reads cycles.
- WSL has `/sys/class/drm/card0` + `renderD128`, but they are `vgem` on the faux
  bus (`device/driver` -> `faux_driver`), not a GPU. Software-only, so no GPU
  probe there; the probe is tested against fake `/proc` and `/sys` trees.
- DRM fdinfo spec (docs.kernel.org/gpu/drm-usage-stats.html): `drm-client-id`
  may be per device (then `drm-pdev` is given), so clients are keyed by adapter
  as well. `drm-memory-` is amdgpu-only, an alias of `drm-resident-`.
- Not root: other users' fds cannot be read, so their GPU work is missing from
  the engine sums (only amdgpu's `gpu_busy_percent` sees it, as "Not in a
  process").
- A process's fds are relisted when its fd count changes, when a cached fd stops
  being a DRM client, and every 10 passes. The cost on a real desktop has not
  been measured yet.
- Python heredocs with triple quotes break the Bash tool; edit scripts live in
  `%TEMP%\ot-linux\`. WSL test runs: `%TEMP%\ot-linux\{probe-test,clippy,ws}.sh`
  via `wsl.exe -d Ubuntu bash /mnt/c/Users/camer/AppData/Local/Temp/ot-linux/<x>.sh`.

## Progress log

- [x] CPU cycles on Linux: rated clock in `cpu.rs`, `ticks_to_cycles` in
  `process.rs`; `ot-probe` tests + clippy pass in WSL (`e27cdd8`)
- [x] Linux GPU probe, unit-tested against fake trees; workspace clippy + tests
  pass in WSL
- [x] Docs (README, linux-probe plan)
- [ ] Run on a real Linux machine with an Intel or AMD GPU: Performance page GPU
  graph, History > GPU, the GPU columns; measure the probe's cost a pass
- [ ] NVIDIA (NVML), if wanted

## Open questions for the user

1. Which Linux machine and GPU did you see this on? The GPU side needs a run
   there to confirm (WSL has no GPU). If it is NVIDIA, the proprietary driver
   needs the NVML step.

## Things not to do

- Don't key clients on `drm-client-id` alone: it may be unique only per device.
- Don't treat `faux_driver`/`simple-framebuffer` devices as GPUs.
