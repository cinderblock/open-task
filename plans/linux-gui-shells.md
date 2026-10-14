# Linux GUI: GTK 4 and Qt 6 shells in one binary

## Goal

Give Linux the window. Today `open-task` on Linux has a complete probe
(`plans/linux-probe.md`) but no GUI shell, so it only runs headless. The user asked
(2026-10-13) for **both a Qt and a GTK shell, in a single binary — "boil the
ocean"**: the full app on Linux, looking and behaving native on KDE/LXQt (Qt) and on
GNOME and everything else (GTK), from one downloaded file that still runs headless on
a server with neither toolkit installed.

## Environment / context

- Windows 11 dev box. Linux testing in WSL2 `Ubuntu` 24.04 (see
  `plans/linux-probe.md` for the build recipe: build from the Windows checkout with
  `CARGO_TARGET_DIR=~/.cache/open-task-target`, run multi-line scripts via a file).
- **WSLg is present**: `WAYLAND_DISPLAY=wayland-0`, `DISPLAY=:0`, so both a Wayland
  and an X11 path can be exercised locally.
- WSL Ubuntu 24.04 offers `libgtk-4-dev` 4.14.5 and `qt6-base-dev` 6.4.2. **Neither is
  installed, nor `pkg-config`, and `sudo` needs a password** — the user has to install
  them (open question 1). `g++` is present.
- Release CI builds `x86_64-unknown-linux-gnu` on `ubuntu-latest` and
  `aarch64-unknown-linux-gnu` natively on `ubuntu-24.04-arm` (no cross-compiling,
  so C++ against arm64 Qt headers is just `apt install`). Binaries are glibc, so
  `dlopen` works (it would not from a static musl binary).
- Supported floor today: glibc 2.34 = Ubuntu 22.04 / Debian 12 / RHEL 9. Those ship
  **GTK 4.6** (22.04), 4.8 (Debian 12), 4.6 (RHEL 9) and **Qt 6.2** (22.04), 6.4
  (Debian 12), 6.6 (RHEL 9).

### What already exists (why this is feasible)

- `ot-ui` (28k lines) is the whole app as UI-agnostic view models: `App::handle(UiEvent)
  -> Reaction { repaint, effect }`, `App::paint(&mut DisplayList)`.
- `ot-paint` is the portable display list: `Clear, FillRect, FillRoundRect,
  StrokeRect, StrokeRoundRect, Line, Polyline, FillPolygon, Band, Graph, Text, Icon,
  Image, PushClip, PopClip`, with point/string arenas, layers for partial repaint
  (`splice`), damage tracking, and a SIMD CPU chart rasterizer (`ChartRaster`).
  Text is measured by the backend (ellipsis, field scrolling, caret are flags).
- `ot-shell-win` (9.6k lines) is the Windows executor + window + platform effects.
  `gfx.rs` (2.3k) runs the display list on Direct2D; `window.rs` (2.2k) is the window,
  input and the App/sampler/updater glue; the rest is platform effects (actions,
  prefs, placement, tray, run dialog, task-manager replacement, single instance).
- `ot-ui::Effect` is the full list of things a shell must perform.

## Decisions already made (don't re-ask)

- **Both toolkits, one file.** User's request, 2026-10-13.
- **Neither toolkit is a link-time dependency of `open-task`.** The binary must still
  start, and run `--headless`, on a machine with no GTK or Qt. So each toolkit lives
  in a **plugin shared object, embedded in the binary** (`include_bytes!`), written
  to a `memfd_create` file at startup and `dlopen`ed from `/proc/self/fd/N`. Nothing
  is extracted to disk. Dev builds without embedded plugins load them from beside
  the executable (`libot_shell_gtk.so`, `libot_shell_qt.so`), which is where a plain
  `cargo build` puts them.
- **One C ABI for both plugins** (`ot-shell-abi`: a hand-written C header and a
  `#[repr(C)]` Rust mirror, with layout tests on both sides). Qt has no C API, so the
  Qt plugin is C++ behind that ABI; making GTK speak the same ABI keeps the host
  ignorant of which toolkit it drives. The ABI carries a version number checked at
  load; a mismatch is a load failure, never a crash.
