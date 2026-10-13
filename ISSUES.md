# Issues

Problems noticed while working on something else, left for a thread of their own.

## The build label can name an older commit and miss `-dirty`

Seen 2026-10-12: `target\debug\open-task.exe`, built at 17:15 from `master` at
`db1435a` with uncommitted changes in `crates/ot-shell-win`, showed `45f39be`
(an earlier commit, no `-dirty`) in the sidebar's version line. An hour earlier
a build showed `a6166dc-dirty` correctly. In between, a second checkout (a
temporary linked worktree at `45f39be`, `git worktree add`) was built with its
own `CARGO_TARGET_DIR`, then removed. `crates/ot-app/build.rs` `watch()`
decides when the label is worked out again.

Matters because the label is how a screenshot or a bug report says which build
it came from.
