# Chart time-axis knee and the History chart's legend

## Goal

The log time axis gave too much width to "now" and too little to a few minutes
ago, and the History chart's legend column took about a quarter of the width.
The user picked:

- a wider knee, τ = 5 s at the default update speed, **tied to the update speed**
  (so it is a number of samples), with a **setting** to change it;
- the History chart's band names **drawn inside the bands** (option A), with a
  **setting to use a floating legend instead** (option B).

## Environment / context

- Axis: `crates/ot-ui/src/sparkline.rs` `TimeAxis` — `x = ln(1 + age/τ)` scaled
  so the span reaches the left edge. `TimeAxis::at` builds the frame's axis;
  `charts::axis_of` (`crates/ot-ui/src/charts.rs`) calls it from
  `view.rs` `paint_at`.
- Update speed: `Settings::update_interval_ms`, `SPEED_STEPS` 4000/2000/1000/500,
  default 1000 ms. (`REFRESH_MS = 2000` in `window.rs` is the Connections page's
  re-read, *not* the sampling — an early misread.)
- History chart: `crates/ot-ui/src/usage_chart.rs`; legend column 28 % of the width
  clamped 150–240 DIP.
- Settings persisted in `crates/ot-shell-win/src/prefs.rs` (registry DWORDs).
- No text measuring in `ot-ui`: backends measure. Label widths are estimated and
  drawn with ellipsis.

## Decisions already made (don't re-ask)

- Knee in samples, default 5 (5 s at Normal speed). Steps 1, 2, 3, 5, 10, 20.
- Legend: In bands (default) / Floating; I also keep the old Beside layout as a
  third choice since it already exists and costs nothing.

## Plan / steps

1. [x] `TimeAxis::at` takes τ; `axis_of` takes it; settings field
       `chart_knee_samples` (`KNEE_STEPS` 1, 2, 3, 5, 10, 20) + "Where the time
       axis bends" card + registry `ChartKneeSamples`. `Settings::knee_ms()` =
       samples × `update_interval_ms`.
2. [x] `ChartLegend` setting (Inline / Floating / Beside) + "History chart legend"
       card + registry `ChartLegend`; `UsageChart::set_legend` from
       `apply_settings`.
3. [x] Inline names: `UsageChart::fit` sweeps the columns with two monotonic
       queues (linear per band) for the window as wide as the name with the most
       clear height; hysteresis by the column's `end_ms`; hovered band gets a
       name + value chip beside the pointer.
4. [x] Floating legend: the existing legend inside the plot on a translucent
       backing, top corner the stack rises into less (switches at 60 %), moves away
       from a marked moment; the pointer on it does not mark or hit bands.
5. [x] Tests, clippy, fmt, README; screenshots checked (dark inline, dark floating,
       light inline, Settings page).

6. [x] Smooth the names (user, after the first pass: "not a fan of the jumps").
       Each band keeps a shown name and a leaving one. A kept place follows the
       column whose stretch of time holds its anchor (columns merge as they age)
       with the step absorbed in an eased `dx`; `y` eases to the clear height's
       middle (`LABEL_EASE_S` 0.15 s); a move cross-fades (`LABEL_FADE_S`
       0.25 s) and may not happen within `LABEL_DWELL` (2 s) of the last unless
       the place no longer fits. `UsageChart::set_now` gets the frame time from
       `View::paint_at`; `View::animating` includes `history.animating()` so
       frames keep coming while names settle, even with smooth scrolling off.

7. [x] Glide instead of cross-fade (user: "fade time should be used a lot
       less... it should move gracefully"). A move re-anchors at once and puts
       the difference in `dx`, so the name travels from where it is drawn; `dx`
       and `y` ride a critically damped spring (`LABEL_SPRING` 10 rad/s, ~0.5 s
       to 95 %, stepped at 4 ms, snapped after a gap over 0.5 s). Fades
       (`LABEL_FADE_S` 0.2 s) only for a name appearing or left with nowhere to
       fit; one fading out that fits again comes back rather than a second copy
       fading in. A kept place holds down to 80 % of the needed height
       (`LABEL_SLACK`) so a band at the edge of fitting does not flicker.

## Findings / gotchas

- Width shares over a 1 h span: τ=1 s gives the last 10 s 29 %, 1–10 min 28 %;
  τ=5 s gives 17 % and 34 %.
- The first version of the window search had both deque comparisons inverted
  (kept the wrong extremes); the "names sit inside their bands" test caught it.
- `format::cycles` clears its buffer, so the chip formats the value first and
  prepends the name.
- Name widths are estimated (no text measuring in `ot-ui`): 0.56 em a character
  cut "MsMpEng.exe" short on screen; 0.6 em fits.
- In the Fading total reading every band starts with its program's lifetime
  cycles, so it is thick at the old end; tests about thin bands use Rate.
- `--page settings` is not a page argument; screenshot the Settings page with
  `-Click "68,724"` on the rail.
- `sed -i` in Git Bash rewrote `lib.rs`/`view.rs` to LF; converted back.

- Column merges under a name are rare: bins are aligned in time, so a column's
  set of samples changes only when the bin size doubles at its age. The jumps
  seen were mostly new samples moving the best place and the band's edges near
  "now". Tests feed a sample a second while scrolling at 60 fps.

## Progress log

- [x] Knee setting, tied to the update speed, default 5 samples
- [x] Legend setting and in-band names
- [x] Floating legend
- [x] Tests (settings: knee + legend cards; usage_chart: names inside bands, thin
      band unnamed, name keeps its place while scrolling, hover chip, floating
      corner + crosshair avoidance)
- [x] README
- [x] Smoothing: eased moves, cross-faded relocations, dwell; tests for gliding
      with samples arriving, the wait-then-fade, and following a merged column
- [x] Moves glide on a spring, fades only to appear or vanish; tests: no frame
      jolts a name with samples arriving and bursts moving the best place, the
      wait-then-glide, setting off gently onto a merged column, fade out and
      back in

## Open questions for the user

None.

## Things not to do

- Don't touch the other threads' uncommitted files (`ot-core/src/usage.rs`,
  `ot-model/src/chromium.rs`, `ot-model/src/process.rs`,
  `plans/chromium-process-roles.md`).
