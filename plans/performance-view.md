# Performance view, and page navigation

> **Status:** active · **Started:** 2026-09-28 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md` (step 6: "then Performance, then the
> rest of the 12"). The user said "keep going" on 2026-09-28 after step 6 was
> summarized for them.

## Goal

The second of TMOG's twelve views, and the navigation that the other ten will hang
off. A Performance page in the style Task Manager made familiar: a list of devices
(CPU, Memory, each disk, each network adapter) with a live mini chart each, and a
detail pane for the selected one with a large chart and the numbers that matter.
Everything on the log-scale time axis (architecture decision 8), with one hover
line shared across every chart on the page.

## Environment / context

- Other threads share this working tree:
  - `plans/charts-log-time-and-live-table.md` built the log time axis, the
    multi-resolution `Series`, and the shared hover (`06779d7`, `2ccdd44`, both
    **unpushed**). It is waiting on the user about table hysteresis and a treemap,
    and will edit `view.rs` and `table.rs` if the user answers. Keep edits to
    `view.rs` small; put new pages in their own modules.
  - `plans/replace-task-manager.md` (untracked, dormant since 2026-09-26) plans an
    Options menu and single instance. Not ours; never stage it.
- Because two unpushed commits from another thread sit under anything committed
  here, **do not push** at the end of this cycle unless that thread has pushed
  first. Pushing would publish its commits and take away its option to amend them.
- The tool session runs elevated; the app normally does not. Everything in the
  Performance view must work unelevated.
- Machine here: 12 logical processors, homogeneous (no P/E split), 64 GB RAM.

## Decisions already made (don't re-ask)

1. **A left navigation rail**, the Windows 11 Task Manager / `NavigationView`
   pattern, rather than tabs: there will be twelve pages. Icons plus labels when
   the window is at least 1008 DIP wide (NavigationView's default threshold),
   icons only below that; a hamburger button flips it. Pages that are not built yet
   are not shown (no dead entries).
2. **Icons are semantic.** `ot-paint` gains an `Icon` enum and a draw command; the
   Direct2D backend maps each to a glyph of Segoe Fluent Icons (Windows 11) with
   Segoe MDL2 Assets as the fallback (Windows 10). Other platforms will map the same
   enum to their own icon sets. `ot-ui` never names a codepoint.
3. **Keyboard:** Ctrl+Tab / Ctrl+Shift+Tab cycle pages (classic Task Manager);
   Ctrl+1..9 jump to a page.
4. **Performance page = device list + detail pane**, as Task Manager does. It
   scales to many disks, adapters and later GPUs, where a single dashboard does not.
   Up/Down move through the device list.
5. **One chart group per page**, generalized from the two-chart `Charts` the
   summary cards use into `charts.rs`, so every chart on a page shares one hover.
   The Processes page keeps its two summary cards.
6. **CPU detail**: overall utilization chart, or one small chart per logical
   processor (a toggle, like Task Manager's "Change graph to"), colored by core
   kind on hybrid parts. Numbers: utilization, speed, processes, threads, handles,
   up time; base speed, sockets, cores, logical processors, L1/L2/L3 cache, and the
   processor's name.
7. **Memory detail**: in-use chart; a composition bar (in use, modified, standby,
   free); in use, available, committed / limit, cached, paged and non-paged pool.
8. **Counters come from PDH, English names** (`PdhAddEnglishCounterW`), in one
   query collected per pass on the sampler thread: processor performance (for the
   current clock), the memory lists, pools, physical disks, network interfaces.
   Works unelevated, which `SystemMemoryListInformation` does not. If PDH is
   unavailable (a machine with corrupt counters), those numbers are simply absent.
9. **Static hardware facts** (CPU name, base speed, sockets, cores, caches, boot
   time) are read once and shared by `Arc` in every snapshot, like process statics.

## Plan / steps

1. ~~Read the code and the other threads' plans; write this plan.~~
2. ~~`ot-paint` `Icon`; backend glyph mapping, verified by rendering the candidate
   glyphs.~~
3. ~~`ot-model` `Hardware`; Windows probe reads it once. Snapshot carries it.~~
4. ~~`ot-ui`: `charts.rs` (N-chart group), `nav.rs` (rail), `perf.rs` (Performance
   page: device list, CPU and memory detail), `App` page switching. Tests.~~
5. ~~Shell: Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+1..9. Screenshots. Commit.~~
6. ~~Probe: PDH counters (clock, memory lists). Model and page use them. Commit.
   (Pools already come from `GetPerformanceInfo`.)~~
7. **[current]** Probe: disks and network adapters from PDH; model, timeline series,
   device list entries and detail panes. Commit.
8. README, parent plan, this plan; verify on three targets; commit.

## Findings / gotchas

- **Icon codepoints, checked by rendering** a grid of candidates from Segoe Fluent
  Icons with `System.Drawing` (`target/glyphs.ps1`, output `target/glyphs.png`):
  `E700` hamburger, `E71D` checklist (Processes), `E9D9` pulse in a box
  (Performance). Also seen and useful later: `E950` chip, `E964` memory module,
  `EDA2` drive, `E839` wired network, `E701` Wi-Fi, `E716` people, `E90F` wrench,
  `E713` gear, `E7E8` power, `E81C` history.
- **Base speed reads 1.61 GHz on this i7-10710U, whose name says "@ 1.10GHz".**
  `CallNtPowerInformation(ProcessorInformation).MaxMhz` is 1608, and WMI's
  `Win32_Processor.MaxClockSpeed` agrees (1608). The laptop's firmware runs the chip
  at its configurable-TDP-up base. Showing 1.61 GHz matches what Windows reports;
  the name string is Intel's nominal figure.
- **Machine facts here:** 1 socket, 6 cores, 12 logical, L1 384 KB, L2 1.5 MB,
  L3 12 MB (sums over every cache entry from `RelationAll`, matching WMI's L2/L3).
- **The first pass has no rates.** The sampler's first snapshot has
  `interval == 0` and every CPU figure at 0%, so each chart started with a cliff
  from zero into the first real sample. `Timeline::observe` now skips a snapshot
  whose interval is zero (it still records the tick).
- **`{:.0}` rounds half to even** (62.5 prints as 62). The memory percentage in the
  device list rounds explicitly.
- `ot-probe/Cargo.toml` had duplicate `Win32_System_Diagnostics_Etw` and
  `Win32_System_Time` entries in HEAD (another thread's); removed while adding
  `Win32_System_Power`.
- **PDH works unelevated and is cheap.** One query with `\Processor
  Information(*)\% Processor Performance` and five `\Memory` list counters, collected
  once per pass: steady-state probe cost stayed at 5 to 6 ms a pass here (first
  pass about 21 ms, as before). Checked against `Get-Counter` a moment apart:
  modified 151 MB vs 153 MB, free 157 MB vs 165 MB; standby 37.6 GB plus free equals
  `GlobalMemoryStatusEx` available (37.8 GB). The clock read 3.5 to 3.8 GHz on a
  1.61 GHz base, which is this chip boosting.
- **`PDH_FMT_NOCAP100` is missing from the `windows` crate metadata** (0x8000).
  Without it `% Processor Performance` is clamped at 100 and a boosting core would
  read as its base clock. Declared by hand in `counters.rs`.
- **The probe already had a private `Counters` type** (per-process CPU and I/O
  tallies); the PDH wrapper is `PerfCounters`.
- **"In use" versus the composition bar.** `MemorySample::in_use` is
  `total - available`, which includes the modified list (modified pages are not
  available). The bar splits them out, so its "In use" segment is `in_use - modified`.
  Left that way on purpose: `In use + Available = Total` holds for the stats, which
  lets a reader check them, and the chart and summary card keep one meaning.
- **Posted input drives the GUI for screenshots without touching the user's
  desktop:** `target/drive-perf.ps1` posts `WM_KEYDOWN` Down to pick Memory, and a
  `WM_LBUTTONDOWN`/`UP` pair at the "Logical processors" segment (client pixels at
  96 DPI, from the layout constants) to switch the CPU graph. Screenshots
  `target/perf-cpu.png`, `perf-memory.png`, `perf-cores.png`, `perf-procs.png`
  (never committed: they show the user's processes).

## Progress log

- [x] Plan written.
- [x] Icons (`ot-paint::Icon`, Direct2D glyph mapping).
- [x] Hardware facts (`ot-model::hardware`, `ot-probe` Windows `hardware.rs`).
- [x] Chart group (`charts.rs`), nav rail (`nav.rs`), Performance page (`perf.rs`)
      with CPU (overall and per logical processor) and memory; pools in
      `MemorySample`; first-pass fix in `Timeline`.
- [x] Keyboard navigation; `--page`; screenshots; verified (fmt, clippy on three
      targets, 108 tests); committed.
- [x] PDH: clock and memory lists (`counters.rs`); speed in the list headline and
      stats; four-part composition bar; headless prints them. 111 tests; clippy on
      three targets; screenshots.
- [ ] Disks and network adapters.
- [ ] README and plans; verified; committed.

## Open questions for the user

None yet.

## Things not to do

- Do not push while another thread's commits are unpushed under ours.
- Do not stage `plans/replace-task-manager.md`.
- Do not show navigation entries for pages that do not exist yet.
