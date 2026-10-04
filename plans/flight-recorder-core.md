# Flight Recorder core: `ot-record`, the `Sampler` hook, `Player` and `Feed`

> **Status:** active · **Started:** 2026-10-03 · **Worktree:** `.claude/worktrees/agent-a6c8b6e2b65bafa55` (branch `worktree-agent-a6c8b6e2b65bafa55`, from master `10e0683`) · **Parent plan:** `plans/v0.8-icons-summary-recorder.md` step 5

## Goal

Record the snapshot stream to a portable file and replay it where the probe normally
goes, without the GUI transport controls (another agent builds those against this API).
Command line: `--record <file>` (GUI and headless), `--replay <file> --headless`,
`--replay-info <file>`. Nothing is written to disk the user did not name (parent plan,
decision 1).

## Environment / context

- Target dir is per worktree, but `~/.cargo/config.toml` moves intermediates to a shared
  `build-dir` (`~/.cargo/build`): check for `Compiling <crate> (<this worktree>)` lines.
- Tree is CRLF (`.gitattributes`); the Write tool writes LF, so normalize with
  `target/crlf.py` before committing and check with `file`.
- Toolchain: rustc 1.99.0, cargo 1.99.0. Crates: `serde 1.0.229`, `postcard 1.1.3`,
  `lz4_flex 0.14.0`.
- Machine has ~400 to 570 processes and roughly 8000 sampled threads per pass.

## Decisions already made (don't re-ask)

