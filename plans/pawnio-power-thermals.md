# Package power and CPU temperature through PawnIO

> **Status:** implemented, awaiting review · **Started:** 2026-10-03 · **Branch:** `worktree-agent-a74633c83e2fe0d0c` (worktree of `open-task`) · **Parent plan:** `plans/v0.8-icons-summary-recorder.md` step 4

## Goal

Show the CPU package's power draw and temperature on the Performance page and in
the headless output, read through PawnIO when it is installed, with no driver of
our own and no change in behaviour when it is absent.

## Environment / context

- PawnIO 2.2.0 installed at `C:\Program Files\PawnIO` (`PawnIOLib.dll`,
  `PawnIOLib.h`); service `PawnIO` running. No `HKLM\SOFTWARE\PawnIO` key; the
  Uninstall entry `HKLM\...\Uninstall\PawnIO` has `InstallLocation`.
- This machine: Intel i7-10710U (Comet Lake, family 6 model 166), 6 cores, 12
  threads. `IA32_PACKAGE_THERM_STATUS` is readable; TjMax 100 °C.
- Modules: PawnIO.Modules release 0.2.11 (2026-08-30), `IntelMSR.bin` and
  `AMDFamily17.bin`, under `crates/ot-probe/pawnio/` with `README.md` and `COPYING`.

## Decisions already made (don't re-ask)

1. PawnIO, never a driver of our own (parent plan, decision 3).
2. `PawnIOLib.dll` loaded at run time by full path from the install directory;
   never linked, never loaded by bare name.
3. No CPU pinning: the registers are package-scoped; the assumption is documented
   in the module header and implausible rates (> 5 kW) are dropped.
4. AMD reports Tctl, labelled "Tctl"; the per-model Tdie offsets are not applied.
5. The sensor kind travels in `Hardware::thermal_sensor` (`ThermalSensor` in
   `ot-model`), so the UI and the headless printer can name the reading.
6. The Performance list's CPU line gains the temperature only; power is on the
   pane (the line is 212 px wide and already carries the clock).

## Findings / gotchas

- `pawnio_execute` runs the module on the calling thread's current processor.
- The AMD module's `ioctl_read_smn` asks callers to hold `\BaseNamedObjects\
  Access_PCI`; the probe opens `Global\Access_PCI`, waits up to 5 ms and skips the
  temperature for the pass when it cannot get it.
- `HANDLE` and `HMODULE` in `windows` 0.62 are raw-pointer newtypes without
  `Send`; `PawnIo` carries its own `unsafe impl Send` like `OwnedHandle` does.
- `.gitattributes` has `* text=auto eol=crlf`; the blobs are marked `binary`
  explicitly because the driver verifies their signature over the exact bytes.
- Not tested on AMD: no Zen machine at hand. The pure arithmetic (`amd_tctl`,
  unit, wrap) has unit tests; the SMN path ran only against the module source.
- Measured here (dev profile, `pawnio_cost`): one `sample` is 0.026 ms mean,
  1.2 ms worst over 100 calls. Live readings while compiling: 40-44 W, 88-92 °C
  (`capabilities` reports `package_power: true, thermals: true`; headless prints
  `power 40.8 W  temp 88 °C`).
- The shared `target/` mixed in other worktrees' builds twice (a stale `ot-model`
  without `ThermalSensor`, a stale `ot-paint` with an `Icon::Summary` variant):
  `touch crates/*/src/lib.rs` before every cargo command and check the
  `Compiling ot-… (<this worktree>)` lines.
- `FreeLibrary` is in `windows::Win32::Foundation`, not `LibraryLoader`, in 0.62.
- My first energy-unit test fixture was wrong (`0x000A_0E03 | 16 << 8` has ESU 30):
  the real Core value is `0x000A_1003`.

## Progress log

- [x] Blobs, licence and attribution README under `crates/ot-probe/pawnio/`.
- [x] `ThermalSensor` in `ot-model`; `Hardware::thermal_sensor`.
- [x] `pawnio.rs`: library loading, vendor detection, module load, energy rate
      with wrap, Intel package/core temperature, AMD Tctl under the PCI lock,
      unit tests, live test, `pawnio_cost`.
- [x] `WindowsProbe`: field, capabilities, per-pass sampling.
- [x] UI: Power and Temperature/Tctl stats, list line temperature, `format::celsius`,
      test.
- [x] Headless `power … W  temp … °C` suffix.
- [x] README.
- [ ] Verified on an AMD Zen machine.

## Things not to do

- Do not load `PawnIOLib.dll` by bare name (DLL search order).
- Do not read MSRs outside the modules' allow-lists; the module returns
  `STATUS_ACCESS_DENIED` and the probe treats it as "no reading".
- Do not modify the `.bin` blobs or let git normalise them.
