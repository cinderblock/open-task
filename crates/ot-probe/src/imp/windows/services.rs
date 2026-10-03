//! Which services live in which process, and the full service list, from the
//! service control manager.
//!
//! One `EnumServicesStatusExW` call per pass returns every Win32 service with its
//! state, the PID it runs in and the controls it accepts. That is the whole per-pass
//! cost: a few hundred entries, no handle per service, unelevated. Names are
//! interned so republishing the lists every second allocates nothing in steady
//! state, and the full list ([`ServiceProbe::list`]) is rebuilt only when something
//! in it changed, so the sampler can hand out the same `Arc` every second for free.
//!
//! The Services page needs more than the enumeration gives: the start type, the
//! description and the `svchost` group come from each service's configuration, which
//! takes an `OpenServiceW` handle per service. Those are read lazily, under a time
//! budget per pass ([`CONFIG_BUDGET`]) from a queue of names still to read, so the
//! first pass does not open 300 handles at once; until read, the start type is
//! `Unknown` and the description absent. A service's configuration is read again
//! when its state changes (disabling a service usually stops it too) and otherwise
//! every [`CONFIG_RECHECK`], round-robin under the same budget.
//!
//! Cost, measured by the `#[ignore]`d timing test (`cargo test -p ot-probe
//! services::tests::timing -- --ignored --nocapture`) on a Windows 11 box with 344
//! services, debug build at `opt-level = 1`, other builds running: a steady-state
//! `refresh` with the queue drained is about 1.8 ms, 1.4 ms of it the enumeration.
//! The first `refresh` is about 20 ms, nearly all of it interning (one registry
//! read per service for the DLL), and only what is left of the budget goes to
//! configurations. A `refresh` with reads to do fits about 18 of them into the
//! 15 ms budget (around 0.7 ms per service: one handle open and three queries, each
//! a call into the control manager), so every start type is known after about 20
//! passes. All 344 configurations in one go take about 240 ms, which is why they
//! are spread out.
//!
//! The service DLL comes from the registry, once per service name. It is a hint for
//! where the service's code lives, shown next to the name; the CPU can still be in
//! another module.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_model::service::{ServiceEntry, ServiceInfo, ServiceState, StartType};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_SERVICE_ALREADY_RUNNING,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_ANY,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW,
    QueryServiceConfig2W, QueryServiceConfigW, QueryServiceStatusEx, StartServiceW,
    ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_STOP,
    SERVICE_AUTO_START, SERVICE_BOOT_START, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO,
    SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_DISABLED, SERVICE_PAUSED,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
    SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_SYSTEM_START, SERVICE_WIN32,
};

use crate::ControlError;

/// How long one pass may spend reading service configurations.
const CONFIG_BUDGET: Duration = Duration::from_millis(15);
/// How often a service's configuration is read again when nothing prompted it.
const CONFIG_RECHECK: Duration = Duration::from_secs(60);
/// How long [`stop`] waits for the service to report stopped.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How often [`stop`] asks whether it has.
const STOP_POLL: Duration = Duration::from_millis(100);
/// Initial size of the configuration scratch buffer, in bytes. The configuration
/// queries cap their answers at 8 KiB, so this is also the largest it gets.
const CONFIG_BUF_BYTES: usize = 8 * 1024;

/// An SCM or service handle that closes on drop, so every early return releases it.
#[derive(Debug)]
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenSCManagerW or OpenServiceW and is closed
        // exactly once.
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

/// What a service's configuration says, as the Services page shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Config {
    start: StartType,
    description: Option<Arc<str>>,
    group: Option<Arc<str>>,
}

/// Interned strings and slow-changing facts for one service name.
#[derive(Debug)]
struct Interned {
    name: Arc<str>,
    display_name: Arc<str>,
    dll: Option<Arc<str>>,
    config: Config,
    /// When the configuration was last read (or last failed to be), or None until
    /// the first attempt.
    config_read: Option<Instant>,
    /// Already in the queue; keeps a flapping service from filling it.
    queued: bool,
    /// State at the previous pass; a change gets the configuration read again.
    last_state: ServiceState,
    /// The pass that last listed this service. One the control manager no longer
    /// lists is not read again.
    seen: u64,
}

