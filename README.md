<img src="assets/logo/open-task.svg" width="96" align="right" alt="The open-task logo: an open ring with a pulse running out through the opening">

# open-task

A fully open source, very high performance, lightweight task manager.

The goal is feature parity with [TMOG](https://tmog.org/), including everything it
paywalls, with the depth of Sysinternals Process Explorer and TMOG's "why is my
computer slow?" diagnostics, in a native app that stays out of the way of the machine
it is measuring.

**Status: real, and at Task Manager's feature set.** On Windows it opens a native
window with eight pages: live CPU and memory graphs over a sortable process table
(a flat list, a process tree, a map or a history of who has been using the CPU),
Performance (CPU, memory, each disk, each network connection, each GPU and the
battery), Users, Services, Startup apps, Connections, Installed apps and System.
Everything is drawn with Direct2D over a Mica backdrop. Linux and macOS compile, run
headless, and measure nothing yet. What is still missing is listed under
[Not yet](#not-yet).

## Using it

**Pages:** the rail down the left edge switches between **Summary**, **Processes**,
**Performance**, **Users**, **Services**, **Startup apps**, **Connections**,
**Installed apps** and **System**, with **Settings** at its bottom and the version
above it, which is also the update button (see [Updates](#updates)). It shows labels
when the window is wide and icons alone when it is narrow; the button at its top
flips that. **Ctrl+Tab** and **Ctrl+Shift+Tab** step through the pages, and
**Ctrl+1** to **Ctrl+9** jump to one. Typing (or **Ctrl+F**) on any page goes to
that page's search. **Ctrl+N** opens **Run new task**. The page, arrangement, sort
and columns you leave open come back at the next start (`--page` and `--view` on
the command line override that). Settings are kept per user, in
`HKCU\Software\open-task`.

**Performance** lists the devices down its left side, each with a small live graph
and its headline number; Up and Down, or a click, pick one. The CPU pane shows its
utilization, either as one graph or as one small graph per logical processor (the
**Overall / Logical processors** switch; on a processor with performance and
efficiency cores the two kinds get different colors), then utilization, the current
clock speed, processes, threads, handles and up time, next to the processor's name,
base speed, sockets, cores, logical processors, whether virtualization is enabled
and a hypervisor running, and cache sizes. The current speed is
the base clock scaled by each processor's performance counter, the way Task Manager
computes it, so a boosting chip reads well above its base. The Memory pane shows
memory in use over time; how physical memory divides into in use, modified (written,
waiting to reach disk), standby (cached, reclaimable) and free; and in use,
available, committed against the commit limit, cached, and the paged and non-paged
kernel pools, with the memory's speed, slots used, form factor and what the
hardware reserves, from the firmware. "In use" counts modified pages, since they
are not available; the bar shows them separately. Each **disk** gets its active
time (the share of time it had work outstanding) and its read and write rates on
one chart, with average response time, capacity, whether it is an SSD, its bus,
and the volumes on it with their free space, file system and whether they hold
Windows or a page file; each **network connection** gets what it received and sent,
with its link speed, IP addresses, DNS suffix and MAC address. Each **GPU** gets
its utilization (the busiest engine's share, as Task Manager counts it) and its
dedicated memory over time, the busiest engines by name, shared memory, and the
driver's version and date; the counters are the same `GPU Engine` ones Task
Manager reads. A machine with a **battery** gets its charge over time, the power
going in or out, time left, and the battery's health (full against design
capacity), cycle count and chemistry. With [PawnIO](https://pawnio.eu) installed
(a signed, scriptable kernel driver; open-task ships none of its own), the CPU
pane adds the **package power** from the processor's energy counter and its
**temperature**: the package sensor on Intel, Tctl on AMD Zen, read through the
LGPL `IntelMSR` and `AMDFamily17` modules embedded from
[PawnIO.Modules](https://github.com/namazso/PawnIO.Modules). Without PawnIO the
two figures are simply absent, and the log says where to get it. Connections are the ones a person
would call connections: physical adapters, Hyper-V `vEthernet` ports and VPNs such
as Tailscale, not the WAN miniports and virtual-switch internals Windows keeps
underneath. Rate charts scale themselves to the busiest moment they show.
The list scrolls when there are more devices than fit. Every graph on the page, the
small ones in the list included, shares one hover line.

**Charts:** the CPU and memory graphs share a log-scale time axis, labeled
`1h 10m 1m 10s now` underneath. The present is on the right edge and the oldest
sample held is on the left, so a chart fills its width from a session's first
second and the axis grows with the history until it reaches the length set on the
Settings page: **How far charts reach back**, five minutes to a day, five minutes
unless changed. Older samples are dropped; the card says how many points a chart
keeps at that length and about how much memory that is for the charts on this
machine. With five minutes, the last ten seconds take about two fifths of the width
and the rest is compressed toward the left end; with an hour, the last minute takes
half. The charts scroll smoothly with time rather than stepping once a sample: the
right edge runs a little over a sample behind the newest, so each new sample slides
in from past it (**Scroll charts smoothly** under **Charts** turns this off). A
frame where only the clock moved repaints just the charts, and only the parts of
the window that changed are redrawn; behind other windows the charts move at half
the display's rate, and not at all when the window is minimized or on another
virtual desktop.
The first sample is on screen a quarter of a second after start, drawn across the
interval it measured. Recent history is drawn sample by sample. Older stretches are summarized, with a faint band from the lowest to the
highest value, so a short spike stays visible after it has been averaged. Point
at either graph and a hairline marks the same moment in both, with each graph's
value there and how long ago it was (where a point summarizes several samples, the
value is their average and the faint band their range). The time sits against the
line and keeps its width as the pointer moves: `2m 05s ago`, `25m ago`,
`1h 05m ago`. Seconds drop away from ten minutes and minutes from two hours, and
come back only once the pointer is well short of those, so a readout resting near
a boundary does not flicker between the two.

**Cycles used.** CPU % says who is busy this second: power, in watts. open-task
also keeps the other number, what each process has consumed: energy, in joules. It
counts the processor clock cycles every process uses and adds them up, and so that
the totals do not grow forever, they fade: every second each total loses 5 % (the
rate is on the **Settings** page, from 1 % to 50 %). A process that was busy lately
is at the top, one that has been steadily at work for a long time stays in view, and
a short burst shows for a while and then sinks. At a steady rate a total settles at
about twenty seconds' worth of cycles (at 5 %; a total halves in 14 s), so read it
as "what was used recently". The **Cycles** column, the Map, the usage strip and the
History all show these totals. The fading goes by the clock, not by the sample, so
the numbers are the same however often the system is sampled; a process that exits
stays in the Map and the strip until its total has faded away, and in the History
for as long as the chart reaches back.

Cycles are what Windows counts for each process, exactly, where CPU time is charged
a clock tick (15.6 ms) at a time, so a short burst that CPU % misses still shows.
They are counted at the processor's fixed base clock (1.6 billion a second for each
busy logical processor on a chip with a 1.6 GHz base speed), whatever speed the core
is running at that moment: a cycle here is a slice of time, not a unit of work done.
Two things are not seen: the cycles a process uses between its last sample and its
exit, and a process that starts and ends between two samples.

The process table has four arrangements, switched with the **List / Tree / Map /
History** control above it; **Ctrl+T** toggles List and Tree (Process Explorer's
binding), **Ctrl+M** opens the Map and **Ctrl+H** the History:

- **List** is one flat list sorted by the column you click. It starts sorted by
  Cycles: who has been using the CPU, rather than who is using it this second.
- **Tree** nests children under their parents. Siblings are sorted by the same
  column, using subtree totals, so the branch that is busy rises to the top at every
  level even when its root is idle. Click a row's chevron, or press **Left** and
  **Right**, to collapse and expand; a collapsed row shows the totals of everything
  underneath it and how many processes that is. Left on a leaf moves to its parent;
  Right on an expanded row moves to its first child.
- **Map** shows the cycles used as areas. Every process is a tile whose area is its
  total, nested by the process tree: a process whose children used a real share of
  its cycles is a frame around them, with one more tile for its own. A chain of
  processes that only launched the next (a shell, a runtime, an app) folds into one
  frame, `pwsh.exe › node.exe › electron.exe`. Color is the table's heat: how busy the
  process is right now. A process that has exited stays, dimmed and marked, until
  its total has faded. Pointing at a tile names it and gives its cycles; a click
  selects it (the ancestry line and the other arrangements keep the selection),
  right-click gives the process menu, and the search dims what does not match. The
  line under the map gives the cycles used by everything together.
- **History** shows the cycles used over the charts' reach, as a chart: a stack of
  bands, one per program, on the same time axis as the graphs above it. A program is
  every process of one name, so twelve `chrome.exe`, or the two hundred `rustc.exe`
  of a build, are one band. The switch above the chart picks what a band's thickness
  is. **Fading total** is the number the Cycles column shows, over time: a band
  swells while its program works and sags once it stops. **Rate** is the cycles the
  program was using each second: the CPU graph cut up by program, of which the
  fading total is a smoothed copy. The eight programs that take the most of the
  chart get a band and a color each, and the rest are one grey band, "Everything
  else". A program keeps its color for as long as it keeps a band, and a band
  changes hands only when a newcomer is clearly bigger, so the chart does not
  recolor itself as you watch. The legend beside it names the bands, top to bottom
  as they are stacked, with each one's value now; point at the chart and it gives
  the values at that moment instead, and the CPU and memory graphs mark the same
  moment (and the other way round). Pointing at a band, or its legend row, sets it
  off from the others; a click selects the program's busiest process, right-click
  gives its menu, and the search dims the programs it does not match.

In List, Tree and History, a thin **usage strip** between the graphs and the table
keeps the Map's answer in view: the Map folded flat into two rows. The top-level
processes run across the first row, each as wide as its share of the cycles used,
and what runs under each sits beneath it within its span; the gap a parent's children
leave is its own cycles. Chains fold and color is heat, as in the Map. Pointing at a
segment reads it out in place of the ancestry line, and a click selects that process
in the table and scrolls to it. A second click on it lets the selection go, as it
does on a selected tile of the Map or band of the History.

Switching keeps the selected process: pick the hottest thing in the list, press
Ctrl+T, and the tree opens with that process revealed and centered, ancestors
expanded. Clicking the control for the arrangement you are already in re-reveals the
selection. The line beside the control shows the selected process's ancestry
(`wininit.exe › services.exe › svchost.exe`) in every arrangement, so the flat list,
the tree and the map read as one thing.

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

**Columns:** twelve show by default: Name, PID, Status, User, CPU %, Cycles,
Memory, Working set, Disk read, Disk write, Threads, Handles. Name carries the
program's icon, as Task Manager's does, read once per program off the UI thread;
the Users and Startup apps pages show them too. Right-click the header
for the rest, every column Task Manager's Details page has: Description, Command
line, GPU %, GPU engine, Type (App, Background or Windows process), Company,
Priority, Architecture, Session, Elevated, CPU time, Started, Page faults, Peak
working set, Virtual size, Paged pool, NP pool, I/O reads, writes and other (counts
and bytes), Image path, Window title and Package name (a packaged app's full name), plus
**Reset columns**. **Show resource values as percentages**, under **Process
table** on the Settings page, shows Memory and Working set as shares of physical
memory and Disk read and Disk write as each process's share of every process's
reads or writes that interval, as Task Manager's View menu does; the order does
not change. Status says when a
process is suspended, in efficiency mode or not responding. Drag a header to move
the column, drag its divider to resize; **Shift+wheel** (or a tilt wheel) scrolls
sideways when the columns are wider than the window. In the tree, a collapsed row's
Cycles are those of everything under it, like its other numbers, and siblings sort
by them. The user, image path, command line, description and company come from a
limited-rights handle to each process; a process that refuses even that (another
user's, from an unelevated open-task) still shows its image path, but those stay
blank.

**Right-click a process** for **End task**, **End process tree** (the process and
everything under it, parents first), **Restart** (end it, then start its command
line again in its directory), **Suspend** / **Resume**, **Efficiency mode** (EcoQoS
and idle priority, as Task Manager's), **Set priority**, **Set affinity** (a check
per logical processor), **Switch to** (bring its window forward), **Open file
location**, **Search online**, **Properties** (Explorer's sheet), **Copy** (the row,
tab-separated), **Create dump file** (a full minidump in `%TEMP%`, then revealed)
**Analyze wait chain** (which thread waits on what, held by whom, across
processes, and whether that is a deadlock: the Wait Chain Traversal API, as Task
Manager uses it) and **Sample CPU for 5 s** (below). **Delete** and **Shift+Delete** are the keyboard
shortcuts for the first two; the menu key or **Shift+F10** opens the menu for the
selected row. Ending a process asks first, with No as the default. Every kill checks
the process's creation time against the one in the table before it acts, so a PID
that has been recycled since the last sample is never killed by mistake. Actions
that need rights this copy does not have are shown disabled with the reason.

**Run new task** (the button above the table, or **Ctrl+N**) is Task Manager's:
a command line, Browse, and **Create this task with administrator privileges**.
The crosshair button beside it is Process Explorer's **Find window's process**:
drag it onto any window and that window's process is selected in the table.

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

**Replace Task Manager.** The switch under **Windows** on the Settings page does what
Process Explorer's option of the same name does: afterwards **Ctrl+Shift+Esc**, Task
Manager in the taskbar's menu, the **Ctrl+Alt+Del** screen and `taskmgr` typed
anywhere all start open-task instead. It works the same way, with a `Debugger` value
under Task Manager's Image File Execution Options key in `HKLM`, so it applies to
everyone who uses the PC and changing it needs an administrator prompt (none when
open-task already runs as administrator). The switch shows what Windows actually
does, read again whenever you come back to the window: it is on only when Windows
starts this copy. When another program stands in for Task Manager (Process Explorer,
another copy of open-task) the card names it; turning the switch on takes over from
it, and turning it off only ever removes this copy's own entry. The card also says
when this copy is not installed for all users, since then it cannot stand in for
everyone. With an open-task window already open, Ctrl+Shift+Esc brings it forward,
restoring it if it was minimized, instead of opening another, as Task Manager does.
One difference: Windows starts the replacement with your ordinary rights, where Task
Manager would have elevated itself, so the administrator-only details above need
open-task run as administrator. From an elevated terminal, `open-task
--replace-task-manager` and `open-task --restore-task-manager` do the same as the
switch. Uninstalling always puts Task Manager back if it was starting that copy.

**Run as administrator** (the Settings page, under **Windows**) starts a copy with
administrator rights and closes this one. That copy reads every process's command
line and user, samples CPU, starts and stops services, and reads service tags.

**Window:** **Always on top**, **Hide when minimized** and **Minimize on use** are
on the Settings page. Minimize on use, on unless you turn it off as in Task
Manager, minimizes open-task when you pick **Switch to**, so the window you
switched to is not covered.
A notification-area icon shows the CPU as a live bar, as Task Manager's does, with
the numbers in its tooltip; a click brings the window back, and its menu has
**Always on top** and **Exit**. With **Hide when minimized** on, minimizing hides
the window and the icon is the way back. **Update speed** (High, Normal, Slow,
Low: every half second to every four) is beside the fade rate under **Process
table**; **Space** still pauses. **How far charts reach back** and **Scroll charts
smoothly** are under **Charts**.

**Summary** puts every other page's headline numbers on one screen, as TMOG's
Summary view does, in cards that sit two across when the window is wide and one
when it is narrow: the CPU (utilization and clock, a small graph of the history
held, processes, threads, handles, up time, and package power and temperature when
the probe reports them), memory (in use of total with a graph, committed, cached),
each disk's active time and read and write rates with the busiest one's graph,
each network connection's traffic with the busiest one's graph (connections at
rest are left out when there are more than four), each GPU's utilization and
dedicated memory, the battery's charge, state, rate and time left, the five
processes using the most CPU right now and the five with the highest cycle
totals (a click on one selects it on the Processes page), and how many users are
signed in, how many services are running, the up time and the Windows edition.
The graphs share one hover line. The app still opens on Processes, as Task
Manager does; the Summary is one click or **Ctrl+1** away.

**Users** lists the logon sessions: user, session id, state (active, disconnected,
idle), station, and the CPU, memory and disk of each session's processes added up.
A session opens to its processes; right-click for **Disconnect** (their programs
keep running) and **Sign out**, both asking first, or on a process, **Go to
process** and **End task**.

**Services** lists every service with its PID, description, status, start type and
group, with **Start**, **Stop** and **Restart** (as administrator; a stop waits
for the service to stop), **Go to process** (the host, on the Processes page) and
**Open Services** (the console). The list is read incrementally, so the page costs
a couple of milliseconds a second once it has settled.

**Startup apps** lists what runs at sign-in, from the Run keys and the Startup
folders of this user and the machine: name, publisher, whether Windows has it
enabled, command, and where the entry lives. **Enable** and **Disable** flip the
same `StartupApproved` value Task Manager uses, so the two agree. Startup impact is
not shown (Windows derives it from boot traces).

**Connections** is TMOG's view: every TCP and UDP endpoint with its protocol, local
and remote address and port, state, and the process that owns it, read from the
same tables `netstat -ano` uses, so no elevation is needed. **Go to process** and
**End process** are on the menu.

**Installed apps** lists the programs in Apps & features with publisher, version,
install date, size, scope (this user or everyone) and location; **Uninstall** runs
the program's own uninstaller after asking, and **Open install location** opens its
folder.

**System** is one page of facts with **Copy all**: the Windows edition, version,
build, install date, computer name and up time; the computer's maker and model,
motherboard, BIOS and its date, firmware kind and Secure Boot; the processor's name,
topology, base speed, caches, virtualization and hypervisor; installed memory, what
the hardware reserves, each memory module's size, speed, form factor and slot; and
the volumes and page files.

### Recording

`open-task --record session.otrec` writes every pass to the file named, with the
window open or with `--headless` (which records its `--passes` passes, then stops and
says what the file cost). `open-task --replay session.otrec --headless` prints the
frames back the way the live headless mode prints passes, and
`open-task --replay-info session.otrec` says what a file holds: the version that
wrote it, the machine, when it started, how many frames over how long and what they
take. `open-task --replay session.otrec` opens the window on the recording instead
of the live machine: every page works as it does live, and a transport bar along
the bottom has step back, play/pause (**Space**), step forward, a slider over the
frames (drag to scrub, wheel to step), the time into the recording, and a speed
button (0.5x to 8x). Recording can also start from the window: **Record to a
file** under **Recording** on the Settings page asks where, and the card then
counts frames and bytes until **Stop**. Nothing is written unless you name a file,
and a recording cut short (the app died, the disk filled) still opens with every
complete frame.

A frame is one pass: every process and thread, the CPU, memory, disks, adapters,
GPUs and battery, everything the pages show. Values shared between passes (a
process's path and command line, a disk's model, a service list) are written once;
every 60th frame is written whole and the rest as the changes since the frame
before, so an idle process costs a few bytes; lz4 compresses the result. On the
development machine (about 450 processes and 6000 threads) a frame is about 13 KB,
or 15 KB with the keyframes counted in, so an hour of one-second passes is around
50 MB.

## Not yet

What the reference tools have that this release does not, and why, so nobody
thinks it was forgotten:

- **Power usage columns.** Windows computes them from its energy estimation engine,
  which has no public API; TMOG estimates them. A guess is not worth a column.
- **Package watts and temperatures without PawnIO.** RAPL and the on-die sensors
  need a kernel driver on Windows (TMOG ships one). open-task reads them through
  [PawnIO](https://pawnio.eu) when it is installed (see Performance) and ships no
  driver of its own; without it the battery's rate is the power figure, where
  there is a battery.
- **App history.** Cumulative per-app use over 30 days needs usage kept across runs,
  and open-task keeps nothing on disk but its settings (a recording is a file you
  ask for). The fading totals and the History chart answer the live question.
- **Startup impact.** Windows derives it from boot traces.
- **Expand/collapse groups, UAC virtualization, Debug** on the process menu (the
  Type column stands in for the groups; the other two are legacy or need a
  debugger), and the **Platform** column (Architecture covers it).
- **Signature verification** (Process Explorer's Verified Signer), the DLL and
  handle lower pane, thread stacks, VirusTotal.
- TMOG's **Benchmarks**.
- **Start with Windows**, an installer task rather than an app feature.

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
| `ot-ui` | UI-agnostic view models: theme, the pages, navigation rail, virtualized table with a column chooser, charts. |
| `ot-record` | Flight Recorder: writes the snapshot stream to a file (shared values once, keyframes plus deltas, lz4) and reads it back by frame. |
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

The installer can also put open-task in Task Manager's place (**Use open-task
instead of Task Manager**, off by default; see
[Replace Task Manager](#using-it) above). Its checkbox shows whether Windows does that
now, whatever was chosen last time, and updates leave it as it is.

Silent install, from an elevated prompt:
`open-task-vX.Y.Z-windows-setup.exe /VERYSILENT /NORESTART`, with
`/MERGETASKS="replacetaskmgr"` to replace Task Manager too. If you cannot elevate,
`/CURRENTUSER` installs into your own profile instead, without the protection above.

The installer and the binaries are not code-signed yet, so SmartScreen asks you to
confirm the first run.

The bare binaries for every platform are on the same release as `.zip` / `.tar.gz`
with a `SHA256SUMS` file. Linux and macOS have no installer because there is nothing
to install yet beyond the headless probe stub.

On Windows, `open-task.exe` comes with `open-task.com` beside it. The `.exe` is a
windowed program, and shells do not wait for those, so a command-line mode typed at
a prompt would print after the prompt had already come back, or not at all under
`cmd /c` or a redirect. `open-task.com` is a small console program that terminals
do wait for, and `open-task` finds it first (`.COM` comes before `.EXE` in
`PATHEXT`). It runs `open-task.exe` with the same arguments and waits while a
command-line mode prints, passing on its output and exit code. When the arguments
open the window instead, it returns to the prompt right away. Call it
`open-task`, not `open-task.exe`, to get this.

## Updates

The version is at the bottom of the rail, above Settings: `v0.3.0` for a release,
the commit (`25c2e9c-dirty`) for a build from source. It is also the update button. A
click looks on GitHub for a newer release and, if there is one, downloads and
verifies it; then the button says **Install v0.3.0**, and a click on that runs the
installer, which closes open-task, updates it (with an administrator prompt for the
usual all-users install) and starts the new version. The Settings page has the same
button with the full version and a sentence on what it is doing.

By default open-task checks when it starts and then once a day, and only says so.
Settings has three switches, each building on the one before: **Check for updates
automatically** (on), **Download updates automatically** (off), and **Install updates
automatically** (off), which installs a downloaded release when you close open-task,
so the next start is the new version. It never restarts itself to update while you
use it, since that would throw away the history it has gathered. A check fetches two
small files from github.com, the latest release's signed checksums, and sends nothing
but the request for them.

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
`open-task --check-update` runs a check and prints what it found (on Windows through
`open-task.com`, see [Install](#install)).

## Build

Requires a stable Rust toolchain.

```
cargo run --release
```

Opens the window on Windows. The theme and title bar follow the Windows app mode
setting, including live changes; `--theme dark` or `--theme light` overrides it.
`--view tree` starts with the process tree instead of the list (`--view map` with the
Map, `--view history` with the History), and `--page performance` on the Performance
page.
`--headless --passes 5` prints a few passes of live system state to the terminal
instead and exits (the processor and its caches, the top processes by CPU with
their CPU time, cycles and owning user, the busiest
service hosts with the services they run, and the hottest threads with the service
each works for); that is the only mode on Linux and macOS for now, where it exits
with code 3 ("no probe on this platform yet"). `--headless --sample <pid>
[--seconds N]` takes one CPU sample of a process and prints it (see "Sample CPU"
above; needs an elevated terminal).

To install the current tree as `open-task` on your `PATH`:

```
cargo install --path crates/ot-app --locked
```

Release builds have no console window; the command-line modes (`--version`,
`--headless`, `--check-update`, `--sample`, `--replay`, `--replay-info`) attach to the
terminal they were started from and print there. Output piped into something that stops reading early
(`| Select-Object -First 5`) ends the program quietly. `cargo install` also
installs `open-task-console`, the console launcher the Windows release ships as
`open-task.com` (see [Install](#install)). On Windows, run that for the
command-line modes, or copy it beside `open-task.exe` as `open-task.com`.

`scripts/screenshot.ps1` launches the app, screenshots its window to
`target/screenshot.png`, and closes it. Handy for checking a rendering change;
`-AppArgs "--view tree"` captures the tree and `-AppArgs "--page performance"` the
Performance page. `-Click "x,y"` clicks first, at a point read off an earlier
screenshot. It only ever captures and closes the
instance it launched, so an installed copy can keep running.

The logo lives in `assets/logo` as SVG: `open-task.svg` for 48 px and up, and
separate drawings for the small sizes (`-32`, `-24`, `-16`), where the big one would
blur. `scripts/render-logo.ps1` turns them into the app icon (`open-task.ico`, every
size Windows asks for), a 512 px PNG and the installer's wizard images; the build
embeds the icon in the exe. The rendered files are committed, so building needs no
SVG renderer; re-rendering needs `resvg` (`cargo install resvg --locked`).

The build names itself from git: a clean checkout of a release tag is that release
(`0.2.1`); anything else carries the commit, `git describe` style
(`0.2.1-21-g25c2e9c`), with `-dirty` for uncommitted changes. On Windows the exe
carries it as its version resource too (Properties > Details).

Two environment variables help with drawing faults on Windows. With
`OT_CHECK_DAMAGE=1` (or a directory path) every frame drawn in part over the last one
is also drawn whole and the two compared; frames that differ are logged and the first
twenty dumped as BMPs, with the commands under the difference, in
`%TEMP%\open-task-damage-check` (or the path given). It costs a full frame and two
read-backs per frame, so it is for diagnosis only. `OT_WARP=1` draws with Direct3D's
software rasterizer instead of the GPU, to tell a driver's faults from the app's.

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
