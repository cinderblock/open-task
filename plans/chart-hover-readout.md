# A steadier chart hover readout

> **Status:** active · **Started:** 2026-09-29 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/charts-log-time-and-live-table.md` (decision 4: the hover readout).

## Goal

The user's feedback on 2026-09-29 about the readout under a hovered chart:

1. "As I move my cursor, the shown text changes. Sometimes I see the peak,
   sometimes I don't. I don't even understand why there are two numbers. Seems
   redundant."
2. "Put the time [at] a fixed relative position too, so that its position doesn't
   jump around as the text lengths change."
3. "Ideally, minutes/seconds have a more fixed size. Don't hide seconds if there
   are 0. Do hide seconds if we're in the 10m+ range. Hide minutes if we're in the
   2h+ range, with hysteresis as the pointer moves, of which unit is the lowest
   visible, so that it doesn't thrash as the user moves the mouse or the chart
   moves underneath."

## Environment / context

- Readout code: `crates/ot-ui/src/charts.rs` (`ChartGroup`, `readout`),
  `crates/ot-ui/src/sparkline.rs` (`paint_readout`), `crates/ot-ui/src/format.rs`
  (`ago`), and two readouts of its own on the Performance page
  (`crates/ot-ui/src/perf.rs`: `paint_core_grid`, `paint_pair`).
- The view has no text measurement (`ot-ui` only builds a display list). The
  readout is one text run, flush against the crosshair, on the roomier side.
- `theme.small` (the readout's style) already has tabular figures. In Segoe UI
  Variable the tabular digit and U+2007 FIGURE SPACE are both 0.539 em (checked
  with WPF `GlyphTypeface` on `SegUIVar.ttf` and `segoeui.ttf`); the default `1`
  is proportional (0.379 em), so tabular matters.
- `plans/replace-task-manager.md` is another thread's untracked plan. Do not stage it.

## Decisions (made in this thread)

1. **One number.** The readout shows the value under the dot (the column's mean).
   The peak is gone from the text; the min-max envelope still shows spikes.
2. **The time sits next to the line**, the value beyond it: `34% · 12s ago|` left
   of the line, `|12s ago · 34%` right of it. With a constant-width time (below),
   the time never moves relative to the line and the value's near edge only moves
   when the time changes shape.
3. **Constant-width time.** Compact units like the axis labels: `now`, ` 9s ago`,
   `2m 05s ago`, ` 8m ago`, `1h 05m ago`, ` 2h ago`. Fields after the first are
   zero-padded; a lone field is padded to two digits with U+2007 (a tabular digit's
   width). Seconds show even when 0.
4. **Hysteresis on the fields shown**, ratio 0.8: a boundary crossed going older at
   T is crossed back only below 0.8 T. Smallest unit: seconds hidden from 10 min
   (back below 8 min), minutes hidden from 2 h (back below 96 min). The leading
   unit too: minutes appear at 1 min (gone below 48 s, so `0m 55s ago` on the way
   back), hours at 1 h (gone below 48 min). Reason for the leading unit: past 10
   minutes a still pointer's snapped age sawtooths by a column (about 100 s per DIP
   near 1 h on a 300-DIP chart), which would flip `59m` / `1h 00m` every few seconds.
   State is per `ChartGroup`, reset when the pointer leaves.
5. **Side hysteresis:** the readout changes side of the line at the middle of the
   axis ± 5 % of the width, not exactly at the middle.

## Plan / steps

1. ~~Read the code; design.~~
2. ~~`format::ago` with `AgoFields` + tests; `Side` in `sparkline.rs`;
   `Crosshair` state in `ChartGroup`; `readout` reordered; Performance page
   readouts. Tests.~~
3. ~~README charts paragraph.~~
4. ~~Checks (tests, clippy on three targets, rustfmt own files), screenshot, commit.~~
5. **[current]** The user's reaction in the real app.

## Findings / gotchas

- In the raw tier (the last 10 minutes at 1 Hz) a still pointer's snapped age is
  stable: every second the set of sample ages is the same, so the nearest one is
  too. It is past the seam, in the 10 s buckets and multi-bucket columns, that
  ages drift and snap back.
- The old `ago` floored minutes (`s / 60` after rounding seconds); the new one
  rounds in the smallest unit shown, so 25.5 minutes reads `26m ago`.
- `Outcome::Done` now takes an `LRESULT`: the scratch hover-shot edit in
  `window.rs` is `Outcome::Done(LRESULT(0)) // SCRATCH-HOVER-SHOT` in the
  `WM_MOUSELEAVE` arm (reverted after building; `git diff` empty).
- `target/hover-sweep.ps1` (scratch, untracked): launches the hover-shot build,
  waits (default 130 s for two minutes of history), posts a list of pointer x
  positions and stacks a crop of the summary cards after each into one PNG.
  Result `target/hover-sweep.png`: the time against the line on both sides, one
  value, the side holding at 1m 10s going left and at 46 s coming back.

## Progress log

- [x] Plan written; code read.
- [x] Implementation and tests: `format::{TimeUnit, AgoFields, ago}`,
      `sparkline::Side`, `charts::Crosshair`, `charts::readout`, both Performance
      page readouts; new tests for the width, the hysteresis (units and side), a
      summarized point, and the two-line readout. 111 `ot-ui` tests.
- [x] README charts paragraph.
- [x] Checks: workspace tests; clippy `-D warnings` on windows / linux / macOS
      targets; `cargo fmt --all --check`. Screenshot sweep in the real app.
- [x] Committed.

## Open questions for the user

1. The leading unit has hysteresis too, so sweeping back from 1m 10s reads
   `0m 50s ago` until 48 s. Needed near the hour (where a still pointer's age
   sawtooths), harmless but visible at one minute. Keep it, or apply it only to
   hours? (Recommendation: keep; one rule everywhere.)
2. A lone single digit is padded with a figure space, which shows as a slightly
   wider gap: `18% ·  1s ago`. That is what keeps the value still between 9 s and
   10 s. (Recommendation: keep.)

## Things not to do

- Do not stage `plans/replace-task-manager.md`.
- Do not run `cargo fmt --all` if another thread has uncommitted files; format only
  our own with `rustfmt --edition 2021 <files>`.
