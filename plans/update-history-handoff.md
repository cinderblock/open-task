# History handoff across an update restart

> **Status:** released in v0.21.0 (2026-11-10 6:50 PM PST, 02:50Z; tag on
> `901f6f8`). Update path verified end to end; the administrator restart only
> unit tested · **Started:** 2026-11-10 · **Branch:** `master` (shared
> working tree; only this thread's files are staged)

## Goal

User (idea-du-jour, re-sent 2026-11-10): "When restarting open-task for an update, it
should hand off the running stats to the new instance so that we don't lose the task
history on update, similar to how Empyrean's app does a hot swap, although being
"frame" perfect is less important here."

Asked for, in their words:

1. After an update restart, the new instance carries on with the history the old one
   collected: the Performance/summary charts (CPU, cores, memory, disks, network,
   GPUs, battery), the per-process CPU history (Map, History chart, the Cycles
   column), and any other accumulated series or counters.
2. Borrow Empyrean's hot-swap design, not its frame-exact timing. A short gap or a
   duplicated sample at the seam is fine.
3. Version skew: a versioned format; anything this build cannot read starts fresh
   and says so in a log. Never a crash, never corrupt graphs.
4. If the handoff fails or times out, the update still completes.
5. Plan in `plans/`, global CLAUDE.md rules, README updated.

## Environment / context

- Windows is the only platform that updates itself (`ot-update` has no Linux
  platform), so the handoff only ever gets written on Windows. The reading side is
  platform-neutral.
- Update flow today (`plans/self-update.md`): a click runs Setup with
  `/SILENT ... /CLOSEAPPLICATIONS /NORESTARTAPPLICATIONS /RELAUNCH=1`. Setup
  elevates, Restart Manager closes the app (`WM_ENDSESSION`, reason
  `ENDSESSION_CLOSEAPP`), Setup replaces the exe, and its `[Run]` entry (gated on
  `/RELAUNCH=1`) starts the new version. "Install updates automatically" runs
  Setup without `/RELAUNCH` as the app closes: no new instance follows.
- Close path in the Windows shell: `WM_ENDSESSION` sets
  `Closing::SessionEnding` and posts `WM_CLOSE` → `close()` → `Shell::close()`
  (saves the layout) → `DestroyWindow` → message loop ends → `stop_feed()` →
  `drop(state)` → `run` returns → `main` calls `process::exit`.
- What accumulates (everything else is recomputed from the newest snapshot):
  - `ot_core::Timeline` (`crates/ot-core/src/timeline.rs`): `Series` of raw samples
    plus 10 s and 60 s bucket tiers, for CPU total, each core, memory, each disk
    (keyed by disk number), each adapter (interface LUID), each GPU (adapter LUID),
    battery.
  - `ot_core::Usage` (`crates/ot-core/src/usage.rs`): per-process fading cycle
    totals with each process's last cumulative cycle and CPU-time counters,
    process lineage (`Seen`), program names, the per-program frame history (raw +
    coarse steps), faded-out totals.
  - Both live in `ot_ui::App` (`crates/ot-ui/src/view.rs`), fed by
    `App::set_snapshot`.
- Every key is OS-level and the same in any process: `ProcessKey` is PID +
  creation FILETIME, adapters and GPUs are LUIDs, disks are disk numbers.
- Logs go to stderr only; a window started by Setup has no log anywhere.

## Empyrean's design, and what carries over

Empyrean (`Empyrean/src-tauri/src/lib.rs` `start_backend`, `server.rs`
`handover_state`/`handover`, `protocol.rs` `HandoverGrant`): the successor starts
while the old instance runs, phase 1 fetches its state over loopback HTTP and warms
up while the old one keeps sending, phase 2 commits (the old instance quiesces,
returns fresh state, exits). Version skew: the grant is serde JSON with
`#[serde(default)]` on every field added later, `Option` where "absent" must
differ from zero. A failed commit is time-limited (10 s), then the successor gives
up rather than lingering.

open-task cannot overlap the two processes: Restart Manager has to end the old
instance before Setup can replace the exe, and only then does Setup start the new
one. So the handoff is a **file** written as the old instance closes and claimed as
the new one starts. Carried over from Empyrean: an explicit grant (a versioned
snapshot of the state, separate from the runtime structs), version skew handled in
the format, a hard time limit on every wait, and a failure that always falls back
to the plain behaviour (here: start fresh, the update unaffected).

## Decisions already made (don't re-ask)

1. **File handoff, written at close, claimed at start** (Claude's call: the only
   design Restart Manager allows). `%LOCALAPPDATA%\open-task\handoff\history.bin`.
   Written to a temporary name and renamed into place, so a reader never sees half
   a file; claimed by renaming it away, so two starting instances cannot both take it.
2. **Written only for an update that relaunches**: the user clicked Install
   (`Updater::install(true)`), Setup is running, and the session-end close arrives.
   Not for "install as it closes" (the user closed the app; nothing follows), not
   for logoff/shutdown, not while a replay plays.
3. **Claimed only by a live window** (not a replay, not `--headless`), and only if
   fresh: written no more than 5 minutes ago and not in the future.
4. **Binary format**: a fixed header that never changes layout (magic, format
   number, writer version, written-at time, writer PID, lengths, checksum), then
   an lz4-compressed postcard payload of dedicated DTO structs. postcard is not
   self-describing, so the format number is the only schema check: any change to
   the DTOs bumps it, and a golden-bytes test fails until it is bumped. A format
   this build does not know is discarded, with the writer's version in the log.
5. **Everything decoded is validated** before it reaches the app: capacities and
   counts capped, program ids in range, lists sorted, values finite, times
   ordered. A file that fails any check is discarded whole.
6. **Writing never holds up the update**: the history is cloned on the UI thread
   (milliseconds), then encoded and written on a worker thread; the process waits
   for it at exit for at most 2 seconds and then exits anyway (a timed-out write
   leaves only a temporary file, which the next start removes).
7. **The seam**: counters (cycles, CPU time) are exact across the gap because
   `Usage` keeps each process's last cumulative counters and the OS keeps
   counting. The gap becomes one frame of the History whose machine-side numbers
   (CPU busy, memory) are the first rated sample's, held across the gap. The
   summary charts show the gap as a straight line between the last old sample and
   the first new one. Sample-exact continuity is not a goal (user: "frame perfect
   is less important").
8. **Current settings win**: retention, history span and fade rate are taken from
   the new instance's settings after adopting, as they would be after changing a
   setting.
9. **Outcome log**: one line per write and per claim (adopted, or discarded and
   why) in `%LOCALAPPDATA%\open-task\handoff\handoff.log`, capped in size, plus the
   same through `tracing`. Needed because the relaunched window has no log.

## Plan / steps

1. [x] Read how updates and restarts work today; read Empyrean's handover.
2. [x] `ot-core::handoff`: DTOs, export from `Timeline`/`Usage`,
   validated restore, file framing (header + lz4 + postcard), tests: continuity
   (a restored copy fed the same later snapshots matches one that never stopped),
   rejection (unknown format, corrupt, truncated, out-of-range ids), golden bytes.
3. [x] `ot-ui::App`: `history_for_handoff()` and `adopt_history()`.
4. [x] `ot-shell-core::handoff`: paths, atomic write on a worker with a deadline,
   claim, freshness, outcome log; `Shell` wiring (install click → close → write;
   start → claim), tests with a temp dir.
5. [x] `ot-shell-win`: `Platform::local_time` (`clock.rs`) for the log; the
   loop's end already drops the shell after `stop_feed`, which waits for the write.
6. [x] README; checks (fmt on own files, clippy, tests on Windows; Linux build of
   the touched crates in WSL).
7. [x] End to end: two local builds, a local feed, per-user install, click install,
   confirm the new window carries the history (charts not empty, handoff.log says
   adopted). Off the user's screen (private desktop), per CLAUDE.md.

## Findings / gotchas

- `Usage::observe` does not skip a zero-interval snapshot (it only matters on its
  very first call, as the baseline). After a restore the new sampler's first pass
  has `interval == 0` and CPU total 0 (no rates yet); feeding it would make the gap
  frame claim an idle machine. A restored `Usage` therefore waits for the first
  snapshot with an interval (decision 7). `Timeline` already skips zero-interval
  snapshots.
- `Tick` numbers are per process and restart at 1: a restored `Timeline`/`Usage`
  must forget `last_tick`, or a coincidence would drop a snapshot.
- `Ring::new` allocates its full capacity up front, so a capacity read from the
  file must be capped before anything is allocated.

## Progress log

- [x] Survey (2026-11-10).
- [x] Format + core tests (`c71e9b3`): round trip, a restored copy continuing
      exactly like the original, cycles across the gap, retention, refusals, 3000
      damaged payloads, layout pin (`crates/ot-core/testdata/handoff-v1.bin`).
- [x] App + shell wiring (`a6ccf79`), log says how much history (`ecebb6e`): 9
      shell tests with a temp dir (once only, stale, future, damaged, unknown
      format, failed write, stale temps, log trimming, UTC dates).
- [x] README (`58a400b`).
- [x] Workspace clippy clean on Windows; ot-core, ot-shell-core, ot-ui clippy
      clean for `x86_64-unknown-linux-gnu`.
- [x] End-to-end update, PASS on the first run (scratchpad rig: `e2e\e2e.ps1`, a throwaway minisign key,
      a local feed on 127.0.0.1:8766, a per-user install, a private desktop
      `WinSta0\ot-e2e`).

## End-to-end result (2026-11-10 5:27-5:29 PM PST, 01:27-01:29Z)

Release build of `ecebb6e` (plus this plan's edit, hence `-dirty`) with
`OT_UPDATE_FEED=http://127.0.0.1:8766/releases` and a throwaway key, two Inno
installers from it (v0.20.3 to install, v0.99.0 offered by the feed), per-user
install, the app started on the private desktop `WinSta0\ot-e2e`, 90 s of history,
then one posted click on the update button (the scheduled check had already
downloaded and verified v0.99.0). Setup started, Restart Manager closed the old
instance, Setup started the new one. `handoff.log`:

```
2026-11-10 5:28:51 PM PST (2026-11-11 01:28:51Z)  pid 46776 v0.20.2-4-gecebb6e-dirty: wrote the history for the update to v0.99.0: 1 min 32 s of it, 318 kB, 47 series, 187 programs, in 9 ms
2026-11-10 5:28:53 PM PST (2026-11-11 01:28:53Z)  pid 32532 v0.20.2-4-gecebb6e-dirty: carried on from v0.20.2-4-gecebb6e-dirty's history, written 1.8 s before by process 46776: 1 min 32 s of it, 47 series, 187 programs
```

The file was gone afterwards (claimed once). Cleanup restored
`HKCU\Software\open-task`, the user's v0.20.2 download, removed the handoff folder
and the per-user install; the user's Program Files instance was not touched.
Not exercised end to end: a refused handoff (unit tested), the all-users path
(same code; the handoff folder is per user either way).

## Follow-ups the user approved

User, 2026-11-10: "Yes. ... wherever is reasonable. History imported is probably
not that important". Read as yes to both, the second kept low-key (`2395905`):

1. **Run as administrator** hands the history on too. Empyrean's order: the new
   copy starts while the old one runs. `relaunch_elevated` passes
   `--handoff-from <pid>`; the old one writes as it closes
   (`Shell::hand_off_to_restart`); the new one waits up to 3 s
   (`handoff::TAKEOVER_WAIT`, `handoff::wait_for`) before taking it. Windows only
   (the Linux relaunch runs as root, whose state folder is another).
   **Not run end to end:** this session is elevated, so the test app is elevated
   and Settings does not offer the card; the parts are unit tested
   (`a_copy_taking_over_waits_for_the_history_but_not_forever`).
2. **Settings → Updates card** note: "The charts carried on from vX." after an
   update (not after a same-version restart), or "The charts started empty: why."
   when a waiting handoff was refused. Shown in the card's note line, which a
   copy that cannot update itself still uses for that instead.

## Open questions for the user

None open.

- The rail's update button is at x = 20 DIP, `gap + 1.5 * ITEM_H` = 68 DIP above
  the client area's bottom (`nav.rs` test); the e2e clicks there with posted
  messages.
- The window has `WS_EX_NOREDIRECTIONBITMAP`, so `PrintWindow` cannot capture it
  on a private desktop: the e2e's evidence is the handoff log, which is why the
  log lines say how much history crossed over.
- The e2e must back up and restore `HKCU\Software\open-task` (the test app saves
  its layout there on close) and the user's
  `%LOCALAPPDATA%\open-task\updates\open-task-v0.20.2-windows-setup.exe` (the
  updater prunes old downloads).

## Things not to do

- Don't block the close on the handoff beyond the deadline: Restart Manager is
  waiting, and a stuck close fails the update.
- Don't decode into the runtime structs directly (no serde derives on `Series`,
  `Usage`): the DTOs are the format, and must only change with a format bump.
- Don't trust anything in the file: it is in a user-writable folder and may be read
  by an elevated instance.
- Don't run GUI tests on the user's desktop.
