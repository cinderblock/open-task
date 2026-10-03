# Charts fill their width; history length is a setting

> **Status:** done; checked in the running app and committed on `master` (2026-10-02) · **Started:** 2026-10-02 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/charts-log-time-and-live-table.md` (the log time axis) and `plans/chart-hover-readout.md`.

## Goal

The user's request on 2026-10-02:

1. "Can we make all the x-axes auto scale, so the full width is always used?"
   Before this, every chart had a fixed one-hour axis, so a young session drew from
   the right edge and left the left empty until an hour had passed.
2. "Still start dropping data after a max time (1h default) that should be
   configurable (include estimated memory usage given the number of samples asked
   to be saved)."
3. A question: "Do we need a system to decimate our internal database to prevent
   runaway RAM/compute?" Answered in the reply: the code already had one (tiers of
   buckets); this work made it scale with the configured length.

## Environment / context

- Time series: `crates/ot-core/src/timeline.rs` (`Series`, `Retention`, `Timeline`)
  over `crates/ot-core/src/history.rs` (`Ring`). Per-program CPU history for the
  History view: `crates/ot-core/src/usage.rs` (`Usage::frames`).
- Axis: `crates/ot-ui/src/sparkline.rs` (`TimeAxis`, `Plot`, `paint_axis`). The
  shared constant `charts::AXIS` is gone; `charts::axis_of(&Timeline)` builds the
  frame's axis, `ChartGroup::begin(n, axis)` takes it, `UsageChart::build` takes it
  as its last argument, and `PerfPage::paint` computes it from the timeline it is
  given.
- Settings: `crates/ot-ui/src/settings.rs` (`Settings::history_minutes`, the
  `Card::History` stepper under a new "Charts" section, `HistoryCost` for the
  estimate), persisted by `crates/ot-shell-win/src/prefs.rs` as `HistoryMinutes`
  (REG_DWORD) under `HKCU\Software\open-task`.
- Pre-existing, out of scope: `update_interval_ms`, `always_on_top` and
  `hide_when_minimized` are **not** persisted by `prefs.rs`.
- Sizes: `Sample` is 16 bytes, `Bucket` 32 bytes. One series at one hour is about
  21 KB (600 raw + 363 buckets); at a day about 79 KB. A 32-core machine has ~50
  series.

## Decisions already made (don't re-ask)

1. **The axis span is the age of the oldest sample still held**, clamped to a floor
   of 10 s (`TimeAxis::MIN_SPAN_MS`) and bounded by the configured length through
   the retention. The chart fills its width from the first seconds; once the
   history is full, the span settles at the length.
2. **History length is a stepper on the Settings page** ("How far charts reach
   back"), steps 10 min, 30 min, 1 h (default), 2 h, 3 h, 6 h, 12 h, 24 h. The card
   says the points kept per chart and the memory that commits to, for the series
   this machine has plus the usage history.
3. **Retention derives from the length** (`Retention::covering`): 600 raw samples
   (unchanged), a 10 s tier covering up to 2 h of the span, and a 60 s tier only
   when the span exceeds 2 h. Changing the length resizes the rings in place
   (`Series::set_retention`); nothing held is lost except what the new length
   excludes, and a new tier fills forward.
4. **The usage history drops frames by age**, not by count, and its step widens
   with the length (`usage::step_ms`: 10 s up to 2 h, `span / 720` beyond) so a day
   is at most 720 steps. Steps already made keep their width.
5. **Axis ticks** gained `6h` and `1d`; ticks past the span are skipped as before.
6. **Memory estimate is a ceiling for the timeline** (rings allocate their capacity
   up front) **and a projection for the usage history** (frames vary with how many
   programs are busy; the average frame so far is used, sixteen programs a frame
   before there is one).

## Plan / steps

1. [x] Read the code; write this plan.
2. [x] `ot-core`: `Ring::set_capacity`/`oldest`; `Retention` owns its tiers,
   `covering`, `points`, `bytes_per_series`; `Series::set_retention`, `oldest_ms`;
   `Timeline::set_retention`, `span_ms`, `series_count`. Tests.
3. [x] `ot-core` usage: `set_history_span`, age-based dropping, step width from the
   span, `history_bytes`, `frame_bytes`, free `history_bytes(span, frame_bytes)`.
   Tests.
4. [x] `ot-ui`: `TimeAxis::spanning`, ticks; `ChartGroup` holds the frame's axis;
   `UsageChart::build` takes it; `PerfPage::paint` computes it; `view.rs` computes
   it per frame. Tests updated, two added.
5. [x] `ot-ui` settings: `history_minutes` with steps, the card, memory estimate;
   `prefs.rs` persistence; `apply_settings` feeds the timeline and usage.
6. [x] README; `cargo fmt`, `clippy` (clean), tests (all pass).
7. [x] Visual check with `scripts/screenshot.ps1`: after 16 s the CPU line reaches
   the left edge under a `10s … now` axis; the Charts card reads "1 h · 963 points
   a chart, about 1.39 MB for the charts on this machine" (12 logical cores, one
   disk, a few adapters). Committed.

## Findings / gotchas

- The log axis's left end needs little resolution: on a 24 h axis a 1000 px chart
  spends about 220 px on the 22 h before the last 2 h, so 60 s buckets are already
  several per pixel there.
- A `Retention` with `&'static [Resolution]` cannot be built at runtime, hence the
  owned `Vec<Resolution>`; `Retention` lost `Copy`, so `Timeline` clones it per
  series (a Vec of one or two elements, once per new device).
- `PerfPage::paint` with an extra `axis` argument hit clippy's
  `too_many_arguments` (8/7); computing the axis inside from the timeline it is
  given was the cleaner fix and matches how the view computes it.
- Test fixtures with twenty seconds of history now get a 19 s axis, so the
  `1m`/`10m`/`1h` labels are not drawn there; the view test checks `now` and `10s`
  and asserts `1m` is absent.
- Adding a Settings section shifted every later card's index in the settings tests
  (`cards[5..8]` → `cards[6..9]`, `TASK_MANAGER` 9 → 10).

## Progress log

- [x] 2026-10-02: plan written; core retention and usage changes, tests pass.
- [x] 2026-10-02: UI axis rewiring, settings card, prefs, README; workspace
  clippy clean, all tests pass (ot-core 30, ot-ui 161).
- [x] 2026-10-02: visual check in the debug build; committed.
- [x] 2026-10-02: released as v0.8.0 (`e12c334`, tag pushed; the release
  workflow builds and publishes it).
- Seen in passing, not mine: the Update speed card's value ("Normal") is clipped
  to "Norma" between its buttons at this window width.

## Open questions for the user

None.

## Things not to do

- Do not rebuild `Series` on a settings change: that throws away the history.
- Do not drop the 10 s floor on the axis span: with one sample the log axis is
  degenerate (span 0).
- Do not reintroduce a shared `const AXIS`: tests and pages must take the frame's
  axis from the group (`ChartGroup::axis()`, test-only) or from `charts::axis_of`.
