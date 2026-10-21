# Settings page per platform, and chart drawing on Linux

## Goal

The Settings page was written for Windows. On Linux about half of it does nothing
or says something untrue (user, 2026-10-21: "does the settings menu need to be
reworked/updated for linux? it doesn't have Direct3D..."). Make every card on the
page either work on the platform it is shown on or not be shown, with text that
names the right system; and give Linux a real chart drawing choice: the toolkit's
own path renderer (Cairo under GTK, QPainter under Qt) or open-task's CPU
rasterizer (`ot_paint::ChartRaster`), which Windows already offers.

## Environment / context

- Repo `C:\Users\camer\git\Personal Projects\open-task`, branch `master`, shared
  working tree (other threads commit here; stage only this work's hunks).
- Linux builds and tests run in WSL `Ubuntu` 24.04 (GTK 4.14.5, Qt 6.4.2), with
  `CARGO_TARGET_DIR=~/.cache/open-task-target`; GUI checks on a private Xvfb. See
  `plans/linux-gui-shells.md` "Testing the Linux GUI" and `plans/linux-probe.md`.
- Cross-clippy for Linux from Windows needs `--exclude ot-shell-gtk`.
- **Another thread** (`plans/linux-cpu-cost.md`) measured Linux chart drawing on
  2026-10-21 and prototyped `ChartRaster` as a GTK texture in a scratch worktree
  (`%TEMP%\ot-scratch-wt`, not for committing). Its numbers are the basis for the
  Linux default below. That thread is waiting on the user about GPU chart
  drawing on Linux; this one does not build a GPU path.

## Audit (2026-10-21): what each card does on Linux

| Card | On Linux before this work |
| --- | --- |
| Chart drawing (GPU / CPU / Direct2D) | Nothing: both plugins draw `Band`/`Graph` with Cairo / QPainter whatever is picked. |
| Run as administrator | "This is not available on Linux yet"; keeps a **Windows** heading on the page. |
| Hide when minimized | Only `ot-shell-win` reads it; no tray on Linux. |
| Minimize on use | Only affects Switch to, which Linux lacks. |
| Updates (card + 3 switches) | `ot-update` on Linux fails every request ("not supported on this platform yet"). |
| Always on top | Qt (X11): works. GTK 4: no API, logs only. Wayland: no protocol for it. |
| Animate rows | Says "Following Windows' animation effects"; host hard-codes animations on. |

Everything else (fade, speed, percentages, history, knee, smooth charts, legend,
recording) works on both.

## Decisions already made (don't re-ask)

- **Option 2** (user, 2026-10-21): a real Linux chart drawing choice, toolkit
  paths vs open-task's CPU rasterizer, not just hiding the card.
- **Capabilities, not `cfg`**: the shell tells `ot-ui` what this platform and
  toolkit can do (a plain struct), and each card asks it whether to show. Same
  shape as `Replacement::Unavailable` hiding the Task Manager card. Keeps `ot-ui`
  toolkit-agnostic and lets a card come back with one line when its feature lands
  ("Don't fork ot-ui behaviour per toolkit": this is data, not a fork).
- **The host rasterizes, not the plugins.** For CPU mode the host
  (`ot-shell-linux`) finds the chart runs and draws them with `ChartRaster`, and
  hands the plugin finished premultiplied bitmaps alongside the display list. One
  implementation for GTK and Qt (Qt's plugin is C++ and cannot call `ot-paint`),
  the same shape as Windows' CPU mode. Plugins only blit.
- `ChartDrawing::Direct2D` becomes `ChartDrawing::Native` ("the platform's general
  path renderer"); its saved number stays 2, so saved settings keep meaning.
  The card names it per platform: Direct2D, Cairo, QPainter.
- A saved choice the platform does not offer (GPU on Linux, the default
  everywhere) falls to the next offered one in `ChartDrawing::ALL` order: CPU.

## Design

- `ot-ui`: `Capabilities` given to `App` by the shell (`Start::capabilities`,
  `App::set_capabilities`):
  system name for the section heading, where row animation is followed from (or
  nothing), chart drawings offered and the native renderer's name, keep-above,
  tray (hide when minimized), switch-to (minimize on use), updates, run elevated.
- ABI v3: `paint(ctx, scale, out)` (the host needs device scale to rasterize;
  neither plugin sends `SCALE` events today); `OtDisplayList` gains `rasters` /
  `raster_count` (`OtRaster { first, end, rect (DIPs), width, height (px),
  pixels }`); `OtPlugin` gains `features()` (keep-above bit, path renderer name)
  and the animation setting where the toolkit knows it.
- Chart runs (`find_runs`, `ChartRun`, `PxRect`) move from `ot-shell-win` to
  `ot-paint` so both shells share them.

## Plan / steps

1. Plan (this document). (done)
2. `ot-paint`: chart runs moved from `ot-shell-win`, plus a helper that draws one
   run's commands into a `ChartRaster`. Windows unchanged in behaviour.
3. `ot-ui`: abilities struct; cards and sections ask it; `ChartDrawing::Native`;
   per-platform texts; tests for a Linux-shaped platform.
4. `ot-shell-core` / `ot-shell-win`: fill in Windows' abilities (all on, as today).
5. ABI v3 + host: scale in paint, rasters, features; host rasterizes CPU runs;
   `OT_CHARTS` on Linux too; Linux abilities.
6. GTK and Qt plugins: draw rasters; report features.
7. Verify: tests and clippy on Windows and Linux (WSL), both toolkits on Xvfb
   (Settings page screenshots, charts in both modes, HiDPI scale 2), measure CPU
   vs native per toolkit and confirm the Linux default.
8. README (Settings, Linux section), this plan, commit.

## Findings / gotchas

- Neither plugin sends `event::SCALE`; the host's comment says "Scale is the
  plugin's business". Host-side rasterizing needs the scale per paint.
- The scratch prototype rasterized at scale 1.0; at scale 2 that would draw
  blurry charts. Rasterize in device pixels and place the texture at the DIP rect.
- Other thread's numbers (WSL, Xvfb, software GSK): History frame, on equal terms
  (both into an image), Cairo 1.9 ms vs `ChartRaster` 1.3 ms; whole-process CPU
  History 146 % (raster) vs 152-162 % (Cairo), List 30-40 % vs 22-33 % (noisy).

## Progress log

- [x] Audit; plan written.
- [x] 2. ot-paint chart runs (`2b06a82`): `find_runs` moved, `ChartRaster::draw_run`,
  parity test against the Windows CPU path.
- [x] 3. ot-ui `Capabilities` (`capabilities.rs`): cards and sections ask it;
  `ChartDrawing::Native`; per-renderer texts; Linux-shaped tests.
- [x] 4. `Start::capabilities`; Windows passes `Capabilities::WINDOWS`; the Linux
  host passes a first Linux set (only `Native` charts, so no chart card yet).
- [ ] 5. ABI v3 + host
- [ ] 6. Plugins
- [ ] 7. Verify + measure
- [ ] 8. Docs, commit

## Open questions for the user

None yet.

## Things not to do

- Don't touch `plans/linux-cpu-cost.md` or `%TEMP%\ot-scratch-wt`: another
  thread's.
- Don't build a GPU chart path for Linux here; that is the other thread's open
  question to the user.
- Don't rasterize in the plugins (two implementations, one of them C++).