- **The plugins are dumb; the app stays in the host.** `App`, the sampler, settings,
  the updater and every effect that is not toolkit UI run in the main binary
  (`ot-shell-linux`). A plugin provides: the main loop, a window, painting a display
  list, input events, DPI scale, a thread-safe wakeup, popup menus, message/confirm
  dialogs, a text-input dialog, a save-file dialog, clipboard, and the desktop's
  theme/fonts/accent when the toolkit knows them. That keeps the ABI small and stable
  and means each plugin is a few thousand lines, not a second app.
- **Native painting per toolkit, not a shared Rust rasterizer.** Architecture
  principle 3 (`plans/open-task-architecture.md`): custom-drawn content on the
  platform's own 2D primitives, native chrome. GTK paints with GSK (`GtkSnapshot`)
  plus Pango for text and Cairo where GSK lacks a primitive at our GTK floor; Qt
  paints with `QPainter` and `QTextLayout`/`QStaticText`. Text then honours the
  desktop's fonts, hinting and scaling exactly as the rest of the desktop does.
  Charts (`Band`/`Graph`) can go through `ot_paint::ChartRaster` into a texture
  (`GdkMemoryTexture` / `QImage`) in both, so the heaviest primitive has one
  implementation.
- **The display list crosses the ABI zero-copy.** `DrawCmd` and its field types
  become `#[repr(C)]` (`#[repr(C, u32)]` for the enum), so the host hands the plugin
  pointers to the command array and the two arenas. No per-frame conversion.
- **Toolkit choice:** `--toolkit gtk|qt` or `OPEN_TASK_TOOLKIT`, else a saved setting,
  else by desktop: `XDG_CURRENT_DESKTOP` containing KDE, LXQt, DDE/Deepin or UKUI →
  Qt, anything else → GTK. If the chosen toolkit's libraries are missing the other is
  tried; if neither loads, or there is no `WAYLAND_DISPLAY`/`DISPLAY`, it says why on
  stderr and runs headless as today.
- **Build the plugins against the oldest supported toolkit** (Qt 6.2, GTK 4.6, i.e.
  an Ubuntu 22.04 build environment) so they run on every newer one; newer features
  (`GskPath` in GTK 4.14, `QStyleHints::colorScheme` in Qt 6.5) are looked up at run
  time and used when present.
- **GTK plugin in Rust (`gtk4-rs`, `cdylib`); Qt plugin in C++** in a `cdylib` crate
  whose `build.rs` compiles it with `cc` against `pkg-config Qt6Widgets`. Both build
  with `cargo build -p …`; no CMake.
- **Toolkit-neutral desktop integration lives in the host**, so it is written once:
  dark/light and accent from the XDG desktop portal (`org.freedesktop.appearance`),
  the tray as a StatusNotifierItem, reveal-in-file-manager through
  `org.freedesktop.FileManager1`, URLs through the OpenURI portal / `xdg-open`,
  settings as a file under `$XDG_CONFIG_HOME/open-task/`, single instance.

## Architecture

```
open-task (one ELF, no GTK/Qt in DT_NEEDED)
├─ ot-app            main(); on Linux, GUI unless --headless or no display
├─ ot-shell-linux    host: pick toolkit, memfd+dlopen, ABI handshake, drive App,
│                    sampler/updater glue, effects, portal/D-Bus, prefs, tray
├─ ot-shell-abi      C header + repr(C) mirror: host↔plugin vtables, events
├─ embedded: libot_shell_gtk.so   gtk4-rs: window, GSK+Pango executor, dialogs
└─ embedded: libot_shell_qt.so    C++/Qt6 Widgets: window, QPainter executor, dialogs
```

The plugin owns the main loop (`g_application_run` / `QApplication::exec`); the host
gets callbacks on the toolkit thread. Sampler and updater threads reach the UI thread
through the plugin's `wakeup()` (`g_idle_add` / queued `QMetaObject::invokeMethod`).

Much of `ot-shell-win/src/window.rs` is App/sampler/updater glue rather than Win32.
Step 1 extracts that glue into a toolkit-neutral place (a new `ot-shell-core`, or
`ot-ui` where it fits) so Windows and Linux share it rather than copy it. That
refactor is behaviour-preserving for Windows and is verified on Windows first.

