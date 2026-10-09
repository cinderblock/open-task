# Chromium process roles: split `chrome.exe` by what each process does

> **Status:** done: roles, Electron app names; ETW per-site attribution ruled out · **Started:** 2026-10-07 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)

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

- [x] 5. ETW spike (2026-10-09): **not feasible** on Chrome stable 154. See
  "Findings: ETW" below.
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

## Findings: ETW (per-site attribution without an extension)

From Chromium source (main, 2026-10) and a 19.7-minute capture on this machine
with the Chrome provider at all keywords (1.26M events, none lost):

- Export is on by default, no flag: `kEnableEtwExports` is
  `FEATURE_ENABLED_BY_DEFAULT` (`components/tracing/common/etw_export_win.cc`),
  enabled in every process from `services/tracing/public/cpp/trace_startup.cc`.
  The provider is TraceLogging `Google.Chrome`,
  `{D2D578D9-2936-45B6-A09F-30E32715F42D}`, exporter
  `base/trace_event/etw_interceptor_win.cc`. `logman query providers` does not
  list it; `xperf -a dumper` decodes it. Electron and CEF apps emit under the same
  GUID and name, so a consumer must classify by PID.
- Only event names, a track id and debug-annotation arguments are exported. Typed
  Perfetto arguments and track descriptors are dropped, so Chrome's
  `process_labels` / site labels **never reach ETW**. Dynamic event names arrive
  as `FILTERED`; URL arguments are not scrubbed.
- No navigation or frame-to-site events: zero `NavigationRequest`,
  `SiteInstance`, `RenderProcessHost`, `site_url`, `process_lock`; every
  `render_frame_host` field was empty.
- The one usable mapping: the network service's `CorsURLLoader::CorsURLLoader`
  carries `url` and `process_id`, the renderer's child id (= its
  `--renderer-client-id`). But it names subresource URLs (often third-party), only
  while a page is loading, so idle open tabs are never seen.
- **Chrome 154 itself went silent** about 6 s into the session (its browser
  process emitted 5 events at 0.014 s, then nothing). Only Discord's older
  Chromium kept emitting for the whole capture. Cause unproven: possibly the
  per-process Perfetto startup session the exporter runs is never adopted.
- Volume at all keywords: about 1,060 events/s, 266 MB in 20 minutes, nearly all
  from one Electron renderer. A consumer would enable only `loading|netlog`
  (`0x40080`).

Verdict: no top-level site per renderer, and nothing for tabs that are idle;
not worth building. An untried check: a 15 s capture at `0x40080` with a kernel
process rundown would show whether Chrome's silence came from the volume of
keywords enabled. Even if it did, the coverage limits above stand.

## Open questions for the user

None open.

## Things not to do

- Don't key off executable names (`chrome.exe`, `msedge.exe`): the scheme covers
  every Chromium embedder.