/// What one pass's enumeration said about a service; the part that changes.
#[derive(Debug)]
struct Seen {
    name: Arc<str>,
    state: ServiceState,
    pid: u32,
    can_stop: bool,
}

/// Maps PIDs to the services they host, and lists every service. One per probe.
#[derive(Debug)]
pub(super) struct ServiceProbe {
    scm: Option<ScHandle>,
    /// Enumeration buffer. `u64` elements so the structures the API writes at its
    /// start are 8-byte aligned; handed to the API as bytes.
    buf: Vec<u64>,
    /// Configuration query buffer, aligned the same way.
    cfg_buf: Vec<u64>,
    interned: HashMap<String, Interned>,
    /// Services whose configuration is still to be read, in order.
    queue: VecDeque<Arc<str>>,
    /// Passes so far; stamps `Interned::seen`.
    pass: u64,
    /// This pass's answer. Vectors are kept between passes and refilled.
    by_pid: HashMap<u32, Vec<ServiceInfo>>,
    /// The full list as of the last refresh; replaced only when it changed.
    list: Arc<[ServiceEntry]>,
    /// Scratch for this pass's enumeration, and for the candidate list.
    seen: Vec<Seen>,
    next: Vec<ServiceEntry>,
    /// Scratch for reading names out of the enumeration buffer.
    name: String,
}

// SAFETY: an SCM handle is a process-wide token, valid from any thread; the probe
// uses it only from the sampler thread and closes it there.
unsafe impl Send for ServiceProbe {}

impl ServiceProbe {
    pub fn new() -> Self {
        // SAFETY: plain call; null machine and database mean the local active DB.
        let scm = unsafe {
            OpenSCManagerW(
                PCWSTR::null(),
                PCWSTR::null(),
                SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE,
            )
        }
        .ok()
        .map(ScHandle);
        if scm.is_none() {
            tracing::warn!("service control manager not readable; no service attribution");
        }
        Self {
            scm,
            buf: Vec::with_capacity(8 * 1024),
            cfg_buf: vec![0; CONFIG_BUF_BYTES / 8],
            interned: HashMap::new(),
            queue: VecDeque::new(),
            pass: 0,
            by_pid: HashMap::new(),
            list: Vec::new().into(),
            seen: Vec::new(),
            next: Vec::new(),
            name: String::new(),
        }
    }

    pub fn available(&self) -> bool {
        self.scm.is_some()
    }

    /// Refresh the PID to services map and the full list for this pass.
    pub fn refresh(&mut self) {
        let Some(scm) = self.scm.as_ref().map(|h| h.0) else {
            return;
        };
        for v in self.by_pid.values_mut() {
            v.clear();
        }
        let Some(count) = self.enumerate(scm) else {
            return;
        };
        self.pass += 1;
        let pass = self.pass;
        let now = Instant::now();
        self.observe(count, pass, now);

        // Read what configurations the budget allows, at least one per pass so a
        // slow pass still makes progress.
        let deadline = now + CONFIG_BUDGET;
        while let Some(name) = self.queue.pop_front() {
            if let Some(i) = self.interned.get_mut(&*name) {
                i.queued = false;
                if i.seen == pass {
                    if let Some(config) = read_config(scm, &name, &mut self.cfg_buf) {
                        i.config = config;
                    }
                    // Also on failure: a service that refuses is tried again at the
                    // normal cadence rather than every pass.
                    i.config_read = Some(Instant::now());
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }

        self.rebuild_list();
    }

    /// One walk over the enumeration in `self.buf`: intern new names, notice state
    /// changes, queue configuration reads, map PIDs and note what was seen.
    fn observe(&mut self, count: usize, pass: u64, now: Instant) {
        // SAFETY: the API wrote `count` structures at the start of the buffer, with
        // the strings they point to further in; the buffer is 8-byte aligned and
        // outlives the slice.
        let entries = unsafe {
            std::slice::from_raw_parts(
                self.buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                count,
            )
        };
        self.seen.clear();
        for e in entries {
            let status = &e.ServiceStatusProcess;
            let pid = status.dwProcessId;
            let state = state_of(status.dwCurrentState);
            // SAFETY: both strings are NUL-terminated and live in `self.buf`.
            let wide = unsafe { e.lpServiceName.as_wide() };
            self.name.clear();
            self.name.extend(
                char::decode_utf16(wide.iter().copied())
                    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)),
            );
            if !self.interned.contains_key(self.name.as_str()) {
                // SAFETY: as above.
                let display = unsafe { e.lpDisplayName.to_string() }.unwrap_or_default();
                let name: Arc<str> = Arc::from(self.name.as_str());
                self.queue.push_back(Arc::clone(&name));
                self.interned.insert(
                    self.name.clone(),
                    Interned {
                        name,
                        display_name: Arc::from(display.as_str()),
                        dll: service_dll(&self.name),
                        config: Config::default(),
                        config_read: None,
                        queued: true,
                        last_state: state,
                        seen: 0,
                    },
                );
            }
            let Some(i) = self.interned.get_mut(self.name.as_str()) else {
                continue;
            };
            let due = i
                .config_read
                .is_some_and(|t| now.duration_since(t) >= CONFIG_RECHECK);
            if !i.queued && (i.last_state != state || due) {
                self.queue.push_back(Arc::clone(&i.name));
                i.queued = true;
            }
            i.last_state = state;
            i.seen = pass;
            self.seen.push(Seen {
                name: Arc::clone(&i.name),
                state,
                pid,
                can_stop: status.dwControlsAccepted & SERVICE_ACCEPT_STOP != 0,
            });
            if pid != 0 {
                self.by_pid.entry(pid).or_default().push(ServiceInfo {
                    name: Arc::clone(&i.name),
                    display_name: Arc::clone(&i.display_name),
                    state,
                    dll: i.dll.clone(),
                });
            }
        }
        for v in self.by_pid.values_mut() {
            v.sort_by(|a, b| a.name.cmp(&b.name));
        }
        self.by_pid.retain(|_, v| !v.is_empty());
    }