## Plan / steps

**Current step: 7 (packaging and CI) is committed but unverified: the workflows
have not run (nothing is pushed). Then steps 5 and 6 (the remaining effects and
desktop integration).**

0. Plan (this document).
1. **Shared glue.** Extract the toolkit-neutral parts of `window.rs` (App lifetime,
   sampler wiring, snapshot hand-off, updater thread, frame pacing decisions) into
   `ot-shell-core`. Windows behaves identically; clippy/tests pass on all targets.
2. **ABI + loader.** `ot-paint` types `#[repr(C)]`; `ot-shell-abi` (header, Rust
   mirror, layout tests); `ot-shell-linux` with toolkit selection, memfd/dlopen,
   version handshake, headless fallback; `ot-app` runs it on Linux. A fake
   in-process plugin drives a whole session through the real callbacks in a test.
3. **GTK plugin, MVP.** Window, executor for every `DrawCmd`, input → events, scale,
   wakeup. Exit criterion: the Processes, Performance and every other page run live
   under GTK on WSLg (Wayland and X11), visually matching the Windows layout.
4. **Qt plugin, MVP.** Same exit criterion under Qt.
5. **Effects.** Menus, terminate confirmation, process actions (probe already does
   them), open file location, properties (file manager), copy, open URL, record
   (save dialog), replay transport, settings persistence, window size/state per
   monitor layout (positions only on X11; Wayland does not let a client place
   itself), theme/accent from the portal with live changes, Run dialog.
6. **Desktop integration.** Tray (SNI), single instance and raise, run as root via
   `pkexec` relaunch, `PickWindow` and `SwitchTo` on X11 (unavailable on Wayland,
   hidden there), services start/stop over systemd D-Bus, session sign-out over
   logind, "Replace Task Manager" as a desktop shortcut binding (KDE: Ctrl+Esc;
   GNOME: custom shortcut), self-update of the single binary.
7. **Packaging and CI.** Release builds plugins first, then `ot-app` with
   `OT_SHELL_GTK_SO`/`OT_SHELL_QT_SO` pointing at them to embed; plugins built in an
   Ubuntu 22.04 container on both arches. CI renders every page under
   `QT_QPA_PLATFORM=offscreen` and GTK on a headless Wayland/Xvfb, and checks that
   the binary has no GTK/Qt in `DT_NEEDED` and still runs `--headless` without them.
   A `.desktop` file and icon (for menus/tray) are written on first GUI run or by an
   `--install-desktop-entry` flag.
8. **Docs.** README (Linux section, Install, "Not on Linux yet"),
   `plans/open-task-architecture.md` step 10, `plans/linux-probe.md` "Not done".

## Architecture as built (steps 1–2)

- **`ot-shell-core::Shell`** owns `App`, the feed, the updater and worker threads.
  A host calls `handle(UiEvent, &mut Vec<Out>)` and carries out each `Out`:
  `Repaint`, `Title`, `Notice`, `Ask` (menu, confirm, record path, Run dialog),
  `Do(HostEffect)`, `Settings`, `Meters` (tray), `RefreshIn`, `Close`. Answers go
  back through `menu_chosen(id)`, `confirmed(Confirmed)`, `record_to(path)`,
  `set_last_run`. Nothing in the core blocks, so Win32's modal loops and GTK 4's
  async dialogs both fit. Workers wake the host through a `Waker` and the host
  calls `drain` on its UI thread. Windows uses one `WM_APP_WAKE` for all of it.
- **`settings_store`** in the core is the one list of setting names; the Windows
  registry and the Linux file (`$XDG_CONFIG_HOME/open-task/settings`, `Name=value`
  lines) both use it.
- **`ot-shell-abi/include/ot_shell.h`** is the ABI (version 1). Entry symbol
  `ot_shell_plugin`. Plugin owns the loop and the one window (global state, one
  window per process). Plugin calls never block or nest a loop; answers come back
  through `OtHost` callbacks with a token. `OtPlugin.wake` is the only
  thread-safe call, and it may be called *before* `run` (the shell's first
  snapshot flag fires during `Shell::start`), so a plugin must remember it and
  deliver it once the loop runs.
