//! Processes and their threads, from `/proc`.
//!
//! Each pass reads `stat` and `status` for every process, `io` and the `fd`
//! directory for those this user may read (all of them as root), and each thread's
//! `stat` for multi-threaded processes. Facts that do not change over a process's
//! life (its name, path, command line, owner, services) are read once, at first
//! sight, and shared by pointer after that.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read as _};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_model::process::{
    Architecture, Integrity, IoCounters, Priority, ProcessKind, ProcessSample, ProcessStatic,
};
use ot_model::service::{ServiceInfo, ServiceState};
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState, WaitReason};
use ot_model::units::{Bytes, Percent};
use ot_model::ProcessKey;

use super::fs;

/// `PF_KTHREAD` in a process's `stat` flags: a kernel thread, with no user space.
const PF_KTHREAD: u64 = 0x0020_0000;
/// `SCHED_FIFO` and `SCHED_RR`, the realtime policies.
const SCHED_FIFO: u64 = 1;
const SCHED_RR: u64 = 2;
/// What `/proc/<pid>/sessionid` and `loginuid` read when never set.
const UNSET_ID: u64 = u32::MAX as u64;
/// The Windows wait reason the UI labels "Suspended", given to a stopped thread.
const WAIT_SUSPENDED: WaitReason = WaitReason(5);

/// The fields of `/proc/<pid>/stat` (or a thread's) that the probe uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Stat {
    state: u8,
    ppid: u32,
    flags: u64,
    minflt: u64,
    majflt: u64,
    /// User plus system time, in clock ticks.
    ticks: u64,
    nice: i64,
    num_threads: u32,
    /// Clock ticks after boot the process started: its birth stamp.
    starttime: u64,
    vsize: u64,
    policy: u64,
}

/// Split a `stat` line into the command name and the parsed fields. The name is
/// in parentheses and may itself hold spaces and parentheses, so the fields start
/// after the last `)`.
fn parse_stat(buf: &[u8]) -> Option<(&[u8], Stat)> {
    let open = buf.iter().position(|&b| b == b'(')?;
    let close = buf.iter().rposition(|&b| b == b')')?;
    let comm = buf.get(open + 1..close)?;
    let mut f = buf
        .get(close + 2..)?
        .split(|&b| b == b' ' || b == b'\n')
        .filter(|p| !p.is_empty());
    // Field 3 (state) is the first after the name; `nth(k)` skips k.
    let state = *f.next()?.first()?;
    let ppid = fs::num(f.next()?) as u32; // 4
    let flags = fs::num(f.nth(4)?); // 9
    let minflt = fs::num(f.next()?); // 10
    let majflt = fs::num(f.nth(1)?); // 12
    let utime = fs::num(f.nth(1)?); // 14
    let stime = fs::num(f.next()?); // 15
    let nice = fs::signed(f.nth(3)?); // 19
    let num_threads = fs::num(f.next()?) as u32; // 20
    let starttime = fs::num(f.nth(1)?); // 22
    let vsize = fs::num(f.next()?); // 23
    let policy = f.nth(17).map_or(0, fs::num); // 41
    Some((
        comm,
        Stat {
            state,
            ppid,
            flags,
            minflt,
            majflt,
            ticks: utime + stime,
            nice,
            num_threads,
            starttime,
            vsize,
            policy,
        },
    ))
}

/// A process's birth stamp from its `stat`, for checking a [`ProcessKey`].
pub(super) fn starttime(stat: &[u8]) -> Option<u64> {
    parse_stat(stat).map(|(_, s)| s.starttime)
}

/// The fields of `/proc/<pid>/status` the probe uses, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Status {
    /// Effective user id.
    euid: Option<u32>,
    rss: u64,
    rss_anon: u64,
    peak_rss: u64,
    swap: u64,
}

