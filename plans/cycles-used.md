# Cycles used: a fading total of what each process consumed, and a chart of it

> **Status:** built and committed on `master`, not pushed, not released · **Started:** 2026-10-01 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/usage-map.md` (the Map and the usage strip, v0.4.0).

## Goal

The user, 2026-10-01: instead of a "right now" view, show "cycles used": not percent
usage but clock cycles consumed, as a proxy for total compute. Their analogy: Joules
rather than Watts. To stop the totals growing without bound, every second all
accumulated values lose a share ("idk, 5%", configurable). Then recent heavy
processes bubble to the top, long-running medium ones stay visible, and short bursts
show briefly. Also a chart: an area accumulating over time, one color per process,
where a process's usage can be seen going up and down.

## Environment / context

- Accounting: `ot-core/src/usage.rs` (`Usage`): per-process fading totals and the
  per-program history. Replaced the Map's sixty-second window outright.
- Probe: `ProcessSample::cycles` from `SYSTEM_PROCESS_INFORMATION.CycleTime`
  (`ot-probe/src/imp/windows/mod.rs`); zero on the stub platforms.
- UI: the Cycles column (`ot-ui/src/process_rows.rs`, `col::CYCLES`, the default
  sort), the Map and strip (`usage_map.rs`), the History (`usage_chart.rs`,
  `ViewMode::History`), the fade card (`settings.rs`), wiring in `view.rs`.
- Setting: `Settings::usage_decay_percent`, stored as `UsageDecayPercent`
  (`REG_DWORD`) under `HKCU\Software\open-task` (`ot-shell-win/src/prefs.rs`).
- Colors: `Theme::series` (eight, in a fixed order) and `series_other`, from the
  `dataviz` skill's reference palette, checked with its
  `scripts/validate_palette.js` against this app's surfaces (`#202020` dark,
  `#f3f3f3` light): every hard gate passes in both; in light four colors are under
  3:1 against the surface, which the always-present legend with names relieves.
- Other threads work in this tree (`89e4f66` landed mid-task): commit only our
  paths with `git commit --only`, format only our files with
  `rustfmt --edition 2021`, and keep the files CRLF (`.gitattributes` says
  `eol=crlf`; the Write tool and Python write LF unless told otherwise).

## Decisions already made (don't re-ask)

1. **The measure is real clock cycles**: Windows' per-process `CycleTime`, already
   in the structure the probe reads, so it costs nothing. The user asked for "clock
   cycles consumed"; it is also exact where kernel + user time is charged a tick
   (15.6 ms) at a time.
2. **Fading is by time, not by sample**, and cycles count as spent evenly across
   the interval they arrived in: `total = total * (1-d)^dt + used * (1 - (1-d)^dt)
   / (-ln(1-d) * dt)`. Tested: sampling every 250 ms and every 4 s agree.
3. **The rate is a setting**, 5 % a second by default, in the steps 1, 2, 3, 5, 7,
   10, 15, 20, 30, 50 (halving in 69 s down to 1 s); the Settings card gives the
   half-life. A stored value that is not a step reads as the nearest.
4. **PID 0 (idle) is left out.**
5. **Chart: both readings, as a switch** (user: "both, as options?"): *Fading
   total* (the default, the number the column shows) and *Rate* (cycles a second).
6. **The chart is a fourth arrangement**, List / Tree / Map / History, Ctrl+H,
   `--view history` (user picked it).
7. **Cycles is a new column and the default sort; CPU % stays; the Map and the
   strip show the fading totals** instead of the last minute (user picked it).
8. **The chart's bands are programs, not processes**: every process of one name
   together. Eight colors can be told apart, not four hundred; a build's two
   hundred `rustc.exe` are worth one band, not two hundred slivers; and it bounds
   the history's memory by the programs that ran, not the processes that existed.
   Mine to decide, not asked; a per-process reading would be a follow-up.