- **Layout guards:** Rust `const` asserts plus header `_Static_assert`s on the same
  numbers (`DrawCmd` 72 bytes, `TextCmd` 68, `OtEvent` 40), and a test that
  compiles the header against Rust's `offset_of!` values when a C compiler is
  present (verified to fail on a wrong size).
- **Host (`ot-shell-linux`)**: `host.rs` maps callbacks to `Shell` and `Out`s to
  plugin calls with the state unborrowed; `loader.rs` does memfd+dlopen and the
  ABI check; `select.rs` the toolkit order; `keys.rs` the Windows key bindings;
  `desktop.rs` `xdg-open`, `FileManager1` over `dbus-send`, launching, and the
  portal's colour scheme; `prefs.rs` the settings file.
- **`ot-app`**: on Linux, GUI when `WAYLAND_DISPLAY` or `DISPLAY` is set and
  `--headless` is not; `--toolkit gtk|qt` / `OPEN_TASK_TOOLKIT`. If no plugin opens
  a window it prints each toolkit's reason and runs headless with a fresh probe.
  `build.rs` embeds the `.so` files named by `OT_SHELL_GTK_SO` / `OT_SHELL_QT_SO`
  into `$OUT_DIR/embedded_shells.rs`; with neither set the binary looks beside
  itself for `libot_shell_gtk.so` / `libot_shell_qt.so`.

## Findings / gotchas

- `ot-paint` is `#![forbid(unsafe_code)]`; `#[repr(C)]` needs no unsafe, so that holds.
- Wayland: no client-side window positioning, no global window picking, no
  activating another app's window. These features must degrade, not fail.
- **`ot-shell-win`'s `the_clipboard_takes_text` fails with "Access is denied" on
  this machine, on unmodified HEAD too** (checked in a temp worktree at `bfb9e68`).
  Environmental (something holds the clipboard), not from this work.
- **Screen captures of the app come out black** from `CopyFromScreen` when the
  window is not on the visible desktop; `PrintWindow(hwnd, hdc, 2)` on the window
  itself works, including on a popup menu (class `#32768`). `scripts/screenshot.ps1`
  only sends left clicks; a right-click/menu check needed a one-off driver
  (WM_RBUTTONDOWN + WM_CONTEXTMENU, then find `#32768` owned by the pid).
- **Python on this box writes CRLF in text mode** (it is Windows Python), and a
  backslash-n escape typed into a Bash-tool heredoc arrives as a real line break,
  even inside a quoted heredoc. Edit with `newline=''`, run scripts with
  `python -X utf8` (the plan has non-ASCII dashes), and use the Edit tool for any
  line that needs an escape sequence.
- **Linux command lines in the probe are argv joined by spaces, unquoted**
  (`process.rs`), so Restart on Linux re-splits them (`desktop::split`) and loses
  an argument that had a space in it. Carrying argv in `ProcessAction::Restart`
  would fix it; not done.
- **Idle cost on Xvfb (release build): about 21% of one core.** The view
  animates continuously (chart clock ticks, rows slide after each re-sort), so
  frames run at 60/s, by design and as on Windows. Host paint ~30 us/frame,
  snapshot building ~0.5 ms for ~400 commands; the rest is GSK on `llvmpipe`
  (software, because WSL/Xvfb has no GPU). Turning SmoothCharts off did not
  change it. Needs a measurement on real hardware before deciding whether to
  cache or split snapshots further. A background window now gets half the
  frames (cannot be seen on Xvfb, where the window always counts as active).
- **Cross-clippy for Linux from Windows must `--exclude ot-shell-gtk`**: its
  `gtk4-sys` build script needs `pkg-config` and the GTK headers, which only
  the Linux environments (WSL, CI) have.
- WSL Ubuntu had no GTK 4 or Qt 6 and no Docker/Podman. `sudo` asks for a
  password, but `wsl.exe -d Ubuntu -u root bash -c '...'` runs as root without
  one; that is how the toolkit packages were installed.

## Progress log

- [x] Plan written (2026-10-13).
- [x] 1. Shared glue extracted: `ot-shell-core` (`399f06b`). Windows verified in
      the real app: live data, Startup apps and Connections lists (worker → waker
      → drain), context menu with greyed items and submenus, clean close.
