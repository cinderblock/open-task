# History chart: a CPU percent reading, and memory by program

## Goal

The user asked (2026-10-11):

1. The Processes page's History chart gets a third reading that matches the
   CPU % chart at the top of the page: the same 0–100 % picture, cut up by
   program — "a detail version of the default chart at the top".
2. A breakdown of memory by program, like the CPU one, in **%** and in
   **bytes**.

## Environment / context

- History chart: `crates/ot-ui/src/usage_chart.rs` (`UsageChart`, `ChartMode`,
  switch in the head row, bands = 8 programs + "Everything else").
- Its data: `crates/ot-core/src/usage.rs` `Usage` — per-interval frames of
  `(ProgramId, cycles)`, raw for 600 samples then folded into coarse steps
  (≥10 s, ≤720 steps). Only consumer of `Usage::frames()` is `usage_chart.rs`.
- Top CPU chart: `snap.cpu.total` (time-based %, whole machine, 0–100 fixed
  scale). Top memory chart: `snap.memory.in_use()` = total − available.
- Per process: `cpu_time` (cumulative, user+kernel), `cpu` = % of one core,
  `private_bytes` = private commit (`PrivatePageCount`; the table's **Memory**
  column), `working_set` = the **full** working set (`WorkingSetSize`; the table's
  **Working set** column) - its doc comment wrongly said "private".
- The kernel's `SystemProcessInformation` also has `WorkingSetPrivateSize`
  (`crates/ot-probe/src/imp/windows/nt.rs`), unused so far.
- Recordings encode every `ProcessSample` field by name
  (`crates/ot-record/src/delta.rs`, postcard, `FORMAT_VERSION = 1` in
  `format.rs`): a new field is a format change.

## Decisions already made (don't re-ask)

- Readings are grouped by resource in the head row: `[CPU | Memory]`, then the
  resource's readings — CPU: Fading total · Rate · Percent; Memory: Percent ·
  Bytes. Default stays CPU / Fading total (unchanged behaviour). Each resource
  remembers its last reading.
- CPU percent is from `cpu_time` differences (time-based like the top chart), as a
  share of the whole machine (span × logical cores). Fixed 0–100 % scale so it
  reads as the top chart.
- Memory stacks each program's **private working set** (user's pick, 2026-10-11,
  over `private_bytes`): `ProcessSample::private_working_set`, from the kernel's
  `WorkingSetPrivateSize`, recorded in format 2 with format 1 files still read.
  Percent = share of installed RAM, fixed 0–100 % scale; Bytes auto-scales in
  binary units (50 GB, not 46.6).
- A remainder band, **"Not in a process"**, tops up the stack to the top chart's
  value (CPU: total % − the programs' sum, e.g. interrupts/DPCs; memory: in use −
  the private working sets, i.e. kernel, drivers, shared and mapped pages),
  clamped at zero. Own neutral color (`Theme::series_rest`). Absent in the cycle
  readings.
- Each resource keeps its own band assignment, so colors stay put per resource
  and switching back does not reshuffle.
- History stores every quantity as an integral over the stretch (cycles, CPU ms,
  byte·ms, capacity ms), so folding raw frames into steps is plain summing and
  averages are exact. Memory gets its own per-frame list (every program holding
  memory), separate from the busy list, to keep the cost down.

## Plan / steps

1. [x] `ot-core` `Usage`: per frame, busy lists (cycles, CPU ms), memory list
       (byte·ms), and machine integrals (CPU capacity ms, busy ms, RAM byte·ms,
       in-use byte·ms). Sorted lists merged linearly into steps. Tests.
2. [x] `ot-ui` `UsageChart`: resource switch + 5 readings, remainder band, fixed
       scales, value formatting, per-resource slots, captions. Tests.
3. [x] `private_working_set` end to end: model field, Windows probe, recorder
       (format 2 = format 1 body + `Extra`; format 1 still read), `Usage` uses it,
       `--replay-info` prints the file's format.
4. [x] README (History bullet, Recording paragraph), screenshots: CPU Fading total
       (default), CPU Percent, Memory Percent (dark), Memory Bytes (light, dark).
5. [x] clippy, fmt, tests; commits.

## Findings / gotchas

- First live screenshot (Memory, Percent): the stack reached ~50 % while the
  Memory chart read 43 % in use, and "Not in a process" never showed. Cause: the
  chart stacked `working_set`, which is the *full* working set - shared pages
  (DLLs, mapped files) counted once per process - so the programs' sum overshoots
  physical memory in use. Its doc comment wrongly said "private" (fixed in
  `97194d4`). With the private working set the stack meets the Memory chart
  exactly (43 %, 27.5 GB) and "Not in a process" is ~14 GB on this machine.
- The GUI's first two samples are ~41 ms apart. CPU time is charged a clock tick
  (~16 ms) at a time, so that stretch read System at 203 ms of CPU in 41 ms and
  the Percent reading spiked to ~100 % at the left edge (cycles are exact, so the
  cycle readings did not). Stretches under 0.25 s (`SHORT_S`) now join the next
  one, time-weighted, in the Percent reading only.
- postcard writes a struct as its fields in a row and ignores trailing bytes, so
  appending a second struct after the v1 `Body` keeps v1 bodies decodable as-is;
  `ProcessSample::private_working_set` is `#[serde(skip)]` so v1 `Row::Full`
  layouts are unchanged.
- Heredocs with nested triple quotes broke the Bash tool's parsing; edit scripts
  went in `%TEMP%\ot-edit\*.py` instead.

## Progress log

- [x] Data layer (CPU ms, memory, machine integrals; tests)
- [x] Chart (CPU/Memory switch, Percent readings, remainder band; tests)
- [x] Short first stretch no longer spikes the CPU Percent reading (test)
- [x] Private working set end to end, recording format 2 (`6829263`)
- [x] Binary-unit scale for Bytes; README; screenshots
- [x] Checks + commits

## Open questions for the user

None.

## Things not to do

- `sed -i` in Git Bash writes LF; files are CRLF (see memory). Edit with the Edit
  tool or Python with `newline=''`.
- Don't stack `working_set` anywhere: it double-counts shared pages.
