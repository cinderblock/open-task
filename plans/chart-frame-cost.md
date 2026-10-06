# Cheaper scrolling frames

> **Status:** done; committed on `master` (2026-10-06), not pushed or released · **Started:** 2026-10-06 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Follows `plans/smooth-chart-scrolling.md` (v0.11.0), whose measurements this starts from.

## Goal

Smooth chart scrolling (v0.11.0) cost about 10 % of one core more than before on
the Processes page (15–19 % in all, at 60 Hz). The user chose three of the four
ways offered on 2026-10-06 to cut that:

1. **Slow down when nobody's watching:** no frames on another virtual desktop
   (cloaked); half rate while the window is not in the foreground.
2. **Charts-only frames:** when only the clock moved, keep the rest of the last
   full paint's display list and repaint just the charts.
3. **Cheaper chart drawing:** fewer and cheaper Direct2D calls per chart.

Declined: fewer frames where motion is slow ("nah").

## Environment / context

- Per frame before this work (Processes page, 60 Hz): app paint 0.48 ms, diff
  and layout bookkeeping about 0.1 ms, replaying the chart areas 0.2–0.7 ms,
  `EndDraw` 0.3 ms, copy 0.07 ms.
- Measuring: `target/tmp/smooth-probe.ps1` (CPU of the debug build's process on
  Processes, then Settings; the debug profile is optimized; `-Background` hands
  the foreground to the desktop first, and the result line says which it was).
- Frame loop: `crates/ot-shell-win/src/window.rs` `repaint`, `seen`,
  `TIMER_FRAME`; renderer `crates/ot-shell-win/src/gfx.rs` (`render(dl, sync)`).
- Layers: `crates/ot-paint/src/display.rs` (`Layer`, `begin_layer`, `end_layer`,
  `splice`). Charts: `ChartGroup::paint` marks each chart's line and time labels
  as a layer, `ChartGroup::repaint` paints it again; Performance's paired-chart
  labels are `PAIR_AXIS_LAYER`; the History view is `HISTORY_LAYER` in `view.rs`.
- App: `full_due` (set by all 19 public mutators), `kept`, `kept_page`,
  `only_the_clock_moved`, `paint_tick`, `repaint_layer`, `paint_full`.

## Decisions already made (don't re-ask)

1. Background: cloaked → no frames; not foreground → `Present(2, …)`, every
   other vertical blank (paced by the display, no timers).
2. A guard: frames closer than 4 ms cannot have waited for a vertical blank, so
   the next is paced by a 16 ms timer instead of spinning.
3. Charts-only frames: a tick happens only when nothing but the clock changed (no
   public mutator called since the full paint, charts scrolling, same page, no
   row slide, no chart marking a moment). The tick splices freshly painted
   layers into a copy of the full paint.
4. The time labels belong in the chart's layer: while the history fills, the
   span grows and the labels move with the clock.
5. Paths are built with one `AddLines` call from a reused buffer.
6. The renderer touches cached layouts every 30 frames on partial frames rather
   than every frame (the cache evicts what was unused for 120 frames).

## Plan / steps

1. [x] Background throttling and the spin guard (shell).
2. [x] Cheaper path building (gfx).
3. [x] Layers and splicing in `ot-paint`; test.
4. [x] `ChartGroup` keeps what a repaint needs; `repaint`, `set_axis`,
   `marking`.
5. [x] App: stale flag, tick path for Processes (cards + History), Summary,
   Performance.
6. [x] Renderer: periodic layout touching.
7. [x] Measured, visual check, tests/clippy, README, commit.

## Findings / gotchas

- Results (CPU of one core, 60 Hz, Processes page; Settings page with no
  frames about 2 %):

  | | Before (v0.11.0) | After |
  | --- | --- | --- |
  | Foreground | 15–19 % | 8–9.6 % |
  | Background | 15–19 % | 4.9 % |

  `AddLines` alone was within the noise (17.6 % measured right after it).
- The splice test (`a_frame_where_only_the_clock_moved_repaints_just_the_charts`)
  compares a tick frame with a full paint at the same moment through
  `damage_since`; it caught the time labels sitting outside the layers.
- Not exercised by me: the cloaked (other virtual desktop) path and the spin
  guard (needs a locked session or a display off). Both are small and use
  documented APIs (`DWMWA_CLOAKED`).
- Python heredocs in Git Bash mangle backslash escapes (`\u{b7}`) and some
  quoting; write patch scripts to `target/tmp/*.py` instead.

## Progress log

- [x] 2026-10-06: plan written; all steps done; committed.

## Open questions for the user

1. Release these as v0.11.1 (or v0.12.0)? Not done without asking.

## Things not to do

- Do not drive the frame loop with posted messages: posted messages are taken
  before input, so a loop that always has one posted starves the mouse and
  keyboard. `InvalidateRect` (WM_PAINT, lowest priority) is the right pump.
- Do not add a public `&mut self` method to `App` without setting `full_due`:
  the next frame would splice a stale paint.
- Do not try "fewer frames where motion is slow": declined.