9. **Eight bands and "Everything else".** Bands go to the programs with the most
   area on the chart. A program keeps its band (and color) until a program without
   one is clearly bigger (its size times 0.8 still above the smallest holder's), or
   it falls under 0.25 % of the chart. Bands stack in band order, so nothing
   reorders; "Everything else" is on top.
10. **The chart is on the app's shared log time axis** (an hour), and shares its
    hover with the CPU and memory graphs both ways.
11. **A click on a band selects the program's busiest running process**, so the
    selection, the ancestry line and the context menu work as in the Map.
12. **An exited process fades as a ghost** in the Map and strip until its total is
    under 1 M cycles or 0.01 % of everything, whichever is more.

## Findings / gotchas

- **A fading total is an exponential moving average.** At a steady rate `r` it
  settles at `r × τ`, `τ = -1 / ln(1 - d)`: 19.5 s at 5 %. So the number is "what
  it used recently", and ranking by it is ranking by smoothed CPU. The Rate
  reading is the unsmoothed one; the Fading total is the same sum run over the
  history, which is how the chart draws it (`fade` in `ot-core`), so the chart's
  newest column equals the table's column (tested).
- **A cycle is time, not work.** Measured here (i7-10710U, base 1.61 GHz, running
  at 2.9 GHz): every process's cycles divided by its CPU time is 1.6 G a second.
  `CycleTime` counts the invariant TSC, which ticks at the base clock whatever the
  core's speed. So it does not weigh a boosted or a P-core second above another;
  it is finer CPU time. Said in the README. It also gives the Cycles column's
  heat: full tint is one core busy throughout, `base_frequency × τ`.
- **Not seen:** cycles between a process's last sample and its exit, and a process
  that lives and dies between two samples. Only tracing (ETW) would catch those.
- **The history's cost**: per interval, only the programs that used cycles, as
  `(program, f32)`; 600 raw frames, then 360 ten-second steps fed from what leaves
  the raw part (so no stretch is stored twice, unlike `Series`' parallel tiers).
  Around 50 busy programs makes it roughly 400 KB.
- **Band takeover, first try, was wrong**: "keep a holder within 80 % of the
  eighth-biggest" kept a small holder while the biggest program on the chart sat in
  "Everything else". Now a newcomer displaces the smallest holder when clearly
  bigger. Test: `programs_past_the_eighth_are_everything_else_and_colors_stay_put`.
- **The existing view tests assumed a CPU sort.** They now build their app with
  `by_cpu()`; the default sort has its own test. `painted_names` also picks up the
  strip's labels now that the strip has something in it under those snapshots.
- **Clippy pedantic** (CI runs `-D warnings`): a fourth `bool` on `App` trips
  `struct_excessive_bools`, hence the `InPlace` enum instead of `map_on` and
  `history_on` fields.
- `scripts/screenshot.ps1 -AppArgs "--view history" -SettleMs 45000` gives the
  chart time to fill; `-Click "348,224"` switches it to Rate.

## Plan / steps

1. ~~Questions to the user.~~
2. ~~Model and probe: `ProcessSample::cycles`.~~
3. ~~`ot-core::Usage`: fading totals, ghosts, the per-program history. Tests.~~
4. ~~Cycles column (default sort, tree rollup, heat); Map and strip on the totals.~~
5. ~~The History chart: both readings, bands, legend, hover, click. Tests.~~
6. ~~Settings: the fade card with its stepper; registry value.~~
7. ~~README; checks; screenshots; commit.~~
8. **Next, the user's call:** push and release (see open questions).

## Progress log

- [x] Read the existing accounting, charts, settings and probe; found `CycleTime`.
- [x] Questions 1 to 3 answered by the user (2026-10-01).
- [x] Probe and model; `--headless` prints each process's CPU time and cycles.
- [x] `ot-core::Usage` rewritten; 11 tests.
- [x] Cycles column, Map, strip; the Map's and the view's tests moved to cycles.
- [x] History chart (`usage_chart.rs`, 7 tests) and its view test.
- [x] Fade card in Settings; `UsageDecayPercent` in the registry.
- [x] Checked in the app (`target/shot-history.png`, `shot-rate.png`,
      `shot-list.png`, `shot-settings.png`, `shot-click.png`).
- [x] `cargo fmt --all --check`; clippy `-D warnings` on x64 Windows, Linux and
      macOS; `cargo test --workspace` passes.
- [x] README.
- [ ] Pushed. Released. (Waiting on the user.)

## Open questions for the user

1. **Push and release?** Committed on `master`, not pushed. It is a feature, so
   v0.6.0 by the repo's habit. Recommendation: use it for a bit first, the default
   sort changing is the kind of thing to feel before shipping.
2. **Is "program" (processes of one name as one band) right for the History?**
   Decision 8 was mine. The alternative is one band per process.
3. **Should the Cycles column, or the chart, also be offered in core-seconds?**
   "68 G" is what was asked for; "42 s of one core" is the same number divided by
   the base clock, and may read more easily. Not built.

## Things not to do

- Do not fade per sample: the interval is adjustable.
- Do not call the number "work done": cycles here are time (see findings).
- Do not give a ninth program a generated color; it belongs in "Everything else".
- Do not reorder the stack by size: bands would swap places as you watch.
- Do not stage other threads' files.
