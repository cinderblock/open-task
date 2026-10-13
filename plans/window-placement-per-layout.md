# Window placement remembered per monitor layout

> **Status:** done · **Started:** 2026-10-13 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

When open-task closes, remember where its window was (position, size, maximized)
for the monitor arrangement it was on, and open there next time that arrangement
is connected. A laptop docked to two screens and the same laptop on its own each
keep their own place.

## Environment / context

- Window creation: `crates/ot-shell-win/src/window.rs` `run()` (`CreateWindowExW`
  with `CW_USEDEFAULT` and `INITIAL_SIZE`, shown with `SW_SHOWDEFAULT`).
- Close: `close()` in the same file, already saves the view layout
  (`prefs::save_layout`) and runs for `WM_CLOSE`, including session end and the
  install-on-close path.
- Settings storage: `crates/ot-shell-win/src/prefs.rs`, `HKCU\Software\open-task`.

## Decisions already made (don't re-ask)

1. **"Layout" = the set of monitors' rectangles and DPIs**, sorted. Monitor device
   names (`\\.\DISPLAY3`) are left out: Windows renumbers them. Work areas are
   left out: moving the taskbar is not a new layout.
2. **Stored under `HKCU\Software\open-task\Placements`**, one `REG_SZ` per layout:
   value name is the layout key, data is `left,top,right,bottom,maximized` from
   `GetWindowPlacement`'s `rcNormalPosition`.
3. **Saved on close and at the end of a user move/resize** (`WM_EXITSIZEMOVE`), so
   a crash or kill does not lose it, and a layout change while running does not
   overwrite the old layout's entry with where Windows pushed the window.
4. **Applied right after `CreateWindowExW`, before the window is shown and before
   the state pointer is set**, so the `WM_DPICHANGED` a move across monitors
   causes goes to `DefWindowProc` and does not rescale the saved size. The DPI and
   swap-chain size are read after it.
5. Minimized at close is restored normal (or maximized if it was maximized
   before minimizing). A shortcut's "Run: Minimized" is still honored.
6. No live re-placement on `WM_DISPLAYCHANGE`: Windows 11 already does that for
   open windows ("Remember window locations based on monitor connection").

## Plan / steps

1. Pure layout key + placement encoding with tests (`placement.rs`). ← current
2. Registry read/write in `prefs.rs`.
3. Hook into `run()`, `close()`, `WM_EXITSIZEMOVE`.
4. README; check by hand: move, close, reopen; maximized; other DPI monitor.

## Findings / gotchas

## Progress log

- [x] Steps 1–4

## Things not to do

- Don't apply the placement after `SetWindowLongPtrW`: `WM_DPICHANGED` would then
  hit our handler and resize the window to the suggested (rescaled) rect.
