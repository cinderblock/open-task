# Feature parity with Task Manager, TMOG and Process Explorer: v0.7.0

> **Status:** shipped in v0.7.0 · **Started:** 2026-10-02 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

The user: "we're missing a bunch of features to get to (or past) feature parity with
Windows 11's task manager, TMOG, and others... make a list of missing features, then
implement them all, and push a new minor version." So: an audit of what the three
reference tools have that open-task v0.6.0 does not, then implement as much of it as
is honestly possible in one release, then release v0.7.0 (the push to origin and the
tag are authorized by that message; nothing else is).

## Environment / context

- v0.6.0 is the last release (commit `5b204db`); the tree was clean at start.
- Release recipe (from `git show 5b204db`): bump every `0.6.0` in `Cargo.toml`
  (workspace version and the eight path deps), `cargo update -w` for `Cargo.lock`,
  commit "Release vX.Y.Z" with a paragraph of what is new, tag `vX.Y.Z`, push both.
  `release.yml` builds and publishes. Previous releases then got a "Plan: ... shipped
  in vX.Y.Z, checked from the published assets" commit after verifying the assets.
- Verify list before pushing is in `plans/open-task-architecture.md` ("Verify before
  pushing"): `rustup update stable`, fmt, clippy on the Windows, Linux and macOS
  targets, tests, headless run, screenshot.
- C: is 99 % full (15 GB free at start). Memory note: cargo builds have failed on a
  full drive; delete only `target/*/incremental` and build with
  `CARGO_INCREMENTAL=0` if link errors appear.
- The tree is CRLF (`.gitattributes`); keep new files CRLF.

## The audit: what the reference tools have that v0.6.0 does not

Legend: **[do]** in scope for v0.7.0 · **[later]** out of scope, with the reason.

### Windows 11 Task Manager

| Feature | v0.6.0 | Plan |
| --- | --- | --- |
| Processes grouped as Apps / Background / Windows processes | no | **[do]** as a sortable Type column (Apps have a top-level window; Windows processes are system images under `%SystemRoot%` run by the system accounts) rather than group headers |
| Status column: suspended, efficiency mode, not responding | no | **[do]** |
| Efficiency mode (EcoQoS + idle priority) on/off | no | **[do]** |
| Set priority (Realtime … Low) | no | **[do]** submenu |
| Set affinity | no | **[do]** submenu with a check per logical processor |
| Create dump file | no | **[do]** `MiniDumpWriteDump` to `%TEMP%`, then reveal it |
| Search online | no | **[do]** |
| Properties | no | **[do]** the shell's Properties sheet |
| Run new task | no | **[do]** the shell's Run dialog (`RunFileDlg`), with "as administrator" |
| Switch to (bring an app's window forward) | no | **[do]** |
| Expand / collapse groups, UAC virtualization, Analyze wait chain, Debug | no | [later] wait chain and debug are rarely used; UAC virtualization is legacy |
| GPU on the Performance page (utilization by engine, dedicated / shared memory, driver, name) | no | **[do]** PDH `GPU Engine` / `GPU Adapter Memory` counters, names from DXGI |
| GPU and GPU engine columns in Processes | no | **[do]** GPU % per process from the same counters |
| Power usage / Power usage trend columns | no | [later] Windows computes these from its energy estimation engine, which has no public API; TMOG estimates them too. Not worth a wrong number |
| Memory page: speed, slots used, form factor, hardware reserved, compressed | partly | **[do]** SMBIOS type 17 records via `GetSystemFirmwareTable`, `GetPhysicallyInstalledSystemMemory`, the compression store's working set |
| Disk page: formatted / system disk / page file / type / capacity | partly | **[do]** the volumes on each disk with their free space, which is also TMOG's Disk Space view |
| Network page: IPv4 / IPv6 address, adapter name, DNS name, SSID | partly | **[do]** addresses |
| CPU page: virtualization enabled, Hyper-V support | partly | **[do]** |
| App history (cumulative per-app usage over 30 days) | the History chart covers the last hour | [later] needs usage persisted across runs; the fading totals and History already answer "who has been using the CPU" |
| Startup apps page: entries, status, impact, enable / disable | no | **[do]** Run keys and Startup folders with `StartupApproved` state; impact [later], Windows derives it from boot traces |
| Users page: sessions, their users, status, per-user usage, disconnect / sign out | no | **[do]** |
| Details page: every column, column chooser | List mode with 12 fixed columns | **[do]** many more columns (below) and a column chooser on the header's context menu, with the layout persisted |
| Services page: name, PID, description, status, group; start / stop / restart; open Services | only under each process in the tree | **[do]** |
| Always on top | no | **[do]** |
| Update speed (High / Normal / Low / Paused) | pause only | **[do]** |
| Minimize on use, hide when minimized, tray icon with CPU use | no | **[do]** hide when minimized and a tray icon with a live CPU bar; "minimize on use" [later] |
| Remember the last page / arrangement / sort | no | **[do]** |
| Resource values as percent or values | no | [later] |
| Start with Windows | no | [later] an installer task, not an app feature |
| Process icons in the Name column | no | [later] the paint layer has no bitmap support yet; a release of its own |
| Column reorder by drag | no | **[do]** if the column chooser leaves time, else [later] |

Detail-page columns Task Manager has that v0.6.0 does not: Status, Session ID,
Base priority, Description, Publisher (company), Architecture, Elevated, Package
name, Commit size (we have Memory = private bytes), Paged pool, NP pool, Page faults,
Peak working set, Virtual size (as Commit / Peak), I/O reads / writes / other (counts
and bytes), CPU time, Start time, GPU, GPU engine, Image path. **[do]** all but
Package name [later] and Platform / Operating system context [later].

### TMOG (including its Pro tier)

| View | v0.6.0 | Plan |
| --- | --- | --- |
| Summary | no | [later] an overview page of the other pages' headline numbers; cheap once they exist, but each page is the priority |
| Performance | yes | GPU and the additions above |
| Processes | yes | the additions above |
| System Info | no | **[do]** OS, build, machine, BIOS, board, processor, memory devices, boot time, uptime |
| Startup Apps | no | **[do]** |
| Users | no | **[do]** |
| Services | no | **[do]** |
| Power & Freq (watts, power state history, frequency per core) | frequency per core | **[do]** battery: charge, charge / discharge rate, time left, as a Performance device when a battery exists. Package watts and thermals [later]: RAPL and the on-die sensors need a kernel driver on Windows; TMOG ships one |
| Connections (TCP / UDP endpoints by process) | no | **[do]** |
| Installed Apps | no | **[do]** with uninstall and open location |
| Disk Space (volumes) | no | **[do]** on the disk panes |
| Benchmarks | no | [later] a different kind of feature |
| Flight Recorder | `ot-record` is a stub | [later] a release of its own; the snapshot stream is designed for it |
| Process identity across PID reuse, P/E cores, memory lists, log-time charts | yes | |

### Process Explorer

| Feature | v0.6.0 | Plan |
| --- | --- | --- |
| Run as administrator (relaunch elevated; decision 10 of the architecture plan) | no | **[do]** |
| Suspend / Resume | no | **[do]** |
| Restart | no | **[do]** (end, then start the same command line in the same directory) |
| Find window's process (crosshair) | no | **[do]** |
| Copy a row | no | **[do]** |
| Company name / description / verified signer | no | **[do]** description and company from the image's version resource; signature verification [later] |
| DLL / handle lower pane, thread stacks, VirusTotal | threads yes | [later] |

## Decisions already made (don't re-ask)

1. Scope is the **[do]** rows above; the **[later]** rows are listed in the README's
   status so nobody thinks they were forgotten. The user asked for "all"; the
   `[later]` rows each have a reason that is not "no time".
2. New pages go on the rail in Task Manager's order after Performance: Users,
   Details is not a page (it is the List with the column chooser), Services,
   Startup, then TMOG's Connections, Installed apps, System. Ctrl+1..9 number them.
3. Actions that need rights the app does not have are shown disabled with the
   reason, as Sample CPU already is; nothing prompts for elevation on its own except
   "Run as administrator" itself.
4. New per-process facts that need a handle (architecture, description, company,
   efficiency mode, windows) are gathered the way details are: lazily, once, under
   the per-pass budget, except the ones that change (efficiency mode, windows), which
   are refreshed on a slow cadence.
5. Version: **0.7.0** (minor, as asked).

## Plan / steps

1. [x] Audit (above).
2. [x] Model types for everything new (`ot-model`), so the probe and the UI could
   be built in parallel.
3. [x] Probe: process facts, control actions, GPU, services list, sessions, startup
   entries, installed apps, connections, system info, volumes, addresses, battery,
   SMBIOS memory.
4. [x] UI: columns and column chooser; context menu additions; new pages; Performance
   additions; settings (always on top, update speed, hide when minimized, remember
   layout).
5. [x] Shell: tray icon, topmost, run dialog, elevation, properties, clipboard,
   crosshair, dump, restart.
6. [x] README, verify list, version bump, release v0.7.0 pushed.
7. [x] Published assets checked; CI's Windows test failures fixed; plan note.

## Findings / gotchas

- **Agent worktrees were created at `2c5f6c6`, one commit behind the scaffold
  commit `d9af87f` they were told to branch from** (the Agent tool branches from
  the checkout's state at the time the tool was called, or so it seems; the
  scaffold commit had just been made). Every agent noticed and fast-forwarded its
  branch. Next time: commit, then wait a moment, or tell agents to check `git log`.
- **Sharing one `target/` between worktrees makes cargo mix up their builds.** A
  workspace member's unit hash depends on its workspace-relative path, which is the
  same in every worktree, so all of them write the same
  `target/debug/deps/ot_probe-<hash>.exe`; whichever built last is "fresh" for all
  of them and `cargo test` may run a peer's binary ("0 tests", or a surprising
  count). Workarounds the agents found: bump the file's mtime before each cargo
  command and confirm the `Compiling ot-probe (<my path>)` line, or give the crate
  its own hash with `--config 'profile.dev.package.ot-probe.debug=1'`. The final
  verification is done in the main tree anyway.
- Agent results (probe modules, costs on this machine): startup entries 32-76 ms
  (`entries()`, on demand only); connections ~2 ms per `list` (keep one probe behind
  a mutex); sessions 5-9 ms per enumeration (cached 5 s); service list ~1.7 ms per
  pass steady, ~20 s at 1 Hz until every start type is read (15 ms budget per pass);
  GPU counters 2.4-3.2 ms per pass (two adapters, ~580 engine instances).
- **New files from agents and patch scripts came in LF.** The tree is CRLF
  (`.gitattributes`), and a Python patch anchored on `\r\n` text silently matches
  nothing in an LF file. `sed -i` on Git Bash also rewrote a CRLF file as LF once.
  Check with `file` (not `grep -c $'\r'`, which lies on Git Bash) and normalize with
  Python before patching.
- **`windows` 0.62 moved `IsDlgButtonChecked` and `EM_SETSEL` to
  `Win32::UI::Controls`**, not `WindowsAndMessaging`. `CreateProcessW`'s current
  directory is a plain `PCWSTR` (`PCWSTR::null()` for none), not an `Option`.
- **`ot-shell-win` must gate every module on `#[cfg(windows)]`**, or the Linux and
  macOS clippy passes fail on the `windows` crate imports; the agents' new modules
  (`actions`, `gfx`, `run_dialog`, `tray`) had lost their gates in the merge.
- **`Page::parse` by label prefix made `--page apps` ambiguous** (Startup apps,
  Installed apps). Pages now have a one-word `name()` that wins an exact match.
- **The Performance page's facts column** was first laid out line by line, which
  either overflowed the pane (nine CPU facts in seven rows) or, when split into two
  columns up front, left the value column too narrow for a volume line. It now
  collects the lines and lays them out once it knows how many there are: one column
  when they fit, two when the pane is wide enough, and the rest left out. The stats
  area grew from 140 to 180 px to hold Task Manager's nine CPU facts.
- **The network probe listed addresses nowhere**: `GetUnicastIpAddressTable` only
  said which interfaces had one. `GetAdaptersAddresses` (one call every 5 s, into a
  kept aligned buffer) now gives each adapter its addresses, DNS suffix and MAC, and
  replaces that table. A physical NIC bound to an external Hyper-V switch has no
  address or MAC of its own; the switch's `vEthernet` port carries them.
- **Screenshot runs steal focus on the user's desktop.** Two shots came back with
  the user's typing in the search field (`nalog`, `break?`). Not a bug of ours; take
  shots when the user is not typing, or warn them.
- `scripts/screenshot.ps1` gained `-Keys "End;Up"` (posted `WM_KEYDOWN`/`WM_KEYUP`)
  so a shot can reach list items below the fold (the GPU pane).

## Progress log

- [x] 2026-10-02 audit written.
- [x] Scaffold commit `d9af87f`: model types, probe traits, snapshot fields.
- [x] Eight probe agents launched in worktrees (gpu, sessions, services, startup,
      installed, connections, system+smbios+volumes+battery+hardware, process
      facts+control). Done and copied into the main tree: startup, connections,
      sessions, services. Waiting: gpu, installed, system group, process facts.
- [x] UI: table columns can hide, reorder by drag and round-trip a layout; the
      process table has 37 columns (12 shown by default); the context menu has
      every Task Manager / Process Explorer action; header right-click is the
      column chooser; Run new task and crosshair buttons; `select_pid`,
      `view_layout`.
- [x] Shell: menu model (submenus, checks, affinity), the new effects, tray,
      topmost, run dialog, elevation, crosshair, inventory worker, layout saved
      to `HKCU\Software\open-task\Layout` (`REG_SZ`).
- [x] Pages: Users, Services, Startup, Connections, Apps, System. All eight render
      with real data (screenshots in `target/shot-*.png`).
- [x] Performance: GPU and battery devices with timeline series, charts and stats;
      memory speed / slots / form factor / hardware reserved / compressed; volumes
      and bus on the disk pane; addresses, DNS suffix and MAC on the adapter pane;
      virtualization and hypervisor on the CPU pane.
- [x] Settings and prefs: always on top, update speed, hide when minimized,
      run as administrator, layout persistence.
- [x] Wired the agents' probe modules into `WindowsProbe` (GPU, battery, volumes,
      sessions, service list, per-process GPU) and `WindowsControl` (services,
      sessions, startup, inventory). Capabilities report `gpu`, `sessions`,
      `service_list`, `per_process_gpu` true on this machine.