fn parse_status(buf: &[u8]) -> Status {
    let mut s = Status::default();
    for line in buf.split(|&b| b == b'\n') {
        let Some((key, value)) = fs::key_value(line) else {
            continue;
        };
        match key {
            b"Uid" => {
                s.euid = value
                    .split(|&b| b == b'\t' || b == b' ')
                    .filter(|p| !p.is_empty())
                    .nth(1)
                    .map(|v| fs::num(v) as u32);
            }
            b"VmRSS" => s.rss = fs::kb_value(value).unwrap_or(0),
            b"RssAnon" => s.rss_anon = fs::kb_value(value).unwrap_or(0),
            b"VmHWM" => s.peak_rss = fs::kb_value(value).unwrap_or(0),
            b"VmSwap" => s.swap = fs::kb_value(value).unwrap_or(0),
            _ => {}
        }
    }
    s
}

/// `/proc/<pid>/io`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Io {
    rchar: u64,
    wchar: u64,
    syscr: u64,
    syscw: u64,
    /// Bytes the process caused to be fetched from, or sent to, storage.
    read_bytes: u64,
    write_bytes: u64,
}

fn parse_io(buf: &[u8]) -> Io {
    let mut io = Io::default();
    for line in buf.split(|&b| b == b'\n') {
        let Some((key, value)) = fs::key_value(line) else {
            continue;
        };
        let v = fs::num(value);
        match key {
            b"rchar" => io.rchar = v,
            b"wchar" => io.wchar = v,
            b"syscr" => io.syscr = v,
            b"syscw" => io.syscw = v,
            b"read_bytes" => io.read_bytes = v,
            b"write_bytes" => io.write_bytes = v,
            _ => {}
        }
    }
    io
}

/// The systemd service a process belongs to, from its `/proc/<pid>/cgroup`:
/// the innermost `*.service` in its control group path, without the suffix. A
/// user service (`user@1000.service/app.slice/foo.service`) is `foo`; the user
/// manager itself (`user@1000.service/init.scope`) is `user@1000`.
fn service_of(cgroup: &str) -> Option<&str> {
    let path = cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .or_else(|| {
            cgroup
                .lines()
                .find_map(|l| l.split_once(":name=systemd:").map(|(_, p)| p))
        })?;
    path.split('/')
        .filter_map(|c| c.strip_suffix(".service"))
        .next_back()
}

/// The instruction set an ELF image is built for, from its header.
fn elf_architecture(header: &[u8]) -> Architecture {
    if header.len() < 20 || &header[..4] != b"\x7fELF" {
        return Architecture::Unknown;
    }
    let machine = if header[5] == 2 {
        u16::from_be_bytes([header[18], header[19]])
    } else {
        u16::from_le_bytes([header[18], header[19]])
    };
    match machine {
        0x3E => Architecture::X64,
        0x03 => Architecture::X86,
        0xB7 => Architecture::Arm64,
        0x28 => Architecture::Arm,
        _ => Architecture::Unknown,
    }
}

/// Windows priority classes from a nice value and scheduling policy. Nice runs
/// from -20 (most favored) to 19; a realtime policy outranks every nice value.
fn priority(nice: i64, policy: u64) -> Priority {
    if policy == SCHED_FIFO || policy == SCHED_RR {
        return Priority::Realtime;
    }
    match nice {
        i64::MIN..=-10 => Priority::High,
        -9..=-1 => Priority::AboveNormal,
        0 => Priority::Normal,
        1..=9 => Priority::BelowNormal,
        _ => Priority::Idle,
    }
}

fn thread_state(state: u8) -> (ThreadState, WaitReason) {
    match state {
        b'R' => (ThreadState::Running, WaitReason::default()),
        b'S' | b'D' | b'I' => (ThreadState::Waiting, WaitReason::default()),
        b'T' | b't' => (ThreadState::Waiting, WAIT_SUSPENDED),
        _ => (ThreadState::Other, WaitReason::default()),
    }
}

/// User names by id, looked up through the system's name service (so directory
/// users resolve too) and remembered.
#[derive(Debug, Default)]
struct Users {
    names: HashMap<u32, String>,
}

