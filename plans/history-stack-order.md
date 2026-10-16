# History chart: steadiest bands at the bottom

## Goal

In the History chart (`crates/ot-ui/src/usage_chart.rs`) the bands were stacked in
slot order (the order programs happened to be given a color), with "Everything
else" and "Not in a process" always on top. A spiky program low in the stack lifts
every band above it with each spike, so the whole chart jumps. The user asked for
the most stable bands at the bottom, so spikes only move what is above them, and
the spikiest bands sit on top where they move nothing else.

## Decisions

- **Stacking order is separate from color.** A band's index (slot) still decides
  its color and stays with its program; a new `stack` permutation decides the order
  bottom to top. Recoloring on reorder would undo the "colors stay put" promise.
- **Steadiness = time-weighted standard deviation of the band's thickness** over the
  history inside the chart's reach, in the reading's own units. Absolute, not
  relative: what lifts the bands above is how far the thickness moves, whatever the
  band's size.
- **All bands take part**, "Everything else" and "Not in a process" included: the
  user asked for the steadiest at the bottom, and "Not in a process" (the kernel's
  memory) is often the steadiest of all.
- **Hysteresis:** worked out when the rows are (once a sample), starting from the
  previous order; two neighbours swap only when the upper one's spread is under
  `RESTACK` (0.7) of the lower one's, so a near tie does not make the chart
  reshuffle as it is watched. A fresh order (first build, or another resource's
  bands brought back) is sorted strictly.

## Steps

- [x] `stack` field + `restack()` after `fill_rows`
- [x] Every place that walks the bands bottom-up follows `stack`: `paint_bands`,
      `place_labels`, `band_at`, `lay_out_legend` (top row = top of stack)
- [x] Tests: helpers that assumed index order; new test for the order and its
      hysteresis
- [x] Module docs + README
- [x] Commit (tests, clippy, fmt clean; not yet looked at in the running app)
