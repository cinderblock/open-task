# Performance page: stack the device list on tall windows

> **Status:** active · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Related: `plans/performance-view.md` (the page's original design).

## Goal

On a tall, narrow window the Performance page's device list (a fixed 212 DIP
column on the left) eats a large share of the width and leaves the charts
skinny. The user asked for a more responsive layout: at tall aspect ratios, put
the device list on top of the chart area instead of beside it.

## Environment / context

- All layout lives in `crates/ot-ui/src/perf.rs`: `layout()` splits the page,
  `PerfPage::lay_out_list()` places the entries, `paint_list()` draws them.
- `scripts/screenshot.ps1` launches the app and screenshots it, for a visual check.

## Decisions already made (don't re-ask)

1. Stacked mode turns the list into a wrapping grid of tiles across the top. The
   side list is the same grid with one column, so one layout routine serves both.
2. Stack when the page is narrower than it is tall (`STACK_ASPECT`); a constant,
   easy to tune.
3. The strip holds at most a share of the page's height (`STACK_SHARE`); more rows
   scroll with the wheel, like the side list does.
4. Columns are balanced: the fewest rows that fit, then the fewest columns that fill
   those rows, so tiles come out as wide as they can.
5. Keys: Up/Down still step through the list in a single column. In the grid they
   move by a row, and Left/Right step one tile.

## Plan / steps

1. ~~Read the layout code; write this plan.~~
2. ~~Generalize the list layout to a grid; stack in `layout()`; keys. Tests.~~
3. ~~Screenshot a tall window and a wide one; commit.~~ Done; awaiting the user's
   verdict on the threshold and the strip's share.

## Findings / gotchas

- `scripts/screenshot.ps1` had no way to size the window; it gained `-Size "W,H"`
  (physical pixels) for this check. At 800x1300 the 16 devices on this machine
  (many vEthernet adapters) came out three tiles across, six rows, about 350 DIPs:
  under the 35% cap, so nothing scrolled. At 1500x900 the side list is unchanged.

## Progress log

- [x] Plan written
- [x] Grid list + stacking + keys + tests (`perf.rs`)
- [x] Visual check (`target/perf-tall.png`, `target/perf-wide.png`), commit

## Open questions for the user

(none)

## Things not to do

- Don't hand-maintain two list layouts (side and top); keep it one grid routine.