1. **`Snapshot` moves to `ot-model::snapshot`.** `ot-record` must sit below `ot-core`
   (ot-core's `Player` owns a `Recording`), and `Snapshot` is a pure value type anyway.
   `ot-core` re-exports it at the old paths, so `ot-ui` and `ot-shell-win` are untouched.
2. **serde is an unconditional dependency of `ot-model`** (features `derive`, `rc`), not a
   feature flag: every consumer serializes, and the gate would be always-on.
3. **Shared `Arc` fields are `#[serde(skip)]` in the model** (`ProcessSample::statics`,
   `::window`, `::services`; `DiskSample::info`, `AdapterSample::info`, `GpuSample::info`;
   `Snapshot::services`, `Snapshot::hardware`). The recorder writes each distinct value
   once in a table record and references it by id; the reader restores one `Arc` per id,
   so `Arc::ptr_eq` identity logic in the UI keeps working across frames.
4. **Dedup is keyed by pointer identity, not `(ProcessKey, generation)`.** The probe
   already shares an `Arc` while a value is unchanged and allocates a new one when it
   changes, so pointer identity *is* the generation counter, for every shared type alike.
   The writer keeps the `Arc` alive while it is in the table (so an address cannot be
   recycled under it) and evicts entries only it still holds.
5. **Frames are keyframes plus deltas.** A frame holding ~8000 thread rows with random
   birth stamps cannot reach the 20 KB target with lz4 alone (see Findings). Every 60th
   frame is a keyframe; the others encode each process as field deltas against the same
   process in the previous frame (zigzag varints, float bits XORed), and a process whose
   thread identities are unchanged encodes its threads as deltas too. Seeking decodes
   forward from the nearest keyframe (at most 60 decodes); sequential reads are O(1)
   through a one-frame cache.
6. **Recording writes on a dedicated writer thread**, fed by a bounded channel of
   `Arc<Snapshot>` (capacity 4). The sampler thread only `try_send`s; a full channel drops
   the frame and counts it. Stopping joins the thread, which finishes the file.
7. **GUI `--record` wraps the probe** (`RecordingProbe`) because `ot-shell-win` creates its
   own `Sampler` and must not be modified here. The wrapper clones the pass's output into
   a `Snapshot` and feeds the same writer thread. Once the shell owns a `Feed`, it should
   call `Sampler::record_to` instead and the wrapper can go.
8. **`Recording::frame` returns `Arc<Snapshot>`**, not `Snapshot`: the player publishes
   it as-is and the cache shares it.
9. **`Player::set_speed` / `Player::speed`** rather than a setter named `speed`.
10. **`--replay` without `--headless`** says the window cannot replay yet (exit 2) rather
    than opening a live window: the shell has no `Feed` yet.

## File format (version 1)

```
"OTREC" u16-LE version
record*            kind:u8  len:u32-LE  payload[len]
  1 Header         postcard RecordHeader (app version, Hardware, Capabilities, start, interval)
  2 KeyFrame       tick:u64-LE  at_unix_ms:i64-LE (i64::MIN = none)  lz4(postcard Body)
  3 DeltaFrame     same layout; Body rows are deltas against the previous frame
  16..=22 tables   lz4(postcard Vec<(id:u32, value)>) for statics, windows, process
                   service lists, disks, adapters, GPUs, the service list
  4 Index          postcard Index { frames: [offset, key, tick, at_unix_ms], tables: [offset] }
trailer            index_len:u32-LE  "OTIDX"
```

Table records precede the first frame that references them; `Recording::open` loads all
tables (via the index, or by scanning when the trailer is missing or inconsistent).
Scanning reads each frame's 21-byte prefix and skips the payload, so a file cut short
yields every complete frame. Id 0 means the empty slice for the slice tables.

## Plan / steps

1. [x] Read the plans, README, sampler, snapshot, probe, model.
2. [x] Workspace deps; serde derives on the model; `Snapshot` moved.
3. [x] `ot-record`: format, tables, deltas, frame codec, `Recorder`, `Recording`, tests
   (9 integration + 10 unit tests green).
4. [x] `ot-core`: writer thread, `Sampler::record_to`/`stop_recording`, `RecordingProbe`,
   `Player`, `Feed`; tests (fake probe; ignored live size test). 4 tests green.
5. [x] `ot-app`: `--record`, `--replay --headless`, `--replay-info` (written; clippy pending).
6. [x] README: Layout row, "Recording" subsection (size figures to fill in).
7. [ ] **[current]** fmt, clippy (host and `x86_64-unknown-linux-gnu`), tests; the size
   breakdown; CRLF normalization.
8. [ ] Commit.

## Findings / gotchas

- Size estimate before building: a `ProcessSample` without statics is ~120 bytes of
  postcard; a `ThreadSample` ~30 bytes, of which the birth stamp (9-byte varint) and
  start time (7) are incompressible noise to lz4. 400 processes + 8000 threads is
  ~290 KB raw per frame, which lz4 alone would leave at well over 100 KB. Hence
  decision 5.
- The Windows probe lays thread rows out contiguously in process order
  (`thread_first = threads_out.len()` before each process), so rebuilding the thread
  list from per-process rows on decode reproduces the original layout.
- **Measured (2026-10-03, 30 live passes at 1 s, 403 processes, 5714 threads):** delta
  frames about 12.5 KB, keyframes about 108 KB, so steady state with a keyframe every
  60th is about 14.3 KB per frame; all-keyframes would be 126 KB per frame. The
  one-time tables for those 403 processes were 544 KB (about 1.3 KB per process: image
  path, command line, user, description, company; browser renderers have multi-KB
  command lines). Nothing dropped. Run
  `cargo test -p ot-core --test record size_of_live_frames -- --ignored --nocapture`.
- **The shared build dir mixes worktrees.** `cargo clippy -p ot-app` failed in
  `ot-shell-win/src/gfx.rs` on `DrawCmd::Image`, a variant that exists only in another
  agent's worktree: cargo reused that worktree's `ot-paint` artifact. `find crates -name
  '*.rs' -exec touch {} +` before the cargo command makes it rebuild from this tree.
- A `python - <<'EOF'` heredoc through the Bash tool mangles backslashes and sometimes
  fails to parse on long scripts; write patch scripts to `target/*.py` and run them.

## Progress log

- [x] Plan written.
- [x] Model derives (31 types), `Snapshot` moved to `ot-model`, re-exported from `ot-core`.
- [x] `ot-record` built and tested; size measured (see Findings).
- [x] `ot-core` hooks, `Player`, `Feed`, `RecordingProbe` built and tested.
- [x] `ot-app` flags and README text written.
- [ ] Workspace-wide checks, CRLF normalization, commit.

## Open questions for the user

None yet.

## Things not to do

- Do not modify `ot-shell-win` or `ot-ui` (another agent wires the transport UI).
- Do not write any file the user did not name on the command line.
- Do not use `sed -i` (converts to LF); do not push.
