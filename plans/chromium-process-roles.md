# Chromium process roles: split `chrome.exe` by what each process does

> **Status:** done (level 1) · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

## Goal

The user, 2026-10-07: Chrome (and other multi-process apps) run many cooperating
processes; can open-task tell them apart instead of one `chrome.exe` bucket? Ideally
per tab / per domain.

Level 1, approved by the user: label each Chromium-family child process with its
role, read from its command line, for Chrome and anything built on Chromium
(Electron apps, Edge, WebView2), with no browser extension.

## Environment / context

- Command lines are already read by the probe
  (`ot-probe/src/imp/windows/details.rs`, `ProcessStatic::command_line`).
- Precedent: the `svchost.exe (group)` Name cell, `ot-ui/src/process_rows.rs`
  (`svchost_group`, `process_name_cell`).
- The usage chart's buckets ("programs") are keyed by process name in
  `ot-core/src/usage.rs` (`program_id`); the UI looks them up by name in
  `ot-ui/src/view.rs` (`selected_program`, and the History paint).

Survey of this machine (2026-10-07), every `--type=` process: Chrome, Edge
WebView2, Discord, Slack, Signal, Logic, a dev `electron.exe`. All follow one
scheme:

| `--type=` | extra | role |
| --- | --- | --- |
| (none) | | the browser / main process |
| `renderer` | `--extension-process` | extension |
| `renderer` | | web content |
| `gpu-process` | | GPU |
| `utility` | `--utility-sub-type=network.mojom.NetworkService` (also `storage.`, `audio.`, `video_capture.`) | that service |
| `crashpad-handler` | | crash reporter (no mojo flags) |

Every child except crashpad carries `--mojo-platform-channel-handle=` and
`--field-trial-handle=`.

## Decisions already made (don't re-ask)

1. **No browser extension**, so no per-tab / per-domain attribution through one.
2. **Chrome and the Electron family alike**: the rule is the command-line scheme,
   not the executable name.
3. **The main process stays unlabelled**: knowing it is Chromium would need its
   children; "unlabelled" reads as "the main one" next to labelled siblings.

## Plan / steps

1. `ot-model::chromium::role(command_line)` and `ProcessStatic::label()`
   (`chrome.exe (GPU)`, the svchost `(group)` idiom). Tests.
2. Usage chart programs keyed by the label, so the History splits
   `chrome.exe (Renderer)` from `chrome.exe (GPU)`; lookups by statics.
3. Name cell, ancestry strip, Map tiles and hover show the label.
4. README, checks, commit.

## Findings / gotchas

- Renderers are one bucket: which site a renderer serves is not on its command
  line, in window titles (all tab windows belong to the browser process), or
  anywhere else the OS exposes.
- Remote debugging (CDP) is ignored for the default profile since Chrome 136;
  `chrome.processes` is Dev/Canary only.

## Progress log

- [x] 1. role parser (`ot-model/src/chromium.rs`, `ProcessStatic::role` / `label`)
- [x] 2. usage programs keyed by label (`Usage::program_of`)
- [x] 3. UI labels: Name cell, ancestry strip, Map tiles and hover
- [x] 4. README, checks (fmt, clippy -D warnings, tests), commit. Checked the
  parser over every live process on 2026-10-07: 37 kinds of Chromium child across
  Chrome, Edge WebView2, Discord, Slack, Signal, Logic and `electron.exe` all
  labelled, no other process labelled.

## Open questions for the user

1. Per-domain attribution through Chrome's ETW provider (no install needed) is
   unexplored: does Chrome publish process-to-site labels to ETW without being
   started with tracing flags? Worth a look only if the user wants it.
2. A generic `electron.exe` (dev setups) runs several apps under one name; its
   renderers' `--app-path=` would tell them apart. Not done.

## Things not to do

- Don't key off executable names (`chrome.exe`, `msedge.exe`): the scheme covers
  every Chromium embedder.
