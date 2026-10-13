//! Logon sessions, from systemd-logind through libsystemd's `sd-login` API.
//!
//! logind's own files under `/run/systemd/sessions` say "This is private data.
//! Do not parse."; `sd-login` is the supported way to read them. libsystemd is
//! loaded at run time rather than linked, so the binary still starts on a system
//! without it (Alpine, or a container) and just lists no sessions there.
//!
//! A session's name is its audit session id when the kernel has auditing, which
//! is what `/proc/<pid>/sessionid` holds, so processes group under the session
//! they belong to. Without auditing logind names sessions `c1`, `c2`; those get
//! ids with the top bit set, which no process carries.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::time::{Duration, Instant};

use ot_model::session::{SessionInfo, SessionState};

const REFRESH: Duration = Duration::from_secs(5);

type GetSessions = unsafe extern "C" fn(*mut *mut *mut c_char) -> c_int;
type GetUid = unsafe extern "C" fn(*const c_char, *mut libc::uid_t) -> c_int;
type GetString = unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> c_int;
type IsFlag = unsafe extern "C" fn(*const c_char) -> c_int;
type PidGetSession = unsafe extern "C" fn(libc::pid_t, *mut *mut c_char) -> c_int;

/// The `sd-login` functions this needs.
struct SdLogin {
    get_sessions: GetSessions,
    get_uid: GetUid,
    get_state: GetString,
    get_class: GetString,
    get_tty: GetString,
    get_seat: GetString,
    get_service: GetString,
    get_remote_host: GetString,
    is_remote: IsFlag,
    pid_get_session: PidGetSession,
}

impl std::fmt::Debug for SdLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SdLogin")
    }
}

impl SdLogin {
    fn load() -> Option<Self> {
        // SAFETY: dlopen with a NUL-terminated name. The handle is never closed:
        // the functions stay in use for the life of the process.
        let lib = unsafe { libc::dlopen(c"libsystemd.so.0".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return None;
        }
        let sym = |name: &CStr| -> Option<*mut c_void> {
            // SAFETY: a live handle and a NUL-terminated name.
            let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
            (!p.is_null()).then_some(p)
        };
        // SAFETY: each symbol is the sd-login function of that name, whose C
        // signature the type spells out.
        unsafe {
            Some(Self {
                get_sessions: std::mem::transmute::<*mut c_void, GetSessions>(sym(
                    c"sd_get_sessions",
                )?),
                get_uid: std::mem::transmute::<*mut c_void, GetUid>(sym(c"sd_session_get_uid")?),
                get_state: std::mem::transmute::<*mut c_void, GetString>(sym(
                    c"sd_session_get_state",
                )?),
                get_class: std::mem::transmute::<*mut c_void, GetString>(sym(
                    c"sd_session_get_class",
                )?),
                get_tty: std::mem::transmute::<*mut c_void, GetString>(sym(c"sd_session_get_tty")?),
                get_seat: std::mem::transmute::<*mut c_void, GetString>(sym(
                    c"sd_session_get_seat",
                )?),
                get_service: std::mem::transmute::<*mut c_void, GetString>(sym(
                    c"sd_session_get_service",
                )?),
                get_remote_host: std::mem::transmute::<*mut c_void, GetString>(sym(
                    c"sd_session_get_remote_host",
                )?),
                is_remote: std::mem::transmute::<*mut c_void, IsFlag>(sym(
                    c"sd_session_is_remote",
                )?),
                pid_get_session: std::mem::transmute::<*mut c_void, PidGetSession>(sym(
                    c"sd_pid_get_session",
                )?),
            })
        }
    }

    /// Call a function that hands back a string the caller must free.
    fn string(f: GetString, session: &CStr) -> Option<String> {
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: a NUL-terminated session name and a writable out-pointer.
        let rc = unsafe { f(session.as_ptr(), &raw mut out) };
        take_string(rc, out)
    }

    fn sessions(&self) -> Vec<CString> {
        let mut list: *mut *mut c_char = std::ptr::null_mut();
        // SAFETY: a writable out-pointer; on success a NULL-terminated array of
        // strings, each freed below, then the array.
        let n = unsafe { (self.get_sessions)(&raw mut list) };
        let mut out = Vec::new();
        if n < 0 || list.is_null() {
            return out;
        }
        let mut i = 0;
        loop {
            // SAFETY: within the NULL-terminated array.
            let p = unsafe { *list.add(i) };
            if p.is_null() {
                break;
            }
            // SAFETY: a NUL-terminated string sd-login allocated; freed once.
            out.push(unsafe { CStr::from_ptr(p) }.to_owned());
            unsafe { libc::free(p.cast()) };
            i += 1;
        }
        // SAFETY: the array sd-login allocated; freed once.
        unsafe { libc::free(list.cast()) };
        out
    }

    fn current(&self) -> Option<String> {
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: pid 0 is the caller; a writable out-pointer.
        let rc = unsafe { (self.pid_get_session)(0, &raw mut out) };
        take_string(rc, out)
    }
}

/// Own and free a string an sd-login call returned.
fn take_string(rc: c_int, p: *mut c_char) -> Option<String> {
    if rc < 0 || p.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string sd-login allocated; freed once.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { libc::free(p.cast()) };
    (!s.is_empty()).then_some(s)
}

/// A session's number from its name: `7` is 7; logind's own `c3` is 3 with the
/// top bit set.
fn session_id(name: &str) -> Option<u32> {
    match name.strip_prefix('c') {
        Some(n) => n.parse::<u32>().ok().map(|n| n | 0x8000_0000),
        None => name.parse().ok(),
    }
}

#[derive(Debug)]
pub(super) struct SessionProbe {
    sd: Option<SdLogin>,
    sessions: Vec<SessionInfo>,
    next: Option<Instant>,
}

impl SessionProbe {
    pub(super) fn new() -> Self {
        Self {
            sd: SdLogin::load(),
            sessions: Vec::new(),
            next: None,
        }
    }

