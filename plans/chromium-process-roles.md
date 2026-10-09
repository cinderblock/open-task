# Chromium process roles: split `chrome.exe` by what each process does

> **Status:** level 1 done; ETW spike and Electron app names in progress · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

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
4. **ETW spike: yes**, if it gets per-site attribution with no plugin or
   extension (user, 2026-10-07).
5. **Generic `electron.exe` apps told apart by `--app-path`: yes** (user,
   2026-10-07).

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

- [ ] 5. ETW spike: does Chrome publish renderer-to-site labels over ETW with no
  flags or install? (research running in a subagent)
- [x] 6. Generic `electron.exe`: named by the project folder holding the
  `node_modules` it runs from, else `--user-data-dir`, else `--app-path`
  (`chromium::electron_app`, `ProcessStatic::app`). Checked live 2026-10-09: the
  T3 Code dev build's main process, its Node child and every Chromium child read
  `electron.exe (t3code ...)`.

## Findings: Electron app names

- `--user-data-dir` is not stable within one app: the T3 Code main process was
  started with `...\Roaming\t3code` and its children carry `...\t3code-v2` (the
  app changes it at runtime). The image path is the same for all of them.
- The app's script child (`electron.exe ...\apps\server\dist\bin.mjs`, run as
  Node) has no Chromium switches at all; only the image path names it.

## Open questions for the user

None open.

## Things not to do

- Don't key off executable names (`chrome.exe`, `msedge.exe`): the scheme covers
  every Chromium embedder.
