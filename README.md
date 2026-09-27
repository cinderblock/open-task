# open-task

A fully open source, very high performance, lightweight task manager.

The goal is feature parity with [TMOG](https://tmog.org/), including everything it
paywalls, with the depth of Sysinternals Process Explorer and TMOG's "why is my
computer slow?" diagnostics, in a native app that stays out of the way of the machine
it is measuring.

**Status: early, but real.** On Windows it opens a native window with live CPU and
memory graphs over a sortable process table, as a flat list or a process tree, drawn
with Direct2D over a Mica backdrop. Linux and macOS compile, run headless, and measure
nothing yet.

## Using it

The process table has two arrangements, switched with the **List / Tree** control
above it or with **Ctrl+T** (Process Explorer's binding):

- **List** is one flat list sorted by the column you click. It starts sorted by CPU.
- **Tree** nests children under their parents. Siblings are sorted by the same
  column, using subtree totals, so the branch that is busy rises to the top at every
  level even when its root is idle. Click a row's chevron, or press **Left** and
  **Right**, to collapse and expand; a collapsed row shows the totals of everything
  underneath it and how many processes that is. Left on a leaf moves to its parent;
  Right on an expanded row moves to its first child.

Switching keeps the selected process: pick the hottest thing in the list, press
Ctrl+T, and the tree opens with that process revealed and centered, ancestors
expanded. Clicking the control for the arrangement you are already in re-reveals the
selection. The strip above the table shows the selected process's ancestry
(`wininit.exe › services.exe › svchost.exe`) in both arrangements, so the flat list
and the tree read as one thing.

Parent links are real identities, not just parent PIDs: a process whose parent
exited, and whose PID was then recycled by a stranger, is shown as a root rather than
adopted by the stranger.

**Search:** just type. Printable keys go to the filter field above the table whether
or not it has focus; **Ctrl+F** puts the caret there, **Escape** clears it, and the
arrow keys keep walking the table while you type. The filter matches name, PID,
user, image path and command line, case-insensitively. In tree mode the ancestors of
a match stay listed, dimmed, so the match keeps its place in the hierarchy.

**Columns:** Name, PID, User, CPU %, Memory, Working set, Disk read, Disk write,
Threads, Handles, Command line. Drag a header divider to resize; **Shift+wheel** (or
a tilt wheel) scrolls sideways when the columns are wider than the window. The user,
image path and command line come from a limited-rights handle to each process; a
process that refuses even that (another user's, from an unelevated open-task) still
shows its image path, but its user and command line stay blank.

**Right-click a process** for **End task**, **End process tree** (the process and
everything under it, parents first) and **Open file location**. **Delete** and
**Shift+Delete** are the keyboard shortcuts for the first two; the menu key or
**Shift+F10** opens the menu for the selected row. Ending a process asks first, with
No as the default. Every kill checks the process's creation time against the one in
the table before it acts, so a PID that has been recycled since the last sample is
never killed by mistake.

**Inside a process.** In tree mode every process can be opened one level further.
Under it sit the **services** it hosts (from the service control manager, so a
`svchost.exe` row reads `svchost.exe (DcomLaunch) · BrokerInfrastructure,
DcomLaunch, PlugPlay, …` and the search finds it by any of those names), a
**Threads (n)** group, and, under each service, the threads that work for it. These
inner rows start folded, so the tree reads as before until you press **Right** on a
process or click its chevron. Each thread row shows its id in the PID column, its
scheduler state and wait reason in the User column, and its CPU.

When open-task runs as administrator it also reads each thread's *service tag*, the
mark the service control manager puts on the threads a service creates. Then a
service row carries the CPU of its own threads, the host's Name cell lists its
services busiest first (`BrokerInfrastructure 98%, PlugPlay, …`), and the
question "which service in this svchost is spinning?" is answered by the table
itself. Threads no service claims stay in the Threads group; a process that refuses
to be read (a protected process such as the Defender engine) lists its services
without numbers.

**Sample CPU for 5 s** (context menu, administrator only) answers the next two
questions: *what code* and *on whose behalf*. It starts a short Event Tracing for
Windows kernel profile, the same read-only sampling `xperf` uses, and shows under
the process the modules its threads were executing (`bisrv.dll 71%`), the same
breakdown under each thread, and, for a service with a known trace provider
(Background Tasks Infrastructure, Plug and Play, Task Scheduler, Windows Update),
the clients it was working for: the package and background task, the device, the
scheduled task, the update. Nothing is suspended, attached or written; only a
private trace session is opened and closed. The sample rows stay until the next
sample or until the process exits. The same sample is available from a terminal as
`open-task --headless --sample <pid> [--seconds 5]`.

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

## Install

**Windows:** download `open-task-vX.Y.Z-windows-setup.exe` from the
[latest release](https://github.com/cinderblock/open-task/releases/latest) and run it.
It installs for all users into Program Files, which needs an administrator prompt,
for the install and for every update. That is on purpose: a task manager is the kind
of program you run elevated, and it should not live somewhere any program running
as you could overwrite it. It installs the x64 or ARM64 build to match the machine,
adds a Start Menu entry and an uninstaller, and can put `open-task` on the PATH for
`open-task --headless` in a terminal. A newer installer upgrades in place.

Silent install, from an elevated prompt:
`open-task-vX.Y.Z-windows-setup.exe /VERYSILENT /NORESTART`. If you cannot elevate,
`/CURRENTUSER` installs into your own profile instead, without the protection above.

The installer and the binaries are not code-signed yet, so SmartScreen asks you to
confirm the first run.

The bare binaries for every platform are on the same release as `.zip` / `.tar.gz`
with a `SHA256SUMS` file. Linux and macOS have no installer because there is nothing
to install yet beyond the headless probe stub.

## Build

Requires a stable Rust toolchain.

```
cargo run --release
```

Opens the window on Windows. The theme and title bar follow the Windows app mode
setting, including live changes; `--theme dark` or `--theme light` overrides it.
`--view tree` starts with the process tree instead of the list.
`--headless --passes 5` prints a few passes of live system state to the terminal
instead and exits (the top processes by CPU with their owning user, the busiest
service hosts with the services they run, and the hottest threads with the service
each works for); that is the only mode on Linux and macOS for now, where it exits
with code 3 ("no probe on this platform yet"). `--headless --sample <pid>
[--seconds N]` takes one CPU sample of a process and prints it (see "Sample CPU"
above; needs an elevated terminal).

To install the current tree as `open-task` on your `PATH`:

```
cargo install --path crates/ot-app --locked
```

Release builds have no console window; `--headless` still prints when run from a
terminal.

`scripts/screenshot.ps1` launches the app, screenshots its window to
`target/screenshot.png`, and closes it. Handy for checking a rendering change;
`-AppArgs "--view tree"` captures the tree. It only ever captures and closes the
instance it launched, so an installed copy can keep running.

## Releases

CI builds every push. Pushing a tag `vX.Y.Z` builds Windows (x64, ARM64), Linux (x64,
ARM64) and macOS (Apple silicon, Intel) and publishes a GitHub release with a
`SHA256SUMS` file. The tag must match the workspace version in `Cargo.toml`.

## License

MIT. See `LICENSE`.