    /// Whether sessions can be listed here.
    pub(super) fn available(&self) -> bool {
        self.sd.is_some()
    }

    pub(super) fn sample(&mut self, out: &mut Vec<SessionInfo>) {
        let now = Instant::now();
        if self.next.is_none_or(|t| now >= t) {
            self.next = Some(now + REFRESH);
            self.refresh();
        }
        out.clone_from(&self.sessions);
    }

    fn refresh(&mut self) {
        let Some(sd) = &self.sd else { return };
        let current = sd.current();
        self.sessions.clear();
        for name in sd.sessions() {
            let Some(id) = session_id(&name.to_string_lossy()) else {
                continue;
            };
            let class = SdLogin::string(sd.get_class, &name).unwrap_or_default();
            // The user's service manager and background jobs are not sign-ins.
            if class.starts_with("manager") || class.starts_with("background") {
                continue;
            }
            let greeter = class == "greeter" || class == "lock-screen";
            let mut uid: libc::uid_t = 0;
            // SAFETY: a NUL-terminated name and a writable uid.
            let has_uid = unsafe { (sd.get_uid)(name.as_ptr(), &raw mut uid) } >= 0;
            // SAFETY: a NUL-terminated name.
            let remote = unsafe { (sd.is_remote)(name.as_ptr()) } > 0;
            let station = if remote {
                SdLogin::string(sd.get_service, &name)
            } else {
                SdLogin::string(sd.get_tty, &name).or_else(|| SdLogin::string(sd.get_seat, &name))
            }
            .unwrap_or_default();
            let state = match SdLogin::string(sd.get_state, &name).as_deref() {
                _ if greeter => SessionState::Idle,
                Some("active") => SessionState::Active,
                // Signed in but not in front: a remote session is still in use; a
                // local one on a background console is like a disconnected one.
                Some("online") if remote => SessionState::Active,
                Some("online") => SessionState::Disconnected,
                _ => SessionState::Other,
            };
            let client = remote
                .then(|| SdLogin::string(sd.get_remote_host, &name))
                .flatten();
            let name = name.to_string_lossy();
            self.sessions.push(SessionInfo {
                id,
                user: (has_uid && !greeter).then(|| super::process::user_name(uid)),
                station,
                state,
                client,
                current: current.as_deref() == Some(&*name),
            });
        }
        self.sessions.sort_by_key(|s| s.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_become_ids() {
        assert_eq!(session_id("7"), Some(7));
        assert_eq!(session_id("c3"), Some(0x8000_0003));
        assert_eq!(session_id("x"), None);
    }

    #[test]
    fn listing_does_not_crash() {
        let mut p = SessionProbe::new();
        let mut out = Vec::new();
        p.sample(&mut out);
        for s in &out {
            assert!(s.user.is_none() || !s.user.as_deref().unwrap_or("").is_empty());
        }
    }
}