    /// The full list from what this pass saw, replacing the published one only
    /// when it differs.
    fn rebuild_list(&mut self) {
        self.next.clear();
        for s in &self.seen {
            let Some(i) = self.interned.get(&*s.name) else {
                continue;
            };
            self.next.push(ServiceEntry {
                name: Arc::clone(&i.name),
                display_name: Arc::clone(&i.display_name),
                description: i.config.description.clone(),
                state: s.state,
                start: i.config.start,
                pid: (s.pid != 0).then_some(s.pid),
                group: i.config.group.clone(),
                can_stop: s.can_stop,
            });
        }
        self.next.sort_by(|a, b| name_order(&a.name, &b.name));
        if self.list[..] != self.next[..] {
            self.list = Arc::from(self.next.as_slice());
        }
    }

    /// Run the enumeration into `self.buf`, growing it as needed. Returns the number
    /// of entries written, or None (after a warning) if the control manager refused.
    fn enumerate(&mut self, scm: SC_HANDLE) -> Option<usize> {
        if self.buf.is_empty() {
            self.buf.resize(8 * 1024, 0);
        }
        let mut returned = 0u32;
        loop {
            let mut needed = 0u32;
            let mut resume = 0u32;
            // SAFETY: the byte view covers exactly the buffer's allocation; the
            // out-pointers are valid locals; the group name is null (all groups).
            let r = unsafe {
                let bytes = std::slice::from_raw_parts_mut(
                    self.buf.as_mut_ptr().cast::<u8>(),
                    self.buf.len() * 8,
                );
                EnumServicesStatusExW(
                    scm,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_STATE_ALL,
                    Some(bytes),
                    &raw mut needed,
                    &raw mut returned,
                    Some(&raw mut resume),
                    PCWSTR::null(),
                )
            };
            match r {
                Ok(()) => break,
                Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                    // Restart from the top with room for everything: simpler than
                    // stitching pages, and this runs once per second at most.
                    let n = self.buf.len() + (needed as usize).div_ceil(8) + 512;
                    self.buf.resize(n, 0);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "EnumServicesStatusExW failed");
                    return None;
                }
            }
        }
        let count = returned as usize;
        (count * size_of::<ENUM_SERVICE_STATUS_PROCESSW>() <= self.buf.len() * 8).then_some(count)
    }

    /// Services hosted by `pid` as of the last refresh, sorted by name.
    pub fn services_of(&self, pid: u32) -> Option<&[ServiceInfo]> {
        self.by_pid.get(&pid).map(Vec::as_slice)
    }

    /// Every service of the machine as of the last refresh, running or not, sorted
    /// by name without regard to case. The same `Arc` comes back until something in
    /// it changes.
    pub fn list(&self) -> Arc<[ServiceEntry]> {
        Arc::clone(&self.list)
    }
}

