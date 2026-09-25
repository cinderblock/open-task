# open-task — Architecture & Build Plan

> **Status:** active · **Started:** 2026-09-24 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`, remote `origin` = github.com/cinderblock/open-task)

## Goal

A fully open source, **very high performance, lightweight, full-featured GUI task manager**.
Feature parity with [TMOG](https://tmog.org/) (including everything it paywalls behind the
$39.95 Pro tier) and the depth of Sysinternals Process Explorer, with TMOG's automatic
diagnostics ("why is my computer slow?") rather than just raw numbers.

Windows is the primary target. Linux and macOS get compiling stubs now and real
implementations later.

Non-negotiables from the user:
- Fully open source.
- CI builds automatically; publishes releases on `v*` tags.
- App self-updates, with **automatic** updates opt-in (check-and-notify is the default).

## The app we are competing with

TMOG is by Dave Plummer, who wrote the original Windows Task Manager. Its published
architecture is worth copying because it is the right one:

| Layer | TMOG | Notes |
| --- | --- | --- |
| Core | one shared **C++20 measuring core** | platform-agnostic sampling/model |
| Windows | **C++ and Win32**, drawing with **Direct2D** | custom-drawn, not XAML |
| Linux | **C++ with Qt 6** | |
| macOS | **Swift and AppKit** | |

Their release notes also mention "shared drawing across all three platforms", implying a
portable paint layer above the per-platform 2D API. TMOG targets macOS 14+, Windows 11
(x64 + ARM64), and Linux desktop (x64 + ARM64).

**TMOG's 12 views** (our parity checklist): Summary, Performance, Processes, System Info,
Startup Apps, Users, Services, Power & Freq, Connections, Installed Apps, Disk Space,
Benchmarks. Plus: live process hierarchy, process identity stable across PID recycling,
P-core/E-core distinction with color coding, memory pressure/compression/cache/swap trend,
instantaneous watts + power state history, CPU hotspot and thermal sensor history, and a
**Flight Recorder** that records and replays seven telemetry panels in a portable
`.tmogtrace` file.

**Pro-gated in TMOG** (therefore deliberately free in open-task): Power & Freq, Flight
Recorder, Connections, Installed Apps, Drivers, Disk Space, Benchmarks.

## Decisions already made (don't re-ask)

1. **Language: Rust.** `windows-rs` is Microsoft's own complete binding set, so Direct2D,
   DirectWrite, DirectComposition and the `Nt*` query surface are all reachable. Single
   static binary, no vcpkg/CMake matrix, trivial cross-compilation, and memory safety
   matters a lot when parsing semi-documented kernel structures. C++ would only be chosen
   to share code with TMOG, which we are not doing.
2. **Not Tauri / not a webview.** See "Why not a webview" below. This is the one place the
   kneejerk answer is wrong.
3. **Custom-drawn content on native 2D primitives, native chrome.** Windows shell draws
   with Direct2D + DirectWrite + DirectComposition. Menus, dialogs, context menus, tray and
   accessibility use real native APIs.
4. **Hard boundary between the sampling core and the UI.** The core emits immutable
   snapshots + deltas; the UI is a consumer. This is what makes Flight Recorder, headless
   mode, and remote monitoring nearly free later.
5. **Branch is `master`.** Per the user's standing preference.
6. **License: MIT.** User decision, 2026-09-25.
7. **GitHub remote: `cinderblock/open-task`, public.** User decision, 2026-09-25.
8. **Charts use a log-scale time axis** like TMOG's history graphs: the last few
   seconds at full resolution on the right, hours compressed toward the left. User
   likes this a lot and wants to *prototype* a log-scale Y axis too. Design
   consequence: history must be stored with timestamps at multiple resolutions, not
   as a single fixed-rate ring. First sparklines may be linear; the storage must not
   paint us into a corner.

## Why not a webview (the decisive argument)

Webviews are not slow at *rendering*; Chromium's compositor is excellent and a virtualized
table of 40 visible rows is trivial for it. The disqualifying problems are elsewhere:

- **Memory floor.** A minimal WebView2 app sits in the ~80–150 MB RSS range across its
  process tree. For *the app whose entire job is telling you what is wasting your RAM*,
  that is a credibility problem before it is a performance problem. Process Explorer is
  ~10–20 MB.
- **Cold start.** WebView2 initialization is hundreds of milliseconds. A task manager is
  launched *because something is already wrong*, often when the machine is thrashing. It
  has to come up instantly under load — that is its worst case and its most important one.
- **IPC marshalling.** 1000 processes × 25 fields at 4 Hz is a lot of JSON per second.
  Solvable with binary framing, but it is pure overhead we would be designing around.
- **Linux divergence.** Tauri uses WebKitGTK there, not Chromium. It is a genuinely
  different and worse engine, and it becomes a permanent maintenance tax.

Native custom drawing instead gives us: real ClearType subpixel text via DirectWrite,
genuine Mica/Acrylic Win11 backdrops, ~10–25 MB RSS, sub-100 ms cold start, and a ~5–8 MB
binary.

## UI building blocks (the user's actual question)

Bottom-up, these are the layers available to any GUI app:

| Layer | Windows | macOS | Linux / portable |
| --- | --- | --- | --- |
| GPU | D3D11/12 | Metal | Vulkan / OpenGL |
| 2D raster | **Direct2D** | Core Graphics | Skia, Vello, Cairo, Blend2D |
| Text shaping + AA | **DirectWrite** | Core Text | HarfBuzz + FreeType + fontconfig |
| Compositor | **DirectComposition** / Windows.UI.Composition | Core Animation | (none in OS; Wayland compositor) |
| Widgets | Win32 comctl32, WPF, WinUI 3 | AppKit / SwiftUI | Qt, GTK |
| Webview | WebView2 | WKWebView | WebKitGTK |

The key realization for *this* app: a task manager's hard widget is a **virtualized
multi-column table with per-cell coloring, tree grouping and live sort**, plus **dozens of
live sparklines**. No toolkit gives you that for free — in every single toolkit, including
Qt and WinUI, you end up custom-drawing it. So choosing a heavyweight widget toolkit buys
us comparatively little and costs us footprint. We draw the content area ourselves and use
native APIs for the chrome around it.

## Architecture

```
open-task/
├─ crates/
│  ├─ ot-model/      # pure data types + units. No I/O, no platform code.
│  ├─ ot-probe/      # trait SystemProbe + per-OS impls (windows real; linux/macos stubs)
│  ├─ ot-core/       # sampling scheduler, ring buffers, derived metrics, diagnostics engine
│  ├─ ot-paint/      # portable retained scene + draw-command layer; backend trait
│  ├─ ot-ui/         # UI-agnostic view models: columns, sort/filter, virtualization, charts
│  ├─ ot-shell-win/  # Win32 + Direct2D/DirectWrite/DComp  (real)
│  ├─ ot-shell-gtk/  # Linux shell                          (stub)
│  ├─ ot-shell-mac/  # macOS shell                          (stub)
│  ├─ ot-record/     # Flight Recorder: record/replay the delta stream
│  ├─ ot-update/     # self-updater; auto-update opt-in
│  └─ ot-app/        # the binary; wires core + shell
├─ .github/workflows/
├─ docs/
└─ plans/
```

**Data flow:** `ot-probe` samples on a dedicated thread → `ot-core` folds into ring buffers
and computes derived metrics → publishes an immutable snapshot + delta → `ot-ui` turns it
into a view model → `ot-shell-*` paints. The UI renders at display refresh, fully
decoupled from the sampling cadence.

Because the delta stream is a first-class serializable thing, Flight Recorder is just
"write the stream to a file" and replay is "feed the file in where the probe normally
goes." Same for a future headless/remote mode.

## Plan / steps

1. ~~Repo skeleton, workspace, plan doc, CI, licensing.~~ Done.
2. ~~`ot-model` + `ot-probe` trait, with a real Windows process/CPU/memory probe and
   compiling stubs for Linux/macOS.~~ Done (first cut; lazy per-process details pending).
3. ~~`ot-core` sampling loop, ring buffers, snapshot publication.~~ Done. Deltas come
   with the Flight Recorder.
4. ~~`ot-paint` draw-command layer + Direct2D backend; window with Mica backdrop.~~ Done.
5. ~~Virtualized table widget + sparkline widget (the two hard ones).~~ Done (first cut:
   linear-axis sparkline, no column resize/reorder yet).
6. **[current]** Processes view end-to-end (basic version works), then Performance,
   then the rest of the 12. Next concrete items: process details (image path, command
   line, user), tree/grouping, search filter, column resize, context menu with End task.
7. Diagnostics engine ("why is my computer slow").
8. Flight Recorder.
9. Self-updater + signed releases.
10. Linux (Qt 6 or GTK 4 — decide later) and macOS shells.

## Findings / gotchas

- TMOG's Windows build is **Win32 + Direct2D**, *not* WinUI 3 or XAML. The person who
  wrote Task Manager chose custom drawing on Direct2D in 2026. That is a strong signal.
- **Research agents all died on a weekly API rate limit (2026-09-24 evening).** The two
  landscape reports (Windows native primitives; Rust GUI toolkits 2026) were never
  produced. The architecture decision stands on the TMOG data point plus prior
  knowledge. Re-run that research when the limit resets if a second opinion is wanted
  before the Direct2D shell work starts; it is not blocking. The Windows-side question
  list survived in `plans/windows-native-gui-rust-2026.md`.
- **`windows` crate 0.62 hides most of `SYSTEM_PROCESS_INFORMATION` behind `Reserved`
  fields** because its metadata comes from the public `winternl.h`. `CreateTime`,
  `KernelTime`, `UserTime`, `InheritedFromUniqueProcessId`, and the six I/O counters are
  all in `Reserved1[48]`, `Reserved2`, `Reserved7[6]`. Fixed by declaring the real
  layout in `crates/ot-probe/src/imp/windows/nt.rs` with `const` offset assertions
  pinning it against the SDK struct. This is what System Informer does too.
- In `windows` 0.62 the struct types live in `Win32::System::WindowsProgramming`, while
  the `NtQuerySystemInformation` function and the `SystemProcessInformation` class
  constants live in `Wdk::System::SystemInformation`. Both features are needed.
- **Run cross-target clippy locally before pushing.** The first CI run failed on Linux
  and macOS for a `dead_code` lint on a helper only the Windows probe calls. Both
  `x86_64-unknown-linux-gnu` and `x86_64-apple-darwin` targets are installed here, and
  `cargo clippy --workspace --all-targets --target <t> -- -D warnings` type-checks without
  linking, so it reproduces the CI failure exactly in seconds. See "Verify before pushing".
- `ID2D1Factory1::CreatePathGeometry` returns `ID2D1PathGeometry1`, not the base type.
- `ID2D1SimplifiedGeometrySink` (with `BeginFigure`/`EndFigure`/`AddLines`) lives in
  `Direct2D::Common`; `ID2D1GeometrySink::AddLine` is in `Direct2D`. Method calls resolve
  through `Deref`, so only the imports care.
- `&raw const expr` requires a place expression; `&raw const rectf(rect)` does not
  compile. Bind to a local first. Plain `&` coerces to `*const` but trips clippy's
  `borrow_as_ptr` under pedantic.
- **ClearType is wrong over a translucent surface.** With a premultiplied-alpha
  composition swap chain and Mica behind it, ClearType produces colour fringes. The shell
  sets grayscale text antialiasing, which is what WinUI does over Mica too. If an opaque
  mode is ever added, switch back to ClearType there.
- **`FindWindowW(class, $null)` from PowerShell does not find the window**; PowerShell
  marshals `$null` as an empty title. Pass the real title. Bit `scripts/screenshot.ps1`.
- Rustfmt reflows long lines, so python string-anchored patches can miss after a `cargo
  fmt`. Anchor on the post-format text, or patch before formatting.
- `SystemProcessorPerformanceInformation` only returns processor group 0 (max 64
  logical processors). Machines with more need `SystemProcessorPerformanceInformationEx`
  per group. Deferred; noted in the probe's module docs.
- `init.defaultBranch` is set at git **system** level to `master`; it does not appear in
  the global or repo config. `git config --get init.defaultBranch` is the check that works.
- Local toolchain confirmed: rustc 1.97.1, cargo 1.97.1, gh 2.83.2, git 2.45.2, cmake 3.30.1.
  Installed targets already include `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`.

## Progress log

- [x] Confirm nothing on disk; greenfield.
- [x] Identify TMOG and its architecture.
- [x] `git init` on `master`.
- [x] Write this plan.
- [ ] ~~Research report: Windows native UI primitives~~ — agent rate-limited, not produced.
- [ ] ~~Research report: Rust GUI landscape 2026~~ — agent rate-limited, not produced.
- [x] Cargo workspace + crate skeletons (`ot-model`, `ot-probe`, `ot-core`, `ot-app` real;
      `ot-paint`, `ot-ui`, `ot-record`, `ot-update` placeholders).
- [x] CI: `ci.yml` (fmt, clippy, test, headless smoke on 4 targets) and `release.yml`
      (6 targets, zip/tar.gz, SHA256SUMS, GitHub release on `v*` tag). **Unverified until
      pushed** — no remote exists yet.
- [x] Windows probe: processes (CPU%, WS, private, disk I/O, threads, handles, parent,
      start time), per-core CPU with P/E-core classes, memory (total/avail/cached/commit).
- [x] `ot-core`: sampler thread, `ArcSwap` snapshot slot, `Ring<T>` history buffer.
- [x] Headless `ot-app` that prints live snapshots; doubles as the CI smoke test.
- [x] README, GPL-3.0 LICENSE, `.gitattributes` (CRLF), rustfmt config.
- [ ] Windows probe: image path / command line / user (lazy `OpenProcess` path).
- [ ] Windows probe: per-core frequency via PDH `% Processor Performance`.
- [ ] Windows probe: processor groups > 0.
- [x] `ot-paint`: DIP geometry, colors, text styles, arena-backed `DisplayList`.
- [x] `ot-ui`: theme, allocation-free number formatting, virtualized sortable `Table`
      with id-based selection and a row-visibility hook, peak-preserving `sparkline`,
      root `App` view with CPU and memory cards over the process table.
- [x] `ot-core`: `Timeline` of timestamped series; `Sampler::start_with_notify`.
- [x] `ot-shell-win`: Win32 window, `WS_EX_NOREDIRECTIONBITMAP` + DirectComposition
      swap chain, Direct2D + DirectWrite renderer with a hashed text-layout cache, Mica
      via DWM, dark title bar, per-monitor-v2 DPI, mouse/wheel/keyboard, sampler wake-up
      by posted message, zero redraws when idle. **Verified visually** on 2026-09-25 via
      `scripts/screenshot.ps1` (screenshot kept out of the repo: it shows the user's
      process list).
- [x] Cross-target clippy clean on Windows, Linux and macOS targets locally.
- [x] Remote `cinderblock/open-task` created and pushed. First CI run failed on Linux and
      macOS (dead-code lint); fixed in the next commit.
- [ ] CI visual smoke: GitHub's Windows runners have a desktop session, so
      `scripts/screenshot.ps1` could run there (WARP fallback covers the missing GPU) and
      upload the PNG as an artifact. Worth doing once the UI settles.
- [ ] Idle-CPU: the app should redraw nothing when unchanged (it does) and cost ~0% CPU
      between samples. Not yet measured with a profiler; do this before optimizing anything.
- [ ] Release-mode console: the binary is still a console-subsystem app so `--headless`
      prints and logs show. Switch to `windows_subsystem = "windows"` for release once
      there is an `AttachConsole` path for headless mode.

## Open questions for the user

1. ~~License.~~ Resolved: MIT.
2. ~~Repo name / GitHub org.~~ Resolved: `cinderblock/open-task`, public.
3. **Code signing.** Releases are far more useful signed (SmartScreen). That needs a
   certificate and secrets. Unsigned for now.
   Separately, the **self-updater must verify a detached signature** on every download
   with a public key compiled into the binary, so a compromised GitHub account cannot
   push a malicious update to every user. Recommend minisign. Generating that keypair
   is a user action; the private key must never be in the repo. Until it exists, the
   updater can check for and *announce* new versions but should not auto-install.
4. **Linux toolkit.** Qt 6 (TMOG's choice, better dense-table story, better on KDE) vs
   GTK 4 + libadwaita (much better Rust bindings, more "native" on GNOME). Only a stub is
   needed now, so this is deferred, not blocking.

## Verify before pushing

Run all of these locally; CI runs the same set on real runners and takes minutes to
tell you what these say in seconds.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-apple-darwin -- -D warnings
cargo test --workspace
cargo run -- --headless --passes 2
pwsh -File scripts/screenshot.ps1      # then look at target/screenshot.png
```

## Things not to do

- Do not reach for Tauri/Electron "just to get the UI up". The footprint is the whole point
  of the project.
- Do not use a heavyweight widget toolkit for the data-dense content area. The table is
  custom-drawn in every toolkit anyway.
- Do not couple sampling cadence to frame rate. They are independent on purpose.
- Do not run the binary in CI without `--headless`. On Windows it opens the window and
  the job hangs until GitHub's six-hour limit. Happened on run 36191451129 (2026-09-25);
  the smoke step now passes `--headless` and has a step-level `timeout-minutes: 2`.
- Do not use GNU `timeout` in CI shell steps: macOS runners do not have it (exit 127,
  run 36193278405). Use the step's `timeout-minutes` instead; it works on every runner.
- Do not push to a remote or create a GitHub repo without explicit per-action approval.
- Do not rename `master`.
