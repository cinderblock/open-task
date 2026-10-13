# History chart: GPU by program

## Goal

The user asked (2026-10-12): the History chart recently gained memory by program
(see `plans/history-cpu-percent-and-memory.md`); do similar breakdowns for the GPU.

## Environment / context

- History chart: `crates/ot-ui/src/usage_chart.rs` (`ChartMode`, `Resource`,
  `RESOURCES`, `modes()`, `fill_rows`, remainder band `REST` = "Not in a process").
- Its data: `crates/ot-core/src/usage.rs` `Usage` — per-frame sorted lists
  `(ProgramId, f32)` (cycles, cpu_ms, memory) plus `Machine` integrals; folding
  into coarse steps is a sum.
- GPU probe: `crates/ot-probe/src/imp/windows/gpu.rs` (`GpuProbe`). Reads PDH
  `\GPU Engine(*)\Utilization Percentage` (instances
  `pid_N_luid_H_L_phys_0_eng_E_engtype_T`) and `\GPU Adapter Memory(*)\{Dedicated,Shared} Usage`.
  Already sums per (pid, luid, engine type) in `by_process`, but only publishes each
  process's *own* busiest engine: `ProcessSample::gpu` (Percent) and
  `gpu_engine` (`GPU 0 - 3D`).
- `GpuSample` per adapter: `utilization` (= busiest engine), `engines`,
  `dedicated_used`, `shared_used`. Performance page charts utilization and
  dedicated memory per non-software adapter (`crates/ot-ui/src/perf.rs`).
- Not read yet: `\GPU Process Memory(*)\{Dedicated,Shared,Local,Non Local} Usage`,
  `Total Committed` (instances `pid_N_luid_H_L_phys_0`).
- This machine: Intel UHD (integrated, LUID 0x12fcf) + Fresco Logic IDDCX
  (indirect display, 0x146f0, no memory use). Dedicated usage is 0 everywhere (iGPU).
- A new `ProcessSample` field is a recording format change (format 2 added
  `private_working_set` as a trailing `Extra`; same pattern would give format 3).

## Findings / gotchas

- **Per-process shared GPU memory double-counts.** Live, 2026-10-12: sum of
  `GPU Process Memory(*)\Shared Usage` = 2875 MB, while
  `GPU Adapter Memory(*)\Shared Usage` for the same adapter = 1924 MB. Allocations
  mapped into several processes are counted in each. Stacking it would overshoot
  the adapter like `working_set` overshot RAM. Dedicated could not be checked here
  (no discrete GPU).
- Engine utilization *does* add up within one engine: the probe derives each
  engine's load as the sum of the processes' instances. So a stack of the
  programs' shares on one engine is exact.
- GPU utilization is the busiest engine (Task Manager's definition); engines do
  not add, so programs' "own busiest engine" values do not stack to it.
- This machine's counters name two adapters: Intel UHD (0x12fcf, hardware) and
  0x146f0 = Microsoft Basic Render Driver (`software = true`). The Fresco Logic
  IDDCX display adapter isn't in the counters. So "combined" = one GPU here.

- An all-zero reading first drew its axis as "1 B" / "0 B" (`nice(0)` = 1, half
  of it rounds to 0 B). Byte scales now floor at 1 MiB and the plot says why it's
  empty.
- The GPU Percent reading showed open-task itself at ~50 % of the iGPU's 3D engine
  on the History view (5–7 % on List), confirmed with raw PDH. Pre-existing, not
  from this work: logged in `ISSUES.md`, then fixed in the GPU chart renderer
  (`db1435a`, `c8ec759`; `plans/history-gpu-cost.md`): now ~10 %.
- The Bash tool can't take Python heredocs with triple quotes; edit scripts live in
  `%TEMP%\ot-edit\` (`edit.py` helper keeps CRLF). Run them without `-I`, or the
  helper import fails.

## Decisions already made (don't re-ask)

User's picks, 2026-10-12:

- A third resource, **GPU**, beside CPU and Memory, with three readings:
  **Percent** (utilization), **Dedicated** (bytes), **Shared** (bytes).
- Percent: a program's band is its share of the engine that is busiest on the
  adapter in that pass (so the stack equals the Performance page's GPU chart).
  "Not in a process" tops up to the adapters' utilization.
- Multiple GPUs combine like CPU cores: capacity = span × number of hardware
  adapters (software adapters, e.g. Microsoft Basic Render Driver, excluded as on
  the Performance page); fixed 0–100 % scale.
- Dedicated: per-process `GPU Process Memory\Dedicated Usage`, summed over
  hardware adapters, auto-scaled binary bytes, "Not in a process" tops up to the
  adapters' dedicated in use (clamped at zero).
- Shared: per-process `Shared Usage`, bytes only, **no** remainder band, caption
  warns that memory several programs share counts in each.

Implementation choices (mine):

- New `ProcessSample` fields `gpu_share: Percent`, `gpu_dedicated: Bytes`,
  `gpu_shared: Bytes`, `#[serde(skip)]`, recorded in format 3 as a second trailing
  struct after format 2's `Extra` (postcard can't grow `Extra` in place).
- `GpuProbe` keeps one `ProcessGpu` per pid (busiest engine + the three above)
  instead of the `busiest` map.

## Plan / steps

1. [x] Decide readings / multi-GPU / engine definition with the user.
2. [x] Probe: publish what the readings need per process; recording format 3.
3. [x] `Usage`: GPU lists per frame + machine integrals (lists became an array
       indexed by `CYCLES`..`GPU_SHARED`).
4. [x] `UsageChart`: GPU resource and readings, per-resource memory for three
       resources (`left_on`, `parked` arrays), captions, empty-reading note, tests.
5. [x] README, live screenshots, checks, commits.

## Progress log

- [x] Survey code and live counters (findings above)
- [x] Probe `ProcessGpu` + `GPU Process Memory` counters; live test checks the
      shares add up to the hardware adapters' utilization. Cost unchanged
      (2.3 ms mean, 3.9 ms worst).
- [x] Recording format 3 (`GpuExtra` after `Extra`); format 2 files read zero.
- [x] `Usage` + chart + tests; README.
- [x] Live check (2026-10-12): GPU Percent stack ~95 %, matching raw PDH (3D
      engine 84–95 %); Shared ~2.7 GB stacked (dwm, chrome, msedgewebview2...);
      Dedicated empty on the iGPU, now with a note and a 1 MB scale floor.

## Open questions for the user

None.

## Things not to do

- Don't stack per-process shared GPU memory against the adapter's total (overcounts).
- `sed -i` in Git Bash writes LF; files are CRLF.