impl Users {
    fn name(&mut self, uid: u32) -> String {
        self.names
            .entry(uid)
            .or_insert_with(|| user_name(uid))
            .clone()
    }
}

/// The name of user `uid`, or the number when it has none.
pub(super) fn user_name(uid: u32) -> String {
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: zeroed `passwd` is a valid out-parameter; every pointer passed is
        // to a live, writable buffer of the stated length.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut pwd,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 16 {
            buf.resize(buf.len() * 4, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
            return uid.to_string();
        }
        // SAFETY: on success `pw_name` points at a NUL-terminated string in `buf`.
        let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
        return name.to_string_lossy().into_owned();
    }
}

/// A process between passes.
#[derive(Debug)]
struct Tracked {
    statics: Arc<ProcessStatic>,
    services: Arc<[ServiceInfo]>,
    kernel: bool,
    last_seen: u64,
    ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
    /// `io` and `fd` are unreadable for another user's process unless root, and
    /// stay so; they are not tried again.
    io_denied: bool,
    fd_denied: bool,
    /// The previous pass's threads, `(tid, birth, ticks)`, by tid.
    threads: Vec<(u32, u64, u64)>,
}

#[derive(Debug)]
pub(super) struct ProcessProbe {
    tracked: HashMap<ProcessKey, Tracked>,
    new_this_pass: Vec<u32>,
    by_pid: HashMap<u32, u32>,
    users: Users,
    /// Interned service lists, by unit name; `None` when systemd is not running.
    services: Option<HashMap<String, Arc<[ServiceInfo]>>>,
    no_services: Arc<[ServiceInfo]>,
    /// Clock ticks per second, the unit of `stat`'s times.
    hz: u64,
    boot_unix_ms: Option<i64>,
    max_cpu: f32,
    pass: u64,
    last_pass: Option<Instant>,
    buf: Vec<u8>,
    path: String,
    threads_scratch: Vec<(u32, u64, u64)>,
}

impl ProcessProbe {
    pub(super) fn new(logical: u32, boot_unix_ms: Option<i64>) -> Self {
        // SAFETY: sysconf has no preconditions.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let systemd = std::path::Path::new("/run/systemd/system").exists();
        Self {
            tracked: HashMap::with_capacity(512),
            new_this_pass: Vec::new(),
            by_pid: HashMap::new(),
            users: Users::default(),
            services: systemd.then(HashMap::new),
            no_services: Vec::new().into(),
            hz: u64::try_from(hz).ok().filter(|&h| h > 0).unwrap_or(100),
            boot_unix_ms,
            max_cpu: logical.max(1) as f32 * 100.0,
            pass: 0,
            last_pass: None,
            buf: Vec::with_capacity(4096),
            path: String::with_capacity(64),
            threads_scratch: Vec::new(),
        }
    }

    /// Whether processes can be matched to the services they run.
    pub(super) fn has_services(&self) -> bool {
        self.services.is_some()
    }

    fn ticks_to_duration(&self, ticks: u64) -> Duration {
        Duration::from_nanos(ticks.saturating_mul(1_000_000_000) / self.hz)
    }

    fn started_unix_ms(&self, starttime: u64) -> Option<i64> {
        self.boot_unix_ms
            .map(|boot| boot + (starttime.saturating_mul(1000) / self.hz).cast_signed())
    }

    fn set_path(&mut self, pid: u32, file: &str) {
        self.path.clear();
        let _ = write!(self.path, "/proc/{pid}/{file}");
    }

    pub(super) fn sample(&mut self, out: &mut Vec<ProcessSample>, threads: &mut Vec<ThreadSample>) {
        let now = Instant::now();
        let wall = self
            .last_pass
            .map_or(0.0, |prev| now.duration_since(prev).as_secs_f64());
        self.last_pass = Some(now);
        self.pass += 1;
        let pass = self.pass;
        self.new_this_pass.clear();

        let Ok(dir) = std::fs::read_dir("/proc") else {
            return;
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            self.sample_one(pid, wall, out, threads);
        }

        self.tracked.retain(|_, t| t.last_seen == pass);
        self.resolve_parents(out);
    }