/// Order for the full list: by name without regard to case, then exactly, so the
/// order is total and stable between passes.
fn name_order(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
        .then_with(|| a.cmp(b))
}

fn state_of(state: SERVICE_STATUS_CURRENT_STATE) -> ServiceState {
    match state {
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_START_PENDING => ServiceState::StartPending,
        SERVICE_STOP_PENDING => ServiceState::StopPending,
        SERVICE_STOPPED => ServiceState::Stopped,
        SERVICE_PAUSED => ServiceState::Paused,
        _ => ServiceState::Unknown,
    }
}

/// Read one service's configuration: one handle open and three queries. None if
/// the service cannot be opened or its configuration read, which a rename or a
/// removal between enumeration and now can cause.
fn read_config(scm: SC_HANDLE, name: &str, buf: &mut Vec<u64>) -> Option<Config> {
    let wide = HSTRING::from(name);
    // SAFETY: plain call; the handle is owned by the guard.
    let h = match unsafe { OpenServiceW(scm, &wide, SERVICE_QUERY_CONFIG) } {
        Ok(h) => ScHandle(h),
        Err(e) => {
            tracing::debug!(service = name, error = %e, "service not openable for configuration");
            return None;
        }
    };

    // SAFETY: the pointer and byte count describe the same 8-byte aligned buffer;
    // the out-pointer is a valid local.
    let ok = query_growing(buf, |ptr, len, needed| unsafe {
        QueryServiceConfigW(
            h.0,
            Some(ptr.cast::<QUERY_SERVICE_CONFIGW>()),
            len as u32,
            needed,
        )
    });
    if !ok {
        return None;
    }
    // SAFETY: the query succeeded, so the buffer starts with a configuration whose
    // strings point further into the same buffer; it is not touched until the
    // description query below, after these are copied out.
    let (start_type, path) = unsafe {
        let cfg = &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>();
        let path = if cfg.lpBinaryPathName.is_null() {
            String::new()
        } else {
            cfg.lpBinaryPathName.to_string().unwrap_or_default()
        };
        (cfg.dwStartType, path)
    };
    let group = svchost_group(&path).map(Arc::from);
    let mut start = match start_type {
        SERVICE_BOOT_START => StartType::Boot,
        SERVICE_SYSTEM_START => StartType::System,
        SERVICE_AUTO_START => StartType::Automatic,
        SERVICE_DEMAND_START => StartType::Manual,
        SERVICE_DISABLED => StartType::Disabled,
        _ => StartType::Unknown,
    };

    if start == StartType::Automatic {
        let mut info = SERVICE_DELAYED_AUTO_START_INFO::default();
        let mut needed = 0u32;
        // SAFETY: the byte view covers exactly `info`, which outlives the call; the
        // out-pointer is a valid local.
        let delayed = unsafe {
            let bytes = std::slice::from_raw_parts_mut(
                (&raw mut info).cast::<u8>(),
                size_of::<SERVICE_DELAYED_AUTO_START_INFO>(),
            );
            QueryServiceConfig2W(
                h.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                Some(bytes),
                &raw mut needed,
            )
        }
        .is_ok()
            && info.fDelayedAutostart.as_bool();
        if delayed {
            start = StartType::AutomaticDelayed;
        }
    }

    // SAFETY: the byte view covers exactly the buffer the pointer and length
    // describe; the out-pointer is a valid local.
    let ok = query_growing(buf, |ptr, len, needed| unsafe {
        let bytes = std::slice::from_raw_parts_mut(ptr.cast::<u8>(), len);
        QueryServiceConfig2W(h.0, SERVICE_CONFIG_DESCRIPTION, Some(bytes), needed)
    });
    let description = if ok {
        // SAFETY: the query succeeded, so the buffer starts with a description whose
        // string, when present, points further into the same buffer.
        unsafe {
            let d = &*buf.as_ptr().cast::<SERVICE_DESCRIPTIONW>();
            if d.lpDescription.is_null() {
                None
            } else {
                d.lpDescription.to_string().ok()
            }
        }
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(Arc::from)
    } else {
        None
    };

    Some(Config {
        start,
        description,
        group,
    })
}

