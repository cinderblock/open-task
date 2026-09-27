# Windows installer

> **Status:** done · **Started:** 2026-09-26 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plan: `plans/open-task-architecture.md`. Follows `plans/process-tree-view.md`, which shipped v0.2.0 as zip/tar.gz only.

## Goal

The user asked, after v0.2.0 shipped as bare archives: "can we distribute an installer
too?" Ship a proper Windows installer as a release asset alongside the archives, built
by the release workflow, so a user can download one file, run it, and get an
Add/Remove Programs entry, a Start Menu shortcut, an uninstaller, and upgrades in
place.

## Environment / context

- Release workflow: `.github/workflows/release.yml`. Builds six targets, packages them,
  and a `publish` job downloads every artifact into `dist/`, writes `SHA256SUMS`, and
  creates the GitHub release. Anything uploaded as an artifact lands on the release.
- GitHub's `windows-latest` image has Inno Setup 6 preinstalled at
  `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`.
- This machine had no Inno Setup. It is extracted in portable mode (the official
  installer's `/PORTABLE=1`) into `target/tools/innosetup`, which is git-ignored and
  registers nothing on the system.
- The user's own copy of open-task is installed with `cargo install` in
  `~/.cargo/bin` and is running. The installer test installs to
  `%LOCALAPPDATA%\Programs\open-task`, a different place, and uninstalls itself
  afterwards.
- No code-signing certificate exists (architecture plan, open question 3). The
  installer is unsigned, like the archives, and SmartScreen will warn on first run.

## Decisions already made (don't re-ask)

1. **Inno Setup, not MSI/WiX, not MSIX.** MSIX cannot be installed unsigned at all.
   MSI is the enterprise shape but makes a self-updater clumsy (it must go through
   `msiexec`). Inno Setup gives per-user installs without elevation, an uninstaller,
   Add/Remove entry, silent switches winget understands, and it is on the runners.
2. **Per-machine, elevated. User decision, 2026-09-26: "I don't mind a UAC prompt on
   update. keep it safe."** Supersedes the first cut, which defaulted to a per-user
   install in `%LOCALAPPDATA%\Programs` to spare the future self-updater a UAC prompt.
   The reason it matters: a task manager gets run elevated, and a binary in a
   user-writable folder that is launched elevated is a privilege-escalation path for
   anything running as that user. Now `PrivilegesRequired=admin` with
   `PrivilegesRequiredOverridesAllowed=commandline`: Program Files, HKLM uninstall
   key and PATH, common Start Menu; `/CURRENTUSER` on the command line still allows a
   per-user install for people who cannot elevate, and the wizard never offers it.
   Consequence for `ot-update`: checking for updates stays unelevated; applying one
   must elevate (run the new installer, or a helper, with a UAC prompt). Landed on
   `master` 2026-09-26, verified locally (per-machine and `/CURRENTUSER` paths, see
   findings); ships with the next release, v0.3.0, which
   `plans/replace-task-manager.md` owns together with the "Replace Task Manager"
   installer task. Until then the published v0.2.1 installer still defaults to
   per-user.
3. **One installer for x64 and ARM64.** It carries both binaries and installs the one
   matching the machine (`IsArm64`). Users should not have to know their
   architecture; the self-updater can still fetch the per-target archive.
4. **Optional "add to PATH" task, off by default**, because `open-task --headless` is
   a real CLI use and the README already tells developers to put it on PATH via
   `cargo install`. Added and removed by `[Code]`, per-user or per-machine to match
   the install mode, deduplicated, and removed again on uninstall.
5. **Windows only for now.** Linux and macOS have no shell yet; packaging a headless
   stub would mislead. Revisit when those shells exist.
6. **Ships as v0.2.1.** The v0.2.0 tag's commit has no installer script, and the
   re-run dispatch builds the tagged commit on purpose, so the installer cannot be
   retro-fitted to v0.2.0 without moving the tag. A patch release is the clean path.
7. Asset name: `open-task-<tag>-windows-setup.exe`, next to the archives, covered by
   `SHA256SUMS`.

## Plan / steps

1. ~~Write this plan.~~
2. **[current]** `installer/windows/open-task.iss` and `scripts/build-installer.ps1`
   (finds ISCC, passes version and the two binaries as defines).
3. Local verification: compile with the local x64 release binary standing in for both
   architectures; silent install per-user with the PATH task; check files, Start Menu
   shortcut, uninstall registry entry, PATH; silent uninstall; check everything is
   gone.
4. Release workflow: an `installer` job after `build` on `windows-latest` that
   downloads the two Windows artifacts, unpacks them, builds the installer, uploads
   it; `publish` needs it too. Update the naming-contract comment.
5. README: "Install" section (installer first, archives, `cargo install`).
6. Bump to 0.2.1, commit, tag, push, watch the release, verify the installer asset by
   downloading it, checking its hash, and running it silently here.
7. Record results here and in the parent plan.

## Findings / gotchas

- **`https://jrsoftware.org/download.php/is.exe` serves an HTML page**, not the
  installer (10 KB of "Inno Setup Downloads"). The real files are GitHub release
  assets on `jrsoftware/issrc`: `innosetup-6.7.3.exe` (last 6.x) and
  `innosetup-7.1.0-x64.exe`. Both are Authenticode-signed by Pyrsys B.V.
- **Inno Setup 7 is current (7.1.0), but GitHub's Windows runners ship 6.7.1** at
  `C:\Program Files (x86)\Inno Setup 6\`. The script compiles under both; the local
  test used 6.7.3 to match CI. `scripts/build-installer.ps1` finds either major.
- **The official installer's `/PORTABLE=1` really is portable.** Extracted to
  `target/tools/innosetup` (7.1.0) and `target/tools/innosetup6` (6.7.3); no
  uninstall key, no `HKCU\Software\Jordan Russell` key, nothing on PATH.
- **PowerShell `Start-Process -ArgumentList` does not quote arguments with spaces.**
  A `/LOG=C:\...\Personal Projects\...` argument was split into several and Setup
  exited 1 ("failed to initialize") before writing any log. Use a log path without
  spaces, or quote the value inside the string.
- **PowerShell array pitfall:** `@($a + $b)` with `$a` a lone string concatenates
  text. The build script casts both candidate lists to `[string[]]`.
- Inno's `IsArm64` and the `x64compatible` architecture identifier need 6.3+; the
  script has a `#if Ver < EncodeVer(6,3,0)` guard so an old compiler fails loudly.
- Local verification (2026-09-26, Inno 6.7.3 build, x64 binary standing in for both
  architectures): silent per-user install with `/TASKS=addtopath` exits 0; `{app}`
  holds the exe, LICENSE, README and the uninstaller; Start Menu shortcut present;
  `HKCU\...\Uninstall\{AppId}_is1` has DisplayName, DisplayVersion 0.2.1, Publisher,
  InstallLocation; HKCU PATH gains the directory; the installed binary runs
  `--headless`; silent uninstall exits 0, removes the directory, shortcut and key,
  and leaves PATH identical to before the install.

## Progress log

- [x] Plan written.
- [x] `installer/windows/open-task.iss` and `scripts/build-installer.ps1`.
- [x] Local install/uninstall verification (see findings).
- [x] Workflow: `installer` job on `windows-latest` after `build`; `publish` needs it;
      naming-contract comment updated.
- [x] README "Install" section.
- [x] v0.2.1 released with the installer (run 36281790335, 2026-09-26; the installer
      job took 21 s). Asset verified from GitHub: 2,735,927-byte
      `open-task-v0.2.1-windows-setup.exe`, SHA-256 matches `SHA256SUMS`, file
      version 0.2.1, silent per-user install exits 0 and puts down an exe
      byte-identical to the one in the x64 archive, it runs `--headless`, silent
      uninstall leaves no directory, shortcut, registry key or PATH change.
      https://github.com/cinderblock/open-task/releases/tag/v0.2.1

- Per-machine verification (2026-09-26, elevated shell, Inno 6.7.3 build of the
  hardened script): silent default install exits 0 into `C:\Program Files\open-task`,
  which `BUILTIN\Users` cannot write; HKLM uninstall key with version and location;
  all-users Start Menu shortcut; machine PATH gains the directory with the task and
  is byte-identical after uninstall; the installed binary runs; uninstall leaves no
  directory, shortcut or key. `/CURRENTUSER` still installs into the profile with an
  HKCU key only, and uninstalls clean.

## Status

Done. The installer is a release asset from v0.2.1 on; the per-machine default is on
`master` and ships with v0.3.0. Open questions above (code signing, winget, an app
icon) are follow-ups, not blockers.

## Open questions for the user

1. **Code signing** is the only thing standing between this installer and a clean
   first-run experience. It needs a certificate (an OV cert from a CA, or Azure
   Trusted Signing which is cheap for individuals) and a secret in the repo. Not
   blocking; the plan's open question 3 already covers it.
2. **winget.** With an installer at a stable URL, a manifest PR to
   `microsoft/winget-pkgs` makes `winget install open-task` work. Recommend doing it
   after code signing, since winget's moderators flag unsigned installers.
3. **An app icon.** The binary has no icon or version resource yet, so the Start Menu
   shortcut and the Add/Remove entry show generic icons. Worth a small follow-up
   (`winres` or `embed-resource` at build time).

## Things not to do

- Do not install Inno Setup system-wide on this machine for the sake of a build; the
  portable extraction under `target/tools` is enough and leaves no trace.
- Do not move the `v0.2.0` tag to add the installer to it. Patch release instead.
- Do not make the installer require admin. Per-user is the default on purpose
  (decision 2).
