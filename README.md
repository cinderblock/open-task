# open-task

A fully open source, very high performance, lightweight task manager.

The goal is feature parity with [TMOG](https://tmog.org/), including everything it
paywalls, with the depth of Sysinternals Process Explorer and TMOG's "why is my
computer slow?" diagnostics, in a native app that stays out of the way of the machine
it is measuring.

**Status: early, but real.** On Windows it opens a native window with two pages: live
CPU and memory graphs over a sortable process table (a flat list or a process tree),
and a Performance page with a chart and the numbers for the CPU, memory, each disk
and each network connection. Everything
is drawn with Direct2D over a Mica backdrop. Linux and macOS compile, run headless,
and measure nothing yet.

## Using it

**Pages:** the rail down the left edge switches between **Processes** and
**Performance**, with **Settings** at its bottom and the version above it, which is
also the update button (see [Updates](#updates)). It shows labels when the window is
wide and icons alone when it is narrow; the button at its top flips that.
**Ctrl+Tab** and **Ctrl+Shift+Tab** step through the pages, and **Ctrl+1**,
**Ctrl+2** jump to one. Typing (or **Ctrl+F**) on any page goes to the process
search. Settings are kept per user, in `HKCU\Software\open-task`.

**Performance** lists the devices down its left side, each with a small live graph
and its headline number; Up and Down, or a click, pick one. The CPU pane shows its
utilization, either as one graph or as one small graph per logical processor (the
**Overall / Logical processors** switch; on a processor with performance and
efficiency cores the two kinds get different colors), then utilization, the current
clock speed, processes, threads, handles and up time, next to the processor's name,
base speed, sockets, cores, logical processors and cache sizes. The current speed is
the base clock scaled by each processor's performance counter, the way Task Manager
computes it, so a boosting chip reads well above its base. The Memory pane shows
memory in use over time; how physical memory divides into in use, modified (written,
waiting to reach disk), standby (cached, reclaimable) and free; and in use,
available, committed against the commit limit, cached, and the paged and non-paged
kernel pools. "In use" counts modified pages, since they are not available; the bar
shows them separately. Each **disk** gets its active time (the share of time it
had work outstanding) and its read and write rates on one chart, with average
response time, capacity, and whether it is an SSD; each **network connection** gets
what it received and sent, with its link speed. Connections are the ones a person
would call connections: physical adapters, Hyper-V `vEthernet` ports and VPNs such
as Tailscale, not the WAN miniports and virtual-switch internals Windows keeps
underneath. Rate charts scale themselves to the busiest moment of the last hour.
The list scrolls when there are more devices than fit. Every graph on the page, the
small ones in the list included, shares one hover line.

**Charts:** the CPU and memory graphs share a log-scale time axis, labeled
`1h 10m 1m 10s now` underneath. The newest sample is on the right edge; the last
ten seconds take about a third of the width, the last minute half, and the rest
of the hour is compressed into the left end. Recent history is drawn sample by
sample. Older stretches are summarized, with a faint band from the lowest to the
highest value, so a short spike stays visible after it has been averaged. Point
at either graph and a hairline marks the same moment in both, with each graph's
value and how long ago it was; where a point summarizes several samples, the
readout gives the average and the peak.

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

**A table that holds still.** Sorting by numbers that change every second would
reshuffle the rows every second, so the order has some give. A row changes place
only when its value moves clearly away from the one it was last sorted by: for CPU
by more than a point or 15 %, whichever is more; for memory by 2 %. Rows at 0.8 %
and 1.1 % no longer trade places on noise, at the price that neighbours within that
margin can show slightly out of order. While the pointer is over the table the order
does not change at all ("Order held" shows above it), so a row cannot move out from
under a click; new processes still appear where they belong, exited ones go, and the
table re-sorts the moment the pointer leaves. When a re-sort moves the selected
row, the table scrolls with it so it stays where it was on screen. Rows slide to
their new places instead of jumping; that follows Windows' animation effects setting
until you set it yourself on the **Settings** page, which says whenever Windows has
animation effects off, whichever way you set the switch.

**Space** pauses the display, as in Process Explorer: the table, the cards and every
graph stay as they were, and the title says "(paused)". Sampling carries on
underneath, so when Space resumes nothing is missing from the graphs. In the search
field Space is just a space.

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
| `ot-core` | Sampling thread, multi-resolution history, lock-free snapshot publication. |
| `ot-paint` | Portable draw-command layer: geometry, colors, text styles, display list. |
| `ot-ui` | UI-agnostic view models: theme, pages, navigation rail, virtualized table, charts. |
| `ot-record` | Flight Recorder: record and replay a session (planned). |
| `ot-update` | Self-updater: finds signed releases, downloads, verifies and installs them. |
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
`open-task --headless` in a terminal. A newer installer upgrades in place, and
open-task can run it for you (see [Updates](#updates)).

Silent install, from an elevated prompt:
`open-task-vX.Y.Z-windows-setup.exe /VERYSILENT /NORESTART`. If you cannot elevate,
`/CURRENTUSER` installs into your own profile instead, without the protection above.

The installer and the binaries are not code-signed yet, so SmartScreen asks you to
confirm the first run.

The bare binaries for every platform are on the same release as `.zip` / `.tar.gz`
with a `SHA256SUMS` file. Linux and macOS have no installer because there is nothing
to install yet beyond the headless probe stub.

## Updates

The version is at the bottom of the rail, above Settings: `v0.3.0` for a release,
the commit (`25c2e9c-dirty`) for a build from source. It is also the update button. A
click looks on GitHub for a newer release and, if there is one, downloads and
verifies it; then the button says **Install v0.3.0**, and a click on that runs the
installer, which closes open-task, updates it (with an administrator prompt for the
usual all-users install) and starts the new version. The Settings page has the same
button with the full version and a sentence on what it is doing.

By default open-task checks when it starts and then once a day, and only says so: it
downloads on its own only if you turn on **Download updates automatically**, and it
installs only when you click. **Check for updates automatically** turns the checks
off. A check fetches two small files from github.com, the latest release's signed
checksums, and sends nothing but the request for them.

Every release's `SHA256SUMS` is signed with
[minisign](https://jedisct1.github.io/minisign/), and open-task has the public key
built in (`minisign.pub` in this repository). It installs nothing that does not match
a signed checksum, and it never offers a version older than the one running.
Releases from before the updater (v0.2.1 and earlier) are not signed. To check a
download yourself:

```
minisign -Vm SHA256SUMS -P RWQFLL2dwtZY9DF+FMseD0gj8++iXgURRbysZlwxPPzFowjJEgtnWcAx
sha256sum --check --ignore-missing SHA256SUMS
```

Only a copy the installer put in place updates itself. Any other copy (a zip,
`cargo install`, your own build) is told about a new release, and the button opens
the release's page.

From a terminal, `open-task --version` prints the version and
`open-task --check-update` runs a check and prints what it found.

## Build

Requires a stable Rust toolchain.

```
cargo run --release
```

Opens the window on Windows. The theme and title bar follow the Windows app mode
setting, including live changes; `--theme dark` or `--theme light` overrides it.
`--view tree` starts with the process tree instead of the list, and
`--page performance` on the Performance page.
`--headless --passes 5` prints a few passes of live system state to the terminal
instead and exits (the processor and its caches, the top processes by CPU with
their owning user, the busiest
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
`-AppArgs "--view tree"` captures the tree and `-AppArgs "--page performance"` the
Performance page. `-Click "x,y"` clicks first, at a point read off an earlier
screenshot. It only ever captures and closes the
instance it launched, so an installed copy can keep running.

The build names itself from git: a clean checkout of a release tag is that release
(`0.2.1`); anything else carries the commit, `git describe` style
(`0.2.1-21-g25c2e9c`), with `-dirty` for uncommitted changes. On Windows the exe
carries it as its version resource too (Properties > Details).

## Releases

CI builds every push. Pushing a tag `vX.Y.Z` builds Windows (x64, ARM64), Linux (x64,
ARM64) and macOS (Apple silicon, Intel) and publishes a GitHub release with a
`SHA256SUMS` file and its minisign signature, `SHA256SUMS.minisig`, which is what the
updater trusts. The tag must match the workspace version in `Cargo.toml`, and each
binary must report it. Signing needs the repository secret `MINISIGN_SECRET_KEY`, the
secret half of `minisign.pub`; without it the release fails rather than publish
something the updater cannot verify.

To try the updater against a local copy of a release, build with
`OT_UPDATE_FEED=http://127.0.0.1:8000/releases` and `OT_UPDATE_PUBLIC_KEY` set to a
throwaway minisign public key; only a binary built that way is affected.

## License

MIT. See `LICENSE`.
