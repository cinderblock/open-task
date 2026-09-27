# Service host attribution: what is svchost actually doing?

> **Status:** proposal, awaiting scope decision · **Started:** 2026-09-26 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md` (step 7, diagnostics). Follows
> `plans/process-details-actions.md`. Another thread owns `plans/replace-task-manager.md`
> (Options menu, single instance, v0.3.0) in this same working tree.

## Goal

When `bad_program.exe` burns a core you kill it. When `svchost.exe` does, the table
shows an opaque Windows process and the real answer is three layers down: which
*service* in that host, which *thread*, which *module* that thread is running, and
on whose behalf. On 2026-09-25/26 that took an evening by hand: a DcomLaunch-group
svchost (BrokerInfrastructure + DcomLaunch + PlugPlay + Power + SystemEventsBroker
in one process) pinned a core; the culprit was `bisrv.dll` (Background Tasks
Infrastructure) re-buffering two background tasks of the Xerox Print Experience
Store app about 15,000 times a second. Nothing in any task manager on the machine
said so. Turning that app's background permission off fixed it instantly.

open-task should surface that chain inline, without the user leaving the table, and
without any probe that can hurt the machine.

## What actually identified the culprit (and what did not)

| Step | Tool used by hand | Told us | Cost / risk |
| --- | --- | --- | --- |
| Which services live in this svchost | `Win32_Service` by PID | 5 services, one process (`SvcHostSplitDisable=1`) | free |
| Which thread is hot | `Get-Process` thread CPU deltas | one thread pool worker at 100% | free (already in our NT buffer) |
| Which service that thread belongs to | (not done; would be the TEB service tag) | would have said BrokerInfrastructure at once | needs `PROCESS_VM_READ`, i.e. elevation |
| Which module the thread runs | 6 minidumps + offline cdb; later ETW sampling | `bisrv.dll`, then the function names | dumps: sub-second suspend; ETW: zero suspend |
| Thread start address | `SYSTEM_THREAD_INFORMATION.StartAddress` | `ntdll!TppWorkerThread`: useless for pool threads | free, and misleading |
| On whose behalf | ETW provider `Microsoft-Windows-BrokerInfrastructure` | package + task names, 147k events / 10 s | zero risk, elevated |
| Live debugger attach | `cdb -pv` | **froze the desktop** when killed mid-attach | never again |

Two lessons shape the design: (1) the *service tag* on each thread is the missing
link between "svchost is hot" and "this service is hot"; the tree, the parent chain
and the start address do not give it. (2) The only safe way to look inside a system
process is a read-only one: NT queries, `ReadProcessMemory` of one TEB field, and
ETW. No `SuspendThread`, no debugger, no dumps of service hosts.

## Environment / context

- The probe already asks `NtQuerySystemInformation(SystemProcessInformation)` every
  pass. One `SYSTEM_THREAD_INFORMATION` per thread follows each process entry in that
  buffer (`KernelTime`, `UserTime`, `StartAddress`, `ClientId`, `ThreadState`,
  `WaitReason`); `sample_processes` skips them today via `NextEntryOffset`. Per-thread
  CPU is therefore free: no extra syscalls, no handles.
- Service to process: `EnumServicesStatusExW(SC_ENUM_PROCESS_INFO)` on the SCM gives
  every service's PID, state and display name in one call, unelevated. Windows Task
  Manager's "Service Host: Background Tasks Infrastructure Service" comes from this.
- Thread to service: each thread in a service process carries a *service tag* in its
  TEB (`SubProcessTag`, x64 offset `0x1720`). Reading it: `NtQueryInformationThread`
  (`ThreadBasicInformation`) for the TEB address, then `ReadProcessMemory` of 8 bytes,
  needing `PROCESS_VM_READ`, which SYSTEM-owned hosts grant only to an elevated
  caller. Tag to name: `advapi32!I_QueryTagInformation(NULL, eTagInfoLevelNameFromTag,
  {pid, tag})`, the same undocumented call Process Explorer and System Informer use.
  Threads with tag 0 belong to the host itself or a service that has not tagged them.
- Thread to module (safe): map the thread's *current* code, not its start address,
  because pool workers all start in ntdll. Without suspending anything the only
  correct source is sampling: an ETW kernel session with `PROFILE` (what `xperf`/WPR
  do), which needs elevation and `SeSystemProfilePrivilege`, delivers a sampled
  instruction pointer per thread; map IPs to modules with `EnumProcessModulesEx` +
  `GetModuleInformation`. Module name alone (`bisrv.dll`) is usually the answer;
  function names need PDBs and are a later, opt-in step.
- Service DLL: `HKLM\SYSTEM\CurrentControlSet\Services\<name>\Parameters\ServiceDll`.
  For a service in a shared host this is the module to look for in the samples; it
  also lets us show "BrokerInfrastructure (bisrv.dll)" without any profiling. Note the
  trap seen here: BrokerInfrastructure's registry says `psmsrv.dll`, but the CPU was
  in `bisrv.dll`, so the registry hint is a hint, not the attribution.
- The elevation split matters: unelevated open-task (decision 10 of the parent plan)
  can do the SCM mapping and per-thread CPU; service tags, module mapping and ETW need
  the elevated relaunch that the Options menu is introducing.
- Windows Task Manager shows services as children of svchost in its Processes tab
  (no CPU split). Process Explorer shows service names in the tooltip, a Services tab,
  and a Threads tab with per-thread service names when elevated. TMOG has a Services
  view. None of them attributes CPU to a service or names the ETW client.

## Proposed decisions (confirm or change before building)

1. **Threads become a first-class layer under a process in tree mode**, folded by
   default. A process row expands to its threads only when the user asks (Right on a
   leaf process, or a chevron that appears when the process is above a CPU threshold).
   Sorting by subtree totals already exists, so the hot thread rises to the top.
   Thread rows show: TID, CPU %, state/wait reason, service (when known), module
   (when sampled), started. Threads are sampled every pass from data we already read;
   cost is a per-thread differencing map, bounded by a global thread cap.
2. **Service hosts get inline attribution in the Name cell**: `svchost.exe` becomes
   `svchost.exe  DcomLaunch: BrokerInfrastructure, DcomLaunch, PlugPlay, Power, ...`
   (group from `-k` in the command line, names from the SCM), with the full list and
   display names in the tooltip and the details strip. Unelevated, this is as far as
   attribution goes, and it is already a large step over today.
3. **Per-service CPU when elevated**: sum thread CPU by service tag and show it in
   the Name cell as `BrokerInfrastructure 98%` and as the sort key of a virtual
   service layer between process and threads (process, then services, then threads).
   Threads with no tag roll up to the host. Tag reads are done once per thread
   lifetime, under the existing per-pass time budget, never on the UI thread.
4. **"Where is this thread running?" is an explicit action**, not continuous: a
   context-menu item on a process or thread ("Sample CPU for 5 s") starts an ETW
   kernel profiling session, aggregates sampled IPs by module (and by thread), and
   shows the top modules inline under the thread rows. Elevated only; disabled with a
   reason otherwise. No `SuspendThread`, no debugger, no minidump of a system process
   anywhere in the code base. Function-level symbolization (PDB download) is a later,
   opt-in feature with a clear network prompt.
5. **"Who is asking?" for known brokers** is the same action extended with a small
   table of service to ETW provider: BrokerInfrastructure to
   `{E6835967-E0D2-41FB-BCEC-58387404E25A}` (event 18 = activation buffered, fields
   `PackageFullName`/`TaskName`), PlugPlay to Kernel-PnP, wuauserv to
   WindowsUpdateClient, DcomLaunch/RPCSS to DCOM activation. When the hot service is
   in the table, the 5 s sample also enables that provider and shows a histogram of
   the payload's identifying field ("XeroxCorp.PrintExperience: 147,356
   activations"). This is the step that would have named Xerox.
6. **Portable model, Windows-only probe.** `ot-model` grows `ThreadSample`,
   `ServiceInfo`, and an `Attribution` result on the snapshot; Linux/macOS return
   empty. The UI never calls a platform API (architecture decision 4).
7. **Stopping services is out of scope here**; it belongs to the Services view. The
   row context menu only gains "Sample CPU for 5 s".

## Answer to the side question: would the tree have shown it?

No, and it could not have. The process tree is parent to child by *creation*. The
svchost that was spinning is the parent of every `backgroundTaskHost.exe`,
`RuntimeBroker.exe` and `dllhost.exe` on the machine, because DcomLaunch launches
them. The Xerox app's own task host was one of dozens of such children and was not
even alive for most of the incident; the CPU was inside svchost itself, working on a
registration in the broker's database. There is no process relationship that points
from svchost to Xerox. What points there is (a) the service tag on the hot thread
(BrokerInfrastructure), then (b) the broker's ETW events (package name). Decisions 3
and 5 are those two arrows. The one thing the tree *does* add is a weak hint: a
child of the hot svchost that keeps appearing and disappearing.

## Plan / steps

1. **[current]** Agree scope. Recommended first cut: steps 2 to 4 (services inline,
   threads layer, per-service CPU when elevated). Steps 5 and 6 are the "diagnose"
   action.
2. `ot-probe` (Windows): parse `SYSTEM_THREAD_INFORMATION`; per-thread CPU deltas
   keyed by (pid birth, tid, thread create time); global cap with "hottest N threads
   per process" fallback. SCM enumeration once per pass (cheap; cache display names).
   `ot-model`: `ThreadSample`, `ServiceInfo`, `ProcessSample::services`,
   `ProcessSample::threads`.
3. `ot-probe` elevated path: service tag per thread (TEB read) once per thread;
   `I_QueryTagInformation` name cache per (pid, tag). Capability flag so the UI can
   say why attribution is missing.
4. `ot-ui`: Name-cell attribution for service hosts; service and thread layers in
   `ProcessRows` (virtual rows with their own `RowId` kinds so PID reuse logic still
   holds); expand/collapse; sort by subtree; search matches service names. Tests.
5. `ot-probe`: ETW kernel profile session (start/stop, `EVENT_TRACE_FLAG_PROFILE`,
   IP only), real-time consumer thread, IP to module map, 5 s window, result
   published as a one-shot `Attribution` on the snapshot. The kernel logger is a
   singleton on the machine; detect and refuse if another tool holds it.
6. Provider table (decision 5), payload histogram, inline presentation under the
   service row. Context menu "Sample CPU for 5 s" wired through `Effect`.
7. README ("Service hosts" section), parent plan step 7 note, screenshots of the
   DcomLaunch host expanded; verify on this machine with the Xerox background
   permission turned back on, which reproduces the spin on demand.

## Findings / gotchas

- Everything in "What actually identified the culprit" above.
- `SvcHostSplitDisable=1` on BrokerInfrastructure keeps five services in one process
  even on a machine with plenty of RAM, so "one service per svchost" cannot be assumed.
- The registry `ServiceDll` for BrokerInfrastructure names `psmsrv.dll`; the work ran
  in `bisrv.dll`. Show the DLL hint as a hint.
- ETW: the provider name is `Microsoft-Windows-BrokerInfrastructure`, not
  `...BackgroundTaskInfrastructure`; event 18 carries `PackageFullName`, `TaskName`,
  `TaskEntryPoint`, `BufferingReason`. 31,722 events were dropped in 10 s with default
  buffers, so size the session for a burst.

## Progress log

- [x] Plan written.
- [ ] Scope agreed.
- [ ] Probe: threads + SCM services.
- [ ] Probe: service tags (elevated).
- [ ] UI: attribution cell, service and thread layers.
- [ ] Probe: ETW sampling action.
- [ ] Provider table + payload histogram.
- [ ] README, parent plan, screenshots, verification.

## Open questions for the user

1. **Scope of the first cut.** Recommendation: steps 2 to 4 now (they need no new
   permissions beyond what the app has, and cover "which service is hot" once
   elevated), then 5 and 6 as a second cycle once the elevated relaunch from the
   Options menu exists.
2. **Threads under every process, or only service hosts?** Recommendation: every
   process (a spinning thread in Chrome is the same question), folded by default.
3. **Continuous vs on-demand profiling.** Recommendation: on-demand only (decision 4).
   A kernel profiling session is cheap but it is still a system-wide logger, and a
   task manager should not hold one open all day.

## Things not to do

- Never `SuspendThread`, attach a debugger, or write a minidump of a service host
  from open-task. The 2026-09-25 freeze came from exactly that.
- Never rely on `StartAddress` for attribution of thread pool threads.
- Do not enumerate the SCM per row on the UI thread; it is one call per pass in the
  probe.
- Do not touch the Options menu, single-instance or installer files: another thread
  owns them (`plans/replace-task-manager.md`).