/// Run a query that wants a caller-supplied buffer, growing `buf` on
/// `ERROR_INSUFFICIENT_BUFFER` until it fits. `call` gets the 8-byte aligned buffer,
/// its length in bytes and the out-pointer for the needed size. False if the query
/// failed for another reason.
fn query_growing(
    buf: &mut Vec<u64>,
    mut call: impl FnMut(*mut u64, usize, *mut u32) -> windows::core::Result<()>,
) -> bool {
    if buf.is_empty() {
        buf.resize(CONFIG_BUF_BYTES / 8, 0);
    }
    loop {
        let mut needed = 0u32;
        match call(buf.as_mut_ptr(), buf.len() * 8, &raw mut needed) {
            Ok(()) => return true,
            Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {
                let n = (needed as usize).div_ceil(8).max(buf.len() + 1);
                buf.resize(n, 0);
            }
            Err(_) => return false,
        }
    }
}

/// The `-k <group>` of a shared host's command line, when the command line runs
/// `svchost.exe`. Other hosts are left alone even if they take a `-k`.
fn svchost_group(path: &str) -> Option<&str> {
    let mut tokens = path.split_whitespace();
    let exe = tokens.next()?.trim_matches('"');
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    if !file.eq_ignore_ascii_case("svchost.exe") {
        return None;
    }
    while let Some(t) = tokens.next() {
        if t.eq_ignore_ascii_case("-k") {
            return tokens.next().filter(|g| !g.is_empty());
        }
    }
    None
}

/// File name of the service's DLL from `Parameters\ServiceDll`, or from the service
/// key itself for the few services that put it there.
fn service_dll(name: &str) -> Option<Arc<str>> {
    let base = format!("SYSTEM\\CurrentControlSet\\Services\\{name}");
    read_dll(&format!("{base}\\Parameters"))
        .or_else(|| read_dll(&base))
        .map(|path| {
            let file = path.rsplit(['\\', '/']).next().unwrap_or(path.as_str());
            Arc::from(file)
        })
}

fn read_dll(subkey: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let subkey = HSTRING::from(subkey);
    // SAFETY: valid out-pointers; `size` is the buffer's byte length.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            windows::core::w!("ServiceDll"),
            RRF_RT_ANY | RRF_NOEXPAND,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
    (end > 0).then(|| String::from_utf16_lossy(&buf[..end]))
}

// Actions. Called from the UI thread, with fresh handles: the probe's SCM handle
// belongs to the sampler thread.

fn is(e: &windows::core::Error, code: WIN32_ERROR) -> bool {
    e.code() == code.to_hresult()
}

/// The errors the control manager gives for a service the caller may not touch,
/// or that is not there, become the matching [`ControlError`]s.
fn control_error(context: &'static str, e: windows::core::Error) -> ControlError {
    if is(&e, ERROR_ACCESS_DENIED) {
        ControlError::NotPermitted
    } else if is(&e, ERROR_SERVICE_DOES_NOT_EXIST) {
        ControlError::Gone
    } else {
        ControlError::os(context, e)
    }
}

/// Open `name` with `access`, through a connection of its own.
fn open_service(name: &str, access: u32) -> Result<ScHandle, ControlError> {
    // SAFETY: plain call; null machine and database mean the local active DB.
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
        .map(ScHandle)
        .map_err(|e| control_error("connecting to the service control manager", e))?;
    let wide = HSTRING::from(name);
    // SAFETY: plain call; the service handle stays valid after the connection it
    // came from is closed.
    unsafe { OpenServiceW(scm.0, &wide, access) }
        .map(ScHandle)
        .map_err(|e| control_error("opening the service", e))
}

/// Start the service. Ok if it was already running.
///
/// # Errors
/// [`ControlError::NotPermitted`] without the right to start it (administrator,
/// for most services), [`ControlError::Gone`] if there is no such service.
pub(super) fn start(name: &str) -> Result<(), ControlError> {
    let h = open_service(name, SERVICE_START)?;
    // SAFETY: plain call with no arguments for the service.
    match unsafe { StartServiceW(h.0, None) } {
        Ok(()) => Ok(()),
        Err(e) if is(&e, ERROR_SERVICE_ALREADY_RUNNING) => Ok(()),
        Err(e) => Err(control_error("starting the service", e)),
    }
}