- [x] 2026-10-02 verify list: `rustup update stable` (1.99.0 unchanged), fmt,
      clippy `-D warnings` on Windows, Linux and macOS targets, 321 tests pass
      (159 ot-ui, 91 ot-probe, 24 ot-core, …), headless run, screenshots of every
      page.
- [x] README: status, pages, columns, menu, the six new pages, window/tray
      settings, "Not yet" list.
- [x] Release v0.7.0: `cbe4b80` (the work), `4073167` (the bump), tag `v0.7.0`,
      pushed 2026-10-02; Release run 37085967683 and CI run 37085966412 started.
- [x] Release run 37085967683 succeeded: nine assets published (x64 and ARM64
      Windows zips and the setup.exe, Linux and macOS tarballs, SHA256SUMS and
      its minisig). CI run 37085966412 failed on the two Windows runners on
      probe tests that assumed real hardware (memory speed, hypervisor on Arm)
      and a 50-pass drain; the tests were loosened in a follow-up commit.

## Open questions for the user

- None blocking. The `[later]` list is the thing to react to.

## Things not to do

- Do not push, tag or create anything on GitHub beyond the v0.7.0 release the user
  asked for.
- Do not show a number that is a guess (power usage per process).
- Do not prompt for elevation from anywhere but the explicit "Run as administrator"
  action.
