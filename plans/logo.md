# A logo and app icon for open-task

> **Status:** done, shipped in v0.3.1 · **Started:** 2026-09-29 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
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
3. **User, 2026-09-29: "let's do the open ring one"** (concept B). Refined as
   recommended: the pulse's older wiggles crowd to the left and the newest spike
   stands wide, like the log time axis; a dot at the end is "now".
4. **Colors:** ring gradient `#0A5FC0` to `#4CC6FF` (bottom left to top right),
   pulse and dot `#FF9A3C`. No backplate: the silhouette carries it on light and
   dark taskbars alike.
5. **Which drawing serves which size:** `open-task-16.svg` for 16; `-24` for 20
   and 24; `-32` for 30 to 40; `open-task.svg` from 48 up (the log wiggles blur
   below that). The `.ico` holds all fourteen sizes Windows' guidance lists (16,
   20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256), each a PNG.
6. **The exe icon is written by `build.rs`** into the same hand-made `.res` as the
   version resource: `RT_ICON` 1..14 plus `RT_GROUP_ICON` 1. The window loads group
   1 at the sizes its DPI wants (`GetSystemMetricsForDpi`) and sets them with
   `WM_SETICON`, again after a DPI change.
7. **Installer:** `SetupIconFile` always; the wizard's corner image
   (`WizardSmallImageFile`, seven PNGs for 100% to 250%) only when the compiler
   is Inno Setup 6.5.2 or newer (PNG support), which GitHub's Windows runners
   (6.7.1) are.

## Plan / steps

1. ~~Draft concepts, render a comparison sheet, put them to the user.~~
2. ~~Refine the chosen one; hand-tune the small sizes.~~
3. ~~Assets: `assets/logo/` (SVGs, `open-task.ico`, `open-task-512.png`,
   `installer/wizard-*.png`), `scripts/render-logo.ps1`.~~
4. ~~Exe icon from `build.rs`; window icons; installer icon and wizard image;
   README.~~
5. ~~Verify and commit.~~
6. Later, if wanted: a GitHub social preview image (1280x640; set in the repo's
   settings page, not through the API); macOS `.icns` and Linux icons with those
   shells.

## Findings / gotchas

- Concept drafts live in `target/logo/*.svg` (scratch, untracked); `python
  target/logo/sheet.py` renders `target/logo/sheet.png`, every concept at 256, 64,
  48, 32, 24 and 16 px on the dark and light backgrounds a taskbar can have.
- **Refining B:** spacing the pulse on the log axis at first made the last stroke
  a long ramp and lost the heartbeat; the final version keeps a sharp spike and
  crowds the older wiggles to its left (`target/logo/sheet-ring2.png`).
- **16 px is its own problem:** with the spike's two strokes 1.6 px apart they
  merge into a plus sign (or, with the dot, a key); a wide zigzag with no flat
  lead-in reads as a pulse (`target/logo/zoom-16b.png`).
- **An `.ico` and an icon resource hold the images identically**; only the
  directory differs (file offset versus resource id in the last field), so
  `build.rs` copies the images and rewrites the directory.
- **Checked in the real thing:** Explorer's extraction of the exe icon gives the
  drawing made for each size (resource ids 1, 5, 8, 14 at 16, 32, 48, 256); the
  running window reports a 32 px big and 16 px small icon and shows the logo in the
  title bar; the test installer (`target/installer-test`, built with the portable
  Inno 6.7.3) has the icon and shows the wizard image (`target/logo/setup-wizard.png`).
- **Portable Inno's `ISCC.exe` has no version resource** (0.0.0.0), and `ISCC /?`
  prints no version either; `#pragma message` is silenced by the build script's
  `/Qp`. The runner version came from GitHub's runner-image readmes instead.
- Python patch scripts through a Bash heredoc lose backslashes (`\a` became a
  BEL character in the `.iss`); write them with the Write tool, and build Windows
  paths with `chr(92)`.
- Rendering is deterministic: re-running `render-logo.ps1` reproduces the `.ico`
  and PNGs byte for byte.
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
- [x] User picked B (the open ring).
- [x] Refined; small sizes drawn by hand; assets and render script.
- [x] Exe icon, window icons, installer icon and wizard image, README.
- [x] Verified (Explorer extraction, running window, test installer); tests and
      clippy on three targets.
- [x] **Shipped in v0.3.1** (release commit `7916bf6`, tag pushed 2026-09-29,
      published 14:51:51 PDT / 21:51:51 UTC; CI and Release green, only the known
      Node 20 and ubuntu-latest notices). Checked from the published assets:
      SHA256SUMS match, the minisign signature verifies against `minisign.pub`,
      the x64 exe reports v0.3.1 and carries the icon at every size, and
      `setup.exe` has the icon and shows the wizard image (the runner's Inno 6.7.1).

## Open questions for the user

1. ~~Does the refined logo work for you?~~ User, 2026-09-29: "i did tell you the
   open ring was ok". Picking concept B was the approval; this question should not
   have stayed open. (Changing it later is a re-render: edit the SVGs, run
   `scripts/render-logo.ps1`.)

## Things not to do

- Do not resemble the Microsoft logo (four equal squares) or Task Manager's icon.
- Do not stage other threads' files.