/// Ask the service to stop and wait for it to, polling every [`STOP_POLL`] for up
/// to [`STOP_TIMEOUT`]. Ok if it was already stopped. Blocks the caller for that
/// long at most.
///
/// # Errors
/// [`ControlError::NotPermitted`] without the right to stop it, [`ControlError::Gone`]
/// if there is no such service, and an [`ControlError::Os`] naming the timeout if
/// it is still stopping when the wait ends.
pub(super) fn stop(name: &str) -> Result<(), ControlError> {
    let h = open_service(name, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let mut status = SERVICE_STATUS::default();
    // SAFETY: plain call; the out-pointer is a valid local.
    match unsafe { ControlService(h.0, SERVICE_CONTROL_STOP, &raw mut status) } {
        Ok(()) => {}
        Err(e) if is(&e, ERROR_SERVICE_NOT_ACTIVE) => return Ok(()),
        Err(e) => return Err(control_error("stopping the service", e)),
    }
    if status.dwCurrentState == SERVICE_STOPPED {
        return Ok(());
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        std::thread::sleep(STOP_POLL);
        let mut now = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        // SAFETY: the byte view covers exactly `now`, which outlives the call; the
        // out-pointer is a valid local.
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(
                (&raw mut now).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            );
            QueryServiceStatusEx(h.0, SC_STATUS_PROCESS_INFO, Some(bytes), &raw mut needed)
        }
        .map_err(|e| control_error("waiting for the service to stop", e))?;
        if now.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ControlError::Os {
                context: "stopping the service",
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{name} is still stopping after {}s", STOP_TIMEOUT.as_secs()),
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Refresh until every queued configuration has been read, bounded.
    fn drain(p: &mut ServiceProbe) {
        for _ in 0..50 {
            p.refresh();
            if p.queue.is_empty() {
                return;
            }
        }
        panic!("configuration queue not drained in 50 passes");
    }

    #[test]
    fn the_scm_maps_at_least_one_service_to_a_live_pid() {
        let mut p = ServiceProbe::new();
        assert!(p.available());
        p.refresh();
        let hosts = p.by_pid.len();
        assert!(hosts > 0, "no service host found");
        // Every listed service names a real process and is sorted by name.
        for v in p.by_pid.values() {
            assert!(v.windows(2).all(|w| w[0].name <= w[1].name));
        }
        // A second refresh produces the same shape without growing the intern table
        // beyond the service population.
        let interned = p.interned.len();
        p.refresh();
        assert_eq!(p.interned.len(), interned);
    }

    #[test]
    fn a_shared_host_service_names_its_dll() {
        // Every Windows install has the RPC endpoint mapper in a shared host.
        let dll = service_dll("RpcEptMapper");
        assert!(dll.is_some(), "RpcEptMapper has a ServiceDll");
        assert!(dll.unwrap().to_ascii_lowercase().ends_with(".dll"));
    }

    #[test]
    fn the_full_list_is_complete_sorted_and_consistent() {
        let mut p = ServiceProbe::new();
        p.refresh();
        let list = p.list();
        assert!(list.len() > 100, "only {} services listed", list.len());
        assert!(
            list.windows(2)
                .all(|w| name_order(&w[0].name, &w[1].name) == Ordering::Less),
            "list is not sorted by name"
        );
        for e in list.iter() {
            if e.pid.is_some() {
                assert!(
                    matches!(
                        e.state,
                        ServiceState::Running
                            | ServiceState::StartPending
                            | ServiceState::StopPending
                            | ServiceState::Paused
                    ),
                    "{} has a PID but state {:?}",
                    e.name,
                    e.state
                );
            }
            assert!(!e.display_name.is_empty() || e.name.is_empty());
        }
        // The map of PIDs and the list agree about who runs where.
        let hosted: usize = p.by_pid.values().map(Vec::len).sum();
        assert_eq!(list.iter().filter(|e| e.pid.is_some()).count(), hosted);
    }

    #[test]
    fn the_endpoint_mapper_is_automatic_with_a_description_once_read() {
        let mut p = ServiceProbe::new();
        let entry = (0..50)
            .map(|_| {
                p.refresh();
                p.list()
                    .iter()
                    .find(|e| &*e.name == "RpcEptMapper")
                    .cloned()
                    .expect("RpcEptMapper is listed")
            })
            .find(|e| e.start != StartType::Unknown)
            .expect("RpcEptMapper's configuration read within 50 passes");
        assert_eq!(entry.start, StartType::Automatic);
        assert!(entry.description.as_deref().is_some_and(|d| !d.is_empty()));
        assert_eq!(entry.state, ServiceState::Running);
        assert!(entry.pid.is_some());
        assert_eq!(entry.group.as_deref(), Some("RPCSS"));
    }

    #[test]
    fn an_unchanged_pass_hands_back_the_same_list() {
        let mut p = ServiceProbe::new();
        drain(&mut p);
        // A service can genuinely change state between two passes; allow a few
        // tries before calling it a failure.
        let mut same = false;
        for _ in 0..5 {
            p.refresh();
            let a = p.list();
            p.refresh();
            let b = p.list();
            if Arc::ptr_eq(&a, &b) {
                same = true;
                break;
            }
        }
        assert!(same, "the list was rebuilt although nothing changed");
    }

    #[test]
    fn the_group_is_parsed_from_a_shared_host_command_line() {
        assert_eq!(
            svchost_group(r"C:\WINDOWS\system32\svchost.exe -k netsvcs -p"),
            Some("netsvcs")
        );
        assert_eq!(
            svchost_group(r#""C:\Windows\System32\svchost.exe" -k LocalServiceNetworkRestricted"#),
            Some("LocalServiceNetworkRestricted")
        );
        assert_eq!(
            svchost_group(r"C:\WINDOWS\System32\SVCHOST.EXE -K RPCSS"),
            Some("RPCSS")
        );
        assert_eq!(svchost_group(r"C:\WINDOWS\system32\svchost.exe -k"), None);
        assert_eq!(svchost_group(r"C:\WINDOWS\system32\svchost.exe"), None);
        assert_eq!(
            svchost_group(r"C:\Program Files\App\app.exe -k group"),
            None
        );
        assert_eq!(svchost_group(""), None);
    }

    #[test]
    fn starting_a_missing_service_is_gone() {
        assert!(matches!(
            start("no-such-service-xyz"),
            Err(ControlError::Gone)
        ));
        assert!(matches!(
            stop("no-such-service-xyz"),
            Err(ControlError::Gone)
        ));
    }

    #[test]
    fn stopping_a_system_service_unelevated_is_not_permitted() {
        if super::super::access::is_elevated() {
            eprintln!("note: elevated, so the access check is not exercised; skipping");
            return;
        }
        // RpcEptMapper never accepts stop, so even a wrong answer here could not
        // stop anything; the point is that the open is refused first.
        assert!(matches!(
            stop("RpcEptMapper"),
            Err(ControlError::NotPermitted)
        ));
    }

    /// Run with `cargo test -p ot-probe services::tests::timing -- --ignored
    /// --nocapture` to refresh the numbers in the module doc.
    #[test]
    #[ignore = "timing, prints numbers for the module doc"]
    fn timing() {
        let mut p = ServiceProbe::new();
        let t = Instant::now();
        p.refresh();
        let first = t.elapsed();
        let read_first = p.interned.len() - p.queue.len();
        let total = p.interned.len();
        let mut passes = 1;
        let t = Instant::now();
        while !p.queue.is_empty() {
            p.refresh();
            passes += 1;
        }
        let drained = t.elapsed();
        let t = Instant::now();
        for _ in 0..10 {
            p.refresh();
        }
        let steady = t.elapsed() / 10;
        let t = Instant::now();
        let scm = p.scm.as_ref().map(|h| h.0).expect("scm");
        let mut buf = Vec::new();
        let mut read = 0usize;
        for name in p.interned.keys() {
            read += usize::from(read_config(scm, name, &mut buf).is_some());
        }
        let all = t.elapsed();
        let t = Instant::now();
        p.enumerate(scm);
        let enumerate = t.elapsed();
        eprintln!(
            "{total} services; first refresh {first:?} ({read_first} configs read); \
             queue drained after {passes} passes, {drained:?} more; steady refresh {steady:?} \
             (enumeration alone {enumerate:?}); all {read} configs in one go {all:?}"
        );
    }
}