    fn sample_one(
        &mut self,
        pid: u32,
        wall: f64,
        out: &mut Vec<ProcessSample>,
        threads: &mut Vec<ThreadSample>,
    ) {
        self.set_path(pid, "stat");
        if fs::read_into(&self.path, &mut self.buf).is_err() {
            return; // Exited since the directory was listed.
        }
        let Some((comm, stat)) = parse_stat(&self.buf) else {
            return;
        };
        let comm = String::from_utf8_lossy(comm).into_owned();
        let key = ProcessKey::new(pid, stat.starttime);

        self.set_path(pid, "status");
        let status = if fs::read_into(&self.path, &mut self.buf).is_ok() {
            parse_status(&self.buf)
        } else {
            Status::default()
        };

        let pass = self.pass;
        let first_sight = !self.tracked.contains_key(&key);
        if first_sight {
            let t = self.first_sight(key, &comm, &stat, &status);
            self.tracked.insert(key, t);
            self.new_this_pass.push(out.len() as u32);
        }

        let (io, handles) = self.read_access_gated(pid, key);

        let thread_first = threads.len() as u32;
        let (suspended_threads, n_threads) = self.sample_threads(pid, key, &stat, wall, threads);
        let thread_rows = threads.len() as u32 - thread_first;

        let hz = self.hz as f64;
        let max_cpu = self.max_cpu;
        let t = self.tracked.get_mut(&key).expect("inserted above");
        let rates = !first_sight && wall > 0.0;
        let cpu = if rates {
            let d = stat.ticks.saturating_sub(t.ticks) as f64 / hz;
            Percent((d / wall * 100.0) as f32).clamped(max_cpu)
        } else {
            Percent::ZERO
        };
        let (disk_read, disk_write) = match (io, rates) {
            (Some(io), true) => (
                Bytes(io.read_bytes.saturating_sub(t.read_bytes)),
                Bytes(io.write_bytes.saturating_sub(t.write_bytes)),
            ),
            _ => (Bytes::ZERO, Bytes::ZERO),
        };
        t.ticks = stat.ticks;
        if let Some(io) = io {
            t.read_bytes = io.read_bytes;
            t.write_bytes = io.write_bytes;
        }
        t.last_seen = pass;
        let statics = Arc::clone(&t.statics);
        let services = Arc::clone(&t.services);
        let kernel = t.kernel;

        let io = io.unwrap_or_default();
        out.push(ProcessSample {
            statics,
            cpu,
            cpu_time: self.ticks_to_duration(stat.ticks),
            working_set: Bytes(status.rss),
            private_working_set: Bytes(status.rss_anon),
            private_bytes: Bytes(status.rss_anon + status.swap),
            disk_read,
            disk_write,
            threads: stat.num_threads.max(n_threads),
            handles,
            // Stopped by a signal or a debugger, every thread with it.
            suspended: matches!(stat.state, b'T' | b't')
                || (n_threads > 0 && suspended_threads == n_threads),
            kind: if kernel {
                ProcessKind::Windows
            } else {
                ProcessKind::Background
            },
            priority: priority(stat.nice, stat.policy),
            page_faults: u32::try_from(stat.minflt + stat.majflt).unwrap_or(u32::MAX),
            peak_working_set: Bytes(status.peak_rss),
            virtual_size: Bytes(stat.vsize),
            io: IoCounters {
                reads: io.syscr,
                writes: io.syscw,
                other: 0,
                read_bytes: Bytes(io.rchar),
                write_bytes: Bytes(io.wchar),
                other_bytes: Bytes::ZERO,
            },
            services,
            thread_first,
            thread_rows,
            ..ProcessSample::default()
        });
    }

