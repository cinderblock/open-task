# open-task

A fully open source, very high performance, lightweight task manager.

The goal is feature parity with [TMOG](https://tmog.org/), including everything it
paywalls, with the depth of Sysinternals Process Explorer and TMOG's "why is my
computer slow?" diagnostics, in a native app that stays out of the way of the machine
it is measuring.

**Status: early, but real.** On Windows it opens a native window with live CPU and
memory graphs over a sortable process table, drawn with Direct2D over a Mica backdrop.
Linux and macOS compile, run headless, and measure nothing yet.

## Why not a webview

A task manager's whole job is telling you what is wasting your RAM and CPU. A webview
shell puts the app itself near the top of that list, and it starts slowest exactly when
the machine is thrashing, which is when you launched it. So the content area is drawn
directly on each platform's native 2D API (Direct2D on Windows), with real native
menus, dialogs and accessibility around it. Target footprint is Process Explorer
class: tens of megabytes, not hundreds.

## Layout

| Crate | Role |
| --- | --- |
| `ot-model` | Pure data types. No I/O, no platform code. |
| `ot-probe` | Platform sampling. Windows is real; Linux and macOS are stubs. |
| `ot-core` | Sampling thread, history ring buffers, lock-free snapshot publication. |
| `ot-paint` | Portable draw-command layer: geometry, colors, text styles, display list. |
| `ot-ui` | UI-agnostic view models: theme, virtualized table, sparklines, root view. |
| `ot-record` | Flight Recorder: record and replay a session (planned). |
| `ot-update` | Self-updater. Automatic updates are opt-in (planned). |
| `ot-shell-win` | Windows shell: Win32 window, DirectComposition swap chain, Direct2D + DirectWrite renderer. |
| `ot-app` | The binary. |

The sampling cadence and the UI frame rate are independent. The core publishes
immutable snapshots; readers never block the sampler and the sampler never waits for
a reader.

## Build

Requires a stable Rust toolchain.

```
cargo run --release
```

Opens the window on Windows. The theme and title bar follow the Windows app mode
setting, including live changes; `--theme dark` or `--theme light` overrides it.
`--headless --passes 5` prints a few passes of live system state to the terminal
instead and exits; that is the only mode on Linux and macOS for now, where it exits
with code 3 ("no probe on this platform yet").

To install the current tree as `open-task` on your `PATH`:

```
cargo install --path crates/ot-app --locked
```

Release builds have no console window; `--headless` still prints when run from a
terminal.

`scripts/screenshot.ps1` launches the app, screenshots its window to
`target/screenshot.png`, and closes it. Handy for checking a rendering change.

## Releases

CI builds every push. Pushing a tag `vX.Y.Z` builds Windows (x64, ARM64), Linux (x64,
ARM64) and macOS (Apple silicon, Intel) and publishes a GitHub release with a
`SHA256SUMS` file. The tag must match the workspace version in `Cargo.toml`.

## License

MIT. See `LICENSE`.
