# Linux probe

## Goal

Make the Linux build measure the machine. Until now `ot-probe` on Linux was the
stub: every pass returned `ProbeError::Unsupported`, and `open-task --headless`
exited 3 ("no probe implementation yet"). The released Linux binaries
(`x86_64-` and `aarch64-unknown-linux-gnu`) were useful for nothing but
`--version`.

Linux has no GUI shell yet (`ot-shell-win` is Direct2D), so headless mode is the
face of this work: `open-task --headless` must print real CPU, memory, process,
disk and network numbers, and `--record` must capture them for replay on Windows.

## Environment / context

- Windows 11 dev box; Linux testing in WSL2 `Ubuntu` (24.04, kernel
  6.18-microsoft, glibc 2.39), which has its own rustup/cargo (`~/.cargo/bin`).
- Build in WSL from the Windows checkout, with a Linux-native target dir so the
  Windows `target/` is untouched and the build is not over 9p:
  `cd "/mnt/c/Users/camer/git/Personal Projects/open-task" && CARGO_TARGET_DIR=~/.cache/open-task-target cargo ...`
- Running a multi-line script through `wsl.exe` from Git Bash mangles it. Write
  the script to a file and run `bash "$(wslpath -u 'C:/…/script.sh')"` with
  `MSYS_NO_PATHCONV=1`.
- Release CI builds Linux on `ubuntu-latest` (glibc 2.39 at build time, binary
  needs 2.34). CI `check` matrix runs clippy + tests + headless smoke on
  `linux-x64`; the smoke step accepts exit 0 or 3.
- `libc` 0.2.189 is already in `Cargo.lock` (transitively); the probe adds it as a
  direct Linux-only dependency.

## Decisions already made (don't re-ask)

- **User asked for real data on Linux** (2026-10-13: "definitely get it to report
  real data").
- **Pure `/proc` and `/sys`, plus `libc` for syscalls.** No `procfs`/`sysinfo`
  crates: the Windows side is hand-written against the OS and owns its cost
  model; Linux does the same.
- **Process identity:** `ProcessKey.birth` = `starttime` (field 22 of
  `/proc/<pid>/stat`, clock ticks since boot), as `ot-model::identity` already
  documents. Threads likewise, from `/proc/<pid>/task/<tid>/stat`.
- **Process actions go through a pidfd** (`pidfd_open` + `pidfd_send_signal`),
  after checking the PID's `starttime` matches the key. Once the pidfd is open it
  names that process even if the PID is recycled, so the check cannot race.
- **"End task" is `SIGKILL`**, matching Windows `TerminateProcess` semantics.
- **macOS stays a stub** in this work.

## Mapping (Windows concept → Linux source)

| Model field | Linux source |
| --- | --- |
| CPU total / per core | `/proc/stat` `cpu`/`cpuN` lines, busy = total − idle − iowait |
| Physical core, socket | `/sys/devices/system/cpu/cpuN/topology/{core_id,physical_package_id}` |
| Core kind (hybrid) | `/sys/devices/cpu_core/cpus`, `/sys/devices/cpu_atom/cpus` (Intel) |
| Core clock | `/sys/devices/system/cpu/cpuN/cpufreq/scaling_cur_freq` |
| Memory | `/proc/meminfo` (see below) |
| Process list, CPU, RSS | `/proc/<pid>/stat`, `/proc/<pid>/status` |
| Private working set / private bytes | `RssAnon` / `RssAnon + VmSwap` |
| Handles | entries in `/proc/<pid>/fd` (readable only for own processes unless root) |
| Disk I/O per process | `/proc/<pid>/io` `read_bytes`/`write_bytes` (own processes unless root) |
| I/O counters | `/proc/<pid>/io` `syscr`/`syscw`/`rchar`/`wchar` |
| Session id | `/proc/<pid>/sessionid` (audit session = logind session); unset → 0 |
| Services in a process | systemd unit from `/proc/<pid>/cgroup` (`…/foo.service`) |
| Priority | nice (stat field 19), policy (field 41: FIFO/RR → Realtime) |
| Suspended | state `T` (stopped) |
| Kernel threads | `PF_KTHREAD` in stat flags → `ProcessKind::Windows` ("part of the OS") |
| Disks | `/proc/diskstats` for disks with `/sys/block/X/device`; facts from `/sys/block/X` |
| Network | `/proc/net/dev`; facts from `/sys/class/net/X`; addresses via `getifaddrs` |
| Volumes | `/proc/self/mountinfo` + `statvfs`; labels from `/dev/disk/by-label` |
| Battery | `/sys/class/power_supply/*` |
| Temperature | hwmon `coretemp` "Package id 0" → Package; `k10temp` Tctl → Tctl |
| Package power | RAPL `/sys/class/powercap/intel-rapl:0/energy_uj` (root-only on most kernels) |
| Connections | `/proc/net/{tcp,tcp6,udp,udp6}`, inode → PID via `/proc/*/fd` |
| System facts | `/etc/os-release`, hostname, `/sys/class/dmi/id/*`, `/sys/firmware/efi`, `/proc/swaps` |
| Sessions | `/run/systemd/sessions/*` |

`/proc/meminfo` mapping: `total`=MemTotal, `available`=MemAvailable,
`cached`=Cached, `committed`=Committed_AS, `commit_limit`=MemTotal+SwapTotal
(the model defines it as physical + swap; the kernel's own `CommitLimit` only
means anything under `overcommit_memory=2`), `swap_used`=SwapTotal−SwapFree,
`free`=MemFree, `modified`=Dirty+Writeback, `nonpaged_pool`=SUnreclaim (kernel
memory that cannot be reclaimed — where a driver leak shows),
`compressed`=Zswap when present. `standby` and `paged_pool` stay `None`.

## Plan / steps

1. Backend skeleton (`imp/linux/`), dispatch in `imp.rs`, `libc` dependency.
2. CPU, memory, processes, threads, hardware. Test in WSL.
3. Disks, network, volumes, battery, thermals, RAPL.
4. Process control (pidfd), inventory (connections, system facts), sessions,
   `is_elevated`.
5. CI smoke step: require exit 0 on Linux (no longer accept 3). README + crate
   docs. Release.

Later (not this pass; listed so they are not forgotten):

- Full service list (systemd over D-Bus, or `systemctl list-units` output).
- Startup entries (`~/.config/autostart`, `/etc/xdg/autostart`, user units).
- Installed apps (dpkg status, rpm, flatpak, snap).
- GPU (amdgpu `gpu_busy_percent`, i915/xe via PMU, NVIDIA via NVML).
- CPU sampling (`perf_event_open`).
- A Linux GUI shell.

## Findings / gotchas

- (none yet)

## Progress log

- [ ] 1. Skeleton
- [ ] 2. CPU / memory / processes / threads / hardware
- [ ] 3. Devices
- [ ] 4. Control, inventory, sessions
- [ ] 5. CI, docs, release

## Open questions for the user

- (none yet)

## Things not to do

- Don't build the Linux target into the Windows `target/` dir from WSL; use
  `CARGO_TARGET_DIR=~/.cache/open-task-target`.
- Don't pipe multi-line scripts through `wsl.exe` inline from Git Bash.
