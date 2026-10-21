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
2. **GPU probe** (`imp/linux/gpu.rs`) (current)
   - Adapters: `/sys/class/drm/card*` (not `cardN-<connector>`), device
     `vendor`/`device`, driver name, PCI slot, VRAM totals and use where the
     driver publishes them (amdgpu `mem_info_vram_*`).
   - Per process: walk `/proc/<pid>/fdinfo/*` for fds whose `drm-pdev` names an
     adapter; dedupe by `drm-client-id`; `drm-engine-<name>` ns (cumulative) and
     `drm-cycles-`/`drm-total-cycles-` (xe); memory `drm-resident-vram`/
     `drm-memory-vram` (dedicated) and `drm-resident-system`/`gtt` (shared).
   - Engine load = sum over clients of Δns / Δwall; utilization = busiest.
   - Fill `gpu`, `gpu_engine`, `gpu_share`, `gpu_dedicated`, `gpu_shared`.
3. Capabilities, README, `plans/linux-probe.md` "Not done" list.

## Findings / gotchas

- (none yet)

## Progress log

- [x] CPU cycles on Linux: rated clock in `cpu.rs`, `ticks_to_cycles` in
  `process.rs`; `ot-probe` tests + clippy pass in WSL
- [ ] Linux GPU probe
- [ ] Docs (README, linux-probe plan)

## Open questions for the user

1. What Linux machine/GPU did you see this on? The GPU probe needs a real-GPU
   run to verify (WSL has none).

## Things not to do

- Don't read DRM fdinfo through `/proc/<pid>/fd` symlink targets alone: the
  fdinfo `drm-pdev`/`drm-client-id` keys are what identify the adapter and
  dedupe clients shared between fds.
