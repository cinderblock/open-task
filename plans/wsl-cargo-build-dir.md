# Cargo build dir: stop worktrees getting each other's artifacts

## Goal

Two checkouts of a repo must never get each other's build artifacts, on any project on
this machine, while keeping what the shared build dir was for: intermediates out of each
project's `target/`, and total disk use bounded. The trigger is the `ISSUES.md` entry
"WSL: worktrees share one cargo build dir, and a green build can be another's code".

## Environment / context

- Machine: Noook, Windows 11, 6 cores / 12 threads. C: is the only local disk (NTFS,
  930 GB, 70 GB free on 2026-10-30). WSL's disk is a vhdx on C: (65 GB), so `df` inside
  WSL (893 GB free) overstates the room.
- cargo 1.99.0 on both sides. WSL distro `Ubuntu`, user `cameron`.
- Files that matter, none of them in a git repo:
  - `C:\Users\camer\.cargo\config.toml`, the machine-wide config
  - `C:\Users\camer\.cargo\prune-build-dir.ps1`, run by the scheduled task "Prune Cargo
    build dirs" at 4 AM, as user `camer`, LogonType S4U
- Scratch work: `%TEMP%\ot-builddir\` (scripts, repro, logs).
- Heavy builds: `node ~/.claude/bin/cpu-slots.mjs run --slots 2` with
  `CARGO_BUILD_JOBS=2`.

## Decisions already made (don't re-ask)

- The fix goes in the config, keeps the disk-space intent, and covers every project.
  The user approved this as its own thread.
- Don't wipe any existing build dir: `qt-gpu-charts` may be using WSL's.
- Whether `qt-gpu-charts`' earlier numbers were affected is the user's open question, not
  part of this task; report evidence only.
- `build-dir = "{cargo-cache-home}/build/{workspace-path-hash}"`. Correctness over
  sharing: third-party crates can't stay shared (see Findings), and the measured cost
  (7.6 GB for the biggest Tauri workspace) fits under the existing 150 GB cap. Everything
  stays in one place, where the prune task can reach it.
- The old flat directories age out by themselves (Windows: TTL and the budget's
  oldest-first pass; WSL: `prune-build-dir.sh`). Nothing deletes them by hand.
- WSL gets its own pruner, whole build dirs at a time. The `.ps1` can't do it over
  `\\wsl.localhost`, because cargo's `flock()` means nothing there, and it starts the
  `.sh` through `wsl.exe`, so the same task and log cover both. WSL's cap is 50 GB.
- Empyrean's project config (`{workspace-root}/target/build`) stays. It is harmless, and
  it isn't this task's to change.

## Findings / gotchas

- **WSL has no `~/.cargo/config.toml`.** The issue and the plans say it does. It is the
  Windows file: cargo walks up from the working directory, and from
  `/mnt/c/Users/camer/...` it finds `/mnt/c/Users/camer/.cargo/config.toml`
  (`cargo -Zunstable-options config get --show-origin build`, with
  `RUSTC_BOOTSTRAP=1`). From `~` in WSL, `build` is not set at all. So there is one file,
  it applies to Windows and to WSL builds under `/mnt/c/Users/camer`, and
  `{cargo-cache-home}` means a different directory on each side.
- **So Windows has the same bug**, and has shown it: `plans/v0.9-icons-summary-recorder.md`
  (three agents overwriting each other's rlibs), `plans/flight-recorder-core.md`
  (`DrawCmd::Image` from another worktree's `ot-paint`), and Empyrean's
  `crates/.cargo/config.toml`, which opts out with `{workspace-root}/target/build`.
- **Cause, reproduced with three tiny crates on both platforms** (`%TEMP%\ot-builddir\repro.sh`,
  a scratch `CARGO_HOME` under the real config). Cargo hashes a path crate relative to the
  workspace root, so a crate in two worktrees has one name in the build dir. Freshness is
  by mtime: if the second worktree's sources are older than the first's build, cargo
  calls the unit fresh, and uplifts the other worktree's artifact:

  ```
  --- build in worktree BB
      Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.35s
      binary: LIB-MARK-AA  plugin: PLUGIN-MARK-AA  -> WRONG CODE
  build/debug/.fingerprint/app-8099edce9b837929    (one entry, two worktrees)
  ```

  Windows gives the same, with `0.11s`. The comment in `config.toml` that "every worktree
  path gets its own copy" holds only for crates outside the workspace root, such as
  Empyrean's `src-tauri` reaching `../crates`, whose hash takes the absolute path.
- **`{workspace-path-hash}` is real in 1.99.0**, documented in
  `share/doc/rust/html/cargo/reference/config.html`, and gives
  `<build-dir>/69/b671320b646d00/debug/...`. In WSL the repro is right with it.
- **Cargo offers no way to keep third-party crates shared while separating workspace
  crates.** One directory means one namespace, and nothing configurable machine-wide feeds
  the workspace path into the hash. `build.rustc-workspace-wrapper` does affect only
  members' hashes, but its value is static in a machine-wide config, and `cargo clippy`
  replaces it. `-Zchecksum-freshness` and `-Zfine-grain-locking` are nightly.
- **Nothing pruned WSL's build dir** until this task. The prune task was Windows-only;
  WSL's `~/.cargo/build` is 38 GB (36 GB of it `debug`), on the vhdx on C:.
- **The real cost of a build dir per checkout** (2026-10-30, `git archive` of Empyrean into
  a scratch dir, nothing shared, full `cargo build`): 7.6 GB, 619 units, 13m49s at
  `CARGO_BUILD_JOBS=2` beside other work. Not the "~20 GB" in the old comment, which was
  written before pruning, of `target/debug`s that had never been cleaned. A private
  open-task dir in WSL, debug and release, is 3.7 GB; Empyrean's own per-worktree
  `crates/target/build` dirs are 0.2-1.7 GB.
- **Estimating from `Cargo.lock` undercounts about 6 times** (1.2 GB for that workspace,
  0.3 GB for open-task) because it takes one mean unit per crate. Scaled by that, the
  checkouts built in the last 7 days would hold about 36 GB of dependencies, and those of
  the last 14 days about 150 GB, the cap. The whole of the prune task's roots held
  133 GB on 2026-10-30, so the equilibrium stays near today's size.
- Other checks of the template: drive-letter case (`c:` vs `C:`, as VS Code passes it)
  gives one hash. `cargo clean` now reaches only its own workspace. The longest path in
  the old shared dir was 175 characters, so the 18 added stay far from 259, and long paths
  are enabled anyway. Nothing on this machine hardcodes the old `~/.cargo/build/debug`.
- **Cargo 1.99 still uses the old layout** under the new path (`.fingerprint/`, `deps/`,
  `build/`, `.cargo-build-lock`; `-Zbuild-dir-new-layout` is nightly), so
  `prune-build-dir.ps1`'s parsing holds.
- `Prune Cargo build dirs` runs as S4U, and `wsl.exe` does work from an S4U task
  (tested with a throwaway task of the same principal).
- **Evidence on `qt-gpu-charts`' numbers, either way:** the 1:33 PM PDT (20:33Z) release
  binary in `~/.cache/open-task-target-qt-gpu` holds its own chart code
  (`glDrawArraysInstanced`) and no NVML; `~/.cache/ot-ab/open-task-base` (9:10 AM PDT,
  16:10Z) holds neither, which is right for a baseline. So those two are what they claim.
  Binaries from earlier in the day were overwritten and can't be checked.
- Git Bash mangles arguments to `wsl.exe` (`/mnt/...` paths) and strips the braces from
  `{...}` arguments to bash; `wsl.exe` expands `$(...)` in a shell without the login
  profile. Run scripts from files, from PowerShell.

## Plan / steps

1. [x] Confirm the cause, on both platforms.
2. [x] Measure what per-workspace dirs cost for a Tauri project: 7.6 GB, so the plain
   template is enough.
3. [x] Change the config; fix the comments that are wrong (applied 2026-10-30).
4. [x] Prune: hollow build dirs are removed, `prune-build-dir.sh` covers WSL, both tested
   on made-up trees and dry-run on the real roots.
5. [x] Prove it (results below).
6. [x] Close the `ISSUES.md` entry.

Originals of the two changed files: `%TEMP%\ot-builddir\orig\`. The change itself is one
line in `config.toml`, the value of `build-dir`.

## Results

Each run used the real `config.toml`, with a scratch `CARGO_HOME` for the tiny repro.

| Check | Before | After |
| --- | --- | --- |
| Tiny repro, WSL | `Finished`, nothing compiled, `BB` runs `AA`'s code | `BB` compiles, right code |
| Tiny repro, Windows | the same | the same fix |
| Fingerprints | one per crate, two worktrees | one per crate per worktree |

Real open-task, 2026-10-30 4:27-4:35 PM PDT (23:27-23:35Z), WSL, `build-linux.sh
--release`, a temporary worktree at `15a308f` (no NVML) first, then this worktree:

- `grep -c libnvidia-ml`: 0 and 1, as each source says. 0 here was the original failure.
- `scripts/linux-gui-smoke.sh`, private Xvfb: OK on both, GTK and Qt.
- Building the first again: `ot-app` alone recompiled, because `build-linux.sh` copies the
  plugins and the copies have new mtimes. Still 0.
- Each release build dir: 476 MB.

Prune, the real scheduled task, 2026-10-30 4:35 PM PDT (23:35Z), result 0: 136.5 GB on
Windows, 5 incremental caches and 3 hollow dirs removed, 0 failed; WSL 41.2 GB, its empty
`tmp` removed. Log: `~/.cargo/prune-build-dir.log`.

## What to expect from here

- The first build of every checkout compiles its dependencies once, and that goes for
  existing checkouts too. It is a one-time cost per checkout.
- The old flat directories are unused now and go without help. On Windows (128 GB) they
  go as the 14- and 30-day TTLs pass, or sooner by the budget's oldest-first pass. WSL's
  (38 GB) goes whole, after 14 idle days or when over budget.
- C: had 64.3 GB free after all this. Worst case is the budget (150 GB, now 136.5) plus
  a day's new build dirs, before the next 4 AM pass.
- WSL's vhdx doesn't shrink when files are deleted; the space is reused inside it.

## Things not to do

- Don't test in `~/.cargo/build` when a scratch `CARGO_HOME` will do.
- Don't put `{workspace-path-hash}` back out to save disk: see the comment in
  `config.toml`.
