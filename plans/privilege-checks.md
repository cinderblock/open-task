# Gate elevated features on what they use, not on "is the token elevated"

> **Status:** done, CI green 2026-09-28 (`6279d1b`, run 36505270604) · **Started:** 2026-09-28 · **Repo:** `C:\Users\camer\git\Personal Projects\open-task` (branch `master`)
> Parent plans: `plans/performance-view.md` (where the flaw was found) and
> `plans/service-host-attribution.md` (which owns the code).

## Goal

The user asked to "fix the flaw you found". While checking the Performance view
without administrator rights (`runas /trustlevel:0x20000`, a SAFER "basic user"
token), the ETW sampling test ran instead of skipping and failed with
`StartTrace (profile): Access is denied`. Service tags and CPU sampling decide
whether they can work by asking `is_elevated()`, which reads `TokenElevation`.

## The flaw, precisely

1. **`is_elevated()` answers the wrong question.** `TokenElevation` says whether
   UAC split this token, not whether it holds what a feature needs. The basic-user
   token reports elevated and High integrity, yet has lost the Administrators group
   and the privileges that matter. With it, open-task claims service tags and CPU
   sampling are available (`Capabilities::service_tags`, `cpu_sampling`, the "tags
   on" headless line, an enabled "Sample CPU" menu item), then every tag read and
   every sample fails.
2. **`enable_privilege()` reports success for a privilege the token lacks.** Its
   comment says "`windows` maps that case to Err". It does not: `BOOL::ok()`
   (windows-result 0.4.1) checks only the return value, and `AdjustTokenPrivileges`
   returns TRUE with the last error set to `ERROR_NOT_ALL_ASSIGNED` when it could not
   enable the privilege. So every caller that trusted the return value was told yes.

## Decisions

1. **Each feature is gated on what it uses.** Service tags read other processes'
   memory, which needs `SeDebugPrivilege` enabled (Administrators get only limited
   query rights on SYSTEM processes otherwise). CPU sampling needs
   `SeSystemProfilePrivilege` for the kernel profile source, and membership in
   Administrators or Performance Log Users to control trace sessions.
2. **A new `access` module** holds the token questions: `enable_privilege` (true only
   if the privilege is now enabled), `holds_privilege` (present in the token,
   enabled or not, without changing anything), `in_group` (`CheckTokenMembership`,
   which ignores deny-only group entries, the way a filtered or basic-user token
   carries Administrators).
3. `is_elevated()` goes away from the probe. `SampleError::NotElevated` becomes
   `SampleError::NotPermitted`; the message still tells the user the remedy (run as
   administrator), and says which privilege is missing.
4. Verified three ways: elevated (this shell), SAFER basic user (`runas
   /trustlevel:0x20000`), and an ordinary unelevated start (`explorer.exe` on a
   `.cmd` wrapper, per memory `elevated-shell-on-user-desktop`).

## Plan / steps

1. ~~Read the code; confirm both flaws; write this plan.~~
2. ~~`access.rs` with tests; `tags.rs`, `profile.rs`, `mod.rs` use it; rename the
   error; headless wording.~~
3. ~~Checks (fmt, clippy on three targets, tests); run the probe tests and the
   headless app under all three tokens.~~
4. ~~Plans (this, attribution, performance view); commit; push with CI watched.~~

## Findings / gotchas

- **Windows' behavior, pinned by a test** (`access::tests::windows_reports_success_
  for_a_privilege_it_did_not_enable`): `AdjustTokenPrivileges` for
  `SeCreateTokenPrivilege`, which no user token holds, returns success and sets the
  last error to `ERROR_NOT_ALL_ASSIGNED`. That is the whole of flaw 2.
- **Results under three tokens** (`--headless` capabilities and the probe's 26
  tests, all passing in every case):

  | Token | Integrity | Administrators | `service_tags` | `cpu_sampling` |
  | --- | --- | --- | --- | --- |
  | This shell (elevated admin) | High | enabled | true | true |
  | `runas /trustlevel:0x20000` (basic user) | High | deny only | false (was true) | false (was true) |
  | `explorer.exe` on a `.cmd` (ordinary unelevated) | Medium | deny only | false | false |

  Elevated, the real 600 ms self-sample test ran; under the other two the new
  "refused before anything starts" test ran instead and got `NotPermitted`.
- **Running a `.cmd` under `runas /trustlevel` from `%TEMP%`**: with
  `cd /d %~dp0` and bare executable names, cmd said `'ot-probe-tests.exe' is not
  recognized` although the working directory was right (the same script started via
  Explorer worked). Absolute paths fixed it; not investigated further.
- **A safety hook blocks `Remove-Item` in a PowerShell command that also contains
  `cd /d` text** (it read `/d` as a path to delete). Delete in a separate command.

## Progress log

- [x] Plan written.
- [x] Fix: `access.rs` (`enable_privilege`, `holds_privilege`, `in_group`);
      `TagProbe::new` needs `SeDebugPrivilege` enabled; `profile::can_sample`;
      `SampleError::NotPermitted`; `Capabilities::cpu_sampling` from `can_sample`;
      headless says "needs administrator".
- [x] Verified under three tokens (table above); fmt; clippy on three targets.
- [x] Committed (`6279d1b`), pushed, CI green on all five jobs (run 36505270604).
      On both Windows runners the probe suite took the same ~0.7 s as before the
      fix (0.73 s and 0.69 s, against 0.73 s and 0.71 s at `a3b36ab`), which fits
      the real 600 ms self-sample still running there rather than skipping.
