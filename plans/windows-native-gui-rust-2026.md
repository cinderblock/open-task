# Windows-Native High-Performance GUI in Rust — 2026 Technology Survey

## Goal

Decide the rendering/UI stack for a high-performance Windows desktop app written in Rust
(project: `open-task`). Needs to look like a first-class Windows 11 Fluent app (Mica/Acrylic,
correct DPI, correct text rendering) while staying fast and low-memory.

## Environment / context

- Machine: Windows 11 Pro N, 10.0.26200 (25H2-era build).
- Project root: `C:\Users\camer\git\Personal Projects\open-task` (git repo, branch `master`).
- Date of survey: 2026-09-24.
- Language: Rust. Bun/TS rules in global CLAUDE.md apply only if a web UI is chosen.

## Questions being answered

1. Direct2D + DirectWrite + DirectComposition / Windows.UI.Composition — still the modern stack?
   What does WinUI 3 render with? Can a non-XAML app get Mica/Acrylic? Gotchas.
2. windows-rs maturity in 2026 for D2D/DWrite/DComp.
3. Win32 common controls (`SysListView32` + LVS_OWNERDATA) — still viable? Real limits.
4. WinUI 3 / Windows App SDK health, deployment, perf, Rust usability.
5. WebView2 / Tauri real memory + cold start + fast IPC paths.

## Status

- [x] Question list written (2026-09-24).
- [ ] Research. **Not started.** The research agent hit a weekly API rate limit before
      making a single query and produced nothing. Re-run when useful; the main plan
      (`plans/open-task-architecture.md`) already commits to Direct2D + DirectWrite +
      DirectComposition on the strength of TMOG's published architecture, so this is a
      second opinion, not a blocker.
- [ ] Report folded into the main plan's "Findings / gotchas".

## Findings

None yet.