- [x] 2. ABI + loader: `ot-shell-abi`, `ot-shell-linux`, `ot-app` wiring; settings
      codec moved to the core. 21 tests on Linux (WSL) including memfd loading and
      a fake-plugin session; clippy clean on Windows, Linux and macOS targets; the
      Linux binary links no GTK/Qt/GLib (`ldd`) and falls back to headless.
- [x] 3. GTK MVP (`b9cee6e`, `299c543`): every `DrawCmd`, input, menus, dialogs,
      Run box. Checked on Xvfb: processes list and tree, Performance page, context
      menu, Run dialog (Escape closes), End-task question (No default). Also on
      WSLg Wayland (first launch, screenshot). Idle cost: see findings.
- [x] 4. Qt MVP (`02bb62e`): C++ against Qt 6 Widgets, no moc. Same checks as
      GTK on Xvfb. Icons need Qt's SVG image plugin (`qt6-svg-plugins`); an icon
      theme (Breeze, else Adwaita) is set when no desktop set one.
- [ ] 5. Effects
- [ ] 6. Desktop integration
- [~] 7. Packaging and CI (`b4e7650`): `scripts/build-linux.sh` (plugins, then
      `ot-app` embedding them), `scripts/linux-gui-smoke.sh` (the binary alone:
      no toolkit in NEEDED, headless without a display, each toolkit's window on
      Xvfb takes a click and is not blank), `scripts/linux-deps.sh`. CI's Linux job
      and the release's Linux builds run in `ubuntu:22.04`; the release checks the
      glibc floor (2.34) and keeps smoke screenshots out of its assets. Verified
      locally (release build 3.7 MB, plugins ~400 KB each, smoke OK, glibc <= 2.34);
      **the workflows themselves have not run yet.** `.desktop` file not done.
- [~] 8. Docs: README Linux section, install note and crate table updated.

### Testing the Linux GUI (how it is done here)

- **Xvfb + xdotool + ImageMagick** in WSL is the reliable harness (and what CI
  will use): a private `Xvfb :77`, `GDK_BACKEND=x11`, `xdotool mousemove X Y
  click 1`, `import -window root shot.png`. No window manager, so dialogs sit at
  0,0 and popovers show a black shadow area; neither happens on a real desktop.
- **WSLg is not drivable**: XTEST *keys* reach an XWayland window but pointer
  clicks do not, and `import -window root` fails on its rootless X server. WSLg
  windows can be captured from Windows with `PrintWindow` on class `RAIL_WINDOW`
  (each popup is its own `RAIL_WINDOW`).
- Driver scripts used this session lived in `%TEMP%\otdrive` (`xdrive.sh`,
  `cpu.sh`, `grab.ps1`); they are scratch, to be turned into a CI script in step 7.

## Open questions for the user

1. ~~Install the toolkit dev packages in WSL.~~ Done 2026-10-14 at the user's
   request, as root through `wsl.exe -d Ubuntu -u root` (no password needed):
   `pkg-config libgtk-4-dev qt6-base-dev` → GTK 4.14.5, Qt 6.4.2.
2. Building against 22.04's GTK 4.6 / Qt 6.2 locally would need a 22.04 WSL distro or
   a container (Docker/Podman). Recommendation: develop against 24.04's packages
   locally and let CI's 22.04 build enforce the floor; no local action needed.

3. **Push, so CI runs the new Linux job?** It is the first run of the container
   jobs (`ubuntu:22.04`, x64 in CI; x64 and arm64 in releases). Nothing is pushed
   yet; recommendation: push `master` and watch CI.
4. **Cut a release (v0.17.0) so a downloaded binary has the window?** That is
   what makes the GUI reach someone who downloads it. Recommendation: after CI
   passes, through the `publishing` skill.

## Things not to do

- Don't link `gtk4`/`Qt6` into `open-task` itself, even behind a feature flag in
  release; one `DT_NEEDED` breaks headless servers.
- Don't extract plugins to `/tmp` or `~/.cache`; `memfd_create` avoids stale files,
  races and noexec mounts.
- Don't fork `ot-ui` behaviour per toolkit. A difference in look belongs in the
  plugin executor; a difference in behaviour belongs nowhere.
- Don't build the Linux target into the Windows `target/` from WSL.
