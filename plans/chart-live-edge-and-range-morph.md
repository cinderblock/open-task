# Charts: the newest sample at the right edge, eased in; ranges where time is compressed

> **Status:** built, awaiting the user's look · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Revises decision 1 of `plans/smooth-chart-scrolling.md` (the clock's lag).

## Goal

The user, 2026-10-13: "where is t=0 on our charts? off the right side? how do we
get the slope that is drawn? I'm thinking it would be more "true" if we drew the
new data points on the right edge, which unfortunately means they need to snap
in, but they're no longer delayed by an average of 1s. can we add a little
smoothing too? also, in the areas that get scrunched in time on the left, maybe
we morph to a range display instead?"

1. Answer: where t=0 is, and where the slope at the right edge comes from.
2. Newest sample drawn at the right edge (no 1.25-interval lag).
3. A little smoothing of the snap.
4. Where many samples share a column, morph from a mean line to a range display.

## How it was before (the answer to 1)

- The right edge ("now" label) was the `ChartClock`, which trailed the newest
  sample by `LAG` = 1.25 intervals, advancing in real time. Samples are stamped at
  the end of the interval they average (`Timeline::observe`, `taken_at`).
- So for a 1 s interval, at `s` seconds after a sample arrived, the newest sample
  sat at age `s - 1.25` (always 0.25–1.25 s *past* the right edge, never inside),
  the one before it at `s - 0.25`. The line crossing the edge was the straight
  segment between those two, clipped: the slope at the edge was a linear
  interpolation toward a sample not yet shown. Everything was drawn 1.25 s late.

## Decisions already made (don't re-ask)

1. **The clock runs at the newest sample's time plus the time since it arrived
   (LAG 0)**: the right edge is "now". A new sample lands at the edge.
2. **Between the newest sample and the edge, the line holds the newest value**
   (flat), rather than moving the newest point itself to the edge: older points
   never jump; only the head changes when a sample arrives. The hold is drawn,
   not a plot point (hover never snaps to it).
3. **The clock stops when the feed stalls**: at most `newest + 1.5 intervals`
   (a paused replay, a stalled sampler), so the hold does not grow without end.
4. **"A little smoothing" = the snap eases**: a new sample's value glides from the
   previous one over `EASE_MS` (150 ms, ease-out), display-only and stateless
   (from the time the clock saw the sample arrive). My reading of the request;
   data smoothing (a moving average) was the other reading, not done.
5. **Range morph keyed on samples per column**, in five steps by
   `floor(log2(count))` (1, 2–3, 4–7, 8–15, 16+): the mean line fades (opacity
   1, .75, .55, .4, .3) and the min–max envelope strengthens (x1 to x2.6). Each
   point's step is the largest from it to the newest (`PlotPoint::step`), so it
   only grows leftward. Steps rather than a gradient because `Graph` and `Band`
   have one color per command (three renderers).
6. **History view (usage chart):** the same hold and ease; no range morph (its
   stacked bands have no envelope).
7. **Readout in a range shows the range** (user, 2026-10-13: "min/max is fine"):
   where a point summarizes several samples that differ, the hover readout says
   `12–48%`, the unit once when both ends share it (`charts::point_value`). Used
   by the chart groups' readouts, the Performance pair and stack readouts and
   the per-core tiles; the port legend keeps the mean (its rows are sums).

## Plan / steps

1. [x] `ChartClock`: LAG 0, stall cap, `moving()` while easing; `settle` fraction.
2. [x] `TimeAxis`: carry the ease (`settle`, `settled()`), used by `Plot` and the usage chart.
3. [x] `Plot::build`/`paint`: eased newest value, hold to the edge.
4. [x] `Plot::paint`: range morph in steps.
5. [x] Usage chart (History view): hold and ease.
6. [x] Tests (clock tests rewritten; hold, ease, step tests), fmt, clippy on
   windows/linux/macOS, screenshots.
7. [x] README charts paragraph; commit.
8. [x] Range readout on hover (`point_value`), tests, README.
9. [ ] The user looks at it live (a session of several minutes shows step 1 at the
   left; an hour shows the full morph).

## Findings / gotchas

- **A column's sample count is not monotonic with age.** Power-of-two bins of
  1024 ms over 1 s samples catch one or two samples depending on phase, so a
  per-point step alternated (line opacities `[.., 1.0, 0.75, 1.0]`): a dashed
  look. Fixed with the running max from the newest (`PlotPoint::step`).
- **The shared tree did not compile mid-task**: another thread had `perf.rs`
  half-edited (`build_stack` missing). Verified in a temporary worktree at HEAD
  with only this task's four files copied in (`target/verify-live-edge`).
- **The hold is wide on a young session**: with a 10 s span, one second at the
  right edge is ~100 DIP, so the flat stretch is long; at five minutes it is
  ~20 DIP. Honest (that much time has passed), but noticeable at start-up.
- Offscreen renders of the morph (CPU rasterizer, scratch test not committed):
  `target/morph-1h.png`, `target/morph-10m.png`, `target/morph-5m.png`; live:
  `target/live-edge.png`.

## Progress log

- [x] 2026-10-13: code read, plan written.
- [x] 2026-10-13: built, tested (214 `ot-ui` tests), clippy on three targets,
  screenshots; committed.
- [x] 2026-10-13: user chose min–max in the readout; built and committed.

## Open questions for the user

1. "A little smoothing": built as the 150 ms ease of a new sample. Data
   smoothing (a moving average) not built; awaiting the user's word.

## Things not to do

- Do not run `cargo fmt --all`: other threads have uncommitted work in the tree
  (`crates/ot-app/src/main.rs`, `ot-model`, `ot-probe`, `ot-record`). Format own
  files only.
