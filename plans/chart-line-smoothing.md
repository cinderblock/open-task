# Charts: a smooth curve through the samples

> **Status:** built and committed · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/chart-live-edge-and-range-morph.md` (released in v0.14.0).

## Goal

The user, 2026-10-13, after v0.14.0: "i was thinking more of a smoothing that
eliminates the derivative discontinuities at each sample point and smoothes out
some of the aliasing we see because of our sampling". So not (only) the 150 ms
glide: the drawn line itself should be smooth (no corners at samples) and
low-passed a little.

## Environment / context

- Line charts: `crates/ot-ui/src/sparkline.rs` `Plot::paint` (wash, range-step
  runs of line and envelope, hold to the edge). Callers pass a scratch buffer:
  `charts.rs` `ChartGroup` (paint, repaint), `perf.rs` (stack total).
- Stacked charts: `usage_chart.rs` `paint_bands` (History view) and `perf.rs`
  `paint_stack` (a network card's ports). Bands as `DrawCmd::Band` (two
  ascending point lists) plus a background hairline `Graph` along each top.
- Renderers (GPU, CPU, Direct2D) draw only polylines/bands: curves must be
  tessellated in `ot-ui`.

## Decisions already made (don't re-ask)

1. **Uniform cubic B-spline over the plotted points for the mean line, wash and
   stacked bands** (approximating, not
   interpolating): C2 everywhere, a [1 4 1]/6 low-pass at the knots, inside the
   convex hull (never overshoots; x stays ascending since control xs ascend). Ends
   clamped (end points tripled) so the curve starts and ends on the end points.
   Interpolating splines (Catmull-Rom, monotone cubic) remove the corners but not
   the aliasing; rejected for that reason.
2. **The envelope's edges use a monotone cubic through every point**
   (Fritsch-Carlson / Fritsch-Butland slopes, `smooth_through`): smooth, no
   overshoot, and it keeps each min and max exactly. First built with the
   B-spline on the envelope too; that cut a spike to 2/3 in the range band,
   whose whole job is showing extremes (a test caught it). Stacked bands keep the
   B-spline (linear, so sums stay stacked).
3. **Tessellated about every 2 DIP** along x per span (1 to 24 pieces), so wide
   recent spans are round and compressed spans cost nothing extra.
4. **Hover dot on the curve; readout the measured value.** A dot off the line
   looks broken.
5. A lone spike is drawn at about 2/3 of its height in the line (the low-pass);
   the range envelope and the readout still show the real peak.

## Plan / steps

1. [x] `sparkline::smooth` (tessellate a B-spline, report each knot's index),
   `smooth_through` (monotone cubic), `sparkline::Scratch`.
2. [x] `Plot::paint`: wash, runs, envelope through the curves.
3. [x] Hover dot on the curve (`Plot::knot_y`).
4. [x] `usage_chart::paint_bands`, `perf::paint_stack` through the curve.
5. [x] Tests (222 `ot-ui`), clippy (three targets), fmt, renders, README, commit.
6. [ ] Release when the user asks (v0.14.0 is out without it).

## Findings / gotchas

- Curves ending a float rounding off their end point (200.00003) broke the
  hold's exact edge; the last point is now set exactly (and every knot of the
  monotone curve).
- Renders: `target/smooth-1m.png`, `target/smooth-5m.png`, `target/smooth-1h.png`
  (offscreen, CPU rasterizer, scratch test not committed); live:
  `target/smooth-live.png`.

## Progress log

- [x] 2026-10-13: plan written.
- [x] 2026-10-13: built, checked, committed.

## Open questions for the user

1. A setting to turn the smoothing off (exact sample-to-sample lines)? Not built
   unless asked.

## Things not to do

- Do not use an interpolating spline: it overshoots (CPU below 0 %) and keeps the
  aliasing.
