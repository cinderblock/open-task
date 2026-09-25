# open-task

A fully open source, very high performance, lightweight task manager.

The goal is feature parity with [TMOG](https://tmog.org/), including everything it
paywalls, with the depth of Sysinternals Process Explorer and TMOG's "why is my
computer slow?" diagnostics, in a native app that stays out of the way of the machine
it is measuring.

**Status: early scaffold.** The measuring core samples real data on Windows and prints
it to a terminal. There is no GUI yet. Linux and macOS compile but measure nothing.

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
| `ot-paint` | Portable draw-command layer (planned). |
| `ot-ui` | UI-agnostic view models: columns, sort, filter, virtualization (planned). |
| `ot-record` | Flight Recorder: record and replay a session (planned). |
| `ot-update` | Self-updater. Automatic updates are opt-in (planned). |
| `ot-app` | The binary. |

The sampling cadence and the UI frame rate are independent. The core publishes
immutable snapshots; readers never block the sampler and the sampler never waits for
a reader.

## Build

Requires a stable Rust toolchain.

```
cargo run --release -- --passes 5
```

Prints a few passes of live system state and exits. On Linux and macOS it exits with
code 3 ("no probe on this platform yet").

## Releases

CI builds every push. Pushing a tag `vX.Y.Z` builds Windows (x64, ARM64), Linux (x64,
ARM64) and macOS (Apple silicon, Intel) and publishes a GitHub release with a
`SHA256SUMS` file. The tag must match the workspace version in `Cargo.toml`.

## License

GPL-3.0-or-later. See `LICENSE`.