    /// `io` and the `fd` count, which need the same access as ptrace's read mode:
    /// this user's processes, or every process as root. A process found unreadable
    /// is not tried again.
    fn read_access_gated(&mut self, pid: u32, key: ProcessKey) -> (Option<Io>, u32) {
        let (io_denied, fd_denied) = {
            let t = &self.tracked[&key];
            (t.io_denied, t.fd_denied)
        };
        let io = if io_denied {
            None
        } else {
            self.set_path(pid, "io");
            match fs::read_into(&self.path, &mut self.buf) {
                Ok(()) => Some(parse_io(&self.buf)),
                Err(e) => {
                    if let Some(t) = self.tracked.get_mut(&key) {
                        t.io_denied = e.kind() == ErrorKind::PermissionDenied;
                    }
                    None
                }
            }
        };
        let handles = if fd_denied {
            0
        } else {
            self.set_path(pid, "fd");
            match std::fs::read_dir(&self.path) {
                Ok(d) => d.count() as u32,
                Err(e) => {
                    if let Some(t) = self.tracked.get_mut(&key) {
                        t.fd_denied = e.kind() == ErrorKind::PermissionDenied;
                    }
                    0
                }
            }
        };

        (io, handles)
    }

    /// Append this process's threads. A single-threaded process's one thread is
    /// the process itself, so its row comes from the process's own `stat` and costs
    /// nothing more. Returns how many threads are stopped, and how many there are.
    fn sample_threads(
        &mut self,
        pid: u32,
        key: ProcessKey,
        stat: &Stat,
        wall: f64,
        out: &mut Vec<ThreadSample>,
    ) -> (u32, u32) {
        let mut now = std::mem::take(&mut self.threads_scratch);
        now.clear();
        let mut stopped = 0;
        let mut push = |probe: &Self, prev: &[(u32, u64, u64)], tid: u32, s: &Stat| {
            let (state, wait_reason) = thread_state(s.state);
            if wait_reason == WAIT_SUSPENDED {
                stopped += 1;
            }
            let cpu = match prev.binary_search_by_key(&tid, |e| e.0) {
                Ok(i) if prev[i].1 == s.starttime && wall > 0.0 => {
                    let d = s.ticks.saturating_sub(prev[i].2) as f64 / probe.hz as f64;
                    Percent((d / wall * 100.0) as f32).clamped(100.0)
                }
                _ => Percent::ZERO,
            };
            out.push(ThreadSample {
                tid,
                birth: s.starttime,
                cpu,
                state,
                wait_reason,
                service: ServiceTag::Unknown,
                started_unix_ms: probe.started_unix_ms(s.starttime),
            });
        };

        let prev = std::mem::take(&mut self.tracked.get_mut(&key).expect("tracked").threads);
        if stat.num_threads <= 1 {
            push(self, &prev, pid, stat);
            now.push((pid, stat.starttime, stat.ticks));
        } else {
            self.set_path(pid, "task");
            if let Ok(dir) = std::fs::read_dir(&self.path) {
                for entry in dir.flatten() {
                    let Some(tid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|s| s.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    self.path.clear();
                    let _ = write!(self.path, "/proc/{pid}/task/{tid}/stat");
                    if fs::read_into(&self.path, &mut self.buf).is_err() {
                        continue;
                    }
                    let Some((_, ts)) = parse_stat(&self.buf) else {
                        continue;
                    };
                    push(self, &prev, tid, &ts);
                    now.push((tid, ts.starttime, ts.ticks));
                }
            }
        }
        let n = now.len() as u32;
        now.sort_unstable_by_key(|e| e.0);
        let t = self.tracked.get_mut(&key).expect("tracked");
        t.threads = now;
        self.threads_scratch = prev;
        (stopped, n)
    }

    /// Read what does not change over the process's life.
    fn first_sight(
        &mut self,
        key: ProcessKey,
        comm: &str,
        stat: &Stat,
        status: &Status,
    ) -> Tracked {
        let pid = key.pid;
        let kernel = stat.flags & PF_KTHREAD != 0;

        let (image_path, command_line, architecture) = if kernel {
            (None, None, Architecture::Unknown)
        } else {
            self.set_path(pid, "exe");
            let image = std::fs::read_link(&self.path)
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            let architecture =
                std::fs::File::open(&self.path)
                    .ok()
                    .map_or(Architecture::Unknown, |mut f| {
                        let mut header = [0u8; 20];
                        f.read_exact(&mut header)
                            .map_or(Architecture::Unknown, |()| elf_architecture(&header))
                    });
            self.set_path(pid, "cmdline");
            let command_line = fs::read_into(&self.path, &mut self.buf)
                .ok()
                .and_then(|()| {
                    let args: Vec<_> = self
                        .buf
                        .split(|&b| b == 0)
                        .filter(|a| !a.is_empty())
                        .map(String::from_utf8_lossy)
                        .collect();
                    (!args.is_empty()).then(|| args.join(" "))
                });
            (image, command_line, architecture)
        };
        let name = process_name(comm, image_path.as_deref(), command_line.as_deref());

        let read_id = |probe: &mut Self, file: &str| {
            probe.set_path(pid, file);
            fs::read_u64(&probe.path).filter(|&v| v != UNSET_ID)
        };
        let session_id = read_id(self, "sessionid").map_or(0, |v| v as u32);
        let integrity = if kernel {
            Integrity::System
        } else {
            match status.euid {
                // Root started by the system rather than by a signed-in user.
                Some(0) if read_id(self, "loginuid").is_none() => Integrity::System,
                Some(0) => Integrity::High,
                Some(_) => Integrity::Medium,
                None => Integrity::Unknown,
            }
        };

        let services = match (&mut self.services, kernel) {
            (Some(interned), false) => {
                self.path.clear();
                let _ = write!(self.path, "/proc/{pid}/cgroup");
                std::fs::read_to_string(&self.path)
                    .ok()
                    .and_then(|cg| service_of(&cg).map(str::to_owned))
                    .map_or_else(
                        || Arc::clone(&self.no_services),
                        |unit| {
                            Arc::clone(interned.entry(unit).or_insert_with_key(|unit| {
                                let name: Arc<str> = unit.as_str().into();
                                vec![ServiceInfo {
                                    display_name: Arc::clone(&name),
                                    name,
                                    state: ServiceState::Running,
                                    dll: None,
                                }]
                                .into()
                            }))
                        },
                    )
            }
            _ => Arc::clone(&self.no_services),
        };

        let statics = ProcessStatic {
            key,
            // Only the PID for now; `resolve_parents` checks it against the table.
            parent: (stat.ppid != 0).then(|| ProcessKey::new(stat.ppid, 0)),
            name,
            image_path,
            command_line,
            user: status.euid.map(|uid| self.users.name(uid)),
            integrity,
            started_unix_ms: self.started_unix_ms(stat.starttime),
            session_id,
            architecture,
            ..ProcessStatic::default()
        };
        Tracked {
            statics: Arc::new(statics),
            services,
            kernel,
            last_seen: self.pass,
            ticks: stat.ticks,
            read_bytes: 0,
            write_bytes: 0,
            io_denied: false,
            fd_denied: false,
            threads: Vec::new(),
        }
    }

    /// Turn the PID-only parent hint of each process first seen this pass into a
    /// real identity, or drop it: the process now holding that PID is the parent
    /// only if it started no later than the child. Otherwise the parent exited and
    /// a stranger inherited its PID.
    fn resolve_parents(&mut self, out: &mut [ProcessSample]) {
        if self.new_this_pass.is_empty() {
            return;
        }
        self.by_pid.clear();
        for (i, p) in out.iter().enumerate() {
            self.by_pid.insert(p.key().pid, i as u32);
        }
        for &i in &self.new_this_pass {
            let child = &out[i as usize];
            let child_key = child.key();
            let Some(hint) = child.statics.parent else {
                continue;
            };
            // `birth` is `starttime` here; ordering it is meaningful in this layer.
            let resolved = self
                .by_pid
                .get(&hint.pid)
                .map(|&j| out[j as usize].key())
                .filter(|parent| parent.birth <= child_key.birth);
            let statics = Arc::new(ProcessStatic {
                parent: resolved,
                ..(*child.statics).clone()
            });
            if let Some(t) = self.tracked.get_mut(&child_key) {
                t.statics = Arc::clone(&statics);
            }
            out[i as usize].statics = statics;
        }
    }
}

/// The name to list a process under: its executable's file name, as on Windows.
/// When the executable cannot be read (another user's process), the first
/// argument's file name if it agrees with the kernel's 15-character `comm`, and
/// otherwise `comm` itself.
fn process_name(comm: &str, image: Option<&str>, command_line: Option<&str>) -> String {
    if let Some(image) = image {
        let base = fs::basename(image);
        return base.strip_suffix(" (deleted)").unwrap_or(base).to_owned();
    }
    if let Some(arg0) = command_line.and_then(|c| c.split(' ').next()) {
        let base = fs::basename(arg0);
        if !comm.is_empty() && base.starts_with(comm) {
            return base.to_owned();
        }
    }
    comm.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &[u8] = b"1234 (my (odd) prog) S 1 1234 1234 0 -1 4194560 2500 0 12 0 \
        350 150 0 0 20 -5 7 0 98765 123456789 1000 18446744073709551615 1 1 0 0 0 0 0 \
        4096 0 0 0 0 17 3 0 2 0 0 0 0 0 0 0 0 0 0\n";

    #[test]
    fn stat_fields_land_where_they_belong() {
        let (comm, s) = parse_stat(STAT).unwrap();
        assert_eq!(comm, b"my (odd) prog");
        assert_eq!(s.state, b'S');
        assert_eq!(s.ppid, 1);
        assert_eq!(s.flags, 4_194_560);
        assert_eq!(s.minflt, 2500);
        assert_eq!(s.majflt, 12);
        assert_eq!(s.ticks, 500);
        assert_eq!(s.nice, -5);
        assert_eq!(s.num_threads, 7);
        assert_eq!(s.starttime, 98765);
        assert_eq!(s.vsize, 123_456_789);
        assert_eq!(s.policy, 2);
    }

    #[test]
    fn status_reads_memory_and_owner() {
        let s = parse_status(
            b"Name:\tbash\nUid:\t1000\t0\t1000\t1000\nVmHWM:\t    5000 kB\n\
              VmRSS:\t    4000 kB\nRssAnon:\t    1000 kB\nVmSwap:\t      10 kB\n",
        );
        assert_eq!(s.euid, Some(0));
        assert_eq!(s.rss, 4000 * 1024);
        assert_eq!(s.rss_anon, 1000 * 1024);
        assert_eq!(s.peak_rss, 5000 * 1024);
        assert_eq!(s.swap, 10 * 1024);
    }

    #[test]
    fn io_counters_parse() {
        let io = parse_io(
            b"rchar: 100\nwchar: 200\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\n\
              write_bytes: 8192\ncancelled_write_bytes: 0\n",
        );
        assert_eq!(
            io,
            Io {
                rchar: 100,
                wchar: 200,
                syscr: 3,
                syscw: 4,
                read_bytes: 4096,
                write_bytes: 8192
            }
        );
    }

    #[test]
    fn services_come_from_the_innermost_unit() {
        assert_eq!(service_of("0::/system.slice/ssh.service\n"), Some("ssh"));
        assert_eq!(
            service_of("0::/user.slice/user-1000.slice/user@1000.service/app.slice/foo.service\n"),
            Some("foo")
        );
        assert_eq!(
            service_of("0::/user.slice/user-1000.slice/user@1000.service/init.scope\n"),
            Some("user@1000")
        );
        assert_eq!(
            service_of("0::/user.slice/user-1000.slice/session-3.scope\n"),
            None
        );
        // cgroup v1, where the systemd hierarchy is named.
        assert_eq!(
            service_of("12:cpu:/\n1:name=systemd:/system.slice/cron.service\n"),
            Some("cron")
        );
    }

    #[test]
    fn names_prefer_the_executable() {
        assert_eq!(process_name("bash", Some("/usr/bin/bash"), None), "bash");
        assert_eq!(process_name("x", Some("/tmp/gone (deleted)"), None), "gone");
        assert_eq!(
            process_name(
                "systemd-journal",
                None,
                Some("/usr/lib/systemd/systemd-journald")
            ),
            "systemd-journald"
        );
        assert_eq!(process_name("kworker/0:1", None, None), "kworker/0:1");
        assert_eq!(
            process_name("sshd", None, Some("sshd: alice [priv]")),
            "sshd:"
        );
    }

    #[test]
    fn priorities_follow_nice() {
        assert_eq!(priority(0, 0), Priority::Normal);
        assert_eq!(priority(19, 0), Priority::Idle);
        assert_eq!(priority(5, 0), Priority::BelowNormal);
        assert_eq!(priority(-5, 0), Priority::AboveNormal);
        assert_eq!(priority(-20, 0), Priority::High);
        assert_eq!(priority(0, SCHED_FIFO), Priority::Realtime);
    }

    #[test]
    fn elf_headers_name_the_machine() {
        let mut h = [0u8; 20];
        h[..4].copy_from_slice(b"\x7fELF");
        h[5] = 1;
        h[18] = 0x3E;
        assert_eq!(elf_architecture(&h), Architecture::X64);
        h[18] = 0xB7;
        assert_eq!(elf_architecture(&h), Architecture::Arm64);
        assert_eq!(elf_architecture(b"MZ"), Architecture::Unknown);
    }

    #[test]
    fn a_pass_finds_this_process_with_rates() {
        let mut probe = ProcessProbe::new(
            1,
            super::super::cpu::boot_time_secs().map(|s| s.cast_signed() * 1000),
        );
        let (mut out, mut threads) = (Vec::new(), Vec::new());
        probe.sample(&mut out, &mut threads);
        // Burn some CPU so the second pass has something to measure.
        let start = Instant::now();
        let mut x = 0u64;
        while start.elapsed() < Duration::from_millis(100) {
            x = x.wrapping_mul(31).wrapping_add(7);
        }
        std::hint::black_box(x);
        out.clear();
        threads.clear();
        probe.sample(&mut out, &mut threads);
        let me = std::process::id();
        let p = out
            .iter()
            .find(|p| p.key().pid == me)
            .expect("this process");
        assert!(p.cpu.get() > 10.0, "cpu {}", p.cpu.get());
        assert!(p.working_set.get() > 0);
        assert!(p.handles > 0);
        assert!(p.statics.user.is_some());
        assert!(p.statics.image_path.is_some());
        assert!(p.thread_rows >= 1);
        assert!(out.iter().any(|p| p.statics.parent.is_some()));
    }

    #[test]
    fn a_suspended_process_reads_suspended() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        // SAFETY: plain signal to our own child.
        unsafe { libc::kill(pid.cast_signed(), libc::SIGSTOP) };
        std::thread::sleep(Duration::from_millis(50));
        let mut probe = ProcessProbe::new(1, None);
        let (mut out, mut threads) = (Vec::new(), Vec::new());
        probe.sample(&mut out, &mut threads);
        let p = out.iter().find(|p| p.key().pid == pid).expect("the child");
        let _ = child.kill();
        let _ = child.wait();
        assert!(p.suspended);
        assert_eq!(p.name(), "sleep");
        assert_eq!(p.statics.parent.map(|k| k.pid), Some(std::process::id()));
    }
}
