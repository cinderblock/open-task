# Process menu: a header naming the process, icons, and dark mode

> **Status:** done, not released · **Started:** 2026-10-14 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

The user, 2026-10-14, after v0.17.3: "the right click menu is still missing the
name of the application at the top (with some extra metadata). The menu also holds
room for icons, and none of the entries have them. also, dark mode".

## Environment / context

- The menu is native: the view returns `Effect::Menu { at, entries }`
  (`crates/ot-ui/src/view.rs`), `ot-shell-core` turns entries into `MenuNode`s
  (`crates/ot-shell-core/src/menu.rs`) and `Ask::Menu`, and the Windows shell shows
  it with `TrackPopupMenuEx` (`crates/ot-shell-win/src/actions.rs::show_menu`,
  called from `window.rs::answer`). Linux plugins get a flat `abi::MenuItem` list
  (`crates/ot-shell-linux/src/host.rs`).
- Icons by meaning: `ot_paint::Icon`, mapped to Segoe Fluent Icons / MDL2 Assets
  codepoints in `ot-shell-win/src/gfx.rs::glyph`, freedesktop names in
  `ot-shell-gtk/src/draw.rs` and `ot-shell-qt/src/plugin.cpp` (index order).
- Real-app capture: `target/menu-shot.ps1` (scratch, not committed): launches the
  debug build with `--theme dark|light`, types a filter (a PID works), selects a
  row, posts `WM_CONTEXTMENU` with lParam -1, and captures the screen around the
  window. Logs to `target/menu/err-<theme>.txt`.

## Decisions (user, 2026-10-14 — don't re-ask)

1. **Keep native menus, extend them** (not a self-drawn popup, not full owner-draw).
2. **Header shows:** the application's name (bold), then PID and user, the
   description / publisher, and the image path.

## What was built

- `ot-ui`: `MenuEntry::Header(MenuHeader)` first in the process menu (title =
  description else name; lines = `name · PID n · user`, company; path).
  `MenuAction::icon()` gives each action its icon, so every menu using an action
  gets the same icon. `MenuEntry::Submenu` has an `icon` (Set priority).
- `ot-paint::Icon`: 15 new meanings (Tree, EndTask, EndTree, Restart, Efficiency,
  Priority, Affinity, SwitchTo, OpenFolder, SearchOnline, Properties, Copy, Dump,
  WaitChain, Stopwatch). Glyphs picked from a rendered contact sheet; all exist in
  both Segoe Fluent Icons and Segoe MDL2 Assets (checked with fontTools).
- `ot-shell-core`: `MenuNode::Item/Submenu { icon }`, `MenuNode::Header`; Set
  affinity gets `Icon::Affinity`.
- Windows (`ot-shell-win/src/menu_art.rs`): dark menus through uxtheme ordinals
  #135 `SetPreferredAppMode` (ForceDark/ForceLight), #133
  `AllowDarkModeForWindow`, #104 `RefreshImmersiveColorPolicyState`, #136
  `FlushMenuThemes`, applied with the title bar's theme. Item icons are the glyph
  drawn by Direct2D into a WIC bitmap, copied into a DIB, set as `hbmpItem`
  (cached per glyph/size/ink; a dim ink for disabled items). The header is one
  bitmap (program icon via `icons::extract`, title semibold in the menu font,
  details and path dimmer, the path cut before its last component) added as a
  disabled `MF_BITMAP` item.
- Linux: the header goes in as disabled text items (no icons: the plugin ABI has
  no field for them).

## Findings / gotchas

- **An owner-drawn item turns the whole menu's theme off**, dark mode with it: the
  first header (`MF_OWNERDRAW` + `WM_MEASUREITEM`/`WM_DRAWITEM`) gave a light,
  classic-looking menu even with ForceDark; removing just that item made it dark.
  Hence the header as a bitmap item.
- `SetPreferredAppMode` + `FlushMenuThemes` alone did not darken the menu in the
  app (a bare WinForms probe did go dark); adding `AllowDarkModeForWindow(hwnd)`
  and the policy refresh, and dropping the owner-drawn item, did.
- **The icon and the check mark share the gutter**: an item with `hbmpItem` shows
  the bitmap instead of its check. Checked items (Efficiency mode on) therefore
  get no icon, so the check shows.
- Some processes have no readable path (other sessions, protected): the header
  then uses the generic program icon (`icons::extract` on a made-up `program.exe`).
- Paths and user may take a few seconds after launch to fill in; capture after
  ~6 s.
- The Bash tool collapses `\\u` in heredocs: write Rust `\u{b7}` escapes with the
  Edit tool, or build the backslash with `chr(92)` in Python.

## Progress log

- [x] Model, shell-core, Linux header, Windows dark/icons/header.
- [x] Tests: view (header in the process menu, entries shifted), shell-core
      (icons from actions, header passes through). Workspace tests; clippy
      `-D warnings` on Windows and Linux targets.
- [x] Real app, dark and light, a process with a path, one without, one with
      efficiency mode on (check shown).
- [x] README.

## Follow-ups (not started)

- Icons in the Linux menus would need an icon field in `abi::MenuItem` (GTK and
  Qt plugins both).

## Things not to do

- Don't owner-draw menu items (loses the theme; see Findings).
- Don't publish from the CLI; releases go through the tag-triggered workflow.
- `sed -i` writes LF on CRLF files; edit with Python, keep CRLF.
