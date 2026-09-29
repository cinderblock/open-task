# A logo and app icon for open-task

> **Status:** active · **Started:** 2026-09-29 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md`.

## Goal

The user asked: "can we make a logo for ourselves?" open-task has no logo or icon
at all today: the exe carries only a version resource, the window uses the default
application icon, the installer and uninstaller use Inno Setup's, the README has
no header image. Make a logo that says something true about the app, then wire it
everywhere Windows shows an icon.

## Environment / context

- No icon files in the repo. `.gitattributes` already marks `*.ico`, `*.icns`,
  `*.png` binary.
- `crates/ot-app/build.rs` writes the Windows `.res` by hand (no resource
  compiler) and passes it to the linker; it holds only `VERSIONINFO` now. Icons
  go in the same `.res` as `RT_ICON` entries plus one `RT_GROUP_ICON` directory.
- The window class (`ot-shell-win/src/window.rs`) sets no `hIcon`.
- Installer: `installer/windows/open-task.iss` (Inno Setup). No `SetupIconFile`;
  `UninstallDisplayIcon={app}\open-task.exe` (so the exe's icon shows in
  Installed apps once the exe has one).
- Tools here: Python 3.12 with Pillow 12.1; no SVG rasterizer until `cargo install
  resvg` (installed 2026-09-29 for this).
- Brand colors already in the app (`ot-ui/src/theme.rs`): accent / CPU cyan-blue
  `#60CDFF` (dark theme) and `#005FB8` (light), memory purple `#C59CFF`, heat
  orange `#FFB454`.
- Other threads work in this tree (see `plans/chart-hover-readout.md`,
  `plans/self-update.md`); commit only our own paths with `git commit --only`.

## Decisions already made (don't re-ask)

1. **The master is SVG**, rasterized with `resvg`; the rendered PNGs and the `.ico`
   are committed, so neither CI nor a contributor's build needs `resvg`.
2. **Small sizes get their own drawing** where the big one would turn to mush
   (16, 20, 24, 32 px), as Microsoft's icon guidance asks, rather than a scaled-down
   256.

## Plan / steps

1. **[current]** Draft concepts, render a comparison sheet (256 down to 16 px, on
   dark and light), put them to the user.
2. Refine the chosen one; hand-tune the small sizes.
3. Assets: `assets/logo/` (SVG masters, PNGs, `open-task.ico`), a render script.
4. Exe icon resource from `build.rs`; window `hIcon`/`hIconSm`; installer
   `SetupIconFile`; README header.
5. Verify: Explorer, taskbar, Alt+Tab, title bar, installer, Installed apps;
   checks on three targets; commit.

## Findings / gotchas

- Concept drafts live in `target/logo/*.svg` (scratch, untracked); `python
  target/logo/sheet.py` renders `target/logo/sheet.png`, every concept at 256, 64,
  48, 32, 24 and 16 px on the dark and light backgrounds a taskbar can have.
- First round, five concepts:
  - **A, log bars**: bars whose widths shrink leftward on the charts' log axis,
    one hot bar. Clean but reads as a generic bar-chart icon; the log spacing is
    not noticeable; mush at 16-24 px.
  - **B, open ring**: an "o" left open on the right, a pulse running through it
    and out of the opening. By far the strongest silhouette; still reads at 16 px
    on both backgrounds. Risk: pulse-in-a-circle is a common health-app motif.
  - **C, treemap**: shares of the machine as nested rectangles in a tile. Reads,
    but like a generic dashboard or app-grid icon. (First draft clipped a cell
    into a wedge at the rounded corner; cells now sit inside the tile.)
  - **D, log pulse**: a line chart on the real log axis with the 10m / 1m / 10s
    grid. The idea shows at 256 px only; scribble below 48.
  - **E, icicle**: rows of shares. **Reads as a calculator keypad** at every size.

## Progress log

- [x] Concepts drafted (A-E) and rendered; `resvg` 0.48.1 installed with `cargo
      install resvg --locked`.
- [ ] User picks a direction.

## Open questions for the user

1. Which concept (or which mix)? Recommendation: B, refined so the pulse's
   spikes sit on the log axis (crowded on the left, spread on the right), which
   makes the one generic element ours.

## Things not to do

- Do not resemble the Microsoft logo (four equal squares) or Task Manager's icon.
- Do not stage other threads' files.
