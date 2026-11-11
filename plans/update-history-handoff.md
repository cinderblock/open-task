# History handoff across an update restart

> **Status:** in progress · **Started:** 2026-11-10 · **Branch:** `master` (shared
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
2. [ ] **Current step.** `ot-core::handoff`: DTOs, export from `Timeline`/`Usage`,
   validated restore, file framing (header + lz4 + postcard), tests: continuity
   (a restored copy fed the same later snapshots matches one that never stopped),
   rejection (unknown format, corrupt, truncated, out-of-range ids), golden bytes.
3. [ ] `ot-ui::App`: `history_for_handoff()` and `adopt_history()`.
4. [ ] `ot-shell-core::handoff`: paths, atomic write on a worker with a deadline,
   claim, freshness, outcome log; `Shell` wiring (install click → close → write;
   start → claim), tests with a temp dir.
5. [ ] `ot-shell-win`: nothing new beyond dropping the shell, which waits for the
   write (check the order after the loop).
6. [ ] README; checks (fmt on own files, clippy, tests on Windows; Linux build of
   the touched crates in WSL).
7. [ ] End to end: two local builds, a local feed, per-user install, click install,
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
- [ ] Format + core tests.
- [ ] App + shell wiring.
- [ ] README.
- [ ] End-to-end update.

## Open questions for the user

1. "Run as administrator" is also a restart that loses the history today. The same
   handoff would work there (the old instance writes before launching the elevated
   copy). Out of scope for now; recommendation: a follow-up, since it is small once
   this lands.
2. Should the Settings page's Updates card say "History carried over from vX" (or
   why not) after an update? Recommendation: yes, small; left out for now since the
   ask was for a log.

## Things not to do

- Don't block the close on the handoff beyond the deadline: Restart Manager is
  waiting, and a stuck close fails the update.
- Don't decode into the runtime structs directly (no serde derives on `Series`,
  `Usage`): the DTOs are the format, and must only change with a format bump.
- Don't trust anything in the file: it is in a user-writable folder and may be read
  by an elevated instance.
- Don't run GUI tests on the user's desktop.
