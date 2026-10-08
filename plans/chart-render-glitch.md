# Yellow bricks flashing in the charts

> **Status:** active · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

The user's report, 2026-10-07: "weird visual glitches in the charts, more often when
I hover. Little yellow bricks appear for a frame and then disappear." Find the cause
first (they asked what it is from), then fix it.

## Environment / context

- Seen on v0.11.1 (after `98f7408` damage-tracked rendering and `051c42f`
  charts-only spliced frames). Both are prime suspects: they are the only recent
  changes to what reaches the screen.
- Renderer: `crates/ot-shell-win/src/gfx.rs` (`render`, `draw_damage`, `draw_all`).
  Damage diff: `crates/ot-paint/src/damage.rs`. Splice: `DisplayList::splice` in
  `crates/ot-paint/src/display.rs`, used by `App::paint_tick` in
  `crates/ot-ui/src/view.rs`.
- Yellow-ish theme colors (dark): `heat` / `memory_modified` `#FFB454` (table
  heat cells, usage map), `battery` `#E8D45A`, `series[3]` `#C98500` (History
  bands), `network` `#F0A04B`.

## Plan / steps

1. ~~Diagnostic: `OT_CHECK_DAMAGE=1` makes the renderer draw every partial frame
   whole into a second bitmap too, read both back, and log (and dump as BMP) any
   pixels that differ. `OT_WARP=1` forces the software rasterizer.~~
2. ~~Run it, hover the charts, read the dumps.~~ Cause: the Intel GPU driver (see
   Findings).
3. ~~The user updated the Intel driver (2026-10-07) to 31.0.101.2145.~~
4. ~~Report.~~ The user chose to leave them (2026-10-08): rare since the
   driver update, a driver fault, kept out of the charts by GPU drawing.

## Findings / gotchas

- **The bricks are GPU tile corruption from the Intel driver, not the app's
  damage logic.** With `OT_CHECK_DAMAGE=1` and a scripted hover sweep (posted
  `WM_MOUSEMOVE`, `target/tmp/hover-sweep.ps1`, about 3000 partial frames):
  - Intel UHD Graphics, driver 30.0.101.1660 (2022-03-17): 15 differing frames in
    the first run, 3 in the second. **Every** difference is exactly 8 x 4 pixels,
    on an 8 x 4 grid (x a multiple of 8, y of 4), e.g. `bbox x 760 y 136 w 8 h 4`,
    outside every damaged rectangle. 8 x 4 pixels at 32 bpp is 128 bytes: the block
    size of Intel's lossless render compression, whose metadata going out of step
    with the data gives exactly this, a block showing other content (seen: a gray
    block on the heat strip, a red one over "chrome.exe", a light one on the
    table background).
  - One corrupt block was in the bitmap drawn **whole from scratch** (frame 1747),
    so it is not the partial path leaving stale pixels; another stayed in the
    canvas, same place, for six frames until a redraw covered it.
  - WARP (`OT_WARP=1`), same sweep: 0 differing frames.
  - Apart from those blocks, partial and whole frames were pixel-identical: the
    damage tracking itself is sound.
  - The machine also has a Fresco Logic IDDCX (USB display) adapter, driver 2020.
- A second, real bug the check found: Direct2D's default mitred joins let a sharp
  chart-line corner reach up to five widths past its point, outside the line's
  damage bounds, leaving 1-3 px behind just above a chart's redrawn area (seen
  with a GDI-compatible canvas experiment: 1-px differences at the row above the
  damage, e.g. damage top y 18.45, difference at y 17). Fixed with bevel joins
  (`4257222`). Not yet re-verified with the check (the driver update interrupted
  the run).
- A GDI-compatible canvas (`D2D1_BITMAP_OPTIONS_GDI_COMPATIBLE`) showed no 8 x 4
  blocks in one run (3670 frames with small differences instead, mostly the miter
  issue above). Not pursued: the user chose the driver update.
- **After the driver update (31.0.101.2145)**, `OT_CHECK_DAMAGE`, GPU chart
  drawing, 60 s each: History view 0 differing frames; list view 2 (both 8 x 4
  blocks, at y 124 and 708, in Direct2D-drawn content). On the old driver with
  Direct2D drawing the History chart, 30 in 30 s (29 of them 8 x 4 blocks inside
  the chart). So: much rarer, not gone; the GPU chart renderer (now the default)
  also keeps them out of the charts.
- Hovering sets `full_due` (every `App::handle` does), so hover frames are full
  paints diffed by the renderer, not spliced frames.

## Progress log

- [x] 2026-10-07: diagnostic built (`OT_CHECK_DAMAGE`, `OT_WARP`), README notes
- [x] 2026-10-07: glitch caught; Intel driver tile corruption, WARP clean
- [x] 2026-10-07: miter-join leftovers fixed with bevel joins (`4257222`)
- [x] 2026-10-07: driver updated by the user; re-checked: rarer, not gone

## Open questions for the user

None. (2026-10-08: leave them.)

## Things not to do

- Do not "fix" the damage tracking for this: it is pixel-exact apart from the
  driver's blocks, and the blocks appear in whole frames too.
